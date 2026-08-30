//! local file transport for repository operations

use std::fmt;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use walkdir::WalkDir;

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::namespace::{inside_to_outside, outside_to_inside};
use crate::repo::Repo;

/// copy objects from source repo to destination repo
pub fn copy_objects(src: &Repo, dst: &Repo, hashes: &ObjectSet) -> Result<TransferStats> {
    let mut stats = TransferStats::default();

    for hash in &hashes.blobs {
        copy_object(src, dst, ObjectKind::Blob, hash, &mut stats)?;
    }
    for hash in &hashes.trees {
        copy_object(src, dst, ObjectKind::Tree, hash, &mut stats)?;
    }
    for hash in &hashes.commits {
        copy_object(src, dst, ObjectKind::Commit, hash, &mut stats)?;
    }

    Ok(stats)
}

fn copy_object(
    src: &Repo,
    dst: &Repo,
    kind: ObjectKind,
    hash: &Hash,
    stats: &mut TransferStats,
) -> Result<()> {
    if object_path(dst, kind, hash).exists() {
        stats.skipped += 1;
        return Ok(());
    }
    let (header, mut input) = open_transfer_object(src, kind, hash)?;
    if install_transfer_reader(dst, &header, &mut input)? {
        stats.bytes_transferred += header.size;
        stats.copied += 1;
    } else {
        stats.skipped += 1;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn read_transfer_object(
    repo: &Repo,
    kind: ObjectKind,
    hash: &Hash,
) -> Result<TransferObject> {
    let (header, mut input) = open_transfer_object(repo, kind, hash)?;
    let capacity = usize::try_from(header.size).map_err(|_| Error::Transport {
        message: format!("object {} is too large for this process", header.hash),
    })?;
    let mut data = Vec::with_capacity(capacity);
    input
        .read_to_end(&mut data)
        .with_path(object_path(repo, kind, hash))?;
    Ok(TransferObject {
        kind: header.kind,
        hash: header.hash,
        data,
        metadata: header.metadata,
    })
}

pub(crate) fn open_transfer_object(
    repo: &Repo,
    kind: ObjectKind,
    hash: &Hash,
) -> Result<(TransferObjectHeader, File)> {
    let path = object_path(repo, kind, hash);
    let input = File::open(&path).with_path(&path)?;
    let stored = input.metadata().with_path(&path)?;
    if !stored.is_file() {
        return Err(Error::CorruptObject(*hash));
    }
    let metadata = if kind == ObjectKind::Blob {
        Some(logical_blob_metadata(repo, &stored)?)
    } else {
        None
    };
    Ok((
        TransferObjectHeader {
            kind,
            hash: *hash,
            size: stored.len(),
            metadata,
        },
        input,
    ))
}

#[cfg(test)]
pub(crate) fn install_transfer_object(repo: &Repo, object: &TransferObject) -> Result<bool> {
    let header = TransferObjectHeader {
        kind: object.kind,
        hash: object.hash,
        size: object.data.len() as u64,
        metadata: object.metadata,
    };
    install_transfer_reader(repo, &header, &mut object.data.as_slice())
}

pub(crate) fn install_transfer_reader(
    repo: &Repo,
    header: &TransferObjectHeader,
    reader: &mut impl Read,
) -> Result<bool> {
    let destination = object_path(repo, header.kind, &header.hash);
    if destination.exists() {
        copy_transfer_bytes(reader, &mut io::sink(), header.size, None)?;
        return Ok(false);
    }

    let parent = object_parent(&destination)?;
    fs::create_dir_all(parent).with_path(parent)?;

    let temporary = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    let result = install_temporary(repo, header, reader, &temporary, &destination);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn install_temporary(
    repo: &Repo,
    header: &TransferObjectHeader,
    reader: &mut impl Read,
    temporary: &Path,
    destination: &Path,
) -> Result<bool> {
    let mut file = File::create(temporary).with_path(temporary)?;
    let actual = copy_transfer_bytes(reader, &mut file, header.size, Some(temporary))?;
    file.sync_all().with_path(temporary)?;

    if header.kind == ObjectKind::Blob {
        let metadata = header.metadata.ok_or_else(|| Error::Transport {
            message: format!("blob {} has no logical metadata", header.hash),
        })?;
        apply_blob_metadata(repo, temporary, metadata)?;
    } else if actual != header.hash {
        return Err(Error::CorruptObject(header.hash));
    }

    install_completed_file(temporary, destination)
}

fn copy_transfer_bytes(
    reader: &mut impl Read,
    writer: &mut impl Write,
    size: u64,
    output_path: Option<&Path>,
) -> Result<Hash> {
    const BUFFER_BYTES: usize = 64 * 1024;

    let mut remaining = size;
    let mut buffer = [0_u8; BUFFER_BYTES];
    let mut hasher = blake3::Hasher::new();
    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(BUFFER_BYTES as u64)).unwrap();
        let read = reader
            .read(&mut buffer[..wanted])
            .map_err(|source| Error::Transport {
                message: format!("failed to read object data: {source}"),
            })?;
        if read == 0 {
            return Err(Error::Transport {
                message: format!(
                    "object data ended after {} of {size} bytes",
                    size - remaining
                ),
            });
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|source| Error::Io {
                path: output_path
                    .unwrap_or_else(|| Path::new("transfer stream"))
                    .to_path_buf(),
                source,
            })?;
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(Hash::from_bytes(*hasher.finalize().as_bytes()))
}

fn apply_blob_metadata(repo: &Repo, path: &Path, metadata: BlobMetadata) -> Result<()> {
    let namespace = &repo.config().namespace;
    let uid = inside_to_outside(metadata.uid, &namespace.uid_map)
        .ok_or(Error::UnmappedUid(metadata.uid))?;
    let gid = inside_to_outside(metadata.gid, &namespace.gid_map)
        .ok_or(Error::UnmappedGid(metadata.gid))?;
    fs::set_permissions(path, Permissions::from_mode(metadata.mode & 0o7777)).with_path(path)?;
    let current_uid = nix::unistd::getuid().as_raw();
    let current_gid = nix::unistd::getgid().as_raw();
    if uid != current_uid || gid != current_gid {
        nix::unistd::chown(
            path,
            Some(nix::unistd::Uid::from_raw(uid)),
            Some(nix::unistd::Gid::from_raw(gid)),
        )
        .map_err(|error| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, error),
        })?;
    }
    Ok(())
}

