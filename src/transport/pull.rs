//! pull operation - fetch objects from remote

use std::collections::HashSet;
use std::path::Path;

use crate::error::Result;
use crate::hash::Hash;
use crate::object::{read_commit, read_tree, verify_commit};
use crate::refs::{read_ref, write_ref};
use crate::repo::Repo;
use crate::transport::local::{
    copy_objects, list_all_objects, remove_objects, ObjectSet, TransferStats,
};
use crate::transport::ssh::SshConnection;
use crate::types::EntryKind;

/// pull options
#[derive(Debug, Clone, Default)]
pub struct PullOptions {
    /// only fetch objects, don't update ref
    pub fetch_only: bool,
    /// dry run - show what would be transferred without doing it
    pub dry_run: bool,
}

/// pull a ref from a local repository
pub fn pull_local(
    src: &Repo,
    dst: &Repo,
    ref_name: &str,
    options: &PullOptions,
) -> Result<PullResult> {
    let src_hash = read_ref(src, ref_name)?;
    verify_commit(src, &src_hash)?;

    // collect all objects reachable from the commit
    let mut needed = ObjectSet::new();
    collect_commit_objects(src, &src_hash, &mut needed, &mut HashSet::new())?;

    // filter out objects we already have
    let _lock = (!options.dry_run).then(|| dst.lock()).transpose()?;
    let existing = list_all_objects(dst)?;
    let existing_blobs: HashSet<_> = existing.blobs.into_iter().collect();
    let existing_trees: HashSet<_> = existing.trees.into_iter().collect();
    let existing_commits: HashSet<_> = existing.commits.into_iter().collect();

    needed.blobs.retain(|h| !existing_blobs.contains(h));
    needed.trees.retain(|h| !existing_trees.contains(h));
    needed.commits.retain(|h| !existing_commits.contains(h));

    // dry run: return what would be transferred without doing anything
    if options.dry_run {
        return Ok(PullResult {
            hash: src_hash,
            stats: TransferStats::default(),
            objects_to_transfer: needed.blobs.len() + needed.trees.len() + needed.commits.len(),
        });
    }

    let result = copy_objects(src, dst, &needed)
        .and_then(|stats| verify_commit(dst, &src_hash).map(|()| stats));
    let stats = match result {
        Ok(stats) => stats,
        Err(error) => {
            remove_objects(dst, &needed)?;
            return Err(error);
        }
    };

    // update ref
    if !options.fetch_only {
        write_ref(dst, ref_name, &src_hash)?;
    }

    Ok(PullResult {
        hash: src_hash,
        stats,
        objects_to_transfer: 0,
    })
}

/// pull a ref from a remote repository via SSH
pub fn pull_ssh(
    remote: &str,
    remote_path: &Path,
    local: &Repo,
    ref_name: &str,
    options: &PullOptions,
) -> Result<PullResult> {
    let mut conn = SshConnection::connect(remote, remote_path)?;

    // get ref from remote
    let remote_hash = conn
        .get_ref(ref_name)?
        .ok_or_else(|| crate::Error::RefNotFound(ref_name.to_string()))?;

    // collect what we have
    let _lock = (!options.dry_run).then(|| local.lock()).transpose()?;
    let existing = list_all_objects(local)?;

    // ask remote what we need
    let needed = conn.have_objects(&existing)?;

    // dry run: return what would be transferred without doing anything
    if options.dry_run {
        conn.close()?;
        return Ok(PullResult {
            hash: remote_hash,
            stats: TransferStats::default(),
            objects_to_transfer: needed.blobs.len() + needed.trees.len() + needed.commits.len(),
        });
    }

    let mut stats = TransferStats::default();
    let mut introduced = ObjectSet::new();

    let receive_result = (|| -> Result<()> {
        while let Some(object) = conn.receive_object(local)? {
            if object.installed {
                stats.bytes_transferred += object.size;
                stats.copied += 1;
                introduced.push(object.kind, object.hash);
            } else {
                stats.skipped += 1;
            }
        }
        verify_commit(local, &remote_hash)
    })();
    if let Err(error) = receive_result {
        remove_objects(local, &introduced)?;
        let _ = conn.close();
        return Err(error);
    }

    // update ref
    if !options.fetch_only {
        write_ref(local, ref_name, &remote_hash)?;
    }

    conn.close()?;

    Ok(PullResult {
        hash: remote_hash,
        stats,
        objects_to_transfer: 0,
    })
}

/// collect all objects reachable from a commit
fn collect_commit_objects(
    repo: &Repo,
    commit_hash: &Hash,
    objects: &mut ObjectSet,
    visited: &mut HashSet<Hash>,
) -> Result<()> {
    if visited.contains(commit_hash) {
        return Ok(());
    }
    visited.insert(*commit_hash);

    objects.commits.push(*commit_hash);

    let commit = read_commit(repo, commit_hash)?;

    // collect tree objects
    collect_tree_objects(repo, &commit.tree, objects, visited)?;

    Ok(())
}

