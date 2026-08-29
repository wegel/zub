//! Tests for ref and immutable-tree union operations.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::{union, union_tree_hashes, ConflictResolution, UnionOptions};
use crate::ops::commit::commit;
use crate::{EntryKind, Error, Repo, Tree, TreeEntry};

#[test]
fn ref_union_keeps_all_parents_and_entries() {
    let (temporary, repo) = test_repo();
    let mut references = Vec::new();
    for index in 0..3 {
        let source = temporary.path().join(format!("source{index}"));
        fs::create_dir(&source).expect("create source");
        fs::write(source.join(format!("file{index}.txt")), index.to_string())
            .expect("write source file");
        let reference = format!("ref{index}");
        commit(&repo, &source, &reference, None, None).expect("commit source");
        references.push(reference);
    }
    let names = references.iter().map(String::as_str).collect::<Vec<_>>();

    let hash = union(&repo, &names, "merged", UnionOptions::default()).expect("union refs");
    let commit = crate::read_commit(&repo, &hash).expect("read union commit");
    let tree = crate::read_tree(&repo, &commit.tree).expect("read union tree");

    assert_eq!(commit.parents.len(), 3);
    assert_eq!(tree.len(), 3);
}

#[test]
fn tree_hash_union_merges_nested_directories() {
    let (temporary, repo) = test_repo();
    let first = committed_tree(&repo, temporary.path(), "first", "dir/a", b"a");
    let second = committed_tree(&repo, temporary.path(), "second", "dir/b", b"b");

    let hash = union_tree_hashes(&repo, &[first, second], ConflictResolution::Error)
        .expect("union tree hashes");
    let root = crate::read_tree(&repo, &hash).expect("read merged root");
    let EntryKind::Directory { hash, .. } = &root.get("dir").expect("merged dir").kind else {
        panic!("dir is not a directory")
    };
    let directory = crate::read_tree(&repo, hash).expect("read merged directory");

    assert!(directory.get("a").is_some());
    assert!(directory.get("b").is_some());
}

#[test]
fn identical_entries_are_one_strict_union_entry() {
    let (temporary, repo) = test_repo();
    let first = committed_tree(&repo, temporary.path(), "first", "same", b"same");
    let second = committed_tree(&repo, temporary.path(), "second", "same", b"same");

    let hash = union_tree_hashes(&repo, &[first, second], ConflictResolution::Error)
        .expect("union identical trees");

    assert_eq!(hash, first);
}

#[test]
fn strict_union_reports_the_full_nested_path() {
    let (temporary, repo) = test_repo();
    let first = committed_tree(&repo, temporary.path(), "first", "a/b/file", b"first");
    let second = committed_tree(&repo, temporary.path(), "second", "a/b/file", b"second");

    let error = union_tree_hashes(&repo, &[first, second], ConflictResolution::Error)
        .expect_err("reject differing file");

    assert!(matches!(error, Error::UnionConflict(path) if path == Path::new("a/b/file")));
}

#[test]
fn strict_union_rejects_directory_metadata_disagreement() {
    let (_temporary, repo) = test_repo();
    let child = crate::write_tree(&repo, &Tree::empty()).expect("write empty child");
    let first = directory_tree(&repo, child, 0o755);
    let second = directory_tree(&repo, child, 0o700);

    let error = union_tree_hashes(&repo, &[first, second], ConflictResolution::Error)
        .expect_err("reject directory metadata");

    assert!(matches!(error, Error::UnionConflict(path) if path == Path::new("dir")));
}

#[test]
fn type_conflicts_remain_errors_for_last_wins_union() {
    let (temporary, repo) = test_repo();
    let file = committed_tree(&repo, temporary.path(), "file", "name", b"file");
    let child = crate::write_tree(&repo, &Tree::empty()).expect("write empty child");
    let directory = crate::write_tree(
        &repo,
        &Tree::new(vec![TreeEntry::new(
            "name",
            EntryKind::directory(child, 0, 0, 0o755),
        )])
        .expect("directory root"),
    )
    .expect("write directory root");

    let error = union_tree_hashes(&repo, &[file, directory], ConflictResolution::Last)
        .expect_err("reject type conflict");

    assert!(matches!(error, Error::UnionTypeConflict { path, .. } if path == Path::new("name")));
}

#[test]
fn tree_hash_union_requires_an_input() {
    let (_temporary, repo) = test_repo();

    assert!(matches!(
        union_tree_hashes(&repo, &[], ConflictResolution::Error),
        Err(Error::EmptyUnion)
    ));
}

fn test_repo() -> (tempfile::TempDir, Repo) {
    let temporary = tempdir().expect("create temporary directory");
    let repo = Repo::init(&temporary.path().join("repo")).expect("initialize repository");
    (temporary, repo)
}

fn committed_tree(repo: &Repo, root: &Path, name: &str, path: &str, content: &[u8]) -> crate::Hash {
    let source = root.join(name);
    let file = source.join(path);
    fs::create_dir_all(file.parent().expect("fixture file parent")).expect("create fixture path");
    fs::write(&file, content).expect("write fixture file");
    let commit = commit(repo, &source, name, None, None).expect("commit fixture");
    crate::read_commit(repo, &commit)
        .expect("read fixture commit")
        .tree
}

fn directory_tree(repo: &Repo, child: crate::Hash, mode: u32) -> crate::Hash {
    let tree = Tree::new(vec![TreeEntry::new(
        "dir",
        EntryKind::directory(child, 0, 0, mode),
    )])
    .expect("directory tree");
    crate::write_tree(repo, &tree).expect("write directory tree")
}
