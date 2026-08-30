use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::error::{Error, IoResultExt, Result};
use crate::hash::{BlobHasher, Hash, SYMLINK_MODE};
use crate::namespace::outside_to_inside;
use crate::types::{EntryKind, Xattr};
use crate::Repo;

use super::{blob_path, read_commit, read_tree};

const VERIFY_BUFFER_BYTES: usize = 64 * 1024;
const VERIFY_INDEX_MAGIC: &[u8] = b"zub-verified-blobs-v1\n";
const VERIFY_INDEX_RECORD_BYTES: usize = 100;

/// Verify one commit and every object reachable from its root tree.
///
/// Parent IDs remain part of the commit hash, but this check does not require
/// parent objects. A remote may transfer one current snapshot without its
/// complete ref history.
pub fn verify_commit(repo: &Repo, commit_hash: &Hash) -> Result<()> {
    verify_commits(repo, std::slice::from_ref(commit_hash), 1)
}

/// Verify several commits while reading each shared tree and blob once.
pub fn verify_commits(repo: &Repo, commit_hashes: &[Hash], workers: usize) -> Result<()> {
    let checks = collect_blob_checks(repo, commit_hashes)?;
    if checks.is_empty() {
        return Ok(());
    }
    let mut cached = VerificationIndexReader::open(&verification_index_path(repo));
    let mut pending = Vec::new();
    for (index, (hash, _)) in checks.iter().enumerate() {
        let fingerprint = blob_fingerprint(repo, hash)?;
        let stored = cached.as_mut().and_then(|reader| reader.find(*hash));
        if stored != Some(fingerprint) {
            pending.push(index);
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let worker_count = workers.max(1).min(pending.len());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .stack_size(256 * 1024)
        .build()
        .map_err(|error| Error::VerifyPool(error.to_string()))?;
    let results = pool.install(|| {
        pending
            .par_iter()
            .map(|index| {
                let (hash, check) = &checks[*index];
                verify_blob_uncached(repo, hash, &check.xattrs, check.symlink).map(|fingerprint| {
                    VerificationIndexRecord {
                        hash: *hash,
                        fingerprint,
                    }
                })
            })
            .collect::<Vec<_>>()
    });
    let mut updates = Vec::with_capacity(results.len());
    for result in results {
        updates.push(result?);
    }
    let _ = write_verification_index(repo, &updates);
    Ok(())
}

#[derive(Clone)]
struct BlobCheck {
    xattrs: Vec<Xattr>,
    symlink: bool,
}

fn collect_blob_checks(repo: &Repo, commit_hashes: &[Hash]) -> Result<Vec<(Hash, BlobCheck)>> {
    let mut pending = Vec::new();
    let mut commits = HashSet::new();
    for hash in commit_hashes {
        if commits.insert(*hash) {
            pending.push(read_commit(repo, hash)?.tree);
        }
    }
    let mut trees = HashSet::new();
    let mut blobs = HashSet::new();
    let mut checks = Vec::new();
    while let Some(tree_hash) = pending.pop() {
        if !trees.insert(tree_hash) {
            continue;
        }
        collect_tree(repo, tree_hash, &mut pending, &mut blobs, &mut checks)?;
    }
    checks.sort_unstable_by_key(|(hash, _)| *hash);
    Ok(checks)
}

fn collect_tree(
    repo: &Repo,
    tree_hash: Hash,
    pending: &mut Vec<Hash>,
    blobs: &mut HashSet<Hash>,
    checks: &mut Vec<(Hash, BlobCheck)>,
) -> Result<()> {
    let tree = read_tree(repo, &tree_hash)?;
    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, xattrs, .. } => {
                if blobs.insert(*hash) {
                    checks.push((
                        *hash,
                        BlobCheck {
                            xattrs: xattrs.clone(),
                            symlink: false,
                        },
                    ));
                }
            }
            EntryKind::Symlink { hash, xattrs } => {
                if blobs.insert(*hash) {
                    checks.push((
                        *hash,
                        BlobCheck {
                            xattrs: xattrs.clone(),
                            symlink: true,
                        },
                    ));
                }
            }
            EntryKind::Directory { hash, .. } => pending.push(*hash),
            EntryKind::BlockDevice { .. }
            | EntryKind::CharDevice { .. }
            | EntryKind::Fifo { .. }
            | EntryKind::Socket { .. }
            | EntryKind::Hardlink { .. } => {}
        }
    }
    Ok(())
}

