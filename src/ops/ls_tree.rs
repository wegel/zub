use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::error::Result;
use crate::object::{blob_path, read_blob, read_commit, read_tree};
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::{EntryKind, Tree, TreeEntry};

/// options for ls-tree output
#[derive(Clone, Default)]
pub struct LsTreeOptions {
    /// show long format (permissions, uid, gid, size)
    pub long: bool,
    /// show human-readable sizes
    pub human: bool,
}

/// resolved metadata for long format display
#[derive(Debug, Clone, Default)]
pub struct EntryMetadata {
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub size: u64,
}

/// list tree entry with full path
#[derive(Debug, Clone)]
pub struct LsTreeEntry {
    pub path: String,
    pub entry: TreeEntry,
    /// resolved metadata (only populated in long mode)
    pub metadata: Option<EntryMetadata>,
}

/// list tree contents, optionally at a specific path
pub fn ls_tree(
    repo: &Repo,
    ref_name: &str,
    path: Option<&Path>,
    opts: &LsTreeOptions,
) -> Result<Vec<LsTreeEntry>> {
    let commit_hash = resolve_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;
    let tree = read_tree(repo, &commit.tree)?;

    match path {
        Some(p) => ls_tree_at_path(repo, &tree, p, opts),
        None => ls_tree_flat(repo, &tree, "", opts),
    }
}

