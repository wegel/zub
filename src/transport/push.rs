//! push operation - send objects to remote

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::error::{IoResultExt, Result};
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::{read_ref, write_ref};
use crate::repo::Repo;
use crate::transport::local::{copy_objects, list_all_objects, ObjectSet, TransferStats};
use crate::transport::ssh::SshConnection;
use crate::types::EntryKind;

/// push options
#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    /// force update even if not fast-forward
    pub force: bool,
    /// dry run - show what would be transferred without doing it
    pub dry_run: bool,
}

/// push a ref to a local repository
pub fn push_local(
    src: &Repo,
    dst: &Repo,
    ref_name: &str,
    options: &PushOptions,
) -> Result<PushResult> {
    let src_hash = read_ref(src, ref_name)?;

    // check if this is a fast-forward (if ref exists in destination)
    if !options.force {
        if let Ok(dst_hash) = read_ref(dst, ref_name) {
            if !is_ancestor(src, &dst_hash, &src_hash)? {
                return Err(crate::Error::Transport {
                    message: "non-fast-forward update rejected (use --force to override)"
                        .to_string(),
                });
            }
        }
    }

    // collect all objects reachable from the commit
    let mut needed = ObjectSet::new();
    collect_commit_objects(src, &src_hash, &mut needed, &mut HashSet::new())?;

    // filter out objects that already exist in destination
    let existing = list_all_objects(dst)?;
    let existing_blobs: HashSet<_> = existing.blobs.into_iter().collect();
    let existing_trees: HashSet<_> = existing.trees.into_iter().collect();
    let existing_commits: HashSet<_> = existing.commits.into_iter().collect();

    needed.blobs.retain(|h| !existing_blobs.contains(h));
    needed.trees.retain(|h| !existing_trees.contains(h));
    needed.commits.retain(|h| !existing_commits.contains(h));

    // dry run: return what would be transferred without doing anything
    if options.dry_run {
        return Ok(PushResult {
            hash: src_hash,
            stats: TransferStats {
                copied: 0,
                hardlinked: 0,
                skipped: 0,
                bytes_transferred: 0,
            },
            objects_to_transfer: needed.blobs.len() + needed.trees.len() + needed.commits.len(),
        });
    }

    // copy objects
    let stats = copy_objects(src, dst, &needed)?;

    // update ref
    write_ref(dst, ref_name, &src_hash)?;

    Ok(PushResult {
        hash: src_hash,
        stats,
        objects_to_transfer: 0,
    })
}

/// push a ref to a remote repository via SSH
pub fn push_ssh(
    local: &Repo,
    remote: &str,
    remote_path: &Path,
    ref_name: &str,
    options: &PushOptions,
) -> Result<PushResult> {
    let local_hash = read_ref(local, ref_name)?;

    let mut conn = SshConnection::connect(remote, remote_path)?;

    // check remote ref for fast-forward
    if !options.force {
        if let Some(remote_hash) = conn.get_ref(ref_name)? {
            if !is_ancestor(local, &remote_hash, &local_hash)? {
                return Err(crate::Error::Transport {
                    message: "non-fast-forward update rejected (use --force to override)"
                        .to_string(),
                });
            }
        }
    }

    // collect all objects we have
    let mut all_objects = ObjectSet::new();
    collect_commit_objects(local, &local_hash, &mut all_objects, &mut HashSet::new())?;

    // ask remote what it needs
    let needed = conn.want_objects(&all_objects)?;

    // dry run: return what would be transferred without doing anything
    if options.dry_run {
        conn.close()?;
        return Ok(PushResult {
            hash: local_hash,
            stats: TransferStats::default(),
            objects_to_transfer: needed.blobs.len() + needed.trees.len() + needed.commits.len(),
        });
    }

    // send needed objects
    let mut stats = TransferStats::default();

    for hash in &needed.blobs {
        let path = object_path(&local.blobs_path(), hash);
        let data = fs::read(&path).with_path(&path)?;
        conn.send_object("blob", hash, &data)?;
        stats.bytes_transferred += data.len() as u64;
        stats.copied += 1;
    }

    for hash in &needed.trees {
        let path = object_path(&local.trees_path(), hash);
        let data = fs::read(&path).with_path(&path)?;
        conn.send_object("tree", hash, &data)?;
        stats.bytes_transferred += data.len() as u64;
        stats.copied += 1;
    }

    for hash in &needed.commits {
        let path = object_path(&local.commits_path(), hash);
        let data = fs::read(&path).with_path(&path)?;
        conn.send_object("commit", hash, &data)?;
        stats.bytes_transferred += data.len() as u64;
        stats.copied += 1;
    }

    // update remote ref
    conn.update_ref(ref_name, &local_hash)?;

    conn.close()?;

    Ok(PushResult {
        hash: local_hash,
        stats,
        objects_to_transfer: 0,
    })
}

/// check if ancestor is an ancestor of descendant
fn is_ancestor(repo: &Repo, ancestor: &Hash, descendant: &Hash) -> Result<bool> {
    if ancestor == descendant {
        return Ok(true);
    }

    let mut to_visit = vec![*descendant];
    let mut visited = HashSet::new();

    while let Some(hash) = to_visit.pop() {
        if hash == *ancestor {
            return Ok(true);
        }

        if visited.contains(&hash) {
            continue;
        }
        visited.insert(hash);

        if let Ok(commit) = read_commit(repo, &hash) {
            for parent in &commit.parents {
                to_visit.push(*parent);
            }
        }
    }

    Ok(false)
}

