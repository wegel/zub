use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::refs::{read_artifact_ref, write_artifact_ref};
use crate::repo::Repo;
use crate::types::Artifact;

/// Write an artifact object and atomically point a semantic key at it.
pub fn write_named_artifact(repo: &Repo, key: &str, artifact: &Artifact) -> Result<Hash> {
    let hash = write_artifact(repo, artifact)?;
    write_artifact_ref(repo, key, &hash)?;
    Ok(hash)
}

/// Read and verify the artifact object named by a semantic key.
pub fn read_named_artifact(repo: &Repo, key: &str) -> Result<Artifact> {
    let hash = read_artifact_ref(repo, key)?;
    read_artifact(repo, &hash)
}

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

    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(artifact, &mut cbor_bytes)?;

    if artifact_path.exists() {
        let existing = fs::read(&artifact_path).with_path(&artifact_path)?;
        if existing == cbor_bytes {
            return Ok(hash);
        }
    }

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

    let actual_hash = Hash::from_bytes(*blake3::hash(&cbor_bytes).as_bytes());
    if actual_hash != *hash {
        return Err(Error::CorruptObject(*hash));
    }

    // deserialize
    let artifact: Artifact = ciborium::from_reader(&cbor_bytes[..])?;

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
#[path = "artifact_tests.rs"]
mod tests;
