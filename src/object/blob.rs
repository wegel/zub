use std::fs::{self, File, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nix::unistd::{Gid, Uid};

use crate::error::{Error, IoResultExt, Result};
use crate::hash::{compute_blob_hash, Hash};
use crate::namespace::inside_to_outside;
use crate::repo::Repo;
use crate::types::Xattr;

/// write a blob to the object store
///
/// the hash is computed over INSIDE (logical namespace) uid/gid values.
/// the file is stored with OUTSIDE (on-disk) uid/gid values.
///
/// returns the blob hash, which can be used to reference this blob.
pub fn write_blob(
    repo: &Repo,
    content: &[u8],
    inside_uid: u32,
    inside_gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<Hash> {
    let hash = compute_blob_hash(inside_uid, inside_gid, mode, xattrs, content);

    let (dir, file) = hash.to_path_components();
    let blob_dir = repo.blobs_path().join(&dir);
    let blob_path = blob_dir.join(&file);

    // deduplication: if blob already exists, we're done
    if blob_path.exists() {
        return Ok(hash);
    }

    // convert inside uid/gid to outside values for storage
    let ns = &repo.config().namespace;
    let outside_uid = inside_to_outside(inside_uid, &ns.uid_map)
        .ok_or(Error::UnmappedUid(inside_uid))?;
    let outside_gid = inside_to_outside(inside_gid, &ns.gid_map)
        .ok_or(Error::UnmappedGid(inside_gid))?;

    // ensure directory exists
    fs::create_dir_all(&blob_dir).with_path(&blob_dir)?;

    // atomic write: temp file -> set metadata -> fsync -> rename
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());

    // write content
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        tmp_file.write_all(content).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    // set permissions (before chown, so we have write access)
    fs::set_permissions(&tmp_path, Permissions::from_mode(mode & 0o7777)).with_path(&tmp_path)?;

    // set ownership (skip if already matches to avoid permission errors when not root)
    let current_uid = nix::unistd::getuid().as_raw();
    let current_gid = nix::unistd::getgid().as_raw();
    if outside_uid != current_uid || outside_gid != current_gid {
        nix::unistd::chown(&tmp_path, Some(Uid::from_raw(outside_uid)), Some(Gid::from_raw(outside_gid)))
            .map_err(|e| Error::Io {
                path: tmp_path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
            })?;
    }

    // set xattrs
    for xattr in xattrs {
        xattr::set(&tmp_path, &xattr.name, &xattr.value).map_err(|e| Error::Xattr {
            path: tmp_path.clone(),
            message: format!("failed to set {}: {}", xattr.name, e),
        })?;
    }

    // rename to final location
    fs::rename(&tmp_path, &blob_path).with_path(&blob_path)?;

    // fsync parent directory
    fsync_dir(&blob_dir)?;

    Ok(hash)
}

/// write a blob with streaming content (for large files)
pub fn write_blob_streaming<R: Read>(
    repo: &Repo,
    reader: &mut R,
    inside_uid: u32,
    inside_gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<Hash> {
    // for streaming, we need to write to temp first, then compute hash
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());

    // write content to temp file while computing hash
    let mut hasher = crate::hash::BlobHasher::new(inside_uid, inside_gid, mode, xattrs);
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        let mut buf = [0u8; 64 * 1024]; // 64KB buffer
        loop {
            let n = reader.read(&mut buf).with_path(&tmp_path)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            tmp_file.write_all(&buf[..n]).with_path(&tmp_path)?;
        }
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    let hash = hasher.finalize();

    let (dir, file) = hash.to_path_components();
    let blob_dir = repo.blobs_path().join(&dir);
    let blob_path = blob_dir.join(&file);

    // dedup check
    if blob_path.exists() {
        fs::remove_file(&tmp_path).with_path(&tmp_path)?;
        return Ok(hash);
    }

    // convert uid/gid
    let ns = &repo.config().namespace;
    let outside_uid =
        inside_to_outside(inside_uid, &ns.uid_map).ok_or(Error::UnmappedUid(inside_uid))?;
    let outside_gid =
        inside_to_outside(inside_gid, &ns.gid_map).ok_or(Error::UnmappedGid(inside_gid))?;

    // ensure directory exists
    fs::create_dir_all(&blob_dir).with_path(&blob_dir)?;

    // set metadata
    fs::set_permissions(&tmp_path, Permissions::from_mode(mode & 0o7777)).with_path(&tmp_path)?;
    let current_uid = nix::unistd::getuid().as_raw();
    let current_gid = nix::unistd::getgid().as_raw();
    if outside_uid != current_uid || outside_gid != current_gid {
        nix::unistd::chown(
            &tmp_path,
            Some(Uid::from_raw(outside_uid)),
            Some(Gid::from_raw(outside_gid)),
        )
        .map_err(|e| Error::Io {
            path: tmp_path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
        })?;
    }

    for xattr in xattrs {
        xattr::set(&tmp_path, &xattr.name, &xattr.value).map_err(|e| Error::Xattr {
            path: tmp_path.clone(),
            message: format!("failed to set {}: {}", xattr.name, e),
        })?;
    }

    // rename to final location
    fs::rename(&tmp_path, &blob_path).with_path(&blob_path)?;
    fsync_dir(&blob_dir)?;

    Ok(hash)
}

/// get the filesystem path to a blob
pub fn blob_path(repo: &Repo, hash: &Hash) -> PathBuf {
    let (dir, file) = hash.to_path_components();
    repo.blobs_path().join(dir).join(file)
}

/// check if a blob exists in the object store
pub fn blob_exists(repo: &Repo, hash: &Hash) -> bool {
    blob_path(repo, hash).exists()
}

/// read blob content
pub fn read_blob(repo: &Repo, hash: &Hash) -> Result<Vec<u8>> {
    let path = blob_path(repo, hash);
    fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*hash)
        } else {
            Error::Io { path, source: e }
        }
    })
}

