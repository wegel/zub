use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::error::{IoResultExt, Result};
use crate::fs::{detect_sparse_regions, read_data_regions, read_xattrs, FileMetadata, FileType};
use crate::hash::{compute_symlink_hash, Hash, SYMLINK_MODE};
use crate::namespace::outside_to_inside;
use crate::object::{write_blob, write_commit, write_tree};
use crate::refs::write_ref;
use crate::repo::Repo;
use crate::types::{Commit, EntryKind, Tree, TreeEntry};

/// commit a directory tree to a ref
pub fn commit(
    repo: &Repo,
    source: &Path,
    ref_name: &str,
    message: Option<&str>,
    author: Option<&str>,
) -> Result<Hash> {
    commit_with_metadata(repo, source, ref_name, message, author, &[])
}

/// commit a directory tree to a ref with custom metadata
pub fn commit_with_metadata(
    repo: &Repo,
    source: &Path,
    ref_name: &str,
    message: Option<&str>,
    author: Option<&str>,
    metadata: &[(&str, &str)],
) -> Result<Hash> {
    // phase 1: collect all files and detect hardlinks
    let mut hardlink_map = HashMap::new();
    for entry in WalkDir::new(source).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if let Ok(meta) = FileMetadata::from_path(path) {
            if meta.file_type == FileType::Regular && meta.could_be_hardlink() {
                let rel_path = path.strip_prefix(source).unwrap().to_string_lossy().to_string();
                let key = (meta.dev, meta.ino);
                hardlink_map.entry(key).or_insert_with(Vec::new).push(rel_path);
            }
        }
    }

    // build hardlink target map: subsequent files point to the first
    let mut hardlink_targets: HashMap<String, String> = HashMap::new();
    for (_key, paths) in hardlink_map {
        if paths.len() > 1 {
            let first = &paths[0];
            for path in paths.iter().skip(1) {
                hardlink_targets.insert(path.clone(), first.clone());
            }
        }
    }

    // phase 2: commit the root tree with parallel file processing
    let tree_hash = commit_tree_parallel(repo, source, "", &hardlink_targets)?;

    // get parent commit if ref exists
    let parents = match crate::refs::read_ref(repo, ref_name) {
        Ok(parent) => vec![parent],
        Err(crate::Error::RefNotFound(_)) => vec![],
        Err(e) => return Err(e),
    };

    // create commit with metadata
    let mut commit = Commit::new(
        tree_hash,
        parents,
        author.unwrap_or("zub"),
        message.unwrap_or(""),
    );
    for (key, value) in metadata {
        commit = commit.with_metadata(*key, *value);
    }

    let commit_hash = write_commit(repo, &commit)?;

    // update ref
    write_ref(repo, ref_name, &commit_hash)?;

    Ok(commit_hash)
}

/// processed file entry ready for tree building
struct ProcessedEntry {
    name: String,
    kind: EntryKind,
}

