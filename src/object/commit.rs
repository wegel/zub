use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::repo::Repo;
use crate::types::Commit;

/// write a commit to the object store
///
/// commits are serialized as CBOR, then zstd compressed.
/// the hash is computed over the compressed bytes.
pub fn write_commit(repo: &Repo, commit: &Commit) -> Result<Hash> {
    // serialize to cbor
    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(commit, &mut cbor_bytes)?;

    // compress with zstd (level 3)
    let compressed = zstd::encode_all(&cbor_bytes[..], 3).map_err(|e| Error::Io {
        path: PathBuf::from("<zstd>"),
        source: e,
    })?;

    // hash the compressed bytes
    let hash = Hash::from_bytes(Sha256::digest(&compressed).into());

    let (dir, file) = hash.to_path_components();
    let commit_dir = repo.commits_path().join(&dir);
    let commit_path = commit_dir.join(&file);

    // dedup: if commit already exists, we're done
    if commit_path.exists() {
        return Ok(hash);
    }

    // ensure directory exists
    fs::create_dir_all(&commit_dir).with_path(&commit_dir)?;

    // atomic write: temp -> fsync -> rename
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        tmp_file.write_all(&compressed).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    // rename to final location
    fs::rename(&tmp_path, &commit_path).with_path(&commit_path)?;

    // fsync parent directory
    let dir_file = File::open(&commit_dir).with_path(&commit_dir)?;
    dir_file.sync_all().with_path(&commit_dir)?;

    Ok(hash)
}

/// read a commit from the object store
pub fn read_commit(repo: &Repo, hash: &Hash) -> Result<Commit> {
    let path = commit_path(repo, hash);

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
    let commit: Commit = ciborium::from_reader(&cbor_bytes[..])?;

    Ok(commit)
}

/// get the filesystem path to a commit object
pub fn commit_path(repo: &Repo, hash: &Hash) -> PathBuf {
    let (dir, file) = hash.to_path_components();
    repo.commits_path().join(dir).join(file)
}

/// check if a commit exists in the object store
pub fn commit_exists(repo: &Repo, hash: &Hash) -> bool {
    commit_path(repo, hash).exists()
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

    #[test]
    fn test_write_and_read_commit() {
        let (_dir, repo) = test_repo();

        let commit = Commit::with_timestamp(Hash::ZERO, vec![], "author", 1234567890, "test commit");

        let hash = write_commit(&repo, &commit).unwrap();
        assert!(commit_exists(&repo, &hash));

        let read_commit = read_commit(&repo, &hash).unwrap();
        assert_eq!(commit, read_commit);
    }

    #[test]
    fn test_commit_deduplication() {
        let (_dir, repo) = test_repo();

        let commit = Commit::with_timestamp(Hash::ZERO, vec![], "author", 1234567890, "test");

        let h1 = write_commit(&repo, &commit).unwrap();
        let h2 = write_commit(&repo, &commit).unwrap();

        assert_eq!(h1, h2);
    }

    #[test]
    fn test_commit_with_parents() {
        let (_dir, repo) = test_repo();

        let parent =
            Hash::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let commit = Commit::with_timestamp(Hash::ZERO, vec![parent], "author", 1234567890, "child commit");

        let hash = write_commit(&repo, &commit).unwrap();
        let read_commit = read_commit(&repo, &hash).unwrap();

        assert_eq!(read_commit.parents.len(), 1);
        assert_eq!(read_commit.parents[0], parent);
    }

    #[test]
    fn test_commit_with_metadata() {
        let (_dir, repo) = test_repo();

        let commit = Commit::with_timestamp(Hash::ZERO, vec![], "author", 1234567890, "test")
            .with_metadata("key1", "value1")
            .with_metadata("key2", "value2");

        let hash = write_commit(&repo, &commit).unwrap();
        let read_commit = read_commit(&repo, &hash).unwrap();

        assert_eq!(read_commit.metadata.get("key1"), Some(&"value1".to_string()));
        assert_eq!(read_commit.metadata.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_read_nonexistent_commit() {
        let (_dir, repo) = test_repo();

        let fake_hash =
            Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();
        let result = read_commit(&repo, &fake_hash);

        assert!(matches!(result, Err(Error::ObjectNotFound(_))));
    }

    #[test]
    fn test_merge_commit() {
        let (_dir, repo) = test_repo();

        let p1 = Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
            .unwrap();
        let p2 = Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
            .unwrap();

        let commit =
            Commit::with_timestamp(Hash::ZERO, vec![p1, p2], "author", 1234567890, "merge commit");

        assert!(commit.is_merge());

        let hash = write_commit(&repo, &commit).unwrap();
        let read_commit = read_commit(&repo, &hash).unwrap();

        assert!(read_commit.is_merge());
        assert_eq!(read_commit.parents.len(), 2);
    }
}