pub(crate) fn verify_blob(
    repo: &Repo,
    expected: &Hash,
    xattrs: &[Xattr],
    symlink: bool,
) -> Result<()> {
    verify_blob_uncached(repo, expected, xattrs, symlink).map(|_| ())
}

fn verify_blob_uncached(
    repo: &Repo,
    expected: &Hash,
    xattrs: &[Xattr],
    symlink: bool,
) -> Result<BlobFingerprint> {
    let path = blob_path(repo, expected);
    let metadata = read_blob_metadata(&path, expected)?;
    if !metadata.is_file() {
        return Err(Error::CorruptObject(*expected));
    }

    let namespace = &repo.config().namespace;
    let uid = outside_to_inside(metadata.uid(), &namespace.uid_map)
        .ok_or(Error::UnmappedUid(metadata.uid()))?;
    let gid = outside_to_inside(metadata.gid(), &namespace.gid_map)
        .ok_or(Error::UnmappedGid(metadata.gid()))?;
    let mode = if symlink {
        SYMLINK_MODE
    } else {
        metadata.mode()
    };
    let actual = hash_file(&path, *expected, uid, gid, mode, xattrs, symlink)?;
    if actual != *expected {
        return Err(Error::CorruptObject(*expected));
    }
    let before = BlobFingerprint::from_metadata(&metadata);
    let after = BlobFingerprint::from_metadata(&read_blob_metadata(&path, expected)?);
    if before != after {
        return Err(Error::CorruptObject(*expected));
    }
    Ok(after)
}

fn read_blob_metadata(path: &Path, expected: &Hash) -> Result<Metadata> {
    fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*expected)
        } else {
            Error::Io {
                path: path.to_path_buf(),
                source: error,
            }
        }
    })
}

fn blob_fingerprint(repo: &Repo, expected: &Hash) -> Result<BlobFingerprint> {
    let path = blob_path(repo, expected);
    let metadata = read_blob_metadata(&path, expected)?;
    if !metadata.is_file() {
        return Err(Error::CorruptObject(*expected));
    }
    Ok(BlobFingerprint::from_metadata(&metadata))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlobFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    uid: u32,
    gid: u32,
    mode: u32,
}

impl BlobFingerprint {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VerificationIndexRecord {
    hash: Hash,
    fingerprint: BlobFingerprint,
}

struct VerificationIndexReader {
    reader: BufReader<File>,
    current: Option<VerificationIndexRecord>,
    previous: Option<Hash>,
    exhausted: bool,
}

impl VerificationIndexReader {
    fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);
        let mut magic = vec![0; VERIFY_INDEX_MAGIC.len()];
        reader.read_exact(&mut magic).ok()?;
        if magic != VERIFY_INDEX_MAGIC {
            return None;
        }
        Some(Self {
            reader,
            current: None,
            previous: None,
            exhausted: false,
        })
    }

    fn find(&mut self, target: Hash) -> Option<BlobFingerprint> {
        loop {
            let record = match self.current.take() {
                Some(record) => record,
                None => self.next_record()?,
            };
            if record.hash < target {
                continue;
            }
            if record.hash == target {
                return Some(record.fingerprint);
            }
            self.current = Some(record);
            return None;
        }
    }

    fn next_record(&mut self) -> Option<VerificationIndexRecord> {
        if self.exhausted {
            return None;
        }
        let mut bytes = [0; VERIFY_INDEX_RECORD_BYTES];
        match self.reader.read(&mut bytes[..1]) {
            Ok(0) => {
                self.exhausted = true;
                return None;
            }
            Ok(1) => {}
            Ok(_) => unreachable!("one-byte read returned more than one byte"),
            Err(_) => {
                self.exhausted = true;
                return None;
            }
        }
        if self.reader.read_exact(&mut bytes[1..]).is_err() {
            self.exhausted = true;
            return None;
        }
        let record = decode_verification_record(&bytes);
        if self
            .previous
            .is_some_and(|previous| record.hash <= previous)
        {
            self.exhausted = true;
            return None;
        }
        self.previous = Some(record.hash);
        Some(record)
    }
}

