use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::object::{read_commit, read_tree, write_commit, write_tree};
use crate::refs::{resolve_ref, write_ref};
use crate::repo::Repo;
use crate::types::{Commit, EntryKind, Tree, TreeEntry};

/// conflict resolution strategy
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConflictResolution {
    /// error on any conflict
    #[default]
    Error,
    /// use entry from first tree that has it
    First,
    /// use entry from last tree that has it
    Last,
}

/// union options
#[derive(Default, Clone)]
pub struct UnionOptions {
    pub message: Option<String>,
    pub author: Option<String>,
    pub on_conflict: ConflictResolution,
}

/// merge multiple refs into a new commit in the object store
///
/// this operation does NOT touch the filesystem - it merges trees directly
/// in the object store.
pub fn union(repo: &Repo, refs: &[&str], output_ref: &str, opts: UnionOptions) -> Result<Hash> {
    if refs.is_empty() {
        return Err(Error::InvalidRef("no refs to union".to_string()));
    }

    // resolve all refs to their root trees
    let mut trees = Vec::new();
    let mut parent_commits = Vec::new();

    for ref_name in refs {
        let commit_hash = resolve_ref(repo, ref_name)?;
        parent_commits.push(commit_hash);

        let commit = read_commit(repo, &commit_hash)?;
        let tree = read_tree(repo, &commit.tree)?;
        trees.push(tree);
    }

    // merge trees
    let merged_tree = merge_trees(repo, &trees, opts.on_conflict)?;
    let tree_hash = write_tree(repo, &merged_tree)?;

    // create commit
    let commit = Commit::new(
        tree_hash,
        parent_commits,
        opts.author.as_deref().unwrap_or("zub"),
        opts.message.as_deref().unwrap_or(""),
    );

    let commit_hash = write_commit(repo, &commit)?;

    // update ref
    write_ref(repo, output_ref, &commit_hash)?;

    Ok(commit_hash)
}

/// merge multiple trees into one
fn merge_trees(repo: &Repo, trees: &[Tree], on_conflict: ConflictResolution) -> Result<Tree> {
    // collect all entry names across all trees
    let mut all_names: Vec<String> = trees
        .iter()
        .flat_map(|t| t.entries().iter().map(|e| e.name.clone()))
        .collect();
    all_names.sort();
    all_names.dedup();

    let mut merged_entries = Vec::new();

    for name in all_names {
        // collect entries with this name from each tree
        let entries_for_name: Vec<(usize, &TreeEntry)> = trees
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.get(&name).map(|e| (i, e)))
            .collect();

        if entries_for_name.len() == 1 {
            // only one tree has this entry, use it
            merged_entries.push(entries_for_name[0].1.clone());
        } else {
            // multiple trees have this entry
            let merged = merge_entries(repo, &name, &entries_for_name, on_conflict)?;
            merged_entries.push(merged);
        }
    }

    Tree::new(merged_entries)
}

