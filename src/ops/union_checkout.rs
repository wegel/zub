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
use crate::ops::union::ConflictResolution;
use crate::refs::resolve_ref;
use crate::repo::Repo;
use crate::types::{EntryKind, Tree, Xattr};

/// checkout options for union checkout
#[derive(Default, Clone)]
pub struct UnionCheckoutOptions {
    /// overwrite existing files
    pub force: bool,
    /// conflict resolution strategy
    pub on_conflict: ConflictResolution,
    /// use hardlinks when possible
    pub hardlink: bool,
}

/// pending hardlink for deferred creation
struct PendingHardlink {
    entry_path: std::path::PathBuf,
    target_path: String,
}

/// checkout multiple refs as a union to a target directory
///
/// unlike the in-store union operation, this writes directly to the filesystem.
/// useful for inspecting or modifying before committing.
pub fn checkout_union(
    repo: &Repo,
    refs: &[&str],
    target: &Path,
    opts: UnionCheckoutOptions,
) -> Result<()> {
    if refs.is_empty() {
        return Err(Error::InvalidRef("no refs to checkout".to_string()));
    }

    // check target
    if target.exists() {
        if !opts.force {
            let is_empty = target.read_dir().with_path(target)?.next().is_none();
            if !is_empty {
                return Err(Error::TargetNotEmpty(target.to_path_buf()));
            }
        }
    } else {
        fs::create_dir_all(target).with_path(target)?;
    }

    let mut hardlink_tracker = CheckoutHardlinkTracker::new();
    let mut pending_hardlinks = Vec::new();

    // process each ref in order
    for ref_name in refs {
        let commit_hash = resolve_ref(repo, ref_name)?;
        let commit = read_commit(repo, &commit_hash)?;
        let tree = read_tree(repo, &commit.tree)?;

        checkout_tree_union(
            repo,
            &tree,
            target,
            "",
            opts.on_conflict,
            &mut hardlink_tracker,
            &mut pending_hardlinks,
        )?;
    }

    // create all hardlinks now that all files are checked out
    for pending in pending_hardlinks {
        let target_fs_path = hardlink_tracker
            .get(&pending.target_path)
            .ok_or_else(|| Error::HardlinkTargetNotFound(pending.target_path.clone()))?;

        create_hardlink(&pending.entry_path, target_fs_path)?;
    }

    Ok(())
}

/// checkout a tree with union semantics
///
/// hardlinks are collected in pending_hardlinks for deferred creation,
/// allowing targets in sibling directories to be processed first.
fn checkout_tree_union(
    repo: &Repo,
    tree: &Tree,
    target: &Path,
    prefix: &str,
    on_conflict: ConflictResolution,
    hardlink_tracker: &mut CheckoutHardlinkTracker,
    pending_hardlinks: &mut Vec<PendingHardlink>,
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
                // check conflict before deferring
                if entry_path.exists() {
                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue,
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }
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
                if entry_path.exists() {
                    // check if it's a directory (type conflict)
                    if entry_path.is_dir() {
                        return Err(Error::UnionTypeConflict {
                            path: entry_path.clone(),
                            first_type: "directory",
                            second_type: "regular",
                        });
                    }

                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue, // keep existing
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }

                checkout_file(repo, &entry_path, hash, sparse_map.as_deref(), xattrs)?;
                hardlink_tracker.record(&logical_path, entry_path);
            }

            EntryKind::Symlink { hash, xattrs } => {
                if entry_path.exists() || entry_path.symlink_metadata().is_ok() {
                    if entry_path.is_dir() {
                        return Err(Error::UnionTypeConflict {
                            path: entry_path.clone(),
                            first_type: "directory",
                            second_type: "symlink",
                        });
                    }

                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue,
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }

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
                if entry_path.exists() && !entry_path.is_dir() {
                    // file exists where we want a directory
                    return Err(Error::UnionTypeConflict {
                        path: entry_path.clone(),
                        first_type: "regular",
                        second_type: "directory",
                    });
                }

                let subtree = read_tree(repo, hash)?;
                checkout_tree_union(
                    repo,
                    &subtree,
                    &entry_path,
                    &logical_path,
                    on_conflict,
                    hardlink_tracker,
                    pending_hardlinks,
                )?;

                // apply directory metadata
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
                if entry_path.exists() {
                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue,
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }

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
            } => {
                if entry_path.exists() {
                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue,
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }

                match create_char_device(&entry_path, *major, *minor, *uid, *gid, *mode, xattrs) {
                    Ok(()) => {}
                    Err(Error::DeviceNodePermission(_)) => {
                        eprintln!(
                            "warning: cannot create char device {:?} without privileges, skipping",
                            entry_path
                        );
                    }
                    Err(e) => return Err(e),
                }
            }

            EntryKind::Fifo {
                uid,
                gid,
                mode,
                xattrs,
            } => {
                if entry_path.exists() {
                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue,
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }

                create_fifo(&entry_path, *uid, *gid, *mode, xattrs)?;
            }

            EntryKind::Socket {
                uid,
                gid,
                mode,
                xattrs,
            } => {
                if entry_path.exists() {
                    match on_conflict {
                        ConflictResolution::Error => {
                            return Err(Error::UnionConflict(entry_path));
                        }
                        ConflictResolution::First => continue,
                        ConflictResolution::Last => {
                            fs::remove_file(&entry_path).with_path(&entry_path)?;
                        }
                    }
                }

                create_socket_placeholder(&entry_path, *uid, *gid, *mode, xattrs)?;
            }
        }
    }

    Ok(())
}

