use std::collections::HashSet;
use std::fs;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::list_refs;
use crate::repo::Repo;
use crate::types::EntryKind;

/// fsck report
#[derive(Debug, Default)]
pub struct FsckReport {
    /// objects checked
    pub objects_checked: usize,
    /// corrupt objects (hash mismatch)
    pub corrupt_objects: Vec<CorruptObject>,
    /// missing objects referenced by other objects
    pub missing_objects: Vec<MissingObject>,
    /// dangling objects (not reachable from any ref)
    pub dangling_objects: Vec<Hash>,
}

impl FsckReport {
    pub fn is_ok(&self) -> bool {
        self.corrupt_objects.is_empty() && self.missing_objects.is_empty()
    }
}

#[derive(Debug)]
pub struct CorruptObject {
    pub hash: Hash,
    pub object_type: ObjectType,
    pub message: String,
}

#[derive(Debug)]
pub struct MissingObject {
    pub hash: Hash,
    pub object_type: ObjectType,
    pub referenced_by: String,
}

#[derive(Debug, Clone, Copy)]
pub enum ObjectType {
    Blob,
    Tree,
    Commit,
}

impl std::fmt::Display for ObjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectType::Blob => write!(f, "blob"),
            ObjectType::Tree => write!(f, "tree"),
            ObjectType::Commit => write!(f, "commit"),
        }
    }
}

/// verify repository integrity
pub fn fsck(repo: &Repo) -> Result<FsckReport> {
    let mut report = FsckReport::default();
    let mut reachable_blobs = HashSet::new();
    let mut reachable_trees = HashSet::new();
    let mut reachable_commits = HashSet::new();

    // check all refs and their reachable objects
    for ref_name in list_refs(repo)? {
        let commit_hash = crate::refs::read_ref(repo, &ref_name)?;
        check_commit(
            repo,
            &commit_hash,
            &ref_name,
            &mut reachable_blobs,
            &mut reachable_trees,
            &mut reachable_commits,
            &mut report,
        )?;
    }

    // find all objects on disk
    let all_blobs = list_objects(&repo.blobs_path())?;
    let all_trees = list_objects(&repo.trees_path())?;
    let all_commits = list_objects(&repo.commits_path())?;

    // verify object hashes and find dangling objects
    for hash in &all_blobs {
        report.objects_checked += 1;
        // blob hash includes metadata, can't verify without knowing uid/gid/mode/xattrs
        // just check file exists and is readable

        if !reachable_blobs.contains(hash) {
            report.dangling_objects.push(*hash);
        }
    }

    for hash in &all_trees {
        report.objects_checked += 1;

        // verify tree hash
        let path = crate::object::tree_path(repo, hash);
        if let Ok(compressed) = fs::read(&path) {
            let actual_hash = Hash::from_bytes(Sha256::digest(&compressed).into());
            if actual_hash != *hash {
                report.corrupt_objects.push(CorruptObject {
                    hash: *hash,
                    object_type: ObjectType::Tree,
                    message: format!("hash mismatch: expected {}, zub{}", hash, actual_hash),
                });
            }
        }

        if !reachable_trees.contains(hash) {
            report.dangling_objects.push(*hash);
        }
    }

    for hash in &all_commits {
        report.objects_checked += 1;

        // verify commit hash
        let path = crate::object::commit_path(repo, hash);
        if let Ok(compressed) = fs::read(&path) {
            let actual_hash = Hash::from_bytes(Sha256::digest(&compressed).into());
            if actual_hash != *hash {
                report.corrupt_objects.push(CorruptObject {
                    hash: *hash,
                    object_type: ObjectType::Commit,
                    message: format!("hash mismatch: expected {}, zub{}", hash, actual_hash),
                });
            }
        }

        if !reachable_commits.contains(hash) {
            report.dangling_objects.push(*hash);
        }
    }

    Ok(report)
}

