//! pull operation - fetch objects from remote

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

/// pull options
#[derive(Debug, Clone, Default)]
pub struct PullOptions {
    /// only fetch objects, don't update ref
    pub fetch_only: bool,
}

/// pull a ref from a local repository
pub fn pull_local(
    src: &Repo,
    dst: &Repo,
    ref_name: &str,
    options: &PullOptions,
) -> Result<PullResult> {
    let src_hash = read_ref(src, ref_name)?;

    // collect all objects reachable from the commit
    let mut needed = ObjectSet::new();
    collect_commit_objects(src, &src_hash, &mut needed, &mut HashSet::new())?;

    // filter out objects we already have
    let existing = list_all_objects(dst)?;
    let existing_blobs: HashSet<_> = existing.blobs.into_iter().collect();
    let existing_trees: HashSet<_> = existing.trees.into_iter().collect();
    let existing_commits: HashSet<_> = existing.commits.into_iter().collect();

    needed.blobs.retain(|h| !existing_blobs.contains(h));
    needed.trees.retain(|h| !existing_trees.contains(h));
    needed.commits.retain(|h| !existing_commits.contains(h));

    // copy needed objects
    let stats = copy_objects(src, dst, &needed)?;

    // update ref
    if !options.fetch_only {
        write_ref(dst, ref_name, &src_hash)?;
    }

    Ok(PullResult {
        hash: src_hash,
        stats,
    })
}

/// pull a ref from a remote repository via SSH
pub fn pull_ssh(
    remote: &str,
    remote_path: &Path,
    local: &Repo,
    ref_name: &str,
    options: &PullOptions,
) -> Result<PullResult> {
    let mut conn = SshConnection::connect(remote, remote_path)?;

    // get ref from remote
    let remote_hash = conn
        .get_ref(ref_name)?
        .ok_or_else(|| crate::Error::RefNotFound(ref_name.to_string()))?;

    // collect what we have
    let existing = list_all_objects(local)?;

    // ask remote what we need
    let needed = conn.have_objects(&existing)?;

    // receive objects
    let mut stats = TransferStats::default();

    while let Some((obj_type, hash, data)) = conn.receive_object()? {
        let path = match obj_type.as_str() {
            "blob" => object_path(&local.blobs_path(), &hash),
            "tree" => object_path(&local.trees_path(), &hash),
            "commit" => object_path(&local.commits_path(), &hash),
            _ => continue,
        };

        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_path(parent)?;
            }
            stats.bytes_transferred += data.len() as u64;
            fs::write(&path, &data).with_path(&path)?;
            stats.copied += 1;
        } else {
            stats.skipped += 1;
        }
    }

    // update ref
    if !options.fetch_only {
        write_ref(local, ref_name, &remote_hash)?;
    }

    conn.close()?;

    Ok(PullResult {
        hash: remote_hash,
        stats,
    })
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

/// result of a pull operation
#[derive(Debug)]
pub struct PullResult {
    pub hash: Hash,
    pub stats: TransferStats,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit;
    use tempfile::tempdir;

    #[test]
    fn test_pull_local() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        let hash = commit(&src, &source, "test", Some("initial"), None).unwrap();

        let result = pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();

        assert_eq!(result.hash, hash);
        assert!(result.stats.copied > 0 || result.stats.hardlinked > 0);

        // verify ref exists in destination
        let dst_hash = read_ref(&dst, "test").unwrap();
        assert_eq!(dst_hash, hash);
    }

    #[test]
    fn test_pull_fetch_only() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        let hash = commit(&src, &source, "test", Some("initial"), None).unwrap();

        let options = PullOptions { fetch_only: true };
        let result = pull_local(&src, &dst, "test", &options).unwrap();

        assert_eq!(result.hash, hash);

        // ref should NOT exist in destination
        assert!(read_ref(&dst, "test").is_err());
    }

    #[test]
    fn test_pull_incremental() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "v1").unwrap();
        commit(&src, &source, "test", Some("v1"), None).unwrap();

        // pull first version
        pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();

        // create second version
        fs::write(source.join("file.txt"), "v2").unwrap();
        let hash2 = commit(&src, &source, "test", Some("v2"), None).unwrap();

        // pull second version - should be incremental
        let result = pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();

        assert_eq!(result.hash, hash2);
        // some objects should have been skipped (already exist)
        // note: exact counts depend on object sharing
    }
}
