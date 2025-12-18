use std::fs;
use std::path::Path;

use crate::error::{Error, IoResultExt, Result};
use crate::fs::{
    apply_metadata_graceful, create_block_device, create_char_device, create_fifo,
    create_hardlink, create_socket_placeholder, create_symlink, write_sparse_file,
    CheckoutHardlinkTracker,
};
use crate::hash::Hash;
use crate::object::{blob_path, read_blob, read_commit, read_tree};
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::{EntryKind, Tree, Xattr};

/// checkout options
#[derive(Clone)]
pub struct CheckoutOptions {
    /// overwrite existing files
    pub force: bool,
    /// use hardlinks when possible (default: true)
    pub hardlink: bool,
    /// preserve sparse file holes
    pub preserve_sparse: bool,
}

impl Default for CheckoutOptions {
    fn default() -> Self {
        Self {
            force: false,
            hardlink: true,
            preserve_sparse: false,
        }
    }
}

/// checkout a ref to a target directory
pub fn checkout(repo: &Repo, ref_name: &str, target: &Path, opts: CheckoutOptions) -> Result<()> {
    // resolve ref to commit
    let commit_hash = resolve_ref(repo, ref_name)?;
    let commit = read_commit(repo, &commit_hash)?;

    checkout_from_tree_hash(repo, &commit.tree, target, opts)
}

/// checkout directly from a tree hash (bypasses commit/ref resolution)
pub fn checkout_from_tree_hash(
    repo: &Repo,
    tree_hash: &Hash,
    target: &Path,
    opts: CheckoutOptions,
) -> Result<()> {
    // load root tree
    let tree = read_tree(repo, tree_hash)?;

    // check target
    if target.exists() {
        if !opts.force {
            // check if empty
            let is_empty = target.read_dir().with_path(target)?.next().is_none();
            if !is_empty {
                return Err(Error::TargetNotEmpty(target.to_path_buf()));
            }
        }
    } else {
        fs::create_dir_all(target).with_path(target)?;
    }

    // checkout tree, collecting pending hardlinks
    let mut hardlink_tracker = CheckoutHardlinkTracker::new();
    let mut pending_hardlinks = Vec::new();
    checkout_tree(
        repo,
        &tree,
        target,
        "",
        &mut hardlink_tracker,
        &mut pending_hardlinks,
        &opts,
    )?;

    // create all hardlinks now that all files are checked out
    for pending in pending_hardlinks {
        let target_fs_path = hardlink_tracker
            .get(&pending.target_path)
            .ok_or_else(|| Error::HardlinkTargetNotFound(pending.target_path.clone()))?;

        create_hardlink(&pending.entry_path, target_fs_path)?;
    }

    Ok(())
}

/// pending hardlink to be created after all files are checked out
struct PendingHardlink {
    entry_path: std::path::PathBuf,
    target_path: String,
}

/// checkout a tree to a directory (recursive helper)
///
/// hardlinks are collected and returned to be processed after all files
/// in the entire tree are checked out. this handles cases where the target
/// is in a sibling directory that would otherwise be processed later.
fn checkout_tree(
    repo: &Repo,
    tree: &Tree,
    target: &Path,
    prefix: &str,
    hardlink_tracker: &mut CheckoutHardlinkTracker,
    pending_hardlinks: &mut Vec<PendingHardlink>,
    opts: &CheckoutOptions,
) -> Result<()> {
    fs::create_dir_all(target).with_path(target)?;

    // checkout all non-hardlink entries, collecting hardlinks for later
    for entry in tree.entries() {
        let entry_path = target.join(&entry.name);
        let logical_path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", prefix, entry.name)
        };

        match &entry.kind {
            EntryKind::Hardlink { target_path } => {
                // defer hardlink creation until all files are checked out
                pending_hardlinks.push(PendingHardlink {
                    entry_path,
                    target_path: target_path.clone(),
                });
            }

            EntryKind::Regular {
                hash,
                sparse_map,
                xattrs,
                ..
            } => {
                checkout_regular_file(repo, &entry_path, hash, sparse_map.as_deref(), xattrs, opts)?;
                hardlink_tracker.record(&logical_path, entry_path);
            }

            EntryKind::Symlink { hash, xattrs } => {
                checkout_symlink(repo, &entry_path, hash, xattrs)?;
                hardlink_tracker.record(&logical_path, entry_path);
            }

            EntryKind::Directory {
                hash,
                uid,
                gid,
                mode,
                xattrs,
            } => {
                // recurse
                let subtree = read_tree(repo, hash)?;
                checkout_tree(
                    repo,
                    &subtree,
                    &entry_path,
                    &logical_path,
                    hardlink_tracker,
                    pending_hardlinks,
                    opts,
                )?;

                // apply directory metadata after contents are created
                apply_metadata_graceful(&entry_path, *uid, *gid, *mode, xattrs)?;
            }

            EntryKind::BlockDevice {
                major,
                minor,
                uid,
                gid,
                mode,
                xattrs,
            } => {
                match create_block_device(&entry_path, *major, *minor, *uid, *gid, *mode, xattrs) {
                    Ok(()) => {}
                    Err(Error::DeviceNodePermission(_)) => {
                        eprintln!(
                            "warning: cannot create block device {:?} without privileges, skipping",
                            entry_path
                        );
                    }
                    Err(e) => return Err(e),
                }
            }

            EntryKind::CharDevice {
                major,
                minor,
                uid,
                gid,
                mode,
                xattrs,
            } => match create_char_device(&entry_path, *major, *minor, *uid, *gid, *mode, xattrs) {
                Ok(()) => {}
                Err(Error::DeviceNodePermission(_)) => {
                    eprintln!(
                        "warning: cannot create char device {:?} without privileges, skipping",
                        entry_path
                    );
                }
                Err(e) => return Err(e),
            },

            EntryKind::Fifo {
                uid,
                gid,
                mode,
                xattrs,
            } => {
                create_fifo(&entry_path, *uid, *gid, *mode, xattrs)?;
            }

            EntryKind::Socket {
                uid,
                gid,
                mode,
                xattrs,
            } => {
                create_socket_placeholder(&entry_path, *uid, *gid, *mode, xattrs)?;
            }
        }
    }

    Ok(())
}

