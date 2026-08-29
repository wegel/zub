use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;

use crate::error::{Error, Result};
use crate::hash::{compute_blob_hash, compute_symlink_hash, Hash};
use crate::namespace::outside_to_inside;
use crate::types::{EntryKind, Xattr};
use crate::Repo;

use super::{blob_path, read_blob, read_commit, read_tree};

/// Verify one commit and every object reachable from its root tree.
///
/// Parent IDs remain part of the commit hash, but this check does not require
/// parent objects. A remote may transfer one current snapshot without its
/// complete ref history.
pub fn verify_commit(repo: &Repo, commit_hash: &Hash) -> Result<()> {
    let commit = read_commit(repo, commit_hash)?;
    let mut trees = HashSet::new();
    verify_tree(repo, &commit.tree, &mut trees)
}

fn verify_tree(repo: &Repo, tree_hash: &Hash, checked: &mut HashSet<Hash>) -> Result<()> {
    if !checked.insert(*tree_hash) {
        return Ok(());
    }

    let tree = read_tree(repo, tree_hash)?;
    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, xattrs, .. } => {
                verify_blob(repo, hash, xattrs, false)?;
            }
            EntryKind::Symlink { hash, xattrs } => {
                verify_blob(repo, hash, xattrs, true)?;
            }
            EntryKind::Directory { hash, .. } => verify_tree(repo, hash, checked)?,
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
    let bytes = read_blob(repo, expected)?;
    let actual = if symlink {
        let target = std::str::from_utf8(&bytes).map_err(|_| Error::CorruptObject(*expected))?;
        compute_symlink_hash(uid, gid, xattrs, target)
    } else {
        compute_blob_hash(uid, gid, metadata.mode(), xattrs, &bytes)
    };
    if actual != *expected {
        return Err(Error::CorruptObject(*expected));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
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
}
