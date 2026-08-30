//! Bounded parallel scans over sharded object directories.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::hash::Hash;

#[derive(Default)]
pub(super) struct ObjectScan {
    pub(super) count: usize,
    pub(super) bytes: u64,
    pub(super) hashed: Vec<ScannedObject>,
}

pub(super) struct ScannedObject {
    pub(super) hash: Hash,
    pub(super) bytes: u64,
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

pub(super) fn scan_objects(dir: &Path, keep_hashes: bool) -> ObjectScan {
    if !dir.exists() {
        return ObjectScan::default();
    }
    object_shards(dir)
        .par_iter()
        .map(|shard| scan_shard(shard, keep_hashes))
        .reduce(ObjectScan::default, |mut total, mut shard| {
            total.count += shard.count;
            total.bytes += shard.bytes;
            total.hashed.append(&mut shard.hashed);
            total
        })
}

pub(super) fn inventory_hash(objects: &mut [ScannedObject], metadata: bool) -> Hash {
    objects.sort_unstable_by_key(|object| object.hash);
    let mut hasher = blake3::Hasher::new();
    for object in objects {
        hasher.update(object.hash.as_bytes());
        hasher.update(&object.bytes.to_le_bytes());
        if metadata {
            hasher.update(&object.device.to_le_bytes());
            hasher.update(&object.inode.to_le_bytes());
            hasher.update(&object.modified_seconds.to_le_bytes());
            hasher.update(&object.modified_nanoseconds.to_le_bytes());
            hasher.update(&object.changed_seconds.to_le_bytes());
            hasher.update(&object.changed_nanoseconds.to_le_bytes());
        }
    }
    Hash::from_bytes(*hasher.finalize().as_bytes())
}

fn object_shards(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.path()))
        .collect()
}

fn scan_shard(shard: &Path, keep_hashes: bool) -> ObjectScan {
    let prefix = shard
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let mut result = ObjectScan::default();
    for entry in fs::read_dir(shard).into_iter().flatten().flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        result.count += 1;
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        result.bytes += metadata.len();
        if keep_hashes {
            let suffix = entry.file_name();
            let suffix = suffix.to_str().unwrap_or("");
            if let Ok(hash) = Hash::from_hex(&format!("{prefix}{suffix}")) {
                result.hashed.push(ScannedObject {
                    hash,
                    bytes: metadata.len(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    modified_seconds: metadata.mtime(),
                    modified_nanoseconds: metadata.mtime_nsec(),
                    changed_seconds: metadata.ctime(),
                    changed_nanoseconds: metadata.ctime_nsec(),
                });
            }
        }
    }
    result
}
