use std::collections::HashSet;
use std::fs;

use walkdir::WalkDir;

use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_artifact, read_commit, read_tree};
use crate::refs::{list_artifact_refs, list_refs, read_artifact_ref};
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
    Artifact,
}

impl std::fmt::Display for ObjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectType::Blob => write!(f, "blob"),
            ObjectType::Tree => write!(f, "tree"),
            ObjectType::Commit => write!(f, "commit"),
            ObjectType::Artifact => write!(f, "artifact"),
        }
    }
}

/// verify repository integrity
pub fn fsck(repo: &Repo) -> Result<FsckReport> {
    let mut report = FsckReport::default();
    let mut reachable_blobs = HashSet::new();
    let mut reachable_trees = HashSet::new();
    let mut reachable_commits = HashSet::new();
    let mut reachable_artifacts = HashSet::new();

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

    for reference in list_artifact_refs(repo)? {
        let hash = read_artifact_ref(repo, &reference)?;
        reachable_artifacts.insert(hash);
        check_artifact(&reference, hash, repo, &mut report);
    }

    // find all objects on disk
    let all_blobs = list_objects(&repo.blobs_path())?;
    let all_trees = list_objects(&repo.trees_path())?;
    let all_commits = list_objects(&repo.commits_path())?;
    let all_artifacts = list_objects(&repo.artifacts_path())?;

    // verify object hashes and find dangling objects
    for hash in &all_blobs {
        report.objects_checked += 1;
        // Reachable blobs were verified with metadata from their tree entries.
        // A dangling blob has no tree-held xattrs, so only its name is known.
        if !reachable_blobs.contains(hash) {
            report.dangling_objects.push(*hash);
        }
    }

    for hash in &all_trees {
        report.objects_checked += 1;

        // verify tree hash
        let path = crate::object::tree_path(repo, hash);
        if let Ok(compressed) = fs::read(&path) {
            let actual_hash = Hash::from_bytes(*blake3::hash(&compressed).as_bytes());
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
            let actual_hash = Hash::from_bytes(*blake3::hash(&compressed).as_bytes());
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

    for hash in &all_artifacts {
        report.objects_checked += 1;
        if let Err(error) = read_artifact(repo, hash) {
            report.corrupt_objects.push(CorruptObject {
                hash: *hash,
                object_type: ObjectType::Artifact,
                message: error.to_string(),
            });
        }
        if !reachable_artifacts.contains(hash) {
            report.dangling_objects.push(*hash);
        }
    }

    Ok(report)
}

fn check_artifact(reference: &str, hash: Hash, repo: &Repo, report: &mut FsckReport) {
    match read_artifact(repo, &hash) {
        Ok(_) => {}
        Err(crate::Error::ObjectNotFound(_)) => report.missing_objects.push(MissingObject {
            hash,
            object_type: ObjectType::Artifact,
            referenced_by: format!("artifact ref {reference}"),
        }),
        Err(error) => report.corrupt_objects.push(CorruptObject {
            hash,
            object_type: ObjectType::Artifact,
            message: error.to_string(),
        }),
    }
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
                    EntryKind::Regular { hash, xattrs, .. } => check_blob(
                        repo,
                        hash,
                        xattrs,
                        false,
                        format!("tree {tree_hash} entry {}", entry.name),
                        reachable_blobs,
                        report,
                    )?,
                    EntryKind::Symlink { hash, xattrs } => check_blob(
                        repo,
                        hash,
                        xattrs,
                        true,
                        format!("tree {tree_hash} entry {}", entry.name),
                        reachable_blobs,
                        report,
                    )?,
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

fn check_blob(
    repo: &Repo,
    hash: &Hash,
    xattrs: &[crate::types::Xattr],
    symlink: bool,
    referenced_by: String,
    reachable_blobs: &mut HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    reachable_blobs.insert(*hash);
    match crate::object::verify_blob(repo, hash, xattrs, symlink) {
        Ok(()) => {}
        Err(crate::Error::ObjectNotFound(_)) => report.missing_objects.push(MissingObject {
            hash: *hash,
            object_type: ObjectType::Blob,
            referenced_by,
        }),
        Err(crate::Error::CorruptObject(_)) => report.corrupt_objects.push(CorruptObject {
            hash: *hash,
            object_type: ObjectType::Blob,
            message: "hash mismatch".to_string(),
        }),
        Err(error) => return Err(error),
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
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
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
#[path = "fsck_tests.rs"]
mod tests;
