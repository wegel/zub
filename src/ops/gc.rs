use std::collections::HashSet;
use std::fs;

use walkdir::WalkDir;

use crate::error::{IoResultExt, Result};
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::list_refs;
use crate::repo::Repo;
use crate::types::EntryKind;

/// gc statistics
#[derive(Debug, Default)]
pub struct GcStats {
    pub blobs_removed: usize,
    pub trees_removed: usize,
    pub commits_removed: usize,
    pub bytes_freed: u64,
}

/// garbage collect unreachable objects
pub fn gc(repo: &Repo, dry_run: bool) -> Result<GcStats> {
    // mark phase: collect all reachable objects
    let mut reachable_blobs = HashSet::new();
    let mut reachable_trees = HashSet::new();
    let mut reachable_commits = HashSet::new();

    // start from all refs
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

    // sweep phase: remove unmarked objects
    let mut stats = GcStats::default();

    // sweep blobs
    sweep_objects(
        &repo.blobs_path(),
        &reachable_blobs,
        dry_run,
        &mut stats.blobs_removed,
        &mut stats.bytes_freed,
    )?;

    // sweep trees
    sweep_objects(
        &repo.trees_path(),
        &reachable_trees,
        dry_run,
        &mut stats.trees_removed,
        &mut stats.bytes_freed,
    )?;

    // sweep commits
    sweep_objects(
        &repo.commits_path(),
        &reachable_commits,
        dry_run,
        &mut stats.commits_removed,
        &mut stats.bytes_freed,
    )?;

    Ok(stats)
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
        mark_commit(repo, parent, reachable_blobs, reachable_trees, reachable_commits)?;
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
            source: e.into_io_error().unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "walkdir error")
            }),
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

    // clean up empty directories
    if !dry_run {
        for entry in WalkDir::new(dir).min_depth(1).max_depth(1) {
            let entry = entry.map_err(|e| crate::Error::Io {
                path: dir.to_path_buf(),
                source: e.into_io_error().unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::Other, "walkdir error")
                }),
            })?;

            if entry.file_type().is_dir() {
                // try to remove if empty
                let _ = fs::remove_dir(entry.path());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit::commit;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_gc_keeps_reachable() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let stats = gc(&repo, false).unwrap();

        // nothing should be removed
        assert_eq!(stats.blobs_removed, 0);
        assert_eq!(stats.trees_removed, 0);
        assert_eq!(stats.commits_removed, 0);
    }

    #[test]
    fn test_gc_dry_run() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        // delete the ref
        crate::refs::delete_ref(&repo, "test").unwrap();

        // dry run
        let stats = gc(&repo, true).unwrap();

        // should report objects to remove
        assert!(stats.blobs_removed > 0 || stats.trees_removed > 0 || stats.commits_removed > 0);

        // but objects should still exist
        let blobs_count = WalkDir::new(repo.blobs_path())
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count();
        assert!(blobs_count > 0);
    }

    #[test]
    fn test_gc_removes_unreachable() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        // delete the ref
        crate::refs::delete_ref(&repo, "test").unwrap();

        // gc
        let stats = gc(&repo, false).unwrap();

        // should have removed objects
        assert!(stats.blobs_removed > 0 || stats.trees_removed > 0 || stats.commits_removed > 0);
    }
}
