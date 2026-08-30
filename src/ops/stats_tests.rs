//! Repository query tests for exact scans and disposable indexes.

use std::fs;

use tempfile::tempdir;

use super::{du, stats};
use crate::object::{blob_path, read_commit, read_tree, tree_path};
use crate::ops::commit;
use crate::{delete_ref, Repo};

#[test]
fn stats_counts_reachable_and_unreachable_objects_once() {
    let temporary = tempdir().unwrap();
    let repo = Repo::init(&temporary.path().join("repo")).unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("one"), b"one").unwrap();
    fs::write(second.join("two"), b"longer orphan").unwrap();
    commit(&repo, &first, "kept", None, None).unwrap();
    commit(&repo, &second, "removed", None, None).unwrap();
    delete_ref(&repo, "removed").unwrap();

    let report = stats(&repo).unwrap();
    assert_eq!(report.total_refs, 1);
    assert_eq!(report.total_blobs, 2);
    assert_eq!(report.total_trees, 2);
    assert_eq!(report.total_commits, 2);
    assert_eq!(report.reachable_blobs, 1);
    assert_eq!(report.reachable_trees, 1);
    assert_eq!(report.reachable_commits, 1);
    assert_eq!(
        report.unreachable_blobs_bytes,
        b"longer orphan".len() as u64
    );
}

#[test]
fn du_rebuilds_deleted_corrupt_and_stale_indexes() {
    let temporary = tempdir().unwrap();
    let repo = Repo::init(&temporary.path().join("repo")).unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload"), b"small").unwrap();
    let commit = commit(&repo, &source, "sample", None, None).unwrap();
    let tree = read_commit(&repo, &commit).unwrap().tree;
    let blob = *read_tree(&repo, &tree)
        .unwrap()
        .get("payload")
        .unwrap()
        .kind
        .hash()
        .unwrap();
    let index = repo.index_path().join("du-v1.cbor");

    assert_eq!(size(&repo), 5);
    assert!(index.is_file());
    fs::remove_file(&index).unwrap();
    assert_eq!(size(&repo), 5);
    fs::write(&index, b"invalid index").unwrap();
    assert_eq!(size(&repo), 5);

    fs::write(blob_path(&repo, &blob), b"changed length").unwrap();
    assert_eq!(size(&repo), 14);

    let tree_file = tree_path(&repo, &tree);
    let original_tree = fs::read(&tree_file).unwrap();
    fs::write(&tree_file, vec![0; original_tree.len()]).unwrap();
    assert!(du(&repo, Some("sample")).is_err());
    fs::write(tree_file, original_tree).unwrap();
    assert_eq!(size(&repo), 14);
}

fn size(repo: &Repo) -> u64 {
    let entries = du(repo, Some("sample")).unwrap();
    assert_eq!(entries.len(), 1);
    entries[0].bytes
}
