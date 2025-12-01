use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::repo::Repo;
use crate::types::Tree;

/// write a tree to the object store
///
/// trees are serialized as CBOR, then zstd compressed.
/// the hash is computed over the compressed bytes.
pub fn write_tree(repo: &Repo, tree: &Tree) -> Result<Hash> {
    // serialize to cbor
    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(tree, &mut cbor_bytes)?;

    // compress with zstd (level 3 - fast, reasonable ratio)
    let compressed = zstd::encode_all(&cbor_bytes[..], 3).map_err(|e| Error::Io {
        path: PathBuf::from("<zstd>"),
        source: e,
    })?;

    // hash the compressed bytes
    let hash = Hash::from_bytes(Sha256::digest(&compressed).into());

    let (dir, file) = hash.to_path_components();
    let tree_dir = repo.trees_path().join(&dir);
    let tree_path = tree_dir.join(&file);

    // dedup: if tree already exists, we're done
    if tree_path.exists() {
        return Ok(hash);
    }

    // ensure directory exists
    fs::create_dir_all(&tree_dir).with_path(&tree_dir)?;

    // atomic write: temp -> fsync -> rename
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        tmp_file.write_all(&compressed).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    // rename to final location
    fs::rename(&tmp_path, &tree_path).with_path(&tree_path)?;

    // fsync parent directory
    let dir_file = File::open(&tree_dir).with_path(&tree_dir)?;
    dir_file.sync_all().with_path(&tree_dir)?;

    Ok(hash)
}

/// read a tree from the object store
pub fn read_tree(repo: &Repo, hash: &Hash) -> Result<Tree> {
    let path = tree_path(repo, hash);

    let compressed = fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*hash)
        } else {
            Error::Io {
                path: path.clone(),
                source: e,
            }
        }
    })?;

    // verify hash
    let actual_hash = Hash::from_bytes(Sha256::digest(&compressed).into());
    if actual_hash != *hash {
        return Err(Error::CorruptObject(*hash));
    }

    // decompress
    let cbor_bytes = zstd::decode_all(&compressed[..]).map_err(|e| Error::Io {
        path: path.clone(),
        source: e,
    })?;

    // deserialize
    let tree: Tree = ciborium::from_reader(&cbor_bytes[..])?;

    Ok(tree)
}

/// get the filesystem path to a tree object
pub fn tree_path(repo: &Repo, hash: &Hash) -> PathBuf {
    let (dir, file) = hash.to_path_components();
    repo.trees_path().join(dir).join(file)
}

/// check if a tree exists in the object store
#[allow(dead_code)]
pub fn tree_exists(repo: &Repo, hash: &Hash) -> bool {
    tree_path(repo, hash).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EntryKind, TreeEntry};
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_write_and_read_tree() {
        let (_dir, repo) = test_repo();

        let entries = vec![
            TreeEntry::new("file.txt", EntryKind::regular(Hash::ZERO, 100)),
            TreeEntry::new("subdir", EntryKind::directory(Hash::ZERO, 0, 0, 0o755)),
        ];
        let tree = Tree::new(entries).unwrap();

        let hash = write_tree(&repo, &tree).unwrap();
        assert!(tree_exists(&repo, &hash));

        let read_tree = read_tree(&repo, &hash).unwrap();
        assert_eq!(tree, read_tree);
    }

    #[test]
    fn test_tree_deduplication() {
        let (_dir, repo) = test_repo();

        let entries = vec![TreeEntry::new("foo", EntryKind::regular(Hash::ZERO, 50))];
        let tree = Tree::new(entries).unwrap();

        let h1 = write_tree(&repo, &tree).unwrap();
        let h2 = write_tree(&repo, &tree).unwrap();

        assert_eq!(h1, h2);
    }

    #[test]
    fn test_empty_tree() {
        let (_dir, repo) = test_repo();

        let tree = Tree::empty();
        let hash = write_tree(&repo, &tree).unwrap();

        let read_tree = read_tree(&repo, &hash).unwrap();
        assert!(read_tree.is_empty());
    }

    #[test]
    fn test_read_nonexistent_tree() {
        let (_dir, repo) = test_repo();

        let fake_hash =
            Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let result = read_tree(&repo, &fake_hash);

        assert!(matches!(result, Err(Error::ObjectNotFound(_))));
    }

    #[test]
    fn test_tree_with_all_entry_types() {
        let (_dir, repo) = test_repo();

        let entries = vec![
            TreeEntry::new("regular", EntryKind::regular(Hash::ZERO, 100)),
            TreeEntry::new("symlink", EntryKind::symlink(Hash::ZERO)),
            TreeEntry::new("dir", EntryKind::directory(Hash::ZERO, 1000, 1000, 0o755)),
            TreeEntry::new(
                "block",
                EntryKind::BlockDevice {
                    major: 8,
                    minor: 0,
                    uid: 0,
                    gid: 6,
                    mode: 0o660,
                    xattrs: vec![],
                },
            ),
            TreeEntry::new(
                "char",
                EntryKind::CharDevice {
                    major: 1,
                    minor: 3,
                    uid: 0,
                    gid: 0,
                    mode: 0o666,
                    xattrs: vec![],
                },
            ),
            TreeEntry::new(
                "fifo",
                EntryKind::Fifo {
                    uid: 0,
                    gid: 0,
                    mode: 0o644,
                    xattrs: vec![],
                },
            ),
            TreeEntry::new("hardlink", EntryKind::hardlink("regular")),
        ];

        let tree = Tree::new(entries).unwrap();
        let hash = write_tree(&repo, &tree).unwrap();

        let read_tree = read_tree(&repo, &hash).unwrap();
        assert_eq!(tree.len(), read_tree.len());

        // verify all entries came back
        assert!(read_tree.get("regular").is_some());
        assert!(read_tree.get("symlink").is_some());
        assert!(read_tree.get("dir").is_some());
        assert!(read_tree.get("block").is_some());
        assert!(read_tree.get("char").is_some());
        assert!(read_tree.get("fifo").is_some());
        assert!(read_tree.get("hardlink").is_some());
    }
}
