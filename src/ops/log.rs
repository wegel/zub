use crate::error::Result;
use crate::hash::Hash;
use crate::object::read_commit;
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::Commit;

/// commit with its hash for log output
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub hash: Hash,
    pub commit: Commit,
}

/// get commit history for a ref
pub fn log(repo: &Repo, ref_name: &str, max_count: Option<usize>) -> Result<Vec<LogEntry>> {
    let head_hash = resolve_ref(repo, ref_name)?;
    let mut entries = Vec::new();
    let mut to_visit = vec![head_hash];
    let mut visited = std::collections::HashSet::new();

    while let Some(hash) = to_visit.pop() {
        if visited.contains(&hash) {
            continue;
        }
        visited.insert(hash);

        if let Some(max) = max_count {
            if entries.len() >= max {
                break;
            }
        }

        let commit = read_commit(repo, &hash)?;

        // add parents to visit queue (oldest first for linear history)
        for parent in commit.parents.iter().rev() {
            to_visit.push(*parent);
        }

        entries.push(LogEntry { hash, commit });
    }

    // sort by timestamp descending (newest first)
    entries.sort_by(|a, b| b.commit.timestamp.cmp(&a.commit.timestamp));

    // apply limit after sorting
    if let Some(max) = max_count {
        entries.truncate(max);
    }

    Ok(entries)
}

/// format a log entry for display
impl std::fmt::Display for LogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "commit {}", self.hash)?;
        writeln!(f, "Author: {}", self.commit.author)?;

        // format timestamp
        let datetime = chrono_format(self.commit.timestamp);
        writeln!(f, "Date:   {}", datetime)?;

        writeln!(f)?;
        for line in self.commit.message.lines() {
            writeln!(f, "    {}", line)?;
        }

        Ok(())
    }
}

/// simple timestamp formatting (without chrono dependency)
fn chrono_format(timestamp: i64) -> String {
    // basic ISO-8601 format
    use std::time::{Duration, UNIX_EPOCH};

    let datetime = UNIX_EPOCH + Duration::from_secs(timestamp as u64);
    let duration_since_epoch = datetime
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = duration_since_epoch.as_secs();

    // very basic formatting - just show unix timestamp if we can't format properly
    // a real implementation would use chrono or time crate
    let days = secs / 86400;
    let years_approx = 1970 + (days / 365);
    let remaining_days = days % 365;
    let months_approx = remaining_days / 30;
    let day_of_month = remaining_days % 30 + 1;

    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        years_approx,
        months_approx + 1,
        day_of_month,
        hours,
        minutes,
        seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit::commit;
    use std::fs;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_log_single_commit() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", Some("first commit"), None).unwrap();

        let entries = log(&repo, "test", None).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].commit.message, "first commit");
    }

    #[test]
    fn test_log_multiple_commits() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();

        fs::write(source.join("file.txt"), "v1").unwrap();
        commit(&repo, &source, "test", Some("commit 1"), None).unwrap();

        fs::write(source.join("file.txt"), "v2").unwrap();
        commit(&repo, &source, "test", Some("commit 2"), None).unwrap();

        fs::write(source.join("file.txt"), "v3").unwrap();
        commit(&repo, &source, "test", Some("commit 3"), None).unwrap();

        let entries = log(&repo, "test", None).unwrap();

        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_log_max_count() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();

        for i in 0..5 {
            fs::write(source.join("file.txt"), format!("v{}", i)).unwrap();
            commit(&repo, &source, "test", Some(&format!("commit {}", i)), None).unwrap();
        }

        let entries = log(&repo, "test", Some(2)).unwrap();

        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_log_entry_display() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", Some("test message"), Some("Test Author")).unwrap();

        let entries = log(&repo, "test", None).unwrap();
        let display = format!("{}", entries[0]);

        assert!(display.contains("commit"));
        assert!(display.contains("Author: Test Author"));
        assert!(display.contains("test message"));
    }
}