/// commit a directory tree with parallel file processing
fn commit_tree_parallel(
    repo: &Repo,
    dir: &Path,
    prefix: &str,
    hardlink_targets: &HashMap<String, String>,
) -> Result<Hash> {
    let ns = &repo.config().namespace;

    // read directory entries
    let mut dir_entries: Vec<_> = fs::read_dir(dir)
        .with_path(dir)?
        .collect::<std::io::Result<Vec<_>>>()
        .with_path(dir)?;
    dir_entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    // separate directories from files for different processing strategies
    let mut directories = Vec::new();
    let mut files = Vec::new();

    for entry in dir_entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let logical_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", prefix, name)
        };

        let meta = FileMetadata::from_path(&path)?;

        if meta.file_type == FileType::Directory {
            directories.push((path, name, logical_path, meta));
        } else {
            files.push((path, name, logical_path, meta));
        }
    }

    // process directories recursively (must be sequential for tree building)
    let dir_entries: Vec<ProcessedEntry> = directories
        .into_iter()
        .map(|(path, name, logical_path, meta)| {
            let inside_uid = outside_to_inside(meta.uid, &ns.uid_map)
                .ok_or(crate::Error::UnmappedUid(meta.uid))?;
            let inside_gid = outside_to_inside(meta.gid, &ns.gid_map)
                .ok_or(crate::Error::UnmappedGid(meta.gid))?;

            let xattrs = read_xattrs(&path)?;
            let subtree_hash = commit_tree_parallel(repo, &path, &logical_path, hardlink_targets)?;

            let kind = EntryKind::directory_with_xattrs(
                subtree_hash,
                inside_uid,
                inside_gid,
                meta.mode,
                xattrs,
            );

            Ok(ProcessedEntry { name, kind })
        })
        .collect::<Result<Vec<_>>>()?;

    // process files in parallel
    let file_entries: Vec<Result<ProcessedEntry>> = files
        .par_iter()
        .map(|(path, name, logical_path, meta)| {
            let inside_uid = outside_to_inside(meta.uid, &ns.uid_map)
                .ok_or(crate::Error::UnmappedUid(meta.uid))?;
            let inside_gid = outside_to_inside(meta.gid, &ns.gid_map)
                .ok_or(crate::Error::UnmappedGid(meta.gid))?;

            let kind = match meta.file_type {
                FileType::Regular => {
                    // check for hardlink
                    if let Some(target) = hardlink_targets.get(logical_path) {
                        return Ok(ProcessedEntry {
                            name: name.clone(),
                            kind: EntryKind::hardlink(target.clone()),
                        });
                    }

                    // read file content and xattrs
                    let xattrs = read_xattrs(path)?;
                    let mut file = File::open(path).with_path(path)?;

                    // check for sparse file
                    let sparse_regions = detect_sparse_regions(&file)?;

                    let (content, sparse_map) = match sparse_regions {
                        Some(ref regions) if !regions.is_empty() => {
                            let data = read_data_regions(&mut file, regions)?;
                            (data, Some(regions.clone()))
                        }
                        Some(_) => (vec![], Some(vec![])),
                        None => {
                            use std::io::Seek;
                            file.seek(std::io::SeekFrom::Start(0)).with_path(path)?;
                            let mut content = Vec::new();
                            file.read_to_end(&mut content).with_path(path)?;
                            (content, None)
                        }
                    };

                    // write blob
                    let hash =
                        write_blob(repo, &content, inside_uid, inside_gid, meta.mode, &xattrs)?;

                    match sparse_map {
                        Some(map) => EntryKind::sparse(hash, meta.size, map, xattrs),
                        None => EntryKind::regular(hash, meta.size, xattrs),
                    }
                }

                FileType::Symlink => {
                    let target = crate::fs::read_symlink_target(path)?;
                    let xattrs = read_xattrs(path)?;
                    let hash = compute_symlink_hash(inside_uid, inside_gid, &xattrs, &target);
                    write_blob(
                        repo,
                        target.as_bytes(),
                        inside_uid,
                        inside_gid,
                        SYMLINK_MODE,
                        &xattrs,
                    )?;
                    EntryKind::symlink(hash, xattrs)
                }

                FileType::BlockDevice => {
                    let (major, minor) = meta.rdev.unwrap_or((0, 0));
                    let xattrs = read_xattrs(path)?;
                    EntryKind::BlockDevice {
                        major,
                        minor,
                        uid: inside_uid,
                        gid: inside_gid,
                        mode: meta.mode,
                        xattrs,
                    }
                }

                FileType::CharDevice => {
                    let (major, minor) = meta.rdev.unwrap_or((0, 0));
                    let xattrs = read_xattrs(path)?;
                    EntryKind::CharDevice {
                        major,
                        minor,
                        uid: inside_uid,
                        gid: inside_gid,
                        mode: meta.mode,
                        xattrs,
                    }
                }

                FileType::Fifo => {
                    let xattrs = read_xattrs(path)?;
                    EntryKind::Fifo {
                        uid: inside_uid,
                        gid: inside_gid,
                        mode: meta.mode,
                        xattrs,
                    }
                }

                FileType::Socket => {
                    let xattrs = read_xattrs(path)?;
                    EntryKind::Socket {
                        uid: inside_uid,
                        gid: inside_gid,
                        mode: meta.mode,
                        xattrs,
                    }
                }

                FileType::Directory => {
                    unreachable!("directories handled separately")
                }
            };

            Ok(ProcessedEntry {
                name: name.clone(),
                kind,
            })
        })
        .collect();

    // collect file entries, propagating errors
    let file_entries: Vec<ProcessedEntry> = file_entries.into_iter().collect::<Result<Vec<_>>>()?;

    // combine and sort entries by name
    let mut entries: Vec<TreeEntry> = dir_entries
        .into_iter()
        .chain(file_entries.into_iter())
        .map(|e| TreeEntry::new(e.name, e.kind))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    // create and write tree
    let tree = Tree::new(entries)?;
    write_tree(repo, &tree)
}