/// checkout a regular file (hardlink from blob store, or copy for sparse/--copy/xattrs)
fn checkout_regular_file(
    repo: &Repo,
    dest: &Path,
    hash: &Hash,
    sparse_map: Option<&[crate::types::SparseRegion]>,
    xattrs: &[Xattr],
    opts: &CheckoutOptions,
) -> Result<()> {
    // remove existing
    if dest.exists() {
        fs::remove_file(dest).with_path(dest)?;
    }

    // can only hardlink if no xattrs (since blob no longer stores xattrs)
    // and no sparse map to preserve
    let can_hardlink = opts.hardlink && xattrs.is_empty() && sparse_map.is_none();

    match sparse_map {
        Some(regions) if !regions.is_empty() && opts.preserve_sparse => {
            // sparse file: must copy and recreate holes
            let data = read_blob(repo, hash)?;
            let total_size: u64 = regions.iter().map(|r| r.end()).max().unwrap_or(0);
            write_sparse_file(dest, &data, regions, total_size)?;

            // apply metadata from blob (uid, gid, mode) and xattrs from tree
            apply_blob_metadata_with_xattrs(repo, hash, dest, xattrs)?;
        }

        Some([]) => {
            // all holes (empty sparse file)
            fs::write(dest, b"").with_path(dest)?;
        }

        _ if can_hardlink => {
            // non-sparse with hardlink and no xattrs: hardlink from blob store
            let blob = blob_path(repo, hash);
            fs::hard_link(&blob, dest).with_path(dest)?;
            // metadata (uid, gid, mode) comes along with the hardlink (shared inode)
            // note: no xattrs to apply since we only hardlink when xattrs is empty
        }

        _ => {
            // copy mode (--copy flag, has xattrs, or sparse without preserve_sparse)
            let blob = blob_path(repo, hash);
            fs::copy(&blob, dest).with_path(dest)?;

            // apply metadata from blob (uid, gid, mode) and xattrs from tree
            apply_blob_metadata_with_xattrs(repo, hash, dest, xattrs)?;
        }
    }

    Ok(())
}

/// apply metadata (uid, gid, mode from blob file, xattrs from tree) to a destination path
fn apply_blob_metadata_with_xattrs(
    repo: &Repo,
    hash: &Hash,
    dest: &Path,
    xattrs: &[Xattr],
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let blob = blob_path(repo, hash);
    let meta = fs::metadata(&blob).with_path(&blob)?;

    apply_metadata_graceful(dest, meta.uid(), meta.gid(), meta.mode(), xattrs)
}

