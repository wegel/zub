use std::fs;
use std::path::Path;

use nix::libc;

use crate::error::{Error, IoResultExt, Result};
use crate::fs::{create_symlink, write_sparse_file};
use crate::hash::Hash;
use crate::object::{blob_path, read_blob, read_commit, read_tree};
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::{EntryKind, Tree};

/// Options controlling how paths are exported.
#[derive(Clone)]
pub struct ExportOptions {
    /// Overwrite an existing destination path (default: true)
    pub overwrite: bool,
    /// Try to hardlink from the blob store (falls back to copy on EXDEV)
    pub hardlink: bool,
    /// Preserve sparse holes when exporting sparse files
    pub preserve_sparse: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            overwrite: true,
            hardlink: true,
            preserve_sparse: false,
        }
    }
}

/// Export a single path from a ref to a destination on disk.
///
/// Supports regular files (with sparse handling), symlinks, and hardlinks.
/// Directories and device nodes are rejected.
pub fn export_path(
    repo: &Repo,
    ref_name: &str,
    path: &str,
    dest: &Path,
    opts: ExportOptions,
) -> Result<()> {
    let commit_hash = resolve_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;
    let tree = read_tree(repo, &commit.tree)?;
    let normalized = path.trim_start_matches('/');

    let entry = resolve_entry(repo, &tree, normalized)?;
    match entry {
        EntryKind::Regular { hash, sparse_map, .. } => {
            export_regular(repo, dest, &hash, sparse_map.as_deref(), &opts)
        }
        EntryKind::Symlink { hash } => export_symlink(repo, dest, &hash, &opts),
        EntryKind::Hardlink { target_path } => {
            let target_norm = target_path.trim_start_matches('/');
            let target = resolve_entry(repo, &tree, target_norm)?;
            match target {
                EntryKind::Regular { hash, sparse_map, .. } => {
                    export_regular(repo, dest, &hash, sparse_map.as_deref(), &opts)
                }
                EntryKind::Symlink { hash } => export_symlink(repo, dest, &hash, &opts),
                _ => Err(Error::InvalidObjectType(target.type_name().to_string())),
            }
        }
        _ => Err(Error::InvalidObjectType(entry.type_name().to_string())),
    }
}

fn resolve_entry(repo: &Repo, root: &Tree, path: &str) -> Result<EntryKind> {
    let mut current_tree = root.clone();
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if components.is_empty() {
        return Err(Error::PathNotFound(path.to_string()));
    }

    for (idx, component) in components.iter().enumerate() {
        let entry = current_tree
            .get(component)
            .ok_or_else(|| Error::PathNotFound(path.to_string()))?;

        let last = idx == components.len() - 1;
        match (&entry.kind, last) {
            (_, true) => return Ok(entry.kind.clone()),
            (EntryKind::Directory { hash, .. }, false) => {
                current_tree = read_tree(repo, hash)?;
            }
            _ => return Err(Error::PathNotFound(path.to_string())),
        }
    }

    Err(Error::PathNotFound(path.to_string()))
}

fn ensure_dest(dest: &Path, overwrite: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }

    if dest.exists() || dest.symlink_metadata().is_ok() {
        if !overwrite {
            return Err(Error::TargetNotEmpty(dest.to_path_buf()));
        }
        if dest.is_dir() {
            return Err(Error::TargetNotEmpty(dest.to_path_buf()));
        }
        fs::remove_file(dest).with_path(dest)?;
    }

    Ok(())
}

fn export_regular(
    repo: &Repo,
    dest: &Path,
    hash: &Hash,
    sparse_map: Option<&[crate::types::SparseRegion]>,
    opts: &ExportOptions,
) -> Result<()> {
    ensure_dest(dest, opts.overwrite)?;

    match sparse_map {
        Some(regions) if !regions.is_empty() && opts.preserve_sparse => {
            let data = read_blob(repo, hash)?;
            let total_size: u64 = regions.iter().map(|r| r.end()).max().unwrap_or(0);
            write_sparse_file(dest, &data, regions, total_size)?;
            let blob = blob_path(repo, hash);
            let meta = fs::metadata(&blob).with_path(&blob)?;
            fs::set_permissions(dest, meta.permissions()).with_path(dest)?;
            return Ok(());
        }
        Some(regions) if regions.is_empty() => {
            fs::write(dest, b"").with_path(dest)?;
            return Ok(());
        }
        _ => {}
    }

    let blob = blob_path(repo, hash);

    if opts.hardlink {
        match fs::hard_link(&blob, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // fall back to copy on cross-device links; otherwise bubble up
                if e.raw_os_error() != Some(libc::EXDEV) {
                    return Err(Error::Io {
                        path: dest.to_path_buf(),
                        source: e,
                    });
                }
            }
        }
    }

    fs::copy(&blob, dest).with_path(dest)?;
    Ok(())
}

fn export_symlink(repo: &Repo, dest: &Path, hash: &Hash, opts: &ExportOptions) -> Result<()> {
    ensure_dest(dest, opts.overwrite)?;

    let target_bytes = read_blob(repo, hash)?;
    let target = String::from_utf8_lossy(&target_bytes);

    let blob = blob_path(repo, hash);
    let meta = fs::symlink_metadata(&blob).with_path(&blob)?;

    use std::os::unix::fs::MetadataExt;
    create_symlink(dest, &target, meta.uid(), meta.gid(), &[])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::commit::commit;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn exports_regular_file() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        let dest = dir.path().join("out.txt");
        export_path(&repo, "ref1", "/file.txt", &dest, Default::default()).unwrap();

        assert_eq!(fs::read_to_string(&dest).unwrap(), "content");

        // hardlink by default
        let commit_obj = read_commit(&repo, &resolve_ref(&repo, "ref1").unwrap()).unwrap();
        let tree = read_tree(&repo, &commit_obj.tree).unwrap();
        let entry = tree.get("file.txt").unwrap();
        if let EntryKind::Regular { hash, .. } = &entry.kind {
            let blob = blob_path(&repo, hash);
            assert_eq!(
                fs::metadata(blob).unwrap().ino(),
                fs::metadata(dest).unwrap().ino()
            );
        } else {
            panic!("expected regular file");
        }
    }

    #[test]
    fn overwrites_when_requested() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "old").unwrap();
        commit(&repo, &source, "ref1", None, None).unwrap();

        let dest = dir.path().join("out.txt");
        fs::write(&dest, "existing").unwrap();

        export_path(
            &repo,
            "ref1",
            "file.txt",
            &dest,
            ExportOptions {
                overwrite: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&dest).unwrap(), "old");
    }
}
