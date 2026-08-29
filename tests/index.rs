//! Reverse-blob queries derive complete answers from current immutable trees.

use std::fs;

use zub::ops::commit;
use zub::{delete_ref, read_commit, read_tree, rebuild_index, refs_containing_blob, Repo};

#[test]
fn reverse_query_tracks_current_refs_and_recovers_without_an_index() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let repo = Repo::init(&temporary.path().join("repo")).expect("initialize repository");
    let source = temporary.path().join("source");
    fs::create_dir(&source).expect("create source");
    fs::write(source.join("payload"), b"indexed bytes").expect("write payload");

    let first = commit(&repo, &source, "one", None, None).expect("commit first ref");
    commit(&repo, &source, "two", None, None).expect("commit second ref");
    let blob = root_blob(&repo, first, "payload");
    assert_eq!(
        refs_containing_blob(&repo, blob).expect("query two refs"),
        ["one", "two"]
    );

    fs::write(source.join("payload"), b"different bytes").expect("change payload");
    commit(&repo, &source, "one", None, None).expect("move first ref");
    assert_eq!(
        refs_containing_blob(&repo, blob).expect("query moved ref"),
        ["two"]
    );
    delete_ref(&repo, "two").expect("delete second ref");
    assert!(refs_containing_blob(&repo, blob)
        .expect("query deleted refs")
        .is_empty());

    fs::write(source.join("payload"), b"indexed bytes").expect("restore payload");
    commit(&repo, &source, "one", None, None).expect("restore first ref");
    fs::remove_dir_all(repo.index_path()).expect("remove derived index");
    assert_eq!(
        refs_containing_blob(&repo, blob).expect("derive missing index"),
        ["one"]
    );
    assert!(rebuild_index(&repo).expect("rebuild index") > 0);
    assert_eq!(
        refs_containing_blob(&repo, blob).expect("query rebuilt index"),
        ["one"]
    );
}

#[test]
fn malformed_elf_never_publishes_its_ref() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let repo = Repo::init(&temporary.path().join("repo")).expect("initialize repository");
    let source = temporary.path().join("source");
    fs::create_dir(&source).expect("create source");
    fs::write(source.join("broken"), b"\x7fELFbroken").expect("write malformed ELF");

    let error = commit(&repo, &source, "broken", None, None).expect_err("reject malformed ELF");

    assert!(error.to_string().contains("cannot derive ELF metadata"));
    assert!(!zub::ref_exists(&repo, "broken"));
}

fn root_blob(repo: &Repo, commit: zub::Hash, name: &str) -> zub::Hash {
    let tree = read_commit(repo, &commit).expect("read commit").tree;
    let tree = read_tree(repo, &tree).expect("read tree");
    match &tree.get(name).expect("root entry").kind {
        zub::EntryKind::Regular { hash, .. } => *hash,
        kind => panic!("expected regular file, found {}", kind.type_name()),
    }
}
