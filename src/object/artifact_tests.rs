use tempfile::tempdir;

use super::*;
use crate::{InterfaceArtifact, ARTIFACT_SCHEMA};

fn test_repo() -> (tempfile::TempDir, Repo) {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let repo = Repo::init(&repo_path).unwrap();
    (dir, repo)
}

fn artifact() -> Artifact {
    Artifact::Interface(InterfaceArtifact {
        schema: ARTIFACT_SCHEMA,
        output: Hash::from_bytes([0xaa; 32]),
        interface: Hash::from_bytes([0xbb; 32]),
        elf_blobs: vec![Hash::from_bytes([0xcc; 32])],
        headers: Vec::new(),
    })
}

#[test]
fn test_write_and_read_artifact() {
    let (_dir, repo) = test_repo();
    let artifact = artifact();
    let expected_hash = artifact.compute_hash();

    let hash = write_artifact(&repo, &artifact).unwrap();

    assert_eq!(hash, expected_hash);
    assert!(artifact_exists(&repo, &hash));
    assert_eq!(read_artifact(&repo, &hash).unwrap(), artifact);
}

#[test]
fn test_artifact_deduplication() {
    let (_dir, repo) = test_repo();
    let artifact = artifact();

    let first = write_artifact(&repo, &artifact).unwrap();
    let second = write_artifact(&repo, &artifact).unwrap();

    assert_eq!(first, second);
}

#[test]
fn test_read_nonexistent_artifact() {
    let (_dir, repo) = test_repo();
    let fake_hash =
        Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222").unwrap();

    let result = read_artifact(&repo, &fake_hash);

    assert!(matches!(result, Err(Error::ObjectNotFound(_))));
}

#[test]
fn test_artifact_hash_is_deterministic() {
    let (_dir, repo) = test_repo();
    let first = artifact();
    let second = artifact();

    let first_hash = write_artifact(&repo, &first).unwrap();
    let second_hash = write_artifact(&repo, &second).unwrap();

    assert_eq!(first_hash, second_hash);
    assert_eq!(first_hash, first.compute_hash());
}

#[test]
fn named_artifact_round_trip() {
    let (_directory, repo) = test_repo();
    let artifact = artifact();

    let hash = write_named_artifact(&repo, "interface/example", &artifact).unwrap();

    assert_eq!(read_artifact_ref(&repo, "interface/example").unwrap(), hash);
    assert_eq!(
        read_named_artifact(&repo, "interface/example").unwrap(),
        artifact
    );
}
