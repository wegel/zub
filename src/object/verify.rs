use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rayon::prelude::*;

use crate::error::{Error, IoResultExt, Result};
use crate::hash::{BlobHasher, Hash, SYMLINK_MODE};
use crate::namespace::outside_to_inside;
use crate::types::{EntryKind, Xattr};
use crate::Repo;

use super::{blob_path, read_commit, read_tree};

const VERIFY_BUFFER_BYTES: usize = 64 * 1024;

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
    let worker_count = workers.max(1).min(checks.len());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count)
        .stack_size(256 * 1024)
        .build()
        .map_err(|error| Error::VerifyPool(error.to_string()))?;
    let results = pool.install(|| {
        checks
            .par_iter()
            .map(|(hash, check)| verify_blob(repo, hash, &check.xattrs, check.symlink))
            .collect::<Vec<_>>()
    });
    for result in results {
        result?;
    }
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
    let mut blobs = BTreeMap::new();
    while let Some(tree_hash) = pending.pop() {
        if !trees.insert(tree_hash) {
            continue;
        }
        collect_tree(repo, tree_hash, &mut pending, &mut blobs)?;
    }
    Ok(blobs.into_iter().collect())
}

fn collect_tree(
    repo: &Repo,
    tree_hash: Hash,
    pending: &mut Vec<Hash>,
    blobs: &mut BTreeMap<Hash, BlobCheck>,
) -> Result<()> {
    let tree = read_tree(repo, &tree_hash)?;
    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, xattrs, .. } => {
                blobs.entry(*hash).or_insert_with(|| BlobCheck {
                    xattrs: xattrs.clone(),
                    symlink: false,
                });
            }
            EntryKind::Symlink { hash, xattrs } => {
                blobs.entry(*hash).or_insert_with(|| BlobCheck {
                    xattrs: xattrs.clone(),
                    symlink: true,
                });
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
    let path = blob_path(repo, expected);
    let metadata = fs::metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*expected)
        } else {
            Error::Io {
                path: path.clone(),
                source: error,
            }
        }
    })?;
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
    Ok(())
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
}