fn verification_index_path(repo: &Repo) -> PathBuf {
    repo.index_path().join("verified-blobs-v1")
}

fn write_verification_index(repo: &Repo, updates: &[VerificationIndexRecord]) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let index_path = verification_index_path(repo);
    let parent = index_path.parent().expect("verification index parent");
    fs::create_dir_all(parent).with_path(parent)?;
    let temporary = repo
        .tmp_path()
        .join(format!("verified-blobs-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = File::create(&temporary).with_path(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(VERIFY_INDEX_MAGIC).with_path(&temporary)?;
        let mut existing = VerificationIndexReader::open(&index_path);
        let mut old = existing
            .as_mut()
            .and_then(VerificationIndexReader::next_record);
        for update in updates {
            while old.is_some_and(|record| record.hash < update.hash) {
                write_verification_record(&mut writer, &temporary, &old.expect("old record"))?;
                old = existing
                    .as_mut()
                    .and_then(VerificationIndexReader::next_record);
            }
            if old.is_some_and(|record| record.hash == update.hash) {
                old = existing
                    .as_mut()
                    .and_then(VerificationIndexReader::next_record);
            }
            write_verification_record(&mut writer, &temporary, update)?;
        }
        while let Some(record) = old {
            write_verification_record(&mut writer, &temporary, &record)?;
            old = existing
                .as_mut()
                .and_then(VerificationIndexReader::next_record);
        }
        writer.flush().with_path(&temporary)?;
        drop(writer);
        fs::rename(&temporary, &index_path).with_path(&index_path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_verification_record(
    writer: &mut impl Write,
    path: &Path,
    record: &VerificationIndexRecord,
) -> Result<()> {
    writer
        .write_all(&encode_verification_record(record))
        .with_path(path)
}

fn encode_verification_record(record: &VerificationIndexRecord) -> [u8; VERIFY_INDEX_RECORD_BYTES] {
    let mut bytes = [0; VERIFY_INDEX_RECORD_BYTES];
    let mut offset = 0;
    put_bytes(&mut bytes, &mut offset, record.hash.as_bytes());
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.device.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.inode.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.size.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.modified_seconds.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.modified_nanoseconds.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.changed_seconds.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.changed_nanoseconds.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.uid.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.gid.to_le_bytes(),
    );
    put_bytes(
        &mut bytes,
        &mut offset,
        &record.fingerprint.mode.to_le_bytes(),
    );
    debug_assert_eq!(offset, VERIFY_INDEX_RECORD_BYTES);
    bytes
}

fn decode_verification_record(bytes: &[u8; VERIFY_INDEX_RECORD_BYTES]) -> VerificationIndexRecord {
    let mut offset = 0;
    let hash = Hash::from_bytes(take_bytes(bytes, &mut offset));
    let fingerprint = BlobFingerprint {
        device: u64::from_le_bytes(take_bytes(bytes, &mut offset)),
        inode: u64::from_le_bytes(take_bytes(bytes, &mut offset)),
        size: u64::from_le_bytes(take_bytes(bytes, &mut offset)),
        modified_seconds: i64::from_le_bytes(take_bytes(bytes, &mut offset)),
        modified_nanoseconds: i64::from_le_bytes(take_bytes(bytes, &mut offset)),
        changed_seconds: i64::from_le_bytes(take_bytes(bytes, &mut offset)),
        changed_nanoseconds: i64::from_le_bytes(take_bytes(bytes, &mut offset)),
        uid: u32::from_le_bytes(take_bytes(bytes, &mut offset)),
        gid: u32::from_le_bytes(take_bytes(bytes, &mut offset)),
        mode: u32::from_le_bytes(take_bytes(bytes, &mut offset)),
    };
    debug_assert_eq!(offset, VERIFY_INDEX_RECORD_BYTES);
    VerificationIndexRecord { hash, fingerprint }
}

fn put_bytes<const N: usize>(target: &mut [u8], offset: &mut usize, bytes: &[u8; N]) {
    target[*offset..*offset + N].copy_from_slice(bytes);
    *offset += N;
}

fn take_bytes<const N: usize>(source: &[u8], offset: &mut usize) -> [u8; N] {
    let mut bytes = [0; N];
    bytes.copy_from_slice(&source[*offset..*offset + N]);
    *offset += N;
    bytes
}

fn hash_file(
    path: &Path,
    expected: Hash,
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: &[Xattr],
    require_utf8: bool,
) -> Result<Hash> {
    let mut file = File::open(path).with_path(path)?;
    let mut hasher = BlobHasher::new(uid, gid, mode, xattrs);
    let mut buffer = vec![0; VERIFY_BUFFER_BYTES];
    let mut utf8_tail = Vec::with_capacity(4);

    loop {
        let length = file.read(&mut buffer).with_path(path)?;
        if length == 0 {
            break;
        }
        let bytes = &buffer[..length];
        hasher.update(bytes);
        if require_utf8 && validate_utf8(&mut utf8_tail, bytes).is_err() {
            return Err(Error::CorruptObject(expected));
        }
    }
    if require_utf8 && !utf8_tail.is_empty() {
        return Err(Error::CorruptObject(expected));
    }
    Ok(hasher.finalize())
}

fn validate_utf8(tail: &mut Vec<u8>, bytes: &[u8]) -> std::result::Result<(), ()> {
    tail.extend_from_slice(bytes);
    match std::str::from_utf8(tail) {
        Ok(_) => tail.clear(),
        Err(error) if error.error_len().is_none() => {
            let suffix = tail.split_off(error.valid_up_to());
            *tail = suffix;
        }
        Err(_) => return Err(()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::hash::compute_blob_hash;
    use crate::namespace::outside_to_inside;
    use crate::object::{blob_path, write_blob, write_commit, write_tree};
    use crate::types::{Commit, Tree, TreeEntry};

    #[test]
    fn verifies_blob_bytes_mode_ownership_and_xattrs() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repo::init(&temporary.path().join("repo")).unwrap();
        let namespace = &repo.config().namespace;
        let metadata = fs::metadata(temporary.path()).unwrap();
        let uid = outside_to_inside(metadata.uid(), &namespace.uid_map).unwrap();
        let gid = outside_to_inside(metadata.gid(), &namespace.gid_map).unwrap();
        let xattrs = vec![Xattr::new("user.test", b"value".to_vec())];
        let blob = write_blob(&repo, b"payload", uid, gid, 0o100755, &xattrs).unwrap();
        let tree = write_tree(
            &repo,
            &Tree::new(vec![TreeEntry::new(
                "probe",
                EntryKind::regular(blob, 7, xattrs),
            )])
            .unwrap(),
        )
        .unwrap();
        let commit = write_commit(
            &repo,
            &Commit::with_timestamp(tree, vec![], "test", 0, "fixture"),
        )
        .unwrap();

        verify_commit(&repo, &commit).unwrap();

        fs::write(blob_path(&repo, &blob), b"corrupt").unwrap();
        assert!(matches!(
            verify_commit(&repo, &commit),
            Err(Error::CorruptObject(hash)) if hash == blob
        ));
    }

    #[test]
    fn streams_content_and_validates_utf8_across_buffer_boundaries() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("blob");
        let mut bytes = vec![b'a'; VERIFY_BUFFER_BYTES - 1];
        bytes.extend_from_slice("é".as_bytes());
        bytes.extend_from_slice(&vec![b'b'; VERIFY_BUFFER_BYTES]);
        fs::write(&path, &bytes).unwrap();
        let expected = compute_blob_hash(0, 0, SYMLINK_MODE, &[], &bytes);

        assert_eq!(
            hash_file(&path, expected, 0, 0, SYMLINK_MODE, &[], true).unwrap(),
            expected
        );
    }

    #[test]
    fn rejects_invalid_streamed_symlink_utf8() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("blob");
        let bytes = [0xff];
        fs::write(&path, bytes).unwrap();
        let expected = compute_blob_hash(0, 0, SYMLINK_MODE, &[], &bytes);

        assert!(matches!(
            hash_file(&path, expected, 0, 0, SYMLINK_MODE, &[], true),
            Err(Error::CorruptObject(hash)) if hash == expected
        ));
    }

    #[test]
    fn collects_shared_commit_objects_once() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repo::init(&temporary.path().join("repo")).unwrap();
        let metadata = fs::metadata(temporary.path()).unwrap();
        let namespace = &repo.config().namespace;
        let uid = outside_to_inside(metadata.uid(), &namespace.uid_map).unwrap();
        let gid = outside_to_inside(metadata.gid(), &namespace.gid_map).unwrap();
        let blob = write_blob(&repo, b"shared", uid, gid, 0o100644, &[]).unwrap();
        let tree = write_tree(
            &repo,
            &Tree::new(vec![TreeEntry::new(
                "shared",
                EntryKind::regular(blob, 6, vec![]),
            )])
            .unwrap(),
        )
        .unwrap();
        let first = write_commit(
            &repo,
            &Commit::with_timestamp(tree, vec![], "test", 0, "first"),
        )
        .unwrap();
        let second = write_commit(
            &repo,
            &Commit::with_timestamp(tree, vec![], "test", 1, "second"),
        )
        .unwrap();

        let checks = collect_blob_checks(&repo, &[first, second]).unwrap();

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].0, blob);
    }

    #[test]
    fn rebuilds_a_missing_or_invalid_verification_index() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repo::init(&temporary.path().join("repo")).unwrap();
        let metadata = fs::metadata(temporary.path()).unwrap();
        let namespace = &repo.config().namespace;
        let uid = outside_to_inside(metadata.uid(), &namespace.uid_map).unwrap();
        let gid = outside_to_inside(metadata.gid(), &namespace.gid_map).unwrap();
        let blob = write_blob(&repo, b"indexed", uid, gid, 0o100644, &[]).unwrap();
        let tree = write_tree(
            &repo,
            &Tree::new(vec![TreeEntry::new(
                "indexed",
                EntryKind::regular(blob, 7, vec![]),
            )])
            .unwrap(),
        )
        .unwrap();
        let commit = write_commit(
            &repo,
            &Commit::with_timestamp(tree, vec![], "test", 0, "fixture"),
        )
        .unwrap();
        let index = verification_index_path(&repo);

        verify_commit(&repo, &commit).unwrap();
        assert!(index.is_file());
        fs::remove_file(&index).unwrap();
        verify_commit(&repo, &commit).unwrap();
        fs::write(&index, b"invalid").unwrap();
        verify_commit(&repo, &commit).unwrap();

        let mut reader = VerificationIndexReader::open(&index).unwrap();
        assert_eq!(
            reader.find(blob).unwrap(),
            blob_fingerprint(&repo, &blob).unwrap()
        );
    }
}
