//! namespace remapping for blob ownership

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use nix::unistd::{chown, Gid, Uid};

use crate::error::{Error, IoResultExt, Result};
use crate::namespace::{
    current_gid_map, current_uid_map, inside_to_outside, mappings_equal, outside_to_inside,
    NsConfig,
};
use crate::Repo;

/// options for the remap operation
#[derive(Debug, Clone, Default)]
pub struct MapOptions {
    /// skip blobs that can't be remapped instead of erroring
    pub force: bool,
    /// only show what would be done, don't actually change anything
    pub dry_run: bool,
}

/// result of a remap operation
#[derive(Debug, Clone, Default)]
pub struct MapStats {
    /// number of blobs that were remapped (or would be in dry-run)
    pub remapped: u64,
    /// number of blobs that were skipped (not in source namespace)
    pub skipped_unmapped_source: u64,
    /// number of blobs that couldn't be remapped to current namespace
    pub skipped_unmapped_target: u64,
    /// total blobs examined
    pub total: u64,
}

/// remap all blob ownership from repository's stored namespace to current namespace.
///
/// reads the namespace from config.toml, compares with /proc/self/{uid,gid}_map,
/// and chowns all blob files to translate ownership.
pub fn map(repo: &mut Repo, options: &MapOptions) -> Result<MapStats> {
    let source_ns = repo.config().namespace.clone();

    // build current namespace from /proc
    let current_ns = NsConfig {
        uid_map: current_uid_map()?,
        gid_map: current_gid_map()?,
    };

    // check if mappings match
    if mappings_equal(&source_ns, &current_ns) {
        return Ok(MapStats::default());
    }

    // acquire exclusive lock
    let _lock = repo.lock()?;

    let stats = remap_blobs(repo.blobs_path(), &source_ns, &current_ns, options)?;

    // update config with current namespace
    if !options.dry_run && stats.remapped > 0 {
        repo.config_mut().namespace = current_ns;
        repo.save_config()?;

        // fsync the config file
        let config_file = fs::File::open(repo.config_path()).with_path(repo.config_path())?;
        config_file.sync_all().with_path(repo.config_path())?;
    }

    Ok(stats)
}

/// remap blobs in a directory tree
fn remap_blobs(
    blobs_path: impl AsRef<Path>,
    source_ns: &NsConfig,
    current_ns: &NsConfig,
    options: &MapOptions,
) -> Result<MapStats> {
    let blobs_path = blobs_path.as_ref();
    let mut stats = MapStats::default();

    // walk objects/blobs/**/*
    for prefix_entry in fs::read_dir(blobs_path).with_path(blobs_path)? {
        let prefix_entry = prefix_entry.with_path(blobs_path)?;
        let prefix_path = prefix_entry.path();

        if !prefix_path.is_dir() {
            continue;
        }

        for blob_entry in fs::read_dir(&prefix_path).with_path(&prefix_path)? {
            let blob_entry = blob_entry.with_path(&prefix_path)?;
            let blob_path = blob_entry.path();

            if !blob_path.is_file() {
                continue;
            }

            stats.total += 1;

            match remap_single_blob(&blob_path, source_ns, current_ns, options)? {
                RemapResult::Remapped => stats.remapped += 1,
                RemapResult::NoChange => {}
                RemapResult::SkippedUnmappedSource => stats.skipped_unmapped_source += 1,
                RemapResult::SkippedUnmappedTarget => stats.skipped_unmapped_target += 1,
            }
        }
    }

    Ok(stats)
}

enum RemapResult {
    Remapped,
    NoChange,
    SkippedUnmappedSource,
    SkippedUnmappedTarget,
}

fn remap_single_blob(
    path: &Path,
    source_ns: &NsConfig,
    current_ns: &NsConfig,
    options: &MapOptions,
) -> Result<RemapResult> {
    let meta = fs::metadata(path).with_path(path)?;
    let old_outside_uid = meta.uid();
    let old_outside_gid = meta.gid();

    // convert old outside -> inside using source namespace
    let old_inside_uid = match outside_to_inside(old_outside_uid, &source_ns.uid_map) {
        Some(uid) => uid,
        None => {
            // uid not in source namespace mapping
            return Ok(RemapResult::SkippedUnmappedSource);
        }
    };

    let old_inside_gid = match outside_to_inside(old_outside_gid, &source_ns.gid_map) {
        Some(gid) => gid,
        None => {
            // gid not in source namespace mapping
            return Ok(RemapResult::SkippedUnmappedSource);
        }
    };

    // convert inside -> new outside using current namespace
    let new_outside_uid = match inside_to_outside(old_inside_uid, &current_ns.uid_map) {
        Some(uid) => uid,
        None => {
            if options.force {
                return Ok(RemapResult::SkippedUnmappedTarget);
            }
            return Err(Error::UnmappedUid(old_inside_uid));
        }
    };

    let new_outside_gid = match inside_to_outside(old_inside_gid, &current_ns.gid_map) {
        Some(gid) => gid,
        None => {
            if options.force {
                return Ok(RemapResult::SkippedUnmappedTarget);
            }
            return Err(Error::UnmappedGid(old_inside_gid));
        }
    };

    // check if anything actually changed
    if old_outside_uid == new_outside_uid && old_outside_gid == new_outside_gid {
        return Ok(RemapResult::NoChange);
    }

    // perform the chown
    if !options.dry_run {
        chown(path, Some(Uid::from_raw(new_outside_uid)), Some(Gid::from_raw(new_outside_gid)))
            .map_err(|e| Error::Io {
                path: path.to_path_buf(),
                source: std::io::Error::from_raw_os_error(e as i32),
            })?;
    }

    Ok(RemapResult::Remapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_mappings_match_returns_early() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let mut repo = Repo::init(&repo_path).unwrap();

        // since we just init'd, the namespace matches current
        let result = map(&mut repo, &MapOptions::default()).unwrap();
        assert_eq!(result.total, 0);
        assert_eq!(result.remapped, 0);
    }

    #[test]
    fn test_remap_result_enum() {
        // basic sanity check that enum variants work
        let _r1 = RemapResult::Remapped;
        let _r2 = RemapResult::NoChange;
        let _r3 = RemapResult::SkippedUnmappedSource;
        let _r4 = RemapResult::SkippedUnmappedTarget;
    }

    #[test]
    fn test_map_stats_default() {
        let stats = MapStats::default();
        assert_eq!(stats.remapped, 0);
        assert_eq!(stats.total, 0);
        assert_eq!(stats.skipped_unmapped_source, 0);
        assert_eq!(stats.skipped_unmapped_target, 0);
    }
}
