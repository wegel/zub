use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::repo::Repo;
use crate::types::Artifact;

/// write an artifact to the object store
///
/// artifacts are serialized as CBOR. the hash is computed from the serialized
/// content (deterministic: same artifact = same hash).
///
/// note: unlike commits, we don't compress artifacts since they're small and
/// the hash must match Artifact::compute_hash() for verification.
pub fn write_artifact(repo: &Repo, artifact: &Artifact) -> Result<Hash> {
    // compute hash (deterministic)
    let hash = artifact.compute_hash();

    let (dir, file) = hash.to_path_components();
    let artifact_dir = repo.artifacts_path().join(&dir);
    let artifact_path = artifact_dir.join(&file);

    // dedup: if artifact already exists, we're done
    if artifact_path.exists() {
        return Ok(hash);
    }

    // serialize to cbor
    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(artifact, &mut cbor_bytes)?;

    // ensure directory exists
    fs::create_dir_all(&artifact_dir).with_path(&artifact_dir)?;

    // atomic write: temp -> fsync -> rename
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        tmp_file.write_all(&cbor_bytes).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    // rename to final location
    fs::rename(&tmp_path, &artifact_path).with_path(&artifact_path)?;

    // fsync parent directory
    let dir_file = File::open(&artifact_dir).with_path(&artifact_dir)?;
    dir_file.sync_all().with_path(&artifact_dir)?;

    Ok(hash)
}

/// read an artifact from the object store
pub fn read_artifact(repo: &Repo, hash: &Hash) -> Result<Artifact> {
    let path = artifact_path(repo, hash);

    let cbor_bytes = fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*hash)
        } else {
            Error::Io {
                path: path.clone(),
                source: e,
            }
        }
    })?;

    // deserialize
    let artifact: Artifact = ciborium::from_reader(&cbor_bytes[..])?;

    // verify hash matches (ensures integrity)
    let actual_hash = artifact.compute_hash();
    if actual_hash != *hash {
        return Err(Error::CorruptObject(*hash));
    }

    Ok(artifact)
}

/// get the filesystem path to an artifact object
pub fn artifact_path(repo: &Repo, hash: &Hash) -> PathBuf {
    let (dir, file) = hash.to_path_components();
    repo.artifacts_path().join(dir).join(file)
}

/// check if an artifact exists in the object store
pub fn artifact_exists(repo: &Repo, hash: &Hash) -> bool {
    artifact_path(repo, hash).exists()
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
    fn test_write_and_read_artifact() {
        let (_dir, repo) = test_repo();

        let tree =
            Hash::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let manifest_hash =
            Hash::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap();

        let artifact = Artifact::new(tree, manifest_hash, "bundles/dev");
        let expected_hash = artifact.compute_hash();

        let hash = write_artifact(&repo, &artifact).unwrap();
        assert_eq!(hash, expected_hash);
        assert!(artifact_exists(&repo, &hash));

        let read = read_artifact(&repo, &hash).unwrap();
        assert_eq!(artifact, read);
    }

    #[test]
    fn test_artifact_deduplication() {
        let (_dir, repo) = test_repo();

        let tree =
            Hash::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let manifest_hash =
            Hash::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap();

        let artifact = Artifact::new(tree, manifest_hash, "bundles/dev");

        let h1 = write_artifact(&repo, &artifact).unwrap();
        let h2 = write_artifact(&repo, &artifact).unwrap();

        assert_eq!(h1, h2);
    }

    #[test]
    fn test_read_nonexistent_artifact() {
        let (_dir, repo) = test_repo();

        let fake_hash =
            Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();
        let result = read_artifact(&repo, &fake_hash);

        assert!(matches!(result, Err(Error::ObjectNotFound(_))));
    }

    #[test]
    fn test_artifact_hash_is_deterministic() {
        let (_dir, repo) = test_repo();

        let tree =
            Hash::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let manifest_hash =
            Hash::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap();

        // create same artifact twice
        let a1 = Artifact::new(tree, manifest_hash, "outputs/bin");
        let a2 = Artifact::new(tree, manifest_hash, "outputs/bin");

        // hashes must be identical
        let h1 = write_artifact(&repo, &a1).unwrap();
        let h2 = write_artifact(&repo, &a2).unwrap();
        assert_eq!(h1, h2);

        // and must match compute_hash()
        assert_eq!(h1, a1.compute_hash());
    }
}