fn checkout_file(
    repo: &Repo,
    dest: &Path,
    hash: &Hash,
    sparse_map: Option<&[crate::types::SparseRegion]>,
    xattrs: &[Xattr],
) -> Result<()> {
    // can only hardlink if no xattrs (since blob no longer stores xattrs)
    let can_hardlink = xattrs.is_empty() && sparse_map.is_none();

    match sparse_map {
        Some(regions) if !regions.is_empty() => {
            let data = read_blob(repo, hash)?;
            let total_size: u64 = regions.iter().map(|r| r.end()).max().unwrap_or(0);
            write_sparse_file(dest, &data, regions, total_size)?;

            // apply metadata from blob and xattrs from tree
            let blob = blob_path(repo, hash);
            let meta = fs::metadata(&blob).with_path(&blob)?;
            use std::os::unix::fs::MetadataExt;
            apply_metadata_graceful(dest, meta.uid(), meta.gid(), meta.mode(), xattrs)?;
        }
        Some(_) => {
            fs::write(dest, b"").with_path(dest)?;
        }
        _ if can_hardlink => {
            let blob = blob_path(repo, hash);
            fs::hard_link(&blob, dest).with_path(dest)?;
        }
        _ => {
            // copy mode (has xattrs)
            let blob = blob_path(repo, hash);
            fs::copy(&blob, dest).with_path(dest)?;

            // apply metadata from blob and xattrs from tree
            let meta = fs::metadata(&blob).with_path(&blob)?;
            use std::os::unix::fs::MetadataExt;
            apply_metadata_graceful(dest, meta.uid(), meta.gid(), meta.mode(), xattrs)?;
        }
    }
    Ok(())
}

fn checkout_symlink(repo: &Repo, dest: &Path, hash: &Hash, xattrs: &[Xattr]) -> Result<()> {
    let target_bytes = read_blob(repo, hash)?;
    let target = String::from_utf8_lossy(&target_bytes);

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
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_union_checkout_no_overlap() {
        let (dir, repo) = test_repo();

        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("file1.txt"), "content1").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("file2.txt"), "content2").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        let target = dir.path().join("target");
        checkout_union(&repo, &["ref1", "ref2"], &target, Default::default()).unwrap();

        assert!(target.join("file1.txt").exists());
        assert!(target.join("file2.txt").exists());
    }

    #[test]
    fn test_union_checkout_conflict_last() {
        let (dir, repo) = test_repo();

        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("conflict.txt"), "version1").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("conflict.txt"), "version2").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        let target = dir.path().join("target");
        let opts = UnionCheckoutOptions {
            on_conflict: ConflictResolution::Last,
            ..Default::default()
        };
        checkout_union(&repo, &["ref1", "ref2"], &target, opts).unwrap();

        let content = fs::read_to_string(target.join("conflict.txt")).unwrap();
        assert_eq!(content, "version2");
    }

    #[test]
    fn test_union_checkout_conflict_first() {
        let (dir, repo) = test_repo();

        let source1 = dir.path().join("source1");
        fs::create_dir(&source1).unwrap();
        fs::write(source1.join("conflict.txt"), "version1").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir(&source2).unwrap();
        fs::write(source2.join("conflict.txt"), "version2").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        let target = dir.path().join("target");
        let opts = UnionCheckoutOptions {
            on_conflict: ConflictResolution::First,
            ..Default::default()
        };
        checkout_union(&repo, &["ref1", "ref2"], &target, opts).unwrap();

        let content = fs::read_to_string(target.join("conflict.txt")).unwrap();
        assert_eq!(content, "version1");
    }

    #[test]
    fn test_union_checkout_directory_merge() {
        let (dir, repo) = test_repo();

        let source1 = dir.path().join("source1");
        fs::create_dir_all(source1.join("shared")).unwrap();
        fs::write(source1.join("shared/a.txt"), "a").unwrap();
        commit(&repo, &source1, "ref1", None, None).unwrap();

        let source2 = dir.path().join("source2");
        fs::create_dir_all(source2.join("shared")).unwrap();
        fs::write(source2.join("shared/b.txt"), "b").unwrap();
        commit(&repo, &source2, "ref2", None, None).unwrap();

        let target = dir.path().join("target");
        checkout_union(&repo, &["ref1", "ref2"], &target, Default::default()).unwrap();

        assert!(target.join("shared/a.txt").exists());
        assert!(target.join("shared/b.txt").exists());
    }
}
