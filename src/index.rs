//! Rebuildable indexes over immutable trees.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{IoResultExt, Result};
use crate::metadata::ensure_elf;
use crate::{list_refs, read_commit, read_ref, read_tree, EntryKind, Hash, Repo};

const TREE_MARKER_SCHEMA: &str = "zub-tree-metadata-v1";

/// Derive or repair every rebuildable index and ELF artifact for a commit.
pub fn ensure_commit_metadata(repo: &Repo, commit: Hash) -> Result<()> {
    ensure_commits_metadata(repo, &[commit])
}

/// Derive or repair indexes and ELF artifacts for several commits once each.
pub fn ensure_commits_metadata(repo: &Repo, commits: &[Hash]) -> Result<()> {
    let mut roots = BTreeSet::new();
    for commit in commits.iter().copied().collect::<BTreeSet<_>>() {
        if crate::commit_path(repo, &commit).exists() {
            roots.insert(read_commit(repo, &commit)?.tree);
        }
    }
    let mut elf_blobs = BTreeSet::new();
    for tree in roots {
        elf_blobs.extend(collect_tree_elf(repo, &repo.index_path(), tree)?);
    }
    for blob in elf_blobs {
        ensure_elf(repo, blob)?;
    }
    Ok(())
}

/// Return sorted current refs whose current trees contain `blob`.
pub fn refs_containing_blob(repo: &Repo, blob: Hash) -> Result<Vec<String>> {
    let refs = current_refs(repo)?;
    for (_, tree) in &refs {
        ensure_tree(repo, &repo.index_path(), *tree)?;
    }
    let containing = indexed_trees(&repo.index_path(), blob)?;
    let mut matches = Vec::new();
    for (name, tree) in refs {
        if contains_tree(repo, tree, &containing, &mut HashSet::new())? {
            matches.push(name);
        }
    }
    Ok(matches)
}

/// Rebuild every reverse-blob marker from current refs.
pub fn rebuild_index(repo: &Repo) -> Result<usize> {
    let temporary = repo
        .tmp_path()
        .join(format!("index-{}", uuid::Uuid::new_v4()));
    let result = build_index(repo, &temporary);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    replace_index(repo, &temporary)
}

fn build_index(repo: &Repo, index: &Path) -> Result<()> {
    fs::create_dir_all(index).with_path(index)?;
    for (_, tree) in current_refs(repo)? {
        ensure_tree(repo, index, tree)?;
    }
    Ok(())
}

fn replace_index(repo: &Repo, replacement: &Path) -> Result<usize> {
    let live = repo.index_path();
    let old = repo
        .tmp_path()
        .join(format!("old-index-{}", uuid::Uuid::new_v4()));
    let had_live = live.exists();
    if had_live {
        fs::rename(&live, &old).with_path(&live)?;
    }
    if let Err(error) = fs::rename(replacement, &live).with_path(replacement) {
        if had_live {
            let _ = fs::rename(&old, &live);
        }
        return Err(error);
    }
    if had_live {
        fs::remove_dir_all(&old).with_path(&old)?;
    }
    count_markers(&live.join("blobs"))
}

fn current_refs(repo: &Repo) -> Result<Vec<(String, Hash)>> {
    list_refs(repo)?
        .into_iter()
        .map(|name| {
            let commit = read_ref(repo, &name)?;
            let tree = read_commit(repo, &commit)?.tree;
            Ok((name, tree))
        })
        .collect()
}

fn ensure_tree(repo: &Repo, index: &Path, tree: Hash) -> Result<Vec<Hash>> {
    let blobs = collect_tree_elf(repo, index, tree)?;
    for blob in &blobs {
        ensure_elf(repo, *blob)?;
    }
    Ok(blobs)
}

fn collect_tree_elf(repo: &Repo, index: &Path, tree: Hash) -> Result<Vec<Hash>> {
    let marker = tree_marker(index, tree);
    if let Some(blobs) = read_tree_marker(&marker)? {
        return Ok(blobs);
    }
    let mut elf_blobs = BTreeSet::new();
    for entry in read_tree(repo, &tree)?.entries() {
        match entry.kind {
            EntryKind::Regular { hash, .. } => {
                write_marker(&blob_tree_marker(index, hash, tree))?;
                if ensure_elf(repo, hash)? {
                    elf_blobs.insert(hash);
                }
            }
            EntryKind::Symlink { hash, .. } => {
                write_marker(&blob_tree_marker(index, hash, tree))?;
            }
            EntryKind::Directory { hash, .. } => {
                elf_blobs.extend(collect_tree_elf(repo, index, hash)?);
            }
            _ => {}
        }
    }
    let elf_blobs = elf_blobs.into_iter().collect::<Vec<_>>();
    write_tree_marker(&marker, &elf_blobs)?;
    Ok(elf_blobs)
}

