//! repository statistics

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::{list_refs, list_refs_matching, read_ref};
use crate::repo::Repo;
use crate::types::EntryKind;

mod du_cache;
mod scan;

use du_cache::DuCache;
use scan::{inventory_hash, scan_objects};

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
    // count and measure objects on disk
    let started = Instant::now();
    let blob_objects = scan_objects(&repo.blobs_path(), true);
    trace("count blobs", started);
    let started = Instant::now();
    let tree_objects = scan_objects(&repo.trees_path(), false);
    trace("count trees", started);
    let started = Instant::now();
    let commit_objects = scan_objects(&repo.commits_path(), false);
    trace("count commits", started);

    // mark reachable objects
    let mut reachable_blobs = HashSet::new();
    let mut reachable_trees = HashSet::new();
    let mut reachable_commits = HashSet::new();

    let refs = list_refs(repo)?;
    let started = Instant::now();
    for ref_name in &refs {
        let commit_hash = crate::refs::read_ref(repo, ref_name)?;
        mark_commit(
            repo,
            &commit_hash,
            &mut reachable_blobs,
            &mut reachable_trees,
            &mut reachable_commits,
        )?;
    }
    trace("mark reachable", started);
    let started = Instant::now();
    let unreachable_blobs_bytes = blob_objects
        .hashed
        .iter()
        .filter(|object| !reachable_blobs.contains(&object.hash))
        .map(|object| object.bytes)
        .sum();
    trace("count unreachable blobs", started);

    Ok(RepoStats {
        total_blobs: blob_objects.count,
        total_trees: tree_objects.count,
        total_commits: commit_objects.count,
        total_refs: refs.len(),
        total_blobs_bytes: blob_objects.bytes,
        total_trees_bytes: tree_objects.bytes,
        total_commits_bytes: commit_objects.bytes,
        reachable_blobs: reachable_blobs.len(),
        reachable_trees: reachable_trees.len(),
        reachable_commits: reachable_commits.len(),
        unreachable_blobs_bytes,
    })
}

fn trace(label: &str, started: Instant) {
    if std::env::var_os("ZUB_PERF_TRACE").is_some() {
        eprintln!("zub perf: {label}: {:.3}s", started.elapsed().as_secs_f64());
    }
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
            EntryKind::Symlink { hash, .. } => {
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
    let started = Instant::now();
    let mut blob_objects = scan_objects(&repo.blobs_path(), true);
    let mut tree_objects = scan_objects(&repo.trees_path(), true);
    let blob_inventory = inventory_hash(&mut blob_objects.hashed, false);
    let tree_inventory = inventory_hash(&mut tree_objects.hashed, true);
    let blob_sizes = blob_objects
        .hashed
        .into_iter()
        .map(|object| (object.hash, object.bytes))
        .collect::<HashMap<_, _>>();
    trace("index object inventory", started);

    let mut results = Vec::new();
    let mut cache = DuCache::open(repo, blob_inventory, tree_inventory);

    let refs = match pattern {
        Some(p) => list_refs_matching(repo, p)?,
        None => list_refs(repo)?,
    };

    let started = Instant::now();
    for ref_name in refs {
        let commit_hash = read_ref(repo, &ref_name)?;
        let tree = cache.commit_tree(repo, commit_hash)?;
        let bytes = cache.tree_bytes(repo, tree, &blob_sizes)?;

        results.push(RefSize { ref_name, bytes });
    }
    trace("measure ref trees", started);
    cache.save(repo, blob_inventory, tree_inventory);

    // sort by size descending
    results.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    Ok(results)
}

fn build_blob_size_map(repo: &Repo) -> Result<HashMap<Hash, u64>> {
    Ok(scan_objects(&repo.blobs_path(), true)
        .hashed
        .into_iter()
        .map(|object| (object.hash, object.bytes))
        .collect())
}

fn collect_tree_blobs(repo: &Repo, tree_hash: &Hash, blobs: &mut HashSet<Hash>) -> Result<()> {
    let tree = read_tree(repo, tree_hash)?;

    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, .. } => {
                blobs.insert(*hash);
            }
            EntryKind::Symlink { hash, .. } => {
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

/// disk usage entry for a path within a tree
#[derive(Debug, Clone)]
pub struct PathSize {
    pub path: String,
    pub bytes: u64,
}

/// calculate disk usage per directory within a ref
/// depth controls how deep to report (1 = top-level dirs only)
pub fn du_tree(repo: &Repo, ref_name: &str, depth: usize) -> Result<Vec<PathSize>> {
    let blob_sizes = build_blob_size_map(repo)?;

    let commit_hash = read_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;

    let mut results: HashMap<String, u64> = HashMap::new();

    collect_tree_sizes(repo, &commit.tree, "", depth, &blob_sizes, &mut results)?;

    let mut sorted: Vec<PathSize> = results
        .into_iter()
        .map(|(path, bytes)| PathSize { path, bytes })
        .collect();

    sorted.sort_by(|a, b| b.bytes.cmp(&a.bytes));

    Ok(sorted)
}

fn collect_tree_sizes(
    repo: &Repo,
    tree_hash: &Hash,
    prefix: &str,
    depth: usize,
    blob_sizes: &HashMap<Hash, u64>,
    results: &mut HashMap<String, u64>,
) -> Result<u64> {
    let tree = read_tree(repo, tree_hash)?;
    let mut total = 0u64;

    for entry in tree.entries() {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        let size = match &entry.kind {
            EntryKind::Regular { hash, .. } | EntryKind::Symlink { hash, .. } => {
                *blob_sizes.get(hash).unwrap_or(&0)
            }
            EntryKind::Directory { hash, .. } => {
                let current_depth = path.matches('/').count() + 1;
                if current_depth < depth {
                    collect_tree_sizes(repo, hash, &path, depth, blob_sizes, results)?
                } else {
                    // at max depth, sum up everything below
                    let mut blobs = HashSet::new();
                    collect_tree_blobs(repo, hash, &mut blobs)?;
                    blobs.iter().filter_map(|h| blob_sizes.get(h)).sum()
                }
            }
            _ => 0,
        };

        total += size;

        // record at the appropriate depth
        let current_depth = path.matches('/').count() + 1;
        if current_depth <= depth
            && (matches!(entry.kind, EntryKind::Directory { .. })
                || current_depth == depth
                || depth == 0)
        {
            *results.entry(path).or_insert(0) += size;
        }
    }

    Ok(total)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