/// merge multiple entries with the same name
fn merge_entries(
    repo: &Repo,
    name: &str,
    entries: &[(usize, &TreeEntry)],
    on_conflict: ConflictResolution,
) -> Result<TreeEntry> {
    // check if all entries are directories
    let all_directories = entries.iter().all(|(_, e)| e.kind.is_directory());

    if all_directories {
        // recursively merge directory contents
        let mut subtrees = Vec::new();
        let mut last_metadata = None;

        for (_, entry) in entries {
            if let EntryKind::Directory {
                hash,
                uid,
                gid,
                mode,
                xattrs,
            } = &entry.kind
            {
                let subtree = read_tree(repo, hash)?;
                subtrees.push(subtree);
                last_metadata = Some((*uid, *gid, *mode, xattrs.clone()));
            }
        }

        let merged_subtree = merge_trees(repo, &subtrees, on_conflict)?;
        let merged_hash = write_tree(repo, &merged_subtree)?;

        // use last directory's metadata
        let (uid, gid, mode, xattrs) = last_metadata.unwrap();

        Ok(TreeEntry::new(
            name,
            EntryKind::directory_with_xattrs(merged_hash, uid, gid, mode, xattrs),
        ))
    } else {
        // type conflict or file conflict
        // check for type mismatch (file vs directory)
        let first_is_dir = entries[0].1.kind.is_directory();
        for (_, entry) in entries.iter().skip(1) {
            if entry.kind.is_directory() != first_is_dir {
                return Err(Error::UnionTypeConflict {
                    path: std::path::PathBuf::from(name),
                    first_type: entries[0].1.type_name(),
                    second_type: entry.type_name(),
                });
            }
        }

        // same type conflict (both files, both symlinks, etc.)
        match on_conflict {
            ConflictResolution::Error => Err(Error::UnionConflict(std::path::PathBuf::from(name))),
            ConflictResolution::First => Ok(entries[0].1.clone()),
            ConflictResolution::Last => Ok(entries[entries.len() - 1].1.clone()),
        }
    }
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
    fn test_union_no_overlap() {
        let (dir, repo) = test_repo();

        // create two sources with different files
        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("file1.txt"), "content1").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("file2.txt"), "content2").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        // union
        let hash = union(&repo, &["ref1", "ref2"], "merged", Default::default()).unwrap();

        // verify
        let commit_obj = read_commit(&repo, &hash).unwrap();
        let tree = read_tree(&repo, &commit_obj.tree).unwrap();

        assert_eq!(tree.len(), 2);
        assert!(tree.get("file1.txt").is_some());
        assert!(tree.get("file2.txt").is_some());
    }

    #[test]
    fn test_union_directory_merge() {
        let (dir, repo) = test_repo();

        // both have same directory with different contents
        let source1 = dir.path().join("source1");
        fs::create_dir_all(source1.join("dir")).unwrap();
        fs::write(source1.join("dir/a.txt"), "a").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir_all(source2.join("dir")).unwrap();
        fs::write(source2.join("dir/b.txt"), "b").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        let hash = union(&repo, &["ref1", "ref2"], "merged", Default::default()).unwrap();

        let commit_obj = read_commit(&repo, &hash).unwrap();
        let tree = read_tree(&repo, &commit_obj.tree).unwrap();

        // should have one directory with both files
        assert_eq!(tree.len(), 1);
        let dir_entry = tree.get("dir").unwrap();
        if let EntryKind::Directory { hash, .. } = &dir_entry.kind {
            let subtree = read_tree(&repo, hash).unwrap();
            assert_eq!(subtree.len(), 2);
            assert!(subtree.get("a.txt").is_some());
            assert!(subtree.get("b.txt").is_some());
        } else {
            panic!("expected directory");
        }
    }

    #[test]
    fn test_union_file_conflict_error() {
        let (dir, repo) = test_repo();

        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("conflict.txt"), "version1").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("conflict.txt"), "version2").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        // default is Error
        let result = union(&repo, &["ref1", "ref2"], "merged", Default::default());
        assert!(matches!(result, Err(Error::UnionConflict(_))));
    }

    #[test]
    fn test_union_file_conflict_last() {
        let (dir, repo) = test_repo();

        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("conflict.txt"), "version1").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("conflict.txt"), "version2").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        let opts = UnionOptions {
            on_conflict: ConflictResolution::Last,
            ..Default::default()
        };
        let hash = union(&repo, &["ref1", "ref2"], "merged", opts).unwrap();

        // checkout and verify content
        let commit_obj = read_commit(&repo, &hash).unwrap();
        let tree = read_tree(&repo, &commit_obj.tree).unwrap();

        // should have the file
        assert!(tree.get("conflict.txt").is_some());
    }

    #[test]
    fn test_union_type_conflict() {
        let (dir, repo) = test_repo();

        // ref1 has "name" as file
        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("name"), "file content").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        // ref2 has "name" as directory
        let source2 = dir.path().join("source2");
        fs::create_dir_all(source2.join("name")).unwrap();
        fs::write(source2.join("name/inside.txt"), "inside").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        // type conflict should error even with Last resolution
        let opts = UnionOptions {
            on_conflict: ConflictResolution::Last,
            ..Default::default()
        };
        let result = union(&repo, &["ref1", "ref2"], "merged", opts);
        assert!(matches!(result, Err(Error::UnionTypeConflict { .. })));
    }

    #[test]
    fn test_union_three_way() {
        let (dir, repo) = test_repo();

        for (i, name) in ["ref1", "ref2", "ref3"].iter().enumerate() {
            let source = dir.path().join(format!("source{}", i));
            fs::create_dir(&source).unwrap();
            fs::write(
                source.join(format!("file{}.txt", i)),
                format!("content{}", i),
            )
            .unwrap();
            commit(&repo, &source, name, None, None).unwrap();
        }

        let hash = union(
            &repo,
            &["ref1", "ref2", "ref3"],
            "merged",
            Default::default(),
        )
        .unwrap();

        let commit_obj = read_commit(&repo, &hash).unwrap();
        let tree = read_tree(&repo, &commit_obj.tree).unwrap();

        assert_eq!(tree.len(), 3);
        assert!(tree.get("file0.txt").is_some());
        assert!(tree.get("file1.txt").is_some());
        assert!(tree.get("file2.txt").is_some());

        // should have all three as parents
        assert_eq!(commit_obj.parents.len(), 3);
    }
}