/// read blob content into a writer (streaming)
pub fn read_blob_to<W: Write>(repo: &Repo, hash: &Hash, writer: &mut W) -> Result<u64> {
    let path = blob_path(repo, hash);
    let mut file = File::open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*hash)
        } else {
            Error::Io {
                path: path.clone(),
                source: e,
            }
        }
    })?;

    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf).with_path(&path)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).with_path(&path)?;
        total += n as u64;
    }
    Ok(total)
}

/// fsync a directory
fn fsync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path).with_path(path)?;
    dir.sync_all().with_path(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    fn current_ids() -> (u32, u32) {
        (nix::unistd::getuid().as_raw(), nix::unistd::getgid().as_raw())
    }

    #[test]
    fn test_write_and_read_blob() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let content = b"hello, world!";
        let hash = write_blob(&repo, content, uid, gid, 0o644, &[]).unwrap();

        // verify it exists
        assert!(blob_exists(&repo, &hash));

        // read it back
        let read_content = read_blob(&repo, &hash).unwrap();
        assert_eq!(read_content, content);
    }

    #[test]
    fn test_blob_deduplication() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let content = b"duplicate content";
        let h1 = write_blob(&repo, content, uid, gid, 0o644, &[]).unwrap();
        let h2 = write_blob(&repo, content, uid, gid, 0o644, &[]).unwrap();

        assert_eq!(h1, h2);
    }

    #[test]
    fn test_different_mode_different_blob() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let content = b"same content";
        let h1 = write_blob(&repo, content, uid, gid, 0o644, &[]).unwrap();
        let h2 = write_blob(&repo, content, uid, gid, 0o755, &[]).unwrap();

        assert_ne!(h1, h2);
        assert!(blob_exists(&repo, &h1));
        assert!(blob_exists(&repo, &h2));
    }

    #[test]
    fn test_blob_path_structure() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let hash = write_blob(&repo, b"test", uid, gid, 0o644, &[]).unwrap();
        let path = blob_path(&repo, &hash);

        // path should be blobs/XX/YYYY...
        let hex = hash.to_hex();
        assert!(path.ends_with(&format!("{}/{}", &hex[..2], &hex[2..])));
    }

    #[test]
    fn test_read_nonexistent_blob() {
        let (_dir, repo) = test_repo();

        let fake_hash =
            Hash::from_hex("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap();
        let result = read_blob(&repo, &fake_hash);

        assert!(matches!(result, Err(Error::ObjectNotFound(_))));
    }

    #[test]
    fn test_blob_with_xattrs() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let xattrs = vec![Xattr::new("user.test", vec![1, 2, 3])];
        let h1 = write_blob(&repo, b"content", uid, gid, 0o644, &xattrs).unwrap();

        // same content without xattrs should have different hash
        let h2 = write_blob(&repo, b"content", uid, gid, 0o644, &[]).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_streaming_write() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let content = b"streaming content test";
        let mut cursor = std::io::Cursor::new(content.as_slice());

        let hash = write_blob_streaming(&repo, &mut cursor, uid, gid, 0o644, &[]).unwrap();

        // should match non-streaming hash
        let expected_hash = write_blob(&repo, content, uid, gid, 0o644, &[]).unwrap();
        assert_eq!(hash, expected_hash);
    }

    #[test]
    fn test_read_blob_to_writer() {
        let (_dir, repo) = test_repo();
        let (uid, gid) = current_ids();

        let content = b"content to stream out";
        let hash = write_blob(&repo, content, uid, gid, 0o644, &[]).unwrap();

        let mut output = Vec::new();
        let bytes_read = read_blob_to(&repo, &hash, &mut output).unwrap();

        assert_eq!(bytes_read, content.len() as u64);
        assert_eq!(output, content);
    }
}
