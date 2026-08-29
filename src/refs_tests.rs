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
        Hash::from_hex("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789").unwrap();

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
        Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
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
        Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111").unwrap();
    let hash2 =
        Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222").unwrap();

    write_ref(&repo, "myref", &hash1).unwrap();
    write_ref(&repo, "myref", &hash2).unwrap();

    let read_hash = read_ref(&repo, "myref").unwrap();
    assert_eq!(read_hash, hash2);
}

// --- Artifact ref tests ---

#[test]
fn test_write_and_read_artifact_ref() {
    let (_dir, repo) = test_repo();

    let artifact_hash =
        Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111").unwrap();

    write_artifact_ref(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/bin",
        &artifact_hash,
    )
    .unwrap();

    let read_hash = read_artifact_ref(&repo, "x86_64/pkg/foo/1.0/abc123/outputs/bin").unwrap();
    assert_eq!(artifact_hash, read_hash);
}

#[test]
fn test_artifact_ref_exists() {
    let (_dir, repo) = test_repo();

    let artifact_hash = Hash::ZERO;

    assert!(!artifact_ref_exists(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/bin"
    ));

    write_artifact_ref(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/bin",
        &artifact_hash,
    )
    .unwrap();

    assert!(artifact_ref_exists(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/bin"
    ));
    assert!(!artifact_ref_exists(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/lib"
    ));
}

#[test]
fn test_list_artifact_refs() {
    let (_dir, repo) = test_repo();

    let artifact_hash = Hash::ZERO;

    write_artifact_ref(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/bundles/dev",
        &artifact_hash,
    )
    .unwrap();
    write_artifact_ref(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/bundles/full",
        &artifact_hash,
    )
    .unwrap();
    write_artifact_ref(
        &repo,
        "x86_64/pkg/bar/2.0/def456/outputs/bin",
        &artifact_hash,
    )
    .unwrap();

    let refs = list_artifact_refs(&repo).unwrap();
    assert_eq!(refs.len(), 3);
}

#[test]
fn test_list_artifact_refs_matching() {
    let (_dir, repo) = test_repo();

    let artifact_hash = Hash::ZERO;

    write_artifact_ref(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/bin",
        &artifact_hash,
    )
    .unwrap();
    write_artifact_ref(
        &repo,
        "x86_64/pkg/bar/2.0/def456/outputs/bin",
        &artifact_hash,
    )
    .unwrap();
    write_artifact_ref(
        &repo,
        "x86_64/bootstrap/baz/1.0/ghi789/outputs/lib",
        &artifact_hash,
    )
    .unwrap();

    let refs = list_artifact_refs_matching(&repo, "*/pkg/*").unwrap();
    assert_eq!(refs.len(), 2);

    let refs = list_artifact_refs_matching(&repo, "*/bootstrap/*").unwrap();
    assert_eq!(refs.len(), 1);
}

#[test]
fn test_delete_artifact_refs_matching() {
    let (_dir, repo) = test_repo();

    let artifact_hash = Hash::ZERO;

    write_artifact_ref(
        &repo,
        "x86_64/pkg/foo/1.0/abc123/outputs/bin",
        &artifact_hash,
    )
    .unwrap();
    write_artifact_ref(
        &repo,
        "x86_64/pkg/bar/2.0/def456/outputs/bin",
        &artifact_hash,
    )
    .unwrap();
    write_artifact_ref(
        &repo,
        "x86_64/bootstrap/baz/1.0/ghi789/outputs/lib",
        &artifact_hash,
    )
    .unwrap();

    let deleted = delete_artifact_refs_matching(&repo, "*/pkg/*").unwrap();
    assert_eq!(deleted.len(), 2);

    let remaining = list_artifact_refs(&repo).unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].contains("bootstrap"));
}

#[test]
fn test_read_nonexistent_artifact_ref() {
    let (_dir, repo) = test_repo();

    let result = read_artifact_ref(&repo, "nonexistent/path");
    assert!(matches!(result, Err(Error::RefNotFound(_))));
}