/// collect all objects reachable from a commit
fn collect_commit_objects(
    repo: &Repo,
    commit_hash: &Hash,
    objects: &mut ObjectSet,
    visited: &mut HashSet<Hash>,
) -> Result<()> {
    if visited.contains(commit_hash) {
        return Ok(());
    }
    visited.insert(*commit_hash);

    objects.commits.push(*commit_hash);

    let commit = read_commit(repo, commit_hash)?;

    // collect tree objects
    collect_tree_objects(repo, &commit.tree, objects, visited)?;

    // recurse into parents
    for parent in &commit.parents {
        collect_commit_objects(repo, parent, objects, visited)?;
    }

    Ok(())
}

/// collect all objects in a tree
fn collect_tree_objects(
    repo: &Repo,
    tree_hash: &Hash,
    objects: &mut ObjectSet,
    visited: &mut HashSet<Hash>,
) -> Result<()> {
    if visited.contains(tree_hash) {
        return Ok(());
    }
    visited.insert(*tree_hash);

    objects.trees.push(*tree_hash);

    let tree = read_tree(repo, tree_hash)?;

    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, .. } => {
                if !visited.contains(hash) {
                    visited.insert(*hash);
                    objects.blobs.push(*hash);
                }
            }
            EntryKind::Symlink { hash } => {
                if !visited.contains(hash) {
                    visited.insert(*hash);
                    objects.blobs.push(*hash);
                }
            }
            EntryKind::Directory { hash, .. } => {
                collect_tree_objects(repo, hash, objects, visited)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn object_path(base: &Path, hash: &Hash) -> std::path::PathBuf {
    let hex = hash.to_hex();
    base.join(&hex[..2]).join(&hex[2..])
}

/// result of a push operation
#[derive(Debug)]
pub struct PushResult {
    pub hash: Hash,
    pub stats: TransferStats,
    /// number of objects that would be transferred (for dry run)
    pub objects_to_transfer: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit;
    use tempfile::tempdir;

    #[test]
    fn test_push_local() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        let hash = commit(&src, &source, "test", Some("initial"), None).unwrap();

        let result = push_local(&src, &dst, "test", &PushOptions::default()).unwrap();

        assert_eq!(result.hash, hash);
        assert!(result.stats.copied > 0 || result.stats.hardlinked > 0);

        // verify ref exists in destination
        let dst_hash = read_ref(&dst, "test").unwrap();
        assert_eq!(dst_hash, hash);
    }

    #[test]
    fn test_push_fast_forward() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "v1").unwrap();
        commit(&src, &source, "test", Some("v1"), None).unwrap();

        // push first version
        push_local(&src, &dst, "test", &PushOptions::default()).unwrap();

        // create second version (fast-forward)
        fs::write(source.join("file.txt"), "v2").unwrap();
        let hash2 = commit(&src, &source, "test", Some("v2"), None).unwrap();

        // push should succeed (fast-forward)
        let result = push_local(&src, &dst, "test", &PushOptions::default()).unwrap();
        assert_eq!(result.hash, hash2);
    }

    #[test]
    fn test_push_non_fast_forward_rejected() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        // create and push first version
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "v1").unwrap();
        commit(&src, &source, "test", Some("v1"), None).unwrap();
        push_local(&src, &dst, "test", &PushOptions::default()).unwrap();

        // create different version in src (not based on v1)
        // first we need a fresh commit that isn't descended from v1
        let src2_path = dir.path().join("src2_repo");
        let src2 = Repo::init(&src2_path).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("other.txt"), "other").unwrap();
        commit(&src2, &source2, "test", Some("other"), None).unwrap();

        // push from src2 should fail (non-fast-forward)
        let result = push_local(&src2, &dst, "test", &PushOptions::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_push_force() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        // create and push first version
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "v1").unwrap();
        commit(&src, &source, "test", Some("v1"), None).unwrap();
        push_local(&src, &dst, "test", &PushOptions::default()).unwrap();

        // create different version in src2
        let src2_path = dir.path().join("src2_repo");
        let src2 = Repo::init(&src2_path).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("other.txt"), "other").unwrap();
        let hash2 = commit(&src2, &source2, "test", Some("other"), None).unwrap();

        // force push should succeed
        let options = PushOptions {
            force: true,
            dry_run: false,
        };
        let result = push_local(&src2, &dst, "test", &options).unwrap();
        assert_eq!(result.hash, hash2);
    }

    #[test]
    fn test_is_ancestor() {
        let dir = tempdir().unwrap();

        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();

        fs::write(source.join("file.txt"), "v1").unwrap();
        let hash1 = commit(&repo, &source, "test", Some("v1"), None).unwrap();

        fs::write(source.join("file.txt"), "v2").unwrap();
        let hash2 = commit(&repo, &source, "test", Some("v2"), None).unwrap();

        fs::write(source.join("file.txt"), "v3").unwrap();
        let hash3 = commit(&repo, &source, "test", Some("v3"), None).unwrap();

        // hash1 is ancestor of hash3
        assert!(is_ancestor(&repo, &hash1, &hash3).unwrap());

        // hash1 is ancestor of hash2
        assert!(is_ancestor(&repo, &hash1, &hash2).unwrap());

        // hash2 is ancestor of hash3
        assert!(is_ancestor(&repo, &hash2, &hash3).unwrap());

        // hash3 is NOT ancestor of hash1
        assert!(!is_ancestor(&repo, &hash3, &hash1).unwrap());

        // same commit is its own ancestor
        assert!(is_ancestor(&repo, &hash2, &hash2).unwrap());
    }
}
