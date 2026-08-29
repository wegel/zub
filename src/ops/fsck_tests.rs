use std::fs;

use tempfile::tempdir;

use super::*;
use crate::ops::commit::commit;
use crate::{write_artifact_ref, write_named_artifact, Artifact, InterfaceArtifact};

fn test_repo() -> (tempfile::TempDir, Repo) {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let repo = Repo::init(&repo_path).unwrap();
    (dir, repo)
}

#[test]
fn test_fsck_healthy_repo() {
    let (dir, repo) = test_repo();

    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), "content").unwrap();
    commit(&repo, &source, "test", None, None).unwrap();

    let report = fsck(&repo).unwrap();

    assert!(report.is_ok());
    assert!(report.corrupt_objects.is_empty());
    assert!(report.missing_objects.is_empty());
    assert!(report.dangling_objects.is_empty());
}

#[test]
fn test_fsck_with_dangling() {
    let (dir, repo) = test_repo();

    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), "content").unwrap();
    commit(&repo, &source, "test", None, None).unwrap();

    crate::refs::delete_ref(&repo, "test").unwrap();

    let report = fsck(&repo).unwrap();
    assert!(!report.dangling_objects.is_empty());
}

#[test]
fn test_fsck_reports_reachable_blob_hash_mismatch() {
    let (dir, repo) = test_repo();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), "content").unwrap();
    let commit_hash = commit(&repo, &source, "test", None, None).unwrap();
    let commit = read_commit(&repo, &commit_hash).unwrap();
    let tree = read_tree(&repo, &commit.tree).unwrap();
    let blob = *tree.get("file.txt").unwrap().kind.hash().unwrap();
    fs::write(crate::object::blob_path(&repo, &blob), "changed").unwrap();

    let report = fsck(&repo).unwrap();
    assert!(!report.is_ok());
    assert!(report
        .corrupt_objects
        .iter()
        .any(|object| object.hash == blob && matches!(object.object_type, ObjectType::Blob)));
}

#[test]
fn fsck_reports_missing_artifact_from_named_ref() {
    let (_dir, repo) = test_repo();
    let missing = Hash::from_bytes([0x42; 32]);
    write_artifact_ref(&repo, "interface/missing", &missing).unwrap();

    let report = fsck(&repo).unwrap();

    assert!(!report.is_ok());
    assert!(report.missing_objects.iter().any(|object| {
        object.hash == missing
            && matches!(object.object_type, ObjectType::Artifact)
            && object.referenced_by == "artifact ref interface/missing"
    }));
}

#[test]
fn fsck_reports_corrupt_artifact_bytes() {
    let (_dir, repo) = test_repo();
    let artifact = Artifact::Interface(InterfaceArtifact {
        schema: crate::ARTIFACT_SCHEMA,
        output: Hash::from_bytes([1; 32]),
        interface: Hash::from_bytes([2; 32]),
        elf_blobs: Vec::new(),
        headers: Vec::new(),
    });
    let hash = write_named_artifact(&repo, "interface/corrupt", &artifact).unwrap();
    fs::write(crate::artifact_path(&repo, &hash), b"corrupt").unwrap();

    let report = fsck(&repo).unwrap();

    assert!(!report.is_ok());
    assert!(report.corrupt_objects.iter().any(|object| {
        object.hash == hash && matches!(object.object_type, ObjectType::Artifact)
    }));
}
