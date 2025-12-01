use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::{ChangeKind, DiffEntry, EntryKind, Tree};

/// compare two refs and return list of changes
pub fn diff(repo: &Repo, ref1: &str, ref2: &str) -> Result<Vec<DiffEntry>> {
    let commit1 = resolve_ref(repo, ref1)?;
    let commit2 = resolve_ref(repo, ref2)?;

    let tree1 = read_commit(repo, &commit1)?.tree;
    let tree2 = read_commit(repo, &commit2)?.tree;

    diff_trees(repo, &tree1, &tree2, "")
}

/// compare two tree hashes
pub fn diff_trees(repo: &Repo, tree1: &Hash, tree2: &Hash, prefix: &str) -> Result<Vec<DiffEntry>> {
    // if trees are identical, no changes
    if tree1 == tree2 {
        return Ok(vec![]);
    }

    let t1 = read_tree(repo, tree1)?;
    let t2 = read_tree(repo, tree2)?;

    diff_tree_contents(repo, &t1, &t2, prefix)
}

/// compare two tree contents
fn diff_tree_contents(repo: &Repo, t1: &Tree, t2: &Tree, prefix: &str) -> Result<Vec<DiffEntry>> {
    let mut changes = Vec::new();

    // collect all names
    let mut all_names: Vec<&str> = t1
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .chain(t2.entries().iter().map(|e| e.name.as_str()))
        .collect();
    all_names.sort();
    all_names.dedup();

    for name in all_names {
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", prefix, name)
        };

        let e1 = t1.get(name);
        let e2 = t2.get(name);

        match (e1, e2) {
            (None, Some(entry)) => {
                // added
                changes.push(DiffEntry {
                    path: path.clone(),
                    kind: ChangeKind::Added,
                });

                // if directory, report all contents as added
                if let EntryKind::Directory { hash, .. } = &entry.kind {
                    let subtree = read_tree(repo, hash)?;
                    report_all_entries(repo, &subtree, &path, ChangeKind::Added, &mut changes)?;
                }
            }

            (Some(entry), None) => {
                // deleted
                changes.push(DiffEntry {
                    path: path.clone(),
                    kind: ChangeKind::Deleted,
                });

                // if directory, report all contents as deleted
                if let EntryKind::Directory { hash, .. } = &entry.kind {
                    let subtree = read_tree(repo, hash)?;
                    report_all_entries(repo, &subtree, &path, ChangeKind::Deleted, &mut changes)?;
                }
            }

            (Some(e1), Some(e2)) => {
                // both exist - check for changes
                let h1 = e1.kind.hash();
                let h2 = e2.kind.hash();

                match (&e1.kind, &e2.kind) {
                    (
                        EntryKind::Directory {
                            hash: h1,
                            uid: u1,
                            gid: g1,
                            mode: m1,
                            xattrs: x1,
                        },
                        EntryKind::Directory {
                            hash: h2,
                            uid: u2,
                            gid: g2,
                            mode: m2,
                            xattrs: x2,
                        },
                    ) => {
                        // both directories - recurse
                        if h1 != h2 {
                            let sub_changes = diff_trees(repo, h1, h2, &path)?;
                            changes.extend(sub_changes);
                        }
                        // check directory metadata (excluding tree hash which is content)
                        if u1 != u2 || g1 != g2 || m1 != m2 || x1 != x2 {
                            changes.push(DiffEntry {
                                path,
                                kind: ChangeKind::MetadataOnly,
                            });
                        }
                    }

                    _ => {
                        // not both directories
                        if e1.kind.type_name() != e2.kind.type_name() {
                            // type changed (e.g., file -> symlink)
                            changes.push(DiffEntry {
                                path,
                                kind: ChangeKind::Modified,
                            });
                        } else if h1 != h2 {
                            // same type, content changed
                            changes.push(DiffEntry {
                                path,
                                kind: ChangeKind::Modified,
                            });
                        } else if e1.kind != e2.kind {
                            // same hash but different metadata (e.g., sparse_map)
                            changes.push(DiffEntry {
                                path,
                                kind: ChangeKind::MetadataOnly,
                            });
                        }
                    }
                }
            }

            (None, None) => unreachable!(),
        }
    }

    // sort by path
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(changes)
}

