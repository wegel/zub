use std::path::Path;

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

    let mut tree_hashes = Vec::new();
    let mut parent_commits = Vec::new();
    for ref_name in refs {
        let commit_hash = resolve_ref(repo, ref_name)?;
        parent_commits.push(commit_hash);
        let commit = read_commit(repo, &commit_hash)?;
        tree_hashes.push(commit.tree);
    }
    let tree_hash = union_tree_hashes(repo, &tree_hashes, opts.on_conflict)?;
    let commit = Commit::new(
        tree_hash,
        parent_commits,
        opts.author.as_deref().unwrap_or("zub"),
        opts.message.as_deref().unwrap_or(""),
    );
    let commit_hash = write_commit(repo, &commit)?;
    write_ref(repo, output_ref, &commit_hash)?;
    Ok(commit_hash)
}

/// Merge immutable tree objects and return the merged tree hash.
///
/// This operation writes only content-addressed tree objects. It does not
/// create commits or update refs.
pub fn union_tree_hashes(
    repo: &Repo,
    tree_hashes: &[Hash],
    on_conflict: ConflictResolution,
) -> Result<Hash> {
    if tree_hashes.is_empty() {
        return Err(Error::EmptyUnion);
    }
    let trees = tree_hashes
        .iter()
        .map(|hash| read_tree(repo, hash))
        .collect::<Result<Vec<_>>>()?;
    let merged = merge_trees(repo, &trees, Path::new(""), on_conflict)?;
    write_tree(repo, &merged)
}

fn merge_trees(
    repo: &Repo,
    trees: &[Tree],
    parent: &Path,
    on_conflict: ConflictResolution,
) -> Result<Tree> {
    let mut all_names: Vec<String> = trees
        .iter()
        .flat_map(|t| t.entries().iter().map(|e| e.name.clone()))
        .collect();
    all_names.sort();
    all_names.dedup();

    let mut merged_entries = Vec::new();
    for name in all_names {
        let entries = trees
            .iter()
            .filter_map(|tree| tree.get(&name))
            .collect::<Vec<_>>();
        if entries.len() == 1 {
            merged_entries.push(entries[0].clone());
        } else {
            merged_entries.push(merge_entries(
                repo,
                &parent.join(&name),
                &entries,
                on_conflict,
            )?);
        }
    }
    Tree::new(merged_entries)
}

fn merge_entries(
    repo: &Repo,
    path: &Path,
    entries: &[&TreeEntry],
    on_conflict: ConflictResolution,
) -> Result<TreeEntry> {
    let first = entries[0];
    if entries.iter().all(|entry| entry.kind == first.kind) {
        return Ok(first.clone());
    }
    if let Some(entry) = entries
        .iter()
        .skip(1)
        .find(|entry| entry.type_name() != first.type_name())
    {
        return Err(Error::UnionTypeConflict {
            path: path.to_path_buf(),
            first_type: first.type_name(),
            second_type: entry.type_name(),
        });
    }
    if first.kind.is_directory() {
        return merge_directories(repo, path, entries, on_conflict);
    }
    choose_entry(path, entries, on_conflict)
}

fn merge_directories(
    repo: &Repo,
    path: &Path,
    entries: &[&TreeEntry],
    on_conflict: ConflictResolution,
) -> Result<TreeEntry> {
    let selected = match on_conflict {
        ConflictResolution::Error => {
            if !entries
                .iter()
                .skip(1)
                .all(|entry| same_directory_metadata(entries[0], entry))
            {
                return Err(Error::UnionConflict(path.to_path_buf()));
            }
            entries[0]
        }
        ConflictResolution::First => entries[0],
        ConflictResolution::Last => entries[entries.len() - 1],
    };
    let subtrees = entries
        .iter()
        .map(|entry| match &entry.kind {
            EntryKind::Directory { hash, .. } => read_tree(repo, hash),
            _ => unreachable!("directory type checked above"),
        })
        .collect::<Result<Vec<_>>>()?;
    let tree = merge_trees(repo, &subtrees, path, on_conflict)?;
    let hash = write_tree(repo, &tree)?;
    let EntryKind::Directory {
        uid,
        gid,
        mode,
        xattrs,
        ..
    } = &selected.kind
    else {
        unreachable!("directory type checked above")
    };
    Ok(TreeEntry::new(
        selected.name.clone(),
        EntryKind::directory_with_xattrs(hash, *uid, *gid, *mode, xattrs.clone()),
    ))
}

fn same_directory_metadata(left: &TreeEntry, right: &TreeEntry) -> bool {
    match (&left.kind, &right.kind) {
        (
            EntryKind::Directory {
                uid: left_uid,
                gid: left_gid,
                mode: left_mode,
                xattrs: left_xattrs,
                ..
            },
            EntryKind::Directory {
                uid: right_uid,
                gid: right_gid,
                mode: right_mode,
                xattrs: right_xattrs,
                ..
            },
        ) => {
            left_uid == right_uid
                && left_gid == right_gid
                && left_mode == right_mode
                && left_xattrs == right_xattrs
        }
        _ => false,
    }
}

fn choose_entry(
    path: &Path,
    entries: &[&TreeEntry],
    on_conflict: ConflictResolution,
) -> Result<TreeEntry> {
    match on_conflict {
        ConflictResolution::Error => Err(Error::UnionConflict(path.to_path_buf())),
        ConflictResolution::First => Ok(entries[0].clone()),
        ConflictResolution::Last => Ok(entries[entries.len() - 1].clone()),
    }
}

#[cfg(test)]
#[path = "union_tests.rs"]
mod tests;
