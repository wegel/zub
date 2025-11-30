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
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_write_and_read_ref() {
        let (_dir, repo) = test_repo();

        let hash =
            Hash::from_hex("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
                .unwrap();

        write_ref(&repo, "test/ref", &hash).unwrap();
        let read_hash = read_ref(&repo, "test/ref").unwrap();

        assert_eq!(hash, read_hash);
    }

    #[test]
    fn test_hierarchical_ref() {
        let (_dir, repo) = test_repo();

        let hash = Hash::ZERO;
        write_ref(&repo, "x86_64/pkg/bzip2/1.0.8/outputs/bin", &hash).unwrap();

        let read_hash = read_ref(&repo, "x86_64/pkg/bzip2/1.0.8/outputs/bin").unwrap();
        assert_eq!(hash, read_hash);
    }

    #[test]
    fn test_delete_ref() {
        let (_dir, repo) = test_repo();

        let hash = Hash::ZERO;
        write_ref(&repo, "test/ref", &hash).unwrap();
        assert!(ref_exists(&repo, "test/ref"));

        delete_ref(&repo, "test/ref").unwrap();
        assert!(!ref_exists(&repo, "test/ref"));
    }

    #[test]
    fn test_delete_nonexistent_ref() {
        let (_dir, repo) = test_repo();

        let result = delete_ref(&repo, "nonexistent");
        assert!(matches!(result, Err(Error::RefNotFound(_))));
    }

    #[test]
    fn test_read_nonexistent_ref() {
        let (_dir, repo) = test_repo();

        let result = read_ref(&repo, "nonexistent");
        assert!(matches!(result, Err(Error::RefNotFound(_))));
    }

    #[test]
    fn test_list_refs() {
        let (_dir, repo) = test_repo();

        write_ref(&repo, "a/b/c", &Hash::ZERO).unwrap();
        write_ref(&repo, "x/y", &Hash::ZERO).unwrap();
        write_ref(&repo, "single", &Hash::ZERO).unwrap();

        let refs = list_refs(&repo).unwrap();
        assert_eq!(refs.len(), 3);
        assert!(refs.contains(&"a/b/c".to_string()));
        assert!(refs.contains(&"x/y".to_string()));
        assert!(refs.contains(&"single".to_string()));
    }

    #[test]
    fn test_list_refs_matching() {
        let (_dir, repo) = test_repo();

        write_ref(&repo, "x86_64/pkg/foo/1.0", &Hash::ZERO).unwrap();
        write_ref(&repo, "x86_64/pkg/bar/2.0", &Hash::ZERO).unwrap();
        write_ref(&repo, "aarch64/pkg/foo/1.0", &Hash::ZERO).unwrap();

        let refs = list_refs_matching(&repo, "x86_64/*").unwrap();
        assert_eq!(refs.len(), 2);

        let refs = list_refs_matching(&repo, "*/pkg/foo/*").unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_resolve_ref_hash() {
        let (_dir, repo) = test_repo();

        // 64 hex chars should be parsed as hash directly
        let hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let hash = resolve_ref(&repo, hex).unwrap();
        assert_eq!(hash.to_hex(), hex);
    }

    #[test]
    fn test_resolve_ref_name() {
        let (_dir, repo) = test_repo();

        let hash =
            Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        write_ref(&repo, "myref", &hash).unwrap();

        let resolved = resolve_ref(&repo, "myref").unwrap();
        assert_eq!(resolved, hash);
    }

    #[test]
    fn test_invalid_ref_names() {
        assert!(validate_ref_name("").is_err());
        assert!(validate_ref_name("/start").is_err());
        assert!(validate_ref_name("end/").is_err());
        assert!(validate_ref_name("double//slash").is_err());
        assert!(validate_ref_name("with/./dot").is_err());
        assert!(validate_ref_name("with/../dotdot").is_err());
        assert!(validate_ref_name("with\0null").is_err());

        // valid names
        assert!(validate_ref_name("simple").is_ok());
        assert!(validate_ref_name("with/slash").is_ok());
        assert!(validate_ref_name("deep/nested/path/ref").is_ok());
    }

    #[test]
    fn test_overwrite_ref() {
        let (_dir, repo) = test_repo();

        let hash1 =
            Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let hash2 =
            Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();

        write_ref(&repo, "myref", &hash1).unwrap();
        write_ref(&repo, "myref", &hash2).unwrap();

        let read_hash = read_ref(&repo, "myref").unwrap();
        assert_eq!(read_hash, hash2);
    }
}