fn read_tree_marker(path: &Path) -> Result<Option<Vec<Hash>>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    let mut lines = text.lines();
    if lines.next() != Some(TREE_MARKER_SCHEMA) {
        return Ok(None);
    }
    let mut blobs = Vec::new();
    for line in lines {
        let Ok(hash) = Hash::from_hex(line) else {
            return Ok(None);
        };
        blobs.push(hash);
    }
    Ok(Some(blobs))
}

fn write_tree_marker(path: &Path, blobs: &[Hash]) -> Result<()> {
    let parent = path.parent().expect("tree marker parent");
    fs::create_dir_all(parent).with_path(parent)?;
    let temporary = parent.join(format!(".tree-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = File::create(&temporary).with_path(&temporary)?;
        writeln!(file, "{TREE_MARKER_SCHEMA}").with_path(&temporary)?;
        for blob in blobs {
            writeln!(file, "{blob}").with_path(&temporary)?;
        }
        file.sync_all().with_path(&temporary)?;
        fs::rename(&temporary, path).with_path(path)?;
        File::open(parent)
            .with_path(parent)?
            .sync_all()
            .with_path(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn contains_tree(
    repo: &Repo,
    tree: Hash,
    targets: &HashSet<Hash>,
    visited: &mut HashSet<Hash>,
) -> Result<bool> {
    if !visited.insert(tree) {
        return Ok(false);
    }
    if targets.contains(&tree) {
        return Ok(true);
    }
    for entry in read_tree(repo, &tree)?.entries() {
        if let EntryKind::Directory { hash, .. } = entry.kind {
            if contains_tree(repo, hash, targets, visited)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn indexed_trees(index: &Path, blob: Hash) -> Result<HashSet<Hash>> {
    let directory = blob_marker_dir(index, blob);
    if !directory.exists() {
        return Ok(HashSet::new());
    }
    let mut trees = HashSet::new();
    for entry in fs::read_dir(&directory).with_path(&directory)? {
        let entry = entry.with_path(&directory)?;
        if entry.file_type().with_path(entry.path())?.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(hash) = Hash::from_hex(name) {
                    trees.insert(hash);
                }
            }
        }
    }
    Ok(trees)
}

fn write_marker(path: &Path) -> Result<()> {
    let parent = path.parent().expect("marker parent");
    fs::create_dir_all(parent).with_path(parent)?;
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file.sync_all().with_path(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(source) => {
            return Err(crate::Error::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
    File::open(parent)
        .with_path(parent)?
        .sync_all()
        .with_path(parent)
}

fn tree_marker(index: &Path, tree: Hash) -> PathBuf {
    let (prefix, suffix) = tree.to_path_components();
    index.join("trees").join(prefix).join(suffix)
}

fn blob_marker_dir(index: &Path, blob: Hash) -> PathBuf {
    let (prefix, suffix) = blob.to_path_components();
    index.join("blobs").join(prefix).join(suffix)
}

fn blob_tree_marker(index: &Path, blob: Hash, tree: Hash) -> PathBuf {
    blob_marker_dir(index, blob).join(tree.to_hex())
}

fn count_markers(root: &Path) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|error| crate::Error::Io {
            path: root.to_path_buf(),
            source: error
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
        })?;
        count += usize::from(entry.file_type().is_file());
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ensure_commit_metadata, read_tree_marker, tree_marker};
    use crate::ops::commit;
    use crate::{
        artifact_ref_exists, delete_artifact_ref, read_commit, read_tree, tree_path, Repo,
    };

    #[test]
    fn cached_tree_metadata_repairs_elf_without_rewalking_the_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::copy(std::env::current_exe().unwrap(), source.join("probe")).unwrap();
        fs::write(source.join("plain"), b"not an ELF").unwrap();
        let repo = Repo::init(&temporary.path().join("repo")).unwrap();
        let commit = commit(&repo, &source, "fixture", None, None).unwrap();
        let tree = read_commit(&repo, &commit).unwrap().tree;
        let entries = read_tree(&repo, &tree).unwrap();
        let blob = *entries.get("probe").unwrap().kind.hash().unwrap();
        let key = format!("elf/{blob}");
        let marker = tree_marker(&repo.index_path(), tree);

        assert_eq!(read_tree_marker(&marker).unwrap().unwrap(), vec![blob]);
        delete_artifact_ref(&repo, &key).unwrap();
        fs::write(tree_path(&repo, &tree), b"corrupt after verified marker").unwrap();

        ensure_commit_metadata(&repo, commit).unwrap();
        assert!(artifact_ref_exists(&repo, &key));
    }

    #[test]
    fn replaces_a_legacy_empty_tree_marker() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("plain"), b"not an ELF").unwrap();
        let repo = Repo::init(&temporary.path().join("repo")).unwrap();
        let commit = commit(&repo, &source, "fixture", None, None).unwrap();
        let tree = read_commit(&repo, &commit).unwrap().tree;
        let marker = tree_marker(&repo.index_path(), tree);
        fs::write(&marker, b"").unwrap();

        ensure_commit_metadata(&repo, commit).unwrap();

        assert_eq!(read_tree_marker(&marker).unwrap(), Some(Vec::new()));
    }
}