/// list tree at a specific path
fn ls_tree_at_path(
    repo: &Repo,
    tree: &Tree,
    path: &Path,
    opts: &LsTreeOptions,
) -> Result<Vec<LsTreeEntry>> {
    let path_str = path.to_string_lossy();
    let components: Vec<&str> = path_str.split('/').filter(|s| !s.is_empty()).collect();

    if components.is_empty() {
        return ls_tree_flat(repo, tree, "", opts);
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
                        return ls_tree_flat(repo, &subtree, &prefix, opts);
                    } else {
                        // return just this entry
                        let full_path = if current_path.is_empty() {
                            component.to_string()
                        } else {
                            format!("{}/{}", current_path, component)
                        };
                        let metadata = if opts.long {
                            resolve_metadata(repo, &entry.kind)
                        } else {
                            None
                        };
                        return Ok(vec![LsTreeEntry {
                            path: full_path,
                            entry: entry.clone(),
                            metadata,
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
    ls_tree_flat(repo, &current_tree, &current_path, opts)
}

/// list tree contents flat (non-recursive)
fn ls_tree_flat(
    repo: &Repo,
    tree: &Tree,
    prefix: &str,
    opts: &LsTreeOptions,
) -> Result<Vec<LsTreeEntry>> {
    let mut entries = Vec::new();

    for entry in tree.entries() {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        let metadata = if opts.long {
            resolve_metadata(repo, &entry.kind)
        } else {
            None
        };

        entries.push(LsTreeEntry {
            path,
            entry: entry.clone(),
            metadata,
        });
    }

    Ok(entries)
}

/// list tree contents recursively
pub fn ls_tree_recursive(
    repo: &Repo,
    ref_name: &str,
    opts: &LsTreeOptions,
) -> Result<Vec<LsTreeEntry>> {
    let commit_hash = resolve_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;
    let tree = read_tree(repo, &commit.tree)?;

    let mut entries = Vec::new();
    ls_tree_recursive_impl(repo, &tree, "", &mut entries, opts)?;
    Ok(entries)
}

fn ls_tree_recursive_impl(
    repo: &Repo,
    tree: &Tree,
    prefix: &str,
    entries: &mut Vec<LsTreeEntry>,
    opts: &LsTreeOptions,
) -> Result<()> {
    for entry in tree.entries() {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        let metadata = if opts.long {
            resolve_metadata(repo, &entry.kind)
        } else {
            None
        };

        entries.push(LsTreeEntry {
            path: path.clone(),
            entry: entry.clone(),
            metadata,
        });

        // recurse into directories
        if let EntryKind::Directory { hash, .. } = &entry.kind {
            let subtree = read_tree(repo, hash)?;
            ls_tree_recursive_impl(repo, &subtree, &path, entries, opts)?;
        }
    }

    Ok(())
}

/// resolve metadata for an entry (reads blob file for regular/symlink)
fn resolve_metadata(repo: &Repo, kind: &EntryKind) -> Option<EntryMetadata> {
    match kind {
        EntryKind::Regular { hash, size, .. } => {
            // read uid/gid/mode from blob file
            let blob = blob_path(repo, hash);
            if let Ok(meta) = fs::metadata(&blob) {
                Some(EntryMetadata {
                    uid: meta.uid(),
                    gid: meta.gid(),
                    mode: meta.mode(),
                    size: *size,
                })
            } else {
                Some(EntryMetadata {
                    size: *size,
                    ..Default::default()
                })
            }
        }
        EntryKind::Symlink { hash, .. } => {
            // read uid/gid from blob file, size is target length
            let blob = blob_path(repo, hash);
            let size = read_blob(repo, hash).map(|b| b.len() as u64).unwrap_or(0);
            if let Ok(meta) = fs::symlink_metadata(&blob) {
                Some(EntryMetadata {
                    uid: meta.uid(),
                    gid: meta.gid(),
                    mode: 0o120777, // symlinks are always lrwxrwxrwx
                    size,
                })
            } else {
                Some(EntryMetadata {
                    mode: 0o120777,
                    size,
                    ..Default::default()
                })
            }
        }
        EntryKind::Directory { uid, gid, mode, .. } => Some(EntryMetadata {
            uid: *uid,
            gid: *gid,
            mode: 0o40000 | (*mode & 0o7777),
            size: 0,
        }),
        EntryKind::BlockDevice {
            uid, gid, mode, ..
        } => Some(EntryMetadata {
            uid: *uid,
            gid: *gid,
            mode: 0o60000 | (*mode & 0o7777),
            size: 0,
        }),
        EntryKind::CharDevice {
            uid, gid, mode, ..
        } => Some(EntryMetadata {
            uid: *uid,
            gid: *gid,
            mode: 0o20000 | (*mode & 0o7777),
            size: 0,
        }),
        EntryKind::Fifo { uid, gid, mode, .. } => Some(EntryMetadata {
            uid: *uid,
            gid: *gid,
            mode: 0o10000 | (*mode & 0o7777),
            size: 0,
        }),
        EntryKind::Socket { uid, gid, mode, .. } => Some(EntryMetadata {
            uid: *uid,
            gid: *gid,
            mode: 0o140000 | (*mode & 0o7777),
            size: 0,
        }),
        EntryKind::Hardlink { .. } => {
            // hardlinks don't have their own metadata
            None
        }
    }
}

impl LsTreeEntry {
    /// format entry with options
    pub fn format(&self, opts: &LsTreeOptions) -> String {
        if opts.long {
            self.format_long(opts.human)
        } else {
            self.format_short()
        }
    }

    /// short format (default)
    fn format_short(&self) -> String {
        let mode = match &self.entry.kind {
            EntryKind::Regular { .. } => "100644",
            EntryKind::Symlink { .. } => "120000",
            EntryKind::Directory { .. } => "040000",
            EntryKind::BlockDevice { .. } => "060000",
            EntryKind::CharDevice { .. } => "020000",
            EntryKind::Fifo { .. } => "010000",
            EntryKind::Socket { .. } => "140000",
            EntryKind::Hardlink { .. } => "100644",
        };

        let type_str = self.entry.kind.type_name();
        let hash_str = match self.entry.kind.hash() {
            Some(h) => h.to_hex()[..12].to_string(),
            None => "-".repeat(12),
        };

        format!("{} {} {}    {}", mode, type_str, hash_str, self.path)
    }

    /// long format (like ls -l)
    fn format_long(&self, human: bool) -> String {
        let meta = self.metadata.as_ref();

        // permissions string (like -rwxr-xr-x)
        let perms = if let Some(m) = meta {
            format_permissions(m.mode)
        } else {
            "----------".to_string()
        };

        // uid/gid
        let uid = meta.map(|m| m.uid).unwrap_or(0);
        let gid = meta.map(|m| m.gid).unwrap_or(0);

        // size
        let size = meta.map(|m| m.size).unwrap_or(0);
        let size_str = if human {
            format_human_size(size)
        } else {
            format!("{:>8}", size)
        };

        // for symlinks, add target
        if let EntryKind::Hardlink { target_path } = &self.entry.kind {
            format!(
                "{} {:>5} {:>5} {} {} -> {}",
                perms, uid, gid, size_str, self.path, target_path
            )
        } else {
            format!(
                "{} {:>5} {:>5} {} {}",
                perms, uid, gid, size_str, self.path
            )
        }
    }
}

/// format mode as permission string (e.g., -rwxr-xr-x)
fn format_permissions(mode: u32) -> String {
    let file_type = match mode & 0o170000 {
        0o100000 => '-', // regular file
        0o040000 => 'd', // directory
        0o120000 => 'l', // symlink
        0o060000 => 'b', // block device
        0o020000 => 'c', // char device
        0o010000 => 'p', // fifo
        0o140000 => 's', // socket
        _ => '?',
    };

    let perms = mode & 0o777;
    let mut s = String::with_capacity(10);
    s.push(file_type);

    // owner
    s.push(if perms & 0o400 != 0 { 'r' } else { '-' });
    s.push(if perms & 0o200 != 0 { 'w' } else { '-' });
    s.push(if perms & 0o100 != 0 { 'x' } else { '-' });

    // group
    s.push(if perms & 0o040 != 0 { 'r' } else { '-' });
    s.push(if perms & 0o020 != 0 { 'w' } else { '-' });
    s.push(if perms & 0o010 != 0 { 'x' } else { '-' });

    // other
    s.push(if perms & 0o004 != 0 { 'r' } else { '-' });
    s.push(if perms & 0o002 != 0 { 'w' } else { '-' });
    s.push(if perms & 0o001 != 0 { 'x' } else { '-' });

    s
}

/// format size in human-readable form
fn format_human_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T", "P"];
    let mut size = size as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{:>5}", size as u64)
    } else if size >= 10.0 {
        format!("{:>4.0}{}", size, UNITS[unit_idx])
    } else {
        format!("{:>3.1}{}", size, UNITS[unit_idx])
    }
}

impl std::fmt::Display for LsTreeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_short())
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

        let entries = ls_tree(&repo, "test", None, &LsTreeOptions::default()).unwrap();

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

        let entries =
            ls_tree(&repo, "test", Some(Path::new("subdir")), &LsTreeOptions::default()).unwrap();

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

        let entries = ls_tree_recursive(&repo, "test", &LsTreeOptions::default()).unwrap();

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

        let entries = ls_tree(&repo, "test", None, &LsTreeOptions::default()).unwrap();
        let display = format!("{}", entries[0]);

        assert!(display.contains("100644"));
        assert!(display.contains("regular"));
        assert!(display.contains("file.txt"));
    }

    #[test]
    fn test_ls_tree_long_format() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        let opts = LsTreeOptions {
            long: true,
            human: false,
        };
        let entries = ls_tree(&repo, "test", None, &opts).unwrap();

        // should have metadata
        assert!(entries[0].metadata.is_some());
        let meta = entries[0].metadata.as_ref().unwrap();
        assert_eq!(meta.size, 7); // "content" is 7 bytes

        // format should include permissions
        let formatted = entries[0].format(&opts);
        assert!(formatted.contains("file.txt"));
        assert!(formatted.contains("-rw")); // regular file with some perms
    }

    #[test]
    fn test_human_size_format() {
        assert_eq!(format_human_size(0), "    0");
        assert_eq!(format_human_size(500), "  500");
        assert_eq!(format_human_size(1024), "1.0K");
        assert_eq!(format_human_size(1536), "1.5K");
        assert_eq!(format_human_size(10240), "  10K");
        assert_eq!(format_human_size(1048576), "1.0M");
        assert_eq!(format_human_size(1073741824), "1.0G");
    }
}