/// checkout a symlink
fn checkout_symlink(repo: &Repo, dest: &Path, hash: &Hash, xattrs: &[Xattr]) -> Result<()> {
    // symlink blob contains the target path as content
    let target_bytes = read_blob(repo, hash)?;
    let target = String::from_utf8_lossy(&target_bytes);

    // read uid/gid from blob file (still stored there), xattrs from tree
    let blob = blob_path(repo, hash);
    let meta = fs::symlink_metadata(&blob).with_path(&blob)?;

    use std::os::unix::fs::MetadataExt;
    create_symlink(dest, &target, meta.uid(), meta.gid(), xattrs)?;

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
    fn test_checkout_single_file() {
        let (dir, repo) = test_repo();

        // create and commit source
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hello.txt"), "world").unwrap();
        commit(&repo, &source, "test/ref", None, None).unwrap();

        // checkout
        let target = dir.path().join("target");
        checkout(&repo, "test/ref", &target, Default::default()).unwrap();

        // verify
        let content = fs::read_to_string(target.join("hello.txt")).unwrap();
        assert_eq!(content, "world");
    }

    #[test]
    fn test_checkout_uses_hardlinks() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        let commit_hash = commit(&repo, &source, "test", None, None).unwrap();

        let target = dir.path().join("target");
        checkout(&repo, "test", &target, Default::default()).unwrap();

        // get blob hash from commit
        let commit_obj = read_commit(&repo, &commit_hash).unwrap();
        let tree = read_tree(&repo, &commit_obj.tree).unwrap();
        let entry = tree.get("file.txt").unwrap();
        if let EntryKind::Regular { hash, .. } = &entry.kind {
            let blob = blob_path(&repo, hash);
            let checked_out = target.join("file.txt");

            // verify same inode (hardlink)
            let blob_ino = fs::metadata(&blob).unwrap().ino();
            let target_ino = fs::metadata(&checked_out).unwrap().ino();
            assert_eq!(blob_ino, target_ino);
        } else {
            panic!("expected regular file");
        }
    }

    #[test]
    fn test_checkout_nested_directories() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir_all(source.join("a/b")).unwrap();
        fs::write(source.join("a/b/deep.txt"), "deep content").unwrap();
        commit(&repo, &source, "nested", None, None).unwrap();

        let target = dir.path().join("target");
        checkout(&repo, "nested", &target, Default::default()).unwrap();

        assert!(target.join("a/b/deep.txt").exists());
        let content = fs::read_to_string(target.join("a/b/deep.txt")).unwrap();
        assert_eq!(content, "deep content");
    }

    #[test]
    fn test_checkout_symlink() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink("/target/path", source.join("link")).unwrap();
        commit(&repo, &source, "symlink", None, None).unwrap();

        let target = dir.path().join("target");
        checkout(&repo, "symlink", &target, Default::default()).unwrap();

        let link_target = fs::read_link(target.join("link")).unwrap();
        assert_eq!(link_target.to_string_lossy(), "/target/path");
    }

    #[test]
    fn test_checkout_hardlinks() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("original"), "content").unwrap();
        fs::hard_link(source.join("original"), source.join("link")).unwrap();
        commit(&repo, &source, "hardlink", None, None).unwrap();

        let target = dir.path().join("target");
        checkout(&repo, "hardlink", &target, Default::default()).unwrap();

        // both files should exist and be hardlinks
        let orig_ino = fs::metadata(target.join("original")).unwrap().ino();
        let link_ino = fs::metadata(target.join("link")).unwrap().ino();
        assert_eq!(orig_ino, link_ino);
    }

    #[test]
    fn test_checkout_force() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        commit(&repo, &source, "test", None, None).unwrap();

        // create non-empty target
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("existing.txt"), "existing").unwrap();

        // checkout without force should fail
        let result = checkout(&repo, "test", &target, Default::default());
        assert!(result.is_err());

        // checkout with force should succeed
        checkout(
            &repo,
            "test",
            &target,
            CheckoutOptions {
                force: true,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(target.join("file.txt").exists());
    }

    #[test]
    fn test_roundtrip() {
        let (dir, repo) = test_repo();

        // create complex source
        let source = dir.path().join("source");
        fs::create_dir_all(source.join("dir1/dir2")).unwrap();
        fs::write(source.join("file1.txt"), "content1").unwrap();
        fs::write(source.join("dir1/file2.txt"), "content2").unwrap();
        fs::write(source.join("dir1/dir2/file3.txt"), "content3").unwrap();
        std::os::unix::fs::symlink("../file1.txt", source.join("dir1/link")).unwrap();

        commit(&repo, &source, "roundtrip", None, None).unwrap();

        // checkout to new location
        let target = dir.path().join("target");
        checkout(&repo, "roundtrip", &target, Default::default()).unwrap();

        // verify structure
        assert_eq!(
            fs::read_to_string(target.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            fs::read_to_string(target.join("dir1/file2.txt")).unwrap(),
            "content2"
        );
        assert_eq!(
            fs::read_to_string(target.join("dir1/dir2/file3.txt")).unwrap(),
            "content3"
        );
        assert_eq!(
            fs::read_link(target.join("dir1/link"))
                .unwrap()
                .to_string_lossy(),
            "../file1.txt"
        );
    }

    #[test]
    fn test_checkout_copy_preserves_xattrs() {
        let (dir, repo) = test_repo();

        // create source with xattrs
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        let file_path = source.join("file.txt");
        fs::write(&file_path, "content").unwrap();

        // set xattr (user namespace works without privileges)
        if xattr::set(&file_path, "user.test", b"xattr_value").is_err() {
            // xattrs not supported on this filesystem, skip test
            return;
        }

        commit(&repo, &source, "xattr-test", None, None).unwrap();

        // checkout in COPY mode (hardlink=false)
        let target = dir.path().join("target");
        checkout(
            &repo,
            "xattr-test",
            &target,
            CheckoutOptions {
                hardlink: false,
                ..Default::default()
            },
        )
        .unwrap();

        // verify xattr was preserved
        let checked_out = target.join("file.txt");
        let value = xattr::get(&checked_out, "user.test").unwrap();
        assert_eq!(value, Some(b"xattr_value".to_vec()));

        // verify it's actually a copy, not a hardlink
        let source_ino = fs::metadata(&file_path).unwrap().ino();
        let target_ino = fs::metadata(&checked_out).unwrap().ino();
        assert_ne!(source_ino, target_ino);
    }
}