/// report all entries in a tree as added/deleted
fn report_all_entries(
    repo: &Repo,
    tree: &Tree,
    prefix: &str,
    kind: ChangeKind,
    changes: &mut Vec<DiffEntry>,
) -> Result<()> {
    for entry in tree.entries() {
        let path = format!("{}/{}", prefix, entry.name);

        changes.push(DiffEntry {
            path: path.clone(),
            kind: kind.clone(),
        });

        if let EntryKind::Directory { hash, .. } = &entry.kind {
            let subtree = read_tree(repo, hash)?;
            report_all_entries(repo, &subtree, &path, kind.clone(), changes)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit::commit;
    use std::fs;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_diff_no_changes() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();

        let hash = commit(&repo, &source, "ref1", None, None).unwrap();
        crate::refs::write_ref(&repo, "ref2", &hash).unwrap();

        let changes = diff(&repo, "ref1", "ref2").unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn test_diff_added_file() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file1.txt"), "content1").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        fs::write(source.join("file2.txt"), "content2").unwrap();
        commit(&repo, &source, "ref2", None, None).unwrap();

        let changes = diff(&repo, "ref1", "ref2").unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "file2.txt");
        assert_eq!(changes[0].kind, ChangeKind::Added);
    }

    #[test]
    fn test_diff_deleted_file() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file1.txt"), "content1").unwrap();
        fs::write(source.join("file2.txt"), "content2").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        fs::remove_file(source.join("file2.txt")).unwrap();
        commit(&repo, &source, "ref2", None, None).unwrap();

        let changes = diff(&repo, "ref1", "ref2").unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "file2.txt");
        assert_eq!(changes[0].kind, ChangeKind::Deleted);
    }

    #[test]
    fn test_diff_modified_file() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "version1").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        fs::write(source.join("file.txt"), "version2").unwrap();
        commit(&repo, &source, "ref2", None, None).unwrap();

        let changes = diff(&repo, "ref1", "ref2").unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "file.txt");
        assert_eq!(changes[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn test_diff_nested_changes() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir_all(source.join("dir")).unwrap();
        fs::write(source.join("dir/file.txt"), "content").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        fs::write(source.join("dir/file.txt"), "modified").unwrap();
        fs::write(source.join("dir/new.txt"), "new").unwrap();
        commit(&repo, &source, "ref2", None, None).unwrap();

        let changes = diff(&repo, "ref1", "ref2").unwrap();

        assert_eq!(changes.len(), 2);
        // changes should be sorted by path
        assert_eq!(changes[0].path, "dir/file.txt");
        assert_eq!(changes[0].kind, ChangeKind::Modified);
        assert_eq!(changes[1].path, "dir/new.txt");
        assert_eq!(changes[1].kind, ChangeKind::Added);
    }

    #[test]
    fn test_diff_added_directory() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        fs::create_dir(source.join("newdir")).unwrap();
        fs::write(source.join("newdir/a.txt"), "a").unwrap();
        fs::write(source.join("newdir/b.txt"), "b").unwrap();
        commit(&repo, &source, "ref2", None, None).unwrap();

        let changes = diff(&repo, "ref1", "ref2").unwrap();

        // should report the directory and its contents
        assert!(changes
            .iter()
            .any(|c| c.path == "newdir" && c.kind == ChangeKind::Added));
        assert!(changes
            .iter()
            .any(|c| c.path == "newdir/a.txt" && c.kind == ChangeKind::Added));
        assert!(changes
            .iter()
            .any(|c| c.path == "newdir/b.txt" && c.kind == ChangeKind::Added));
    }
}
