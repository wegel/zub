use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::error::{IoResultExt, Result};
use crate::fs::{detect_sparse_regions, read_xattrs, FileMetadata, FileType};
use crate::hash::{compute_symlink_hash, Hash, SYMLINK_MODE};
use crate::namespace::outside_to_inside;
use crate::object::{
    sync_repository, write_blob_streaming_with_durability, write_blob_with_durability,
    write_commit_with_durability, write_tree_with_durability, ObjectDurability,
};
use crate::refs::write_ref;
use crate::repo::Repo;
use crate::types::{Commit, EntryKind, SparseRegion, Tree, TreeEntry};

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
                let rel_path = path
                    .strip_prefix(source)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                let key = (meta.dev, meta.ino);
                hardlink_map
                    .entry(key)
                    .or_insert_with(Vec::new)
                    .push(rel_path);
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
    let started = Instant::now();
    let durability = ObjectDurability::Deferred;
    let tree_hash = commit_tree_parallel(repo, source, "", &hardlink_targets, durability)?.hash;
    trace("tree", started);

    let started = Instant::now();
    let result = commit_tree_with_metadata_inner(
        repo, &tree_hash, ref_name, message, author, metadata, durability,
    );
    trace("commit-ref", started);
    result
}

/// Commit an existing tree to a ref without materializing it on disk.
pub fn commit_tree(
    repo: &Repo,
    tree: &Hash,
    ref_name: &str,
    message: Option<&str>,
    author: Option<&str>,
) -> Result<Hash> {
    commit_tree_with_metadata(repo, tree, ref_name, message, author, &[])
}

