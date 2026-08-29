use std::fs;

use tempfile::tempdir;
use walkdir::WalkDir;

use super::*;
use crate::ops::commit::commit;
use crate::{delete_artifact_ref, write_named_artifact, Artifact, InterfaceArtifact};

fn test_repo() -> (tempfile::TempDir, Repo) {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let repo = Repo::init(&repo_path).unwrap();
    (dir, repo)
}

#[test]
fn test_gc_keeps_reachable() {
    let (dir, repo) = test_repo();
    let source = source_with_file(&dir);
    commit(&repo, &source, "test", None, None).unwrap();

    let stats = gc(&repo, false).unwrap();

    assert_eq!(stats.blobs_removed, 0);
    assert_eq!(stats.trees_removed, 0);
    assert_eq!(stats.commits_removed, 0);
}

#[test]
fn test_gc_dry_run() {
    let (dir, repo) = test_repo();
    let source = source_with_file(&dir);
    commit(&repo, &source, "test", None, None).unwrap();
    crate::refs::delete_ref(&repo, "test").unwrap();

    let stats = gc(&repo, true).unwrap();

    assert!(stats.blobs_removed > 0 || stats.trees_removed > 0 || stats.commits_removed > 0);
    let blobs_count = WalkDir::new(repo.blobs_path())
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .count();
    assert!(blobs_count > 0);
}

#[test]
fn test_gc_removes_unreachable() {
    let (dir, repo) = test_repo();
    let source = source_with_file(&dir);
    commit(&repo, &source, "test", None, None).unwrap();
    crate::refs::delete_ref(&repo, "test").unwrap();

    let stats = gc(&repo, false).unwrap();

    assert!(stats.blobs_removed > 0 || stats.trees_removed > 0 || stats.commits_removed > 0);
}

#[test]
fn configured_deployment_directory_keeps_its_tree() {
    let (dir, mut repo) = test_repo();
    let source = source_with_file(&dir);
    let commit = commit(&repo, &source, "test", None, None).unwrap();
    let tree = read_commit(&repo, &commit).unwrap().tree;
    crate::delete_ref(&repo, "test").unwrap();
    let deployments = dir.path().join("deployments");
    fs::create_dir_all(deployments.join(format!("{tree}.7"))).unwrap();
    repo.config_mut().gc_roots.push(deployments);
    repo.save_config().unwrap();

    let stats = gc(&repo, false).unwrap();

    assert_eq!(stats.trees_removed, 0);
    assert_eq!(stats.blobs_removed, 0);
    assert!(read_tree(&repo, &tree).is_ok());
}

#[test]
fn artifact_refs_control_artifact_collection() {
    let (_dir, repo) = test_repo();
    let artifact = Artifact::Interface(InterfaceArtifact {
        schema: crate::ARTIFACT_SCHEMA,
        output: Hash::from_bytes([1; 32]),
        interface: Hash::from_bytes([2; 32]),
        elf_blobs: Vec::new(),
        headers: Vec::new(),
    });
    let hash = write_named_artifact(&repo, "interface/test", &artifact).unwrap();
    assert_eq!(gc(&repo, false).unwrap().artifacts_removed, 0);

    delete_artifact_ref(&repo, "interface/test").unwrap();
    assert_eq!(gc(&repo, false).unwrap().artifacts_removed, 1);
    assert!(!crate::artifact_exists(&repo, &hash));
}

fn source_with_file(directory: &tempfile::TempDir) -> std::path::PathBuf {
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), "content").unwrap();
    source
}