fn install_completed_file(temporary: &Path, destination: &Path) -> Result<bool> {
    let installed = match fs::hard_link(temporary, destination) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            return Err(Error::Io {
                path: destination.to_path_buf(),
                source,
            })
        }
    };
    fs::remove_file(temporary).with_path(temporary)?;
    let parent = destination.parent().expect("checked parent");
    File::open(parent)
        .with_path(parent)?
        .sync_all()
        .with_path(parent)?;
    Ok(installed)
}

fn logical_blob_metadata(repo: &Repo, stored: &fs::Metadata) -> Result<BlobMetadata> {
    let namespace = &repo.config().namespace;
    Ok(BlobMetadata {
        uid: outside_to_inside(stored.uid(), &namespace.uid_map)
            .ok_or(Error::UnmappedUid(stored.uid()))?,
        gid: outside_to_inside(stored.gid(), &namespace.gid_map)
            .ok_or(Error::UnmappedGid(stored.gid()))?,
        mode: stored.mode(),
    })
}

fn object_parent(path: &Path) -> Result<&Path> {
    path.parent().ok_or_else(|| Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other("object path has no parent"),
    })
}

pub(crate) fn remove_objects(repo: &Repo, objects: &ObjectSet) -> Result<()> {
    for (kind, hashes) in [
        (ObjectKind::Blob, &objects.blobs),
        (ObjectKind::Tree, &objects.trees),
        (ObjectKind::Commit, &objects.commits),
    ] {
        for hash in hashes {
            let path = object_path(repo, kind, hash);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Error::Io {
                        path,
                        source: error,
                    })
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn object_path(repo: &Repo, kind: ObjectKind, hash: &Hash) -> std::path::PathBuf {
    let (prefix, suffix) = hash.to_path_components();
    let base = match kind {
        ObjectKind::Blob => repo.blobs_path(),
        ObjectKind::Tree => repo.trees_path(),
        ObjectKind::Commit => repo.commits_path(),
    };
    base.join(prefix).join(suffix)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectKind {
    Blob,
    Tree,
    Commit,
}

impl ObjectKind {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "blob" => Ok(Self::Blob),
            "tree" => Ok(Self::Tree),
            "commit" => Ok(Self::Commit),
            _ => Err(Error::InvalidObjectType(value.to_string())),
        }
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlobMetadata {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) mode: u32,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransferObject {
    pub(crate) kind: ObjectKind,
    pub(crate) hash: Hash,
    pub(crate) data: Vec<u8>,
    pub(crate) metadata: Option<BlobMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransferObjectHeader {
    pub(crate) kind: ObjectKind,
    pub(crate) hash: Hash,
    pub(crate) size: u64,
    pub(crate) metadata: Option<BlobMetadata>,
}

/// list all objects in a repository
pub fn list_all_objects(repo: &Repo) -> Result<ObjectSet> {
    Ok(ObjectSet {
        blobs: list_objects_in_dir(&repo.blobs_path())?,
        trees: list_objects_in_dir(&repo.trees_path())?,
        commits: list_objects_in_dir(&repo.commits_path())?,
    })
}

/// list objects in a directory
fn list_objects_in_dir(dir: &Path) -> Result<Vec<Hash>> {
    let mut hashes = Vec::new();

    if !dir.exists() {
        return Ok(hashes);
    }

    for entry in WalkDir::new(dir).min_depth(2).max_depth(2) {
        let entry = entry.map_err(|e| crate::Error::Io {
            path: dir.to_path_buf(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let parent_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");

        let hex = format!("{}{}", parent_name, file_name);
        if let Ok(hash) = Hash::from_hex(&hex) {
            hashes.push(hash);
        }
    }

    Ok(hashes)
}

/// set of objects for transfer
#[derive(Debug, Default, Clone)]
pub struct ObjectSet {
    pub blobs: Vec<Hash>,
    pub trees: Vec<Hash>,
    pub commits: Vec<Hash>,
}

impl ObjectSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty() && self.trees.is_empty() && self.commits.is_empty()
    }

    pub fn total_count(&self) -> usize {
        self.blobs.len() + self.trees.len() + self.commits.len()
    }

    pub(crate) fn push(&mut self, kind: ObjectKind, hash: Hash) {
        match kind {
            ObjectKind::Blob => self.blobs.push(hash),
            ObjectKind::Tree => self.trees.push(hash),
            ObjectKind::Commit => self.commits.push(hash),
        }
    }
}

/// transfer statistics
#[derive(Debug, Default, Clone)]
pub struct TransferStats {
    pub copied: usize,
    pub hardlinked: usize,
    pub skipped: usize,
    pub bytes_transferred: u64,
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::ops::commit;
    use tempfile::tempdir;

    #[test]
    fn test_list_objects() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let objects = list_all_objects(&repo).unwrap();

        assert!(!objects.blobs.is_empty());
        assert!(!objects.trees.is_empty());
        assert!(!objects.commits.is_empty());
    }

    #[test]
    fn test_copy_objects() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&src, &source, "test", None, None).unwrap();

        let objects = list_all_objects(&src).unwrap();
        let stats = copy_objects(&src, &dst, &objects).unwrap();

        assert!(stats.copied > 0 || stats.hardlinked > 0);

        // verify objects exist in destination
        let dst_objects = list_all_objects(&dst).unwrap();
        assert_eq!(objects.blobs.len(), dst_objects.blobs.len());
        assert_eq!(objects.trees.len(), dst_objects.trees.len());
        assert_eq!(objects.commits.len(), dst_objects.commits.len());
    }

    #[test]
    fn test_transfer_object_preserves_blob_metadata() {
        let dir = tempdir().unwrap();
        let source = Repo::init(&dir.path().join("source-repo")).unwrap();
        let destination = Repo::init(&dir.path().join("destination-repo")).unwrap();
        let namespace = &source.config().namespace;
        let current_uid = nix::unistd::getuid().as_raw();
        let current_gid = nix::unistd::getgid().as_raw();
        let uid = outside_to_inside(current_uid, &namespace.uid_map).unwrap();
        let gid = outside_to_inside(current_gid, &namespace.gid_map).unwrap();
        let hash = crate::write_blob(&source, b"content", uid, gid, 0o100755, &[]).unwrap();
        let object = read_transfer_object(&source, ObjectKind::Blob, &hash).unwrap();

        assert!(install_transfer_object(&destination, &object).unwrap());

        let copied = fs::metadata(object_path(&destination, ObjectKind::Blob, &hash)).unwrap();
        assert_eq!(copied.mode() & 0o7777, 0o755);
        assert_eq!(copied.uid(), current_uid);
        assert_eq!(copied.gid(), current_gid);
    }

    #[test]
    fn transfer_reader_bounds_memory_for_an_object_larger_than_the_process_budget() {
        const SIZE: u64 = 129 * 1024 * 1024;

        let dir = tempdir().unwrap();
        let repo = Repo::init(&dir.path().join("repo")).unwrap();
        let namespace = &repo.config().namespace;
        let uid = outside_to_inside(nix::unistd::getuid().as_raw(), &namespace.uid_map).unwrap();
        let gid = outside_to_inside(nix::unistd::getgid().as_raw(), &namespace.gid_map).unwrap();
        let largest_request = Rc::new(Cell::new(0));
        let mut reader = GeneratedReader {
            remaining: SIZE,
            largest_request: Rc::clone(&largest_request),
        };
        let hash = Hash::from_bytes([0x5a; 32]);
        let header = TransferObjectHeader {
            kind: ObjectKind::Blob,
            hash,
            size: SIZE,
            metadata: Some(BlobMetadata {
                uid,
                gid,
                mode: 0o100644,
            }),
        };

        assert!(install_transfer_reader(&repo, &header, &mut reader).unwrap());
        assert_eq!(
            fs::metadata(object_path(&repo, ObjectKind::Blob, &hash))
                .unwrap()
                .len(),
            SIZE
        );
        assert!(largest_request.get() <= 64 * 1024);
    }

    struct GeneratedReader {
        remaining: u64,
        largest_request: Rc<Cell<usize>>,
    }

    impl Read for GeneratedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.largest_request
                .set(self.largest_request.get().max(buffer.len()));
            let read = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap();
            buffer[..read].fill(0x6d);
            self.remaining -= read as u64;
            Ok(read)
        }
    }
}
