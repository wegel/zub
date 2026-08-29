use std::collections::HashSet;
use std::fs;

use walkdir::WalkDir;

use crate::error::{IoResultExt, Result};
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::{list_artifact_refs, list_refs, read_artifact_ref};
use crate::repo::Repo;
use crate::types::EntryKind;

/// gc statistics
#[derive(Debug, Default)]
pub struct GcStats {
    pub blobs_removed: usize,
    pub trees_removed: usize,
    pub commits_removed: usize,
    pub artifacts_removed: usize,
    pub bytes_freed: u64,
}

#[derive(Default)]
struct Reachable {
    blobs: HashSet<Hash>,
    trees: HashSet<Hash>,
    commits: HashSet<Hash>,
    artifacts: HashSet<Hash>,
}

/// garbage collect unreachable objects
pub fn gc(repo: &Repo, dry_run: bool) -> Result<GcStats> {
    let mut reachable = Reachable::default();
    mark_roots(repo, &mut reachable)?;
    sweep(repo, &reachable, dry_run)
}

fn mark_roots(repo: &Repo, reachable: &mut Reachable) -> Result<()> {
    for ref_name in list_refs(repo)? {
        let commit_hash = crate::refs::read_ref(repo, &ref_name)?;
        mark_commit(
            repo,
            &commit_hash,
            &mut reachable.blobs,
            &mut reachable.trees,
            &mut reachable.commits,
        )?;
    }
    for root in &repo.config().gc_roots {
        mark_deployments(repo, root, &mut reachable.blobs, &mut reachable.trees)?;
    }
    for reference in list_artifact_refs(repo)? {
        reachable
            .artifacts
            .insert(read_artifact_ref(repo, &reference)?);
    }
    Ok(())
}

fn sweep(repo: &Repo, reachable: &Reachable, dry_run: bool) -> Result<GcStats> {
    let mut stats = GcStats::default();
    sweep_objects(
        &repo.blobs_path(),
        &reachable.blobs,
        dry_run,
        &mut stats.blobs_removed,
        &mut stats.bytes_freed,
    )?;
    sweep_objects(
        &repo.trees_path(),
        &reachable.trees,
        dry_run,
        &mut stats.trees_removed,
        &mut stats.bytes_freed,
    )?;
    sweep_objects(
        &repo.commits_path(),
        &reachable.commits,
        dry_run,
        &mut stats.commits_removed,
        &mut stats.bytes_freed,
    )?;

    sweep_objects(
        &repo.artifacts_path(),
        &reachable.artifacts,
        dry_run,
        &mut stats.artifacts_removed,
        &mut stats.bytes_freed,
    )?;

    Ok(stats)
}

fn mark_deployments(
    repo: &Repo,
    root: &std::path::Path,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root).with_path(root)? {
        let entry = entry.with_path(root)?;
        if !entry.file_type().with_path(entry.path())?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((tree, serial)) = name.split_once('.') else {
            continue;
        };
        if serial.is_empty() || !serial.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if let Ok(tree) = Hash::from_hex(tree) {
            mark_tree(repo, &tree, reachable_blobs, reachable_trees)?;
        }
    }
    Ok(())
}

/// recursively mark a commit and all its reachable objects
fn mark_commit(
    repo: &Repo,
    commit_hash: &Hash,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
    reachable_commits: &mut HashSet<Hash>,
) -> Result<()> {
    // avoid re-processing
    if reachable_commits.contains(commit_hash) {
        return Ok(());
    }
    reachable_commits.insert(*commit_hash);

    let commit = read_commit(repo, commit_hash)?;

    // mark tree
    mark_tree(repo, &commit.tree, reachable_blobs, reachable_trees)?;

    // recurse into parents
    for parent in &commit.parents {
        mark_commit(
            repo,
            parent,
            reachable_blobs,
            reachable_trees,
            reachable_commits,
        )?;
    }

    Ok(())
}

/// recursively mark a tree and all its reachable objects
fn mark_tree(
    repo: &Repo,
    tree_hash: &Hash,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
) -> Result<()> {
    if reachable_trees.contains(tree_hash) {
        return Ok(());
    }
    reachable_trees.insert(*tree_hash);

    let tree = read_tree(repo, tree_hash)?;

    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, .. } => {
                reachable_blobs.insert(*hash);
            }
            EntryKind::Symlink { hash, .. } => {
                reachable_blobs.insert(*hash);
            }
            EntryKind::Directory { hash, .. } => {
                mark_tree(repo, hash, reachable_blobs, reachable_trees)?;
            }
            // devices, fifos, sockets, hardlinks don't have blob content
            _ => {}
        }
    }

    Ok(())
}

/// sweep a directory, removing objects not in the reachable set
fn sweep_objects(
    dir: &std::path::Path,
    reachable: &HashSet<Hash>,
    dry_run: bool,
    removed_count: &mut usize,
    bytes_freed: &mut u64,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(dir).min_depth(2).max_depth(2) {
        let entry = entry.map_err(|e| crate::Error::Io {
            path: dir.to_path_buf(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        // reconstruct hash from path: objects/type/XX/YYYYYY...
        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let parent_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");

        let hex = format!("{}{}", parent_name, file_name);
        if let Ok(hash) = Hash::from_hex(&hex) {
            if !reachable.contains(&hash) {
                let meta = fs::metadata(path).with_path(path)?;
                *bytes_freed += meta.len();
                *removed_count += 1;

                if !dry_run {
                    fs::remove_file(path).with_path(path)?;
                }
            }
        }
    }

    if !dry_run {
        remove_empty_object_dirs(dir)?;
    }
    Ok(())
}

fn remove_empty_object_dirs(dir: &std::path::Path) -> Result<()> {
    for entry in WalkDir::new(dir).min_depth(1).max_depth(1) {
        let entry = entry.map_err(|error| crate::Error::Io {
            path: dir.to_path_buf(),
            source: error
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
        })?;
        if entry.file_type().is_dir() {
            let _ = fs::remove_dir(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
