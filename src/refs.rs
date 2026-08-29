use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::repo::Repo;

/// write a ref (create or update)
///
/// ref_name can contain slashes for hierarchical refs like "x86_64/pkg/foo/1.0/outputs/bin"
pub fn write_ref(repo: &Repo, ref_name: &str, hash: &Hash) -> Result<()> {
    validate_ref_name(ref_name)?;
    crate::index::prepare_commit(repo, *hash)?;

    let ref_path = ref_path(repo, ref_name);

    // ensure parent directories exist
    if let Some(parent) = ref_path.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }

    // atomic write: temp -> fsync -> rename
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        writeln!(tmp_file, "{}", hash.to_hex()).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    // rename to final location
    fs::rename(&tmp_path, &ref_path).with_path(&ref_path)?;

    // fsync parent directory
    if let Some(parent) = ref_path.parent() {
        let dir = File::open(parent).with_path(parent)?;
        dir.sync_all().with_path(parent)?;
    }

    Ok(())
}

/// read a ref
pub fn read_ref(repo: &Repo, ref_name: &str) -> Result<Hash> {
    let ref_path = ref_path(repo, ref_name);

    let content = fs::read_to_string(&ref_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::RefNotFound(ref_name.to_string())
        } else {
            Error::Io {
                path: ref_path.clone(),
                source: e,
            }
        }
    })?;

    let hex = content.trim();
    Hash::from_hex(hex)
}

/// delete a ref
pub fn delete_ref(repo: &Repo, ref_name: &str) -> Result<()> {
    let ref_path = ref_path(repo, ref_name);

    fs::remove_file(&ref_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::RefNotFound(ref_name.to_string())
        } else {
            Error::Io {
                path: ref_path,
                source: e,
            }
        }
    })
}

/// delete refs matching a glob pattern, returns list of deleted refs
pub fn delete_refs_matching(repo: &Repo, pattern: &str) -> Result<Vec<String>> {
    let matching = list_refs_matching(repo, pattern)?;
    for ref_name in &matching {
        delete_ref(repo, ref_name)?;
    }
    Ok(matching)
}

/// resolve a ref or hash string to a hash
///
/// if the string looks like a hash (64 hex chars), parse it directly.
/// otherwise, look it up as a ref name.
pub fn resolve_ref(repo: &Repo, ref_or_hash: &str) -> Result<Hash> {
    // if it's 64 hex chars, treat as hash
    if ref_or_hash.len() == 64 && ref_or_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Hash::from_hex(ref_or_hash);
    }

    // otherwise, look up as ref
    read_ref(repo, ref_or_hash)
}

/// list all refs
pub fn list_refs(repo: &Repo) -> Result<Vec<String>> {
    let refs_dir = repo.refs_path();
    let mut refs = Vec::new();

    if refs_dir.exists() {
        collect_refs(&refs_dir, &refs_dir, &mut refs)?;
    }

    refs.sort();
    Ok(refs)
}

/// list refs matching a glob pattern
pub fn list_refs_matching(repo: &Repo, pattern: &str) -> Result<Vec<String>> {
    let all_refs = list_refs(repo)?;
    let glob = glob::Pattern::new(pattern).map_err(|e| Error::InvalidRef(e.to_string()))?;

    Ok(all_refs.into_iter().filter(|r| glob.matches(r)).collect())
}

/// check if a ref exists
pub fn ref_exists(repo: &Repo, ref_name: &str) -> bool {
    ref_path(repo, ref_name).exists()
}

/// get filesystem path for a ref
fn ref_path(repo: &Repo, ref_name: &str) -> PathBuf {
    repo.refs_path().join(ref_name)
}

/// recursively collect refs from directory
fn collect_refs(base: &PathBuf, dir: &PathBuf, refs: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_path(dir)? {
        let entry = entry.with_path(dir)?;
        let path = entry.path();

        if path.is_dir() {
            collect_refs(base, &path, refs)?;
        } else if path.is_file() {
            // compute ref name relative to base
            if let Ok(rel) = path.strip_prefix(base) {
                let ref_name = rel.to_string_lossy().to_string();
                refs.push(ref_name);
            }
        }
    }
    Ok(())
}