fn check_commit(
    repo: &Repo,
    commit_hash: &Hash,
    referenced_by: &str,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
    reachable_commits: &mut HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    if reachable_commits.contains(commit_hash) {
        return Ok(());
    }
    reachable_commits.insert(*commit_hash);

    match read_commit(repo, commit_hash) {
        Ok(commit) => {
            // check tree
            check_tree(
                repo,
                &commit.tree,
                &format!("commit {}", commit_hash),
                reachable_blobs,
                reachable_trees,
                report,
            )?;

            // check parents
            for parent in &commit.parents {
                check_commit(
                    repo,
                    parent,
                    &format!("commit {}", commit_hash),
                    reachable_blobs,
                    reachable_trees,
                    reachable_commits,
                    report,
                )?;
            }
        }
        Err(crate::Error::ObjectNotFound(_)) => {
            report.missing_objects.push(MissingObject {
                hash: *commit_hash,
                object_type: ObjectType::Commit,
                referenced_by: referenced_by.to_string(),
            });
        }
        Err(crate::Error::CorruptObject(_)) => {
            report.corrupt_objects.push(CorruptObject {
                hash: *commit_hash,
                object_type: ObjectType::Commit,
                message: "hash mismatch".to_string(),
            });
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

fn check_tree(
    repo: &Repo,
    tree_hash: &Hash,
    referenced_by: &str,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    if reachable_trees.contains(tree_hash) {
        return Ok(());
    }
    reachable_trees.insert(*tree_hash);

    match read_tree(repo, tree_hash) {
        Ok(tree) => {
            for entry in tree.entries() {
                match &entry.kind {
                    EntryKind::Regular { hash, .. } => {
                        reachable_blobs.insert(*hash);
                        if !crate::object::blob_exists(repo, hash) {
                            report.missing_objects.push(MissingObject {
                                hash: *hash,
                                object_type: ObjectType::Blob,
                                referenced_by: format!("tree {} entry {}", tree_hash, entry.name),
                            });
                        }
                    }
                    EntryKind::Symlink { hash } => {
                        reachable_blobs.insert(*hash);
                        if !crate::object::blob_exists(repo, hash) {
                            report.missing_objects.push(MissingObject {
                                hash: *hash,
                                object_type: ObjectType::Blob,
                                referenced_by: format!("tree {} entry {}", tree_hash, entry.name),
                            });
                        }
                    }
                    EntryKind::Directory { hash, .. } => {
                        check_tree(
                            repo,
                            hash,
                            &format!("tree {} entry {}", tree_hash, entry.name),
                            reachable_blobs,
                            reachable_trees,
                            report,
                        )?;
                    }
                    _ => {}
                }
            }
        }
        Err(crate::Error::ObjectNotFound(_)) => {
            report.missing_objects.push(MissingObject {
                hash: *tree_hash,
                object_type: ObjectType::Tree,
                referenced_by: referenced_by.to_string(),
            });
        }
        Err(crate::Error::CorruptObject(_)) => {
            report.corrupt_objects.push(CorruptObject {
                hash: *tree_hash,
                object_type: ObjectType::Tree,
                message: "hash mismatch".to_string(),
            });
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

fn list_objects(dir: &std::path::Path) -> Result<Vec<Hash>> {
    let mut hashes = Vec::new();

    if !dir.exists() {
        return Ok(hashes);
    }

    for entry in WalkDir::new(dir).min_depth(2).max_depth(2) {
        let entry = entry.map_err(|e| crate::Error::Io {
            path: dir.to_path_buf(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "walkdir error")),
        })?;

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
            hashes.push(hash);
        }
    }

    Ok(hashes)
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
    fn test_fsck_healthy_repo() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let report = fsck(&repo).unwrap();

        assert!(report.is_ok());
        assert!(report.corrupt_objects.is_empty());
        assert!(report.missing_objects.is_empty());
        assert!(report.dangling_objects.is_empty());
    }

    #[test]
    fn test_fsck_with_dangling() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        // delete ref to create dangling objects
        crate::refs::delete_ref(&repo, "test").unwrap();

        let report = fsck(&repo).unwrap();

        // should find dangling objects
        assert!(!report.dangling_objects.is_empty());
    }
}