/// Commit an existing tree to a ref with custom metadata.
pub fn commit_tree_with_metadata(
    repo: &Repo,
    tree: &Hash,
    ref_name: &str,
    message: Option<&str>,
    author: Option<&str>,
    metadata: &[(&str, &str)],
) -> Result<Hash> {
    commit_tree_with_metadata_inner(
        repo,
        tree,
        ref_name,
        message,
        author,
        metadata,
        ObjectDurability::Immediate,
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_tree_with_metadata_inner(
    repo: &Repo,
    tree: &Hash,
    ref_name: &str,
    message: Option<&str>,
    author: Option<&str>,
    metadata: &[(&str, &str)],
    durability: ObjectDurability,
) -> Result<Hash> {
    // get parent commit if ref exists
    let parents = match crate::refs::read_ref(repo, ref_name) {
        Ok(parent) => vec![parent],
        Err(crate::Error::RefNotFound(_)) => vec![],
        Err(e) => return Err(e),
    };

    // create commit with metadata
    let mut commit = Commit::new(
        *tree,
        parents,
        author.unwrap_or("zub"),
        message.unwrap_or(""),
    );
    for (key, value) in metadata {
        commit = commit.with_metadata(*key, *value);
    }

    let commit_hash = write_commit_with_durability(repo, &commit, durability)?;

    if durability == ObjectDurability::Deferred {
        let started = Instant::now();
        sync_repository(repo)?;
        trace("sync", started);
    }

    // update ref
    write_ref(repo, ref_name, &commit_hash)?;

    Ok(commit_hash)
}

fn trace(phase: &str, started: Instant) {
    if std::env::var_os("ZUB_PERF_TRACE").is_some() {
        eprintln!(
            "perf commit {phase} {:.6}s",
            started.elapsed().as_secs_f64()
        );
    }
}

/// processed file entry ready for tree building
struct ProcessedEntry {
    name: String,
    kind: EntryKind,
    elf_blobs: Vec<Hash>,
}

struct CommittedTree {
    hash: Hash,
    elf_blobs: Vec<Hash>,
}

/// commit a directory tree with parallel file processing
fn commit_tree_parallel(
    repo: &Repo,
    dir: &Path,
    prefix: &str,
    hardlink_targets: &HashMap<String, String>,
    durability: ObjectDurability,
) -> Result<CommittedTree> {
    let ns = &repo.config().namespace;

    // read directory entries
    let mut dir_entries: Vec<_> = fs::read_dir(dir)
        .with_path(dir)?
        .collect::<std::io::Result<Vec<_>>>()
        .with_path(dir)?;
    dir_entries.sort_by_key(|a| a.file_name());

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
            let subtree =
                commit_tree_parallel(repo, &path, &logical_path, hardlink_targets, durability)?;

            let kind = EntryKind::directory_with_xattrs(
                subtree.hash,
                inside_uid,
                inside_gid,
                meta.mode,
                xattrs,
            );

            Ok(ProcessedEntry {
                name,
                kind,
                elf_blobs: subtree.elf_blobs,
            })
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

            let mut elf_blobs = Vec::new();
            let kind = match meta.file_type {
                FileType::Regular => {
                    // check for hardlink
                    if let Some(target) = hardlink_targets.get(logical_path) {
                        return Ok(ProcessedEntry {
                            name: name.clone(),
                            kind: EntryKind::hardlink(target.clone()),
                            elf_blobs,
                        });
                    }

                    // read file content and xattrs
                    let xattrs = read_xattrs(path)?;
                    let mut file = File::open(path).with_path(path)?;
                    let elf = has_elf_magic(&mut file, path)?;

                    // check for sparse file
                    let sparse_regions = detect_sparse_regions(&file)?;

                    let (hash, sparse_map) = match sparse_regions {
                        Some(ref regions) if !regions.is_empty() => {
                            let mut reader = SparseReader::new(&mut file, regions);
                            let hash = write_blob_streaming_with_durability(
                                repo,
                                &mut reader,
                                inside_uid,
                                inside_gid,
                                meta.mode,
                                &xattrs,
                                durability,
                            )?;
                            (hash, Some(regions.clone()))
                        }
                        Some(_) => {
                            let hash = write_blob_with_durability(
                                repo,
                                &[],
                                inside_uid,
                                inside_gid,
                                meta.mode,
                                &xattrs,
                                durability,
                            )?;
                            (hash, Some(vec![]))
                        }
                        None => {
                            file.seek(SeekFrom::Start(0)).with_path(path)?;
                            let hash = write_blob_streaming_with_durability(
                                repo, &mut file, inside_uid, inside_gid, meta.mode, &xattrs,
                                durability,
                            )?;
                            (hash, None)
                        }
                    };

                    if elf {
                        elf_blobs.push(hash);
                    }

                    match sparse_map {
                        Some(map) => EntryKind::sparse(hash, meta.size, map, xattrs),
                        None => EntryKind::regular(hash, meta.size, xattrs),
                    }
                }

                FileType::Symlink => {
                    let target = crate::fs::read_symlink_target(path)?;
                    let xattrs = read_xattrs(path)?;
                    let hash = compute_symlink_hash(inside_uid, inside_gid, &xattrs, &target);
                    write_blob_with_durability(
                        repo,
                        target.as_bytes(),
                        inside_uid,
                        inside_gid,
                        SYMLINK_MODE,
                        &xattrs,
                        durability,
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
                elf_blobs,
            })
        })
        .collect();

    // collect file entries, propagating errors
    let file_entries: Vec<ProcessedEntry> = file_entries.into_iter().collect::<Result<Vec<_>>>()?;

    // combine and sort entries by name
    let mut elf_blobs = Vec::new();
    let mut entries: Vec<TreeEntry> = dir_entries
        .into_iter()
        .chain(file_entries)
        .map(|entry| {
            elf_blobs.extend(entry.elf_blobs);
            TreeEntry::new(entry.name, entry.kind)
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    // create and write tree
    let tree = Tree::new(entries)?;
    let hash = write_tree_with_durability(repo, &tree, durability)?;
    crate::index::record_tree_elf(repo, hash, &elf_blobs);
    Ok(CommittedTree { hash, elf_blobs })
}

fn has_elf_magic(file: &mut File, path: &Path) -> Result<bool> {
    let mut magic = [0_u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(magic == *b"\x7fELF"),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(source) => Err(crate::Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

struct SparseReader<'a> {
    file: &'a mut File,
    regions: &'a [SparseRegion],
    index: usize,
    offset: u64,
}

impl<'a> SparseReader<'a> {
    fn new(file: &'a mut File, regions: &'a [SparseRegion]) -> Self {
        Self {
            file,
            regions,
            index: 0,
            offset: 0,
        }
    }
}

impl Read for SparseReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let Some(region) = self.regions.get(self.index) else {
            return Ok(0);
        };
        if self.offset == 0 {
            self.file.seek(SeekFrom::Start(region.offset))?;
        }
        let remaining = region.length - self.offset;
        let length = remaining.min(buffer.len() as u64) as usize;
        let read = self.file.read(&mut buffer[..length])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "sparse data region ended early",
            ));
        }
        self.offset += read as u64;
        if self.offset == region.length {
            self.index += 1;
            self.offset = 0;
        }
        Ok(read)
    }
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

    #[test]
    fn test_commit_existing_tree_with_metadata() {
        let (dir, repo) = test_repo();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "content").unwrap();
        let source_commit = commit(&repo, &source, "source", None, None).unwrap();
        let tree = crate::object::read_commit(&repo, &source_commit)
            .unwrap()
            .tree;

        let hash = commit_tree_with_metadata(
            &repo,
            &tree,
            "selected",
            Some("selected tree"),
            Some("test"),
            &[("key", "value")],
        )
        .unwrap();
        let committed = crate::object::read_commit(&repo, &hash).unwrap();

        assert_eq!(committed.tree, tree);
        assert_eq!(committed.metadata["key"], "value");
        assert_eq!(crate::refs::resolve_ref(&repo, "selected").unwrap(), hash);
    }
}
