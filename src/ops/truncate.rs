//! truncate commit history

use crate::error::Result;
use crate::object::{read_commit, write_commit};
use crate::refs::{list_refs, read_ref, write_ref};
use crate::repo::Repo;
use crate::types::Commit;

/// truncate history statistics
#[derive(Debug, Default)]
pub struct TruncateStats {
    pub refs_processed: usize,
    pub refs_truncated: usize,
}

/// truncate history, keeping only the latest commit per ref
///
/// for each ref, creates a new commit with the same tree but no parents,
/// then updates the ref to point to the new commit
pub fn truncate_history(repo: &Repo, dry_run: bool) -> Result<TruncateStats> {
    let mut stats = TruncateStats::default();

    for ref_name in list_refs(repo)? {
        stats.refs_processed += 1;

        let commit_hash = read_ref(repo, &ref_name)?;
        let commit = read_commit(repo, &commit_hash)?;

        // skip if already has no parents
        if commit.parents.is_empty() {
            continue;
        }

        stats.refs_truncated += 1;

        if dry_run {
            continue;
        }

        // create new commit with same tree but no parents
        let new_commit = Commit {
            tree: commit.tree,
            parents: vec![],
            message: commit.message,
            author: commit.author,
            timestamp: commit.timestamp,
            metadata: commit.metadata,
        };

        let new_hash = write_commit(repo, &new_commit)?;
        write_ref(repo, &ref_name, &new_hash)?;
    }

    Ok(stats)
}
