use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_commit, read_tree, verify_artifact};
use crate::refs::{list_artifact_refs, list_refs, read_artifact_ref};
use crate::repo::Repo;
use crate::types::EntryKind;
use std::collections::HashSet;

#[path = "fsck_store.rs"]
mod store;

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
    let mut reachable = Reachable::default();
    check_reachable(repo, &mut reachable, &mut report)?;
    trace_memory("reachable objects", &reachable);
    store::check_stored_objects(repo, &reachable, &mut report)?;
    trace_memory("stored objects", &reachable);
    Ok(report)
}

#[derive(Default)]
pub(super) struct Reachable {
    blobs: HashSet<Hash>,
    trees: HashSet<Hash>,
    commits: HashSet<Hash>,
    artifacts: HashSet<Hash>,
}

fn check_reachable(repo: &Repo, reachable: &mut Reachable, report: &mut FsckReport) -> Result<()> {
    for ref_name in list_refs(repo)? {
        let commit_hash = crate::refs::read_ref(repo, &ref_name)?;
        check_commit(
            repo,
            &commit_hash,
            &ref_name,
            &mut reachable.blobs,
            &mut reachable.trees,
            &mut reachable.commits,
            report,
        )?;
    }
    trace_memory("refs", reachable);
    for reference in list_artifact_refs(repo)? {
        let hash = read_artifact_ref(repo, &reference)?;
        reachable.artifacts.insert(hash);
        check_artifact(&reference, hash, repo, report);
    }
    trace_memory("artifact refs", reachable);
    Ok(())
}

pub(super) fn trace_memory(label: &str, reachable: &Reachable) {
    if std::env::var_os("ZUB_PERF_TRACE").is_none() {
        return;
    }
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let rss = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .unwrap_or("VmRSS: unknown");
    let peak = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap_or("VmHWM: unknown");
    eprintln!(
        "zub perf: fsck {label}: {rss}; {peak}; reachable blobs={} trees={} commits={} artifacts={}",
        reachable.blobs.len(),
        reachable.trees.len(),
        reachable.commits.len(),
        reachable.artifacts.len()
    );
}

fn check_artifact(reference: &str, hash: Hash, repo: &Repo, report: &mut FsckReport) {
    match verify_artifact(repo, &hash) {
        Ok(()) => {}
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
        Ok(tree) => check_tree_entries(
            repo,
            tree_hash,
            &tree,
            reachable_blobs,
            reachable_trees,
            report,
        )?,
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

fn check_tree_entries(
    repo: &Repo,
    tree_hash: &Hash,
    tree: &crate::Tree,
    reachable_blobs: &mut HashSet<Hash>,
    reachable_trees: &mut HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    for entry in tree.entries() {
        let referenced_by = format!("tree {tree_hash} entry {}", entry.name);
        match &entry.kind {
            EntryKind::Regular { hash, xattrs, .. } => check_blob(
                repo,
                hash,
                xattrs,
                false,
                referenced_by,
                reachable_blobs,
                report,
            )?,
            EntryKind::Symlink { hash, xattrs } => check_blob(
                repo,
                hash,
                xattrs,
                true,
                referenced_by,
                reachable_blobs,
                report,
            )?,
            EntryKind::Directory { hash, .. } => check_tree(
                repo,
                hash,
                &referenced_by,
                reachable_blobs,
                reachable_trees,
                report,
            )?,
            _ => {}
        }
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

#[cfg(test)]
#[path = "fsck_tests.rs"]
mod tests;