// --- Artifact ref helpers ---

/// write an artifact ref at the given path
///
/// path can be hierarchical like "x86_64/pkg/foo/1.0/<hash>/outputs/bin"
/// the caller controls the path structure for filtering purposes
pub fn write_artifact_ref(repo: &Repo, path: &str, artifact_hash: &Hash) -> Result<()> {
    validate_ref_name(path)?;

    let ref_path = repo.artifact_refs_path().join(path);

    // ensure parent directories exist
    if let Some(parent) = ref_path.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }

    // atomic write
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        writeln!(tmp_file, "{}", artifact_hash.to_hex()).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    fs::rename(&tmp_path, &ref_path).with_path(&ref_path)?;

    if let Some(parent) = ref_path.parent() {
        let dir = File::open(parent).with_path(parent)?;
        dir.sync_all().with_path(parent)?;
    }

    Ok(())
}

/// read an artifact ref at the given path
pub fn read_artifact_ref(repo: &Repo, path: &str) -> Result<Hash> {
    let ref_path = repo.artifact_refs_path().join(path);

    let content = fs::read_to_string(&ref_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::RefNotFound(format!("artifacts/{}", path))
        } else {
            Error::Io {
                path: ref_path.clone(),
                source: e,
            }
        }
    })?;

    Hash::from_hex(content.trim())
}

/// check if an artifact ref exists at the given path
pub fn artifact_ref_exists(repo: &Repo, path: &str) -> bool {
    repo.artifact_refs_path().join(path).exists()
}

/// delete an artifact ref
pub fn delete_artifact_ref(repo: &Repo, path: &str) -> Result<()> {
    let ref_path = repo.artifact_refs_path().join(path);

    fs::remove_file(&ref_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::RefNotFound(format!("artifacts/{}", path))
        } else {
            Error::Io {
                path: ref_path,
                source: e,
            }
        }
    })
}

/// list all artifact refs
pub fn list_artifact_refs(repo: &Repo) -> Result<Vec<String>> {
    let artifacts_dir = repo.artifact_refs_path();
    let mut refs = Vec::new();

    if artifacts_dir.exists() {
        collect_refs(&artifacts_dir, &artifacts_dir, &mut refs)?;
    }

    refs.sort();
    Ok(refs)
}

/// list artifact refs matching a glob pattern
pub fn list_artifact_refs_matching(repo: &Repo, pattern: &str) -> Result<Vec<String>> {
    let all_refs = list_artifact_refs(repo)?;
    let glob = glob::Pattern::new(pattern).map_err(|e| Error::InvalidRef(e.to_string()))?;

    Ok(all_refs.into_iter().filter(|r| glob.matches(r)).collect())
}

/// delete artifact refs matching a glob pattern, returns list of deleted refs
pub fn delete_artifact_refs_matching(repo: &Repo, pattern: &str) -> Result<Vec<String>> {
    let matching = list_artifact_refs_matching(repo, pattern)?;
    for ref_name in &matching {
        delete_artifact_ref(repo, ref_name)?;
    }
    Ok(matching)
}

/// validate ref name
fn validate_ref_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidRef("empty ref name".to_string()));
    }

    if name.starts_with('/') || name.ends_with('/') {
        return Err(Error::InvalidRef(format!(
            "ref name cannot start or end with '/': {}",
            name
        )));
    }

    if name.contains("//") {
        return Err(Error::InvalidRef(format!(
            "ref name cannot contain '//': {}",
            name
        )));
    }

    if name.contains('\0') {
        return Err(Error::InvalidRef(format!(
            "ref name cannot contain null byte: {}",
            name
        )));
    }

    // check for path traversal
    for component in name.split('/') {
        if component == "." || component == ".." {
            return Err(Error::InvalidRef(format!(
                "ref name cannot contain '.' or '..': {}",
                name
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "refs_tests.rs"]
mod tests;
