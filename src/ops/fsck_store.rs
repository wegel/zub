use std::fs;

use walkdir::WalkDir;

use super::*;

pub(super) fn check_stored_objects(
    repo: &Repo,
    reachable: &Reachable,
    report: &mut FsckReport,
) -> Result<()> {
    check_stored_blobs(repo, &reachable.blobs, report)?;
    trace_memory("stored blobs", reachable);
    check_stored_trees(repo, &reachable.trees, report)?;
    trace_memory("stored trees", reachable);
    check_stored_commits(repo, &reachable.commits, report)?;
    trace_memory("stored commits", reachable);
    check_stored_artifacts(repo, &reachable.artifacts, report)
}

fn check_stored_blobs(
    repo: &Repo,
    reachable: &HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    for hash in list_objects(&repo.blobs_path())? {
        report.objects_checked += 1;
        if !reachable.contains(&hash) {
            report.dangling_objects.push(hash);
        }
    }
    Ok(())
}

fn check_stored_trees(
    repo: &Repo,
    reachable: &HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    for hash in list_objects(&repo.trees_path())? {
        report.objects_checked += 1;
        let path = crate::object::tree_path(repo, &hash);
        if let Ok(compressed) = fs::read(&path) {
            let actual_hash = Hash::from_bytes(*blake3::hash(&compressed).as_bytes());
            if actual_hash != hash {
                report.corrupt_objects.push(CorruptObject {
                    hash,
                    object_type: ObjectType::Tree,
                    message: format!("hash mismatch: expected {}, zub{}", hash, actual_hash),
                });
            }
        }
        if !reachable.contains(&hash) {
            report.dangling_objects.push(hash);
        }
    }
    Ok(())
}

fn check_stored_commits(
    repo: &Repo,
    reachable: &HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    for hash in list_objects(&repo.commits_path())? {
        report.objects_checked += 1;
        let path = crate::object::commit_path(repo, &hash);
        if let Ok(compressed) = fs::read(&path) {
            let actual_hash = Hash::from_bytes(*blake3::hash(&compressed).as_bytes());
            if actual_hash != hash {
                report.corrupt_objects.push(CorruptObject {
                    hash,
                    object_type: ObjectType::Commit,
                    message: format!("hash mismatch: expected {}, zub{}", hash, actual_hash),
                });
            }
        }
        if !reachable.contains(&hash) {
            report.dangling_objects.push(hash);
        }
    }
    Ok(())
}

fn check_stored_artifacts(
    repo: &Repo,
    reachable: &HashSet<Hash>,
    report: &mut FsckReport,
) -> Result<()> {
    for hash in list_objects(&repo.artifacts_path())? {
        report.objects_checked += 1;
        if reachable.contains(&hash) {
            continue;
        }
        if let Err(error) = verify_artifact(repo, &hash) {
            report.corrupt_objects.push(CorruptObject {
                hash,
                object_type: ObjectType::Artifact,
                message: error.to_string(),
            });
        }
        if !reachable.contains(&hash) {
            report.dangling_objects.push(hash);
        }
    }
    Ok(())
}

fn list_objects(dir: &std::path::Path) -> Result<Vec<Hash>> {
    let mut hashes = Vec::new();

    if !dir.exists() {
        return Ok(hashes);
    }

    for entry in WalkDir::new(dir).min_depth(2).max_depth(2) {
        let entry = entry.map_err(|e| crate::Error::Io {
            path: dir.to_path_buf(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
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