/// count files in a directory (for progress reporting)
#[allow(dead_code)]
pub fn count_files(path: &Path) -> usize {
    WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn test_repo() -> (tempfile::TempDir, Repo) {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let repo = Repo::init(&repo_path).unwrap();
        (dir, repo)
    }

    #[test]
    fn test_commit_single_file() {
        let (dir, repo) = test_repo();

        // create source directory with a file
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hello.txt"), "world").unwrap();

        // commit
        let hash = commit(&repo, &source, "test/ref", Some("test commit"), None).unwrap();

        // verify ref was created
        let resolved = crate::refs::resolve_ref(&repo, "test/ref").unwrap();
        assert_eq!(hash, resolved);

        // read commit and tree
        let commit_obj = crate::object::read_commit(&repo, &hash).unwrap();
        let tree = crate::object::read_tree(&repo, &commit_obj.tree).unwrap();

        assert_eq!(tree.len(), 1);
        assert!(tree.get("hello.txt").is_some());
    }

    #[test]
    fn test_commit_nested_directories() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir_all(source.join("a/b/c")).unwrap();
        fs::write(source.join("a/b/c/file.txt"), "deep").unwrap();
        fs::write(source.join("top.txt"), "top").unwrap();

        let hash = commit(&repo, &source, "nested", None, None).unwrap();

        let commit_obj = crate::object::read_commit(&repo, &hash).unwrap();
        let tree = crate::object::read_tree(&repo, &commit_obj.tree).unwrap();

        assert_eq!(tree.len(), 2);
        assert!(tree.get("a").is_some());
        assert!(tree.get("top.txt").is_some());

        // check nested
        if let Some(entry) = tree.get("a") {
            if let EntryKind::Directory { hash, .. } = &entry.kind {
                let subtree = crate::object::read_tree(&repo, hash).unwrap();
                assert!(subtree.get("b").is_some());
            } else {
                panic!("expected directory");
            }
        }
    }

    #[test]
    fn test_commit_symlink() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        symlink("/target/path", source.join("link")).unwrap();

        let hash = commit(&repo, &source, "symlink-test", None, None).unwrap();

        let commit_obj = crate::object::read_commit(&repo, &hash).unwrap();
        let tree = crate::object::read_tree(&repo, &commit_obj.tree).unwrap();

        let entry = tree.get("link").unwrap();
        assert!(entry.kind.is_symlink());
    }

    #[test]
    fn test_commit_hardlinks() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("original"), "content").unwrap();
        fs::hard_link(source.join("original"), source.join("link")).unwrap();

        let hash = commit(&repo, &source, "hardlink-test", None, None).unwrap();

        let commit_obj = crate::object::read_commit(&repo, &hash).unwrap();
        let tree = crate::object::read_tree(&repo, &commit_obj.tree).unwrap();

        // one should be regular, one should be hardlink
        let mut found_regular = false;
        let mut found_hardlink = false;

        for entry in tree.entries() {
            match &entry.kind {
                EntryKind::Regular { .. } => found_regular = true,
                EntryKind::Hardlink { .. } => found_hardlink = true,
                _ => {}
            }
        }

        assert!(found_regular);
        assert!(found_hardlink);
    }

    #[test]
    fn test_commit_updates_parent() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "v1").unwrap();

        // first commit
        let hash1 = commit(&repo, &source, "versioned", Some("v1"), None).unwrap();

        // modify and commit again
        fs::write(source.join("file.txt"), "v2").unwrap();
        let hash2 = commit(&repo, &source, "versioned", Some("v2"), None).unwrap();

        // second commit should have first as parent
        let commit2 = crate::object::read_commit(&repo, &hash2).unwrap();
        assert_eq!(commit2.parents.len(), 1);
        assert_eq!(commit2.parents[0], hash1);
    }

    #[test]
    fn test_commit_empty_directory() {
        let (dir, repo) = test_repo();

        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();

        let hash = commit(&repo, &source, "empty", None, None).unwrap();

        let commit_obj = crate::object::read_commit(&repo, &hash).unwrap();
        let tree = crate::object::read_tree(&repo, &commit_obj.tree).unwrap();

        assert!(tree.is_empty());
    }
}
