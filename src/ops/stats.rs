//! repository statistics

use std::collections::{HashMap, HashSet};
use std::fs;

use walkdir::WalkDir;

use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::{list_refs, list_refs_matching, read_ref};
use crate::repo::Repo;
use crate::types::EntryKind;

/// repository statistics
#[derive(Debug, Default)]
pub struct RepoStats {
    pub total_blobs: usize,
    pub total_trees: usize,
    pub total_commits: usize,
    pub total_refs: usize,
    pub total_blobs_bytes: u64,
    pub total_trees_bytes: u64,
    pub total_commits_bytes: u64,
    pub reachable_blobs: usize,
    pub reachable_trees: usize,
    pub reachable_commits: usize,
    pub unreachable_blobs_bytes: u64,
}

/// collect repository statistics
pub fn stats(repo: &Repo) -> Result<RepoStats> {
    let mut s = RepoStats::default();

    // count refs
    s.total_refs = list_refs(repo)?.len();

    // count and measure objects on disk
    let (blobs, blob_bytes) = count_objects(&repo.blobs_path());
    let (trees, tree_bytes) = count_objects(&repo.trees_path());
    let (commits, commit_bytes) = count_objects(&repo.commits_path());

    s.total_blobs = blobs;
    s.total_blobs_bytes = blob_bytes;
    s.total_trees = trees;
    s.total_trees_bytes = tree_bytes;
    s.total_commits = commits;
    s.total_commits_bytes = commit_bytes;

    // mark reachable objects
    let mut reachable_blobs = HashSet::new();
    let mut reachable_trees = HashSet::new();
    let mut reachable_commits = HashSet::new();

    for ref_name in list_refs(repo)? {
        let commit_hash = crate::refs::read_ref(repo, &ref_name)?;
        mark_commit(
            repo,
            &commit_hash,
            &mut reachable_blobs,
            &mut reachable_trees,
            &mut reachable_commits,
        )?;
    }

    s.reachable_blobs = reachable_blobs.len();
    s.reachable_trees = reachable_trees.len();
    s.reachable_commits = reachable_commits.len();

    // calculate unreachable blob bytes
    s.unreachable_blobs_bytes = calculate_unreachable_bytes(&repo.blobs_path(), &reachable_blobs);

    Ok(s)
}

fn count_objects(dir: &std::path::Path) -> (usize, u64) {
    if !dir.exists() {
        return (0, 0);
    }

    let mut count = 0;
    let mut bytes = 0;

    for entry in WalkDir::new(dir).min_depth(2).max_depth(2) {
        if let Ok(entry) = entry {
            if entry.file_type().is_file() {
                count += 1;
                if let Ok(meta) = fs::metadata(entry.path()) {
                    bytes += meta.len();
                }
            }
        }
    }

    (count, bytes)
}

fn calculate_unreachable_bytes(dir: &std::path::Path, reachable: &HashSet<Hash>) -> u64 {
    if !dir.exists() {
        return 0;
    }

    let mut bytes = 0;

    for entry in WalkDir::new(dir).min_depth(2).max_depth(2) {
        if let Ok(entry) = entry {
            if !entry.file_type().is_file() {
                continue;
            }

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
                    if let Ok(meta) = fs::metadata(path) {
                        bytes += meta.len();
                    }
                }
            }
        }
    }

    bytes
}

/// recursively mark a commit and all its reachable objects
fn mark_commit(
    repo: &Repo,
    commit_hash: &Hash,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
    reachable_commits: &mut HashSet<Hash>,
) -> Result<()> {
    if reachable_commits.contains(commit_hash) {
        return Ok(());
    }
    reachable_commits.insert(*commit_hash);

    let commit = read_commit(repo, commit_hash)?;
    mark_tree(repo, &commit.tree, reachable_blobs, reachable_trees)?;

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
            EntryKind::Symlink { hash } => {
                reachable_blobs.insert(*hash);
            }
            EntryKind::Directory { hash, .. } => {
                mark_tree(repo, hash, reachable_blobs, reachable_trees)?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// ref size entry
#[derive(Debug)]
pub struct RefSize {
    pub ref_name: String,
    pub bytes: u64,
}

/// calculate size per ref (disk usage)
/// optionally filter refs by glob pattern
pub fn du(repo: &Repo, pattern: Option<&str>) -> Result<Vec<RefSize>> {
    // first build a map of blob hash -> size on disk
    let blob_sizes = build_blob_size_map(repo)?;

    let mut results = Vec::new();

    let refs = match pattern {
        Some(p) => list_refs_matching(repo, p)?,
        None => list_refs(repo)?,
    };

    for ref_name in refs {
        let commit_hash = read_ref(repo, &ref_name)?;
        let commit = read_commit(repo, &commit_hash)?;

        // collect all blobs reachable from this ref's tree
        let mut blobs = HashSet::new();
        collect_tree_blobs(repo, &commit.tree, &mut blobs)?;

        // sum up sizes
        let bytes: u64 = blobs.iter().filter_map(|h| blob_sizes.get(h)).sum();

        results.push(RefSize { ref_name, bytes });
    }

    // sort by size descending
    results.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    Ok(results)
}

fn build_blob_size_map(repo: &Repo) -> Result<HashMap<Hash, u64>> {
    let mut sizes = HashMap::new();
    let blobs_path = repo.blobs_path();

    if !blobs_path.exists() {
        return Ok(sizes);
    }

    for entry in WalkDir::new(&blobs_path).min_depth(2).max_depth(2) {
        if let Ok(entry) = entry {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let parent_name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");

            let hex = format!("{}{}", parent_name, file_name);
            if let Ok(hash) = Hash::from_hex(&hex) {
                if let Ok(meta) = fs::metadata(path) {
                    sizes.insert(hash, meta.len());
                }
            }
        }
    }

    Ok(sizes)
}

fn collect_tree_blobs(repo: &Repo, tree_hash: &Hash, blobs: &mut HashSet<Hash>) -> Result<()> {
    let tree = read_tree(repo, tree_hash)?;

    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, .. } => {
                blobs.insert(*hash);
            }
            EntryKind::Symlink { hash } => {
                blobs.insert(*hash);
            }
            EntryKind::Directory { hash, .. } => {
                collect_tree_blobs(repo, hash, blobs)?;
            }
            _ => {}
        }
    }

    Ok(())
}