/// collect all objects in a tree
fn collect_tree_objects(
    repo: &Repo,
    tree_hash: &Hash,
    objects: &mut ObjectSet,
    visited: &mut HashSet<Hash>,
) -> Result<()> {
    if visited.contains(tree_hash) {
        return Ok(());
    }
    visited.insert(*tree_hash);

    objects.trees.push(*tree_hash);

    let tree = read_tree(repo, tree_hash)?;

    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, .. } => {
                if !visited.contains(hash) {
                    visited.insert(*hash);
                    objects.blobs.push(*hash);
                }
            }
            EntryKind::Symlink { hash, .. } => {
                if !visited.contains(hash) {
                    visited.insert(*hash);
                    objects.blobs.push(*hash);
                }
            }
            EntryKind::Directory { hash, .. } => {
                collect_tree_objects(repo, hash, objects, visited)?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// result of a pull operation
#[derive(Debug)]
pub struct PullResult {
    pub hash: Hash,
    pub stats: TransferStats,
    /// number of objects that would be transferred (for dry run)
    pub objects_to_transfer: usize,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;
    use crate::ops::{checkout, commit, CheckoutOptions};
    use crate::{MapEntry, NsConfig};
    use tempfile::tempdir;

    #[test]
    fn test_pull_local() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        let hash = commit(&src, &source, "test", Some("initial"), None).unwrap();

        let result = pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();

        assert_eq!(result.hash, hash);
        assert!(result.stats.copied > 0 || result.stats.hardlinked > 0);

        // verify ref exists in destination
        let dst_hash = read_ref(&dst, "test").unwrap();
        assert_eq!(dst_hash, hash);
    }

    #[test]
    fn test_pull_fetch_only() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "content").unwrap();
        let hash = commit(&src, &source, "test", Some("initial"), None).unwrap();

        let options = PullOptions {
            fetch_only: true,
            dry_run: false,
        };
        let result = pull_local(&src, &dst, "test", &options).unwrap();

        assert_eq!(result.hash, hash);

        // ref should NOT exist in destination
        assert!(read_ref(&dst, "test").is_err());
    }

    #[test]
    fn test_pull_incremental() {
        let dir = tempdir().unwrap();

        let src_path = dir.path().join("src_repo");
        let src = Repo::init(&src_path).unwrap();

        let dst_path = dir.path().join("dst_repo");
        let dst = Repo::init(&dst_path).unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "v1").unwrap();
        commit(&src, &source, "test", Some("v1"), None).unwrap();

        // pull first version
        pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();

        // create second version
        fs::write(source.join("file.txt"), "v2").unwrap();
        let hash2 = commit(&src, &source, "test", Some("v2"), None).unwrap();

        // pull second version - should be incremental
        let result = pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();

        assert_eq!(result.hash, hash2);
        // some objects should have been skipped (already exist)
        // note: exact counts depend on object sharing
    }

    #[test]
    fn transfer_preserves_blob_identity() {
        let dir = tempdir().unwrap();
        let mut src = Repo::init(&dir.path().join("src_repo")).unwrap();
        let mut dst = Repo::init(&dir.path().join("dst_repo")).unwrap();
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        let namespace = NsConfig {
            uid_map: vec![MapEntry::new(37, uid, 1)],
            gid_map: vec![MapEntry::new(43, gid, 1)],
        };
        src.config_mut().namespace = namespace.clone();
        src.save_config().unwrap();
        dst.config_mut().namespace = namespace;
        dst.save_config().unwrap();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        let probe = source.join("probe");
        fs::write(&probe, "payload").unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o751)).unwrap();
        xattr::set(&probe, "user.zub-test", b"metadata").unwrap();
        let commit_hash = commit(&src, &source, "test", Some("fixture"), None).unwrap();

        let result = pull_local(&src, &dst, "test", &PullOptions::default()).unwrap();
        assert_eq!(result.hash, commit_hash);
        assert_eq!(result.stats.hardlinked, 0);
        verify_commit(&dst, &commit_hash).unwrap();

        let tree = read_tree(&dst, &read_commit(&dst, &commit_hash).unwrap().tree).unwrap();
        let blob = *tree.get("probe").unwrap().kind.hash().unwrap();
        let source_blob = crate::object::blob_path(&src, &blob);
        let destination_blob = crate::object::blob_path(&dst, &blob);
        assert_ne!(
            fs::metadata(source_blob).unwrap().ino(),
            fs::metadata(&destination_blob).unwrap().ino()
        );
        let metadata = fs::metadata(destination_blob).unwrap();
        assert_eq!(metadata.uid(), uid);
        assert_eq!(metadata.gid(), gid);
        assert_eq!(metadata.mode() & 0o7777, 0o751);

        let checkout_path = dir.path().join("checkout");
        checkout(&dst, "test", &checkout_path, CheckoutOptions::default()).unwrap();
        assert_eq!(fs::read(checkout_path.join("probe")).unwrap(), b"payload");
        assert_eq!(
            xattr::get(checkout_path.join("probe"), "user.zub-test").unwrap(),
            Some(b"metadata".to_vec())
        );
    }

    #[test]
    fn transfer_rejects_corrupt_blob_without_publishing_a_ref() {
        let dir = tempdir().unwrap();
        let src = Repo::init(&dir.path().join("src_repo")).unwrap();
        let dst = Repo::init(&dir.path().join("dst_repo")).unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("probe"), "payload").unwrap();
        let commit_hash = commit(&src, &source, "test", Some("fixture"), None).unwrap();
        let tree = read_tree(&src, &read_commit(&src, &commit_hash).unwrap().tree).unwrap();
        let blob = *tree.get("probe").unwrap().kind.hash().unwrap();
        fs::write(crate::object::blob_path(&src, &blob), b"corrupt").unwrap();

        assert!(matches!(
            pull_local(&src, &dst, "test", &PullOptions::default()),
            Err(crate::Error::CorruptObject(hash)) if hash == blob
        ));
        assert!(matches!(
            read_ref(&dst, "test"),
            Err(crate::Error::RefNotFound(_))
        ));
        assert!(list_all_objects(&dst).unwrap().is_empty());
    }
}
