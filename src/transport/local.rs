//! local file transport for repository operations

use std::fs;
use std::path::Path;

use walkdir::WalkDir;

use crate::error::{IoResultExt, Result};
use crate::hash::Hash;
use crate::repo::Repo;

/// copy objects from source repo to destination repo
pub fn copy_objects(src: &Repo, dst: &Repo, hashes: &ObjectSet) -> Result<TransferStats> {
    let mut stats = TransferStats::default();

    // copy blobs
    for hash in &hashes.blobs {
        copy_object(&src.blobs_path(), &dst.blobs_path(), hash, &mut stats)?;
    }

    // copy trees
    for hash in &hashes.trees {
        copy_object(&src.trees_path(), &dst.trees_path(), hash, &mut stats)?;
    }

    // copy commits
    for hash in &hashes.commits {
        copy_object(&src.commits_path(), &dst.commits_path(), hash, &mut stats)?;
    }

    Ok(stats)
}

/// copy a single object file
fn copy_object(
    src_dir: &Path,
    dst_dir: &Path,
    hash: &Hash,
    stats: &mut TransferStats,
) -> Result<()> {
    let hex = hash.to_hex();
    let prefix = &hex[..2];
    let suffix = &hex[2..];

    let src_path = src_dir.join(prefix).join(suffix);
    let dst_path = dst_dir.join(prefix).join(suffix);

    if dst_path.exists() {
        stats.skipped += 1;
        return Ok(());
    }

    // ensure parent directory exists
    if let Some(parent) = dst_path.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }

    // try hardlink first (same filesystem), fall back to copy
    if fs::hard_link(&src_path, &dst_path).is_ok() {
        stats.hardlinked += 1;
    } else {
        let content = fs::read(&src_path).with_path(&src_path)?;
        stats.bytes_transferred += content.len() as u64;
        fs::write(&dst_path, &content).with_path(&dst_path)?;
        stats.copied += 1;
    }

    Ok(())
}

/// list all objects in a repository
pub fn list_all_objects(repo: &Repo) -> Result<ObjectSet> {
    Ok(ObjectSet {
        blobs: list_objects_in_dir(&repo.blobs_path())?,
        trees: list_objects_in_dir(&repo.trees_path())?,
        commits: list_objects_in_dir(&repo.commits_path())?,
    })
}

/// list objects in a directory
fn list_objects_in_dir(dir: &Path) -> Result<Vec<Hash>> {
    let mut hashes = Vec::new();

    if !dir.exists() {
        return Ok(hashes);
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

/// set of objects for transfer
#[derive(Debug, Default, Clone)]
pub struct ObjectSet {
    pub blobs: Vec<Hash>,
    pub trees: Vec<Hash>,
    pub commits: Vec<Hash>,
}

impl ObjectSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty() && self.trees.is_empty() && self.commits.is_empty()
    }

    pub fn total_count(&self) -> usize {
        self.blobs.len() + self.trees.len() + self.commits.len()
    }
}

/// transfer statistics
#[derive(Debug, Default, Clone)]
pub struct TransferStats {
    pub copied: usize,
    pub hardlinked: usize,
    pub skipped: usize,
    pub bytes_transferred: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit;
    use tempfile::tempdir;

    #[test]
    fn test_list_objects() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let objects = list_all_objects(&repo).unwrap();

        assert!(!objects.blobs.is_empty());
        assert!(!objects.trees.is_empty());
        assert!(!objects.commits.is_empty());
    }

    #[test]
    fn test_copy_objects() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&src, &source, "test", None, None).unwrap();

        let objects = list_all_objects(&src).unwrap();
        let stats = copy_objects(&src, &dst, &objects).unwrap();

        assert!(stats.copied > 0 || stats.hardlinked > 0);

        // verify objects exist in destination
        let dst_objects = list_all_objects(&dst).unwrap();
        assert_eq!(objects.blobs.len(), dst_objects.blobs.len());
        assert_eq!(objects.trees.len(), dst_objects.trees.len());
        assert_eq!(objects.commits.len(), dst_objects.commits.len());
    }
}
