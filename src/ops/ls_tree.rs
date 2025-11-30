use std::path::Path;

use crate::error::Result;
use crate::object::{read_commit, read_tree};
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::{EntryKind, Tree, TreeEntry};

/// list tree entry with full path
#[derive(Debug, Clone)]
pub struct LsTreeEntry {
    pub path: String,
    pub entry: TreeEntry,
}

/// list tree contents, optionally at a specific path
pub fn ls_tree(repo: &Repo, ref_name: &str, path: Option<&Path>) -> Result<Vec<LsTreeEntry>> {
    let commit_hash = resolve_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;
    let tree = read_tree(repo, &commit.tree)?;

    match path {
        Some(p) => ls_tree_at_path(repo, &tree, p),
        None => ls_tree_flat(repo, &tree, ""),
    }
}

/// list tree at a specific path
fn ls_tree_at_path(repo: &Repo, tree: &Tree, path: &Path) -> Result<Vec<LsTreeEntry>> {
    let path_str = path.to_string_lossy();
    let components: Vec<&str> = path_str
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if components.is_empty() {
        return ls_tree_flat(repo, tree, "");
    }

    // navigate to target directory
    let mut current_tree = tree.clone();
    let mut current_path = String::new();

    for (i, component) in components.iter().enumerate() {
        match current_tree.get(component) {
            Some(entry) => {
                if i < components.len() - 1 {
                    // not the last component, must be a directory
                    if let EntryKind::Directory { hash, .. } = &entry.kind {
                        current_tree = read_tree(repo, hash)?;
                        if current_path.is_empty() {
                            current_path = component.to_string();
                        } else {
                            current_path = format!("{}/{}", current_path, component);
                        }
                    } else {
                        // path component is not a directory
                        return Ok(vec![]);
                    }
                } else {
                    // last component - could be file or directory
                    if let EntryKind::Directory { hash, .. } = &entry.kind {
                        let subtree = read_tree(repo, hash)?;
                        let prefix = if current_path.is_empty() {
                            component.to_string()
                        } else {
                            format!("{}/{}", current_path, component)
                        };
                        return ls_tree_flat(repo, &subtree, &prefix);
                    } else {
                        // return just this entry
                        let full_path = if current_path.is_empty() {
                            component.to_string()
                        } else {
                            format!("{}/{}", current_path, component)
                        };
                        return Ok(vec![LsTreeEntry {
                            path: full_path,
                            entry: entry.clone(),
                        }]);
                    }
                }
            }
            None => {
                // path not found
                return Ok(vec![]);
            }
        }
    }

    // shouldn't reach here
    ls_tree_flat(repo, &current_tree, &current_path)
}

/// list tree contents flat (non-recursive)
fn ls_tree_flat(_repo: &Repo, tree: &Tree, prefix: &str) -> Result<Vec<LsTreeEntry>> {
    let mut entries = Vec::new();

    for entry in tree.entries() {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        entries.push(LsTreeEntry {
            path,
            entry: entry.clone(),
        });
    }

    Ok(entries)
}

/// list tree contents recursively
pub fn ls_tree_recursive(repo: &Repo, ref_name: &str) -> Result<Vec<LsTreeEntry>> {
    let commit_hash = resolve_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;
    let tree = read_tree(repo, &commit.tree)?;

    let mut entries = Vec::new();
    ls_tree_recursive_impl(repo, &tree, "", &mut entries)?;
    Ok(entries)
}

fn ls_tree_recursive_impl(
    repo: &Repo,
    tree: &Tree,
    prefix: &str,
    entries: &mut Vec<LsTreeEntry>,
) -> Result<()> {
    for entry in tree.entries() {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        entries.push(LsTreeEntry {
            path: path.clone(),
            entry: entry.clone(),
        });

        // recurse into directories
        if let EntryKind::Directory { hash, .. } = &entry.kind {
            let subtree = read_tree(repo, hash)?;
            ls_tree_recursive_impl(repo, &subtree, &path, entries)?;
        }
    }

    Ok(())
}

impl std::fmt::Display for LsTreeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = match &self.entry.kind {
            EntryKind::Regular { .. } => "100644",
            EntryKind::Symlink { .. } => "120000",
            EntryKind::Directory { .. } => "040000",
            EntryKind::BlockDevice { .. } => "060000",
            EntryKind::CharDevice { .. } => "020000",
            EntryKind::Fifo { .. } => "010000",
            EntryKind::Socket { .. } => "140000",
            EntryKind::Hardlink { .. } => "100644", // shows as regular file mode
        };

        let type_str = self.entry.kind.type_name();

        let hash_str = match self.entry.kind.hash() {
            Some(h) => h.to_hex()[..12].to_string(),
            None => "-".repeat(12),
        };

        write!(f, "{} {} {}    {}", mode, type_str, hash_str, self.path)
    }
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
    fn test_ls_tree_root() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        fs::create_dir(source.join("subdir")).unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let entries = ls_tree(&repo, "test", None).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.entry.name == "file.txt"));
        assert!(entries.iter().any(|e| e.entry.name == "subdir"));
    }

    #[test]
    fn test_ls_tree_subdir() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::write(source.join("subdir/a.txt"), "a").unwrap();
        fs::write(source.join("subdir/b.txt"), "b").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let entries = ls_tree(&repo, "test", Some(Path::new("subdir"))).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.path == "subdir/a.txt"));
        assert!(entries.iter().any(|e| e.path == "subdir/b.txt"));
    }

    #[test]
    fn test_ls_tree_recursive() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir_all(source.join("a/b")).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        fs::write(source.join("a/nested.txt"), "nested").unwrap();
        fs::write(source.join("a/b/deep.txt"), "deep").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let entries = ls_tree_recursive(&repo, "test").unwrap();

        // should have: file.txt, a/, a/nested.txt, a/b/, a/b/deep.txt
        assert!(entries.iter().any(|e| e.path == "file.txt"));
        assert!(entries.iter().any(|e| e.path == "a"));
        assert!(entries.iter().any(|e| e.path == "a/nested.txt"));
        assert!(entries.iter().any(|e| e.path == "a/b"));
        assert!(entries.iter().any(|e| e.path == "a/b/deep.txt"));
    }

    #[test]
    fn test_ls_tree_entry_display() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let entries = ls_tree(&repo, "test", None).unwrap();
        let display = format!("{}", entries[0]);

        assert!(display.contains("100644"));
        assert!(display.contains("regular"));
        assert!(display.contains("file.txt"));
    }
}
