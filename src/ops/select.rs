//! Select paths from an existing tree without copying file contents.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, PathBuf};

use crate::object::{read_commit, read_tree, write_tree};
use crate::refs::resolve_ref;
use crate::types::{EntryKind, Tree, TreeEntry};
use crate::{Error, Hash, Repo, Result};

#[derive(Default)]
struct Selection {
    whole: bool,
    children: BTreeMap<String, Selection>,
}

impl Selection {
    fn from_paths(paths: &[PathBuf]) -> Result<Self> {
        if paths.is_empty() {
            return Err(Error::InvalidPath("no paths were selected".to_string()));
        }

        let mut root = Self::default();
        for path in paths {
            let mut components = Vec::new();
            let mut root_path = false;
            for component in path.components() {
                match component {
                    Component::RootDir => root_path = true,
                    Component::Normal(name) => components.push(
                        name.to_str()
                            .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?
                            .to_string(),
                    ),
                    Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                        return Err(Error::InvalidPath(path.display().to_string()));
                    }
                }
            }
            if components.is_empty() {
                if root_path {
                    root.whole = true;
                    root.children.clear();
                    continue;
                }
                return Err(Error::InvalidPath(path.display().to_string()));
            }

            let mut selection = &mut root;
            for component in components {
                if selection.whole {
                    break;
                }
                selection = selection.children.entry(component).or_default();
            }
            selection.whole = true;
            selection.children.clear();
        }
        Ok(root)
    }

    fn includes(&self, path: &str) -> bool {
        let mut selection = self;
        if selection.whole {
            return true;
        }
        for component in path.split('/').filter(|component| !component.is_empty()) {
            let Some(child) = selection.children.get(component) else {
                return false;
            };
            selection = child;
            if selection.whole {
                return true;
            }
        }
        selection.whole
    }
}

/// Return a tree containing only the selected paths from a ref or commit hash.
///
/// Parent directories retain their metadata. Blobs and unchanged subtrees are
/// reused directly. A hardlink whose target was not selected becomes an
/// independent entry with the target file's content and metadata.
pub fn select_tree(repo: &Repo, revision: &str, paths: &[PathBuf]) -> Result<Hash> {
    let commit_hash = resolve_ref(repo, revision)?;
    let commit = read_commit(repo, &commit_hash)?;
    select_tree_from_hash(repo, &commit.tree, paths)
}

/// Return a tree containing only the selected paths from an existing tree.
pub fn select_tree_from_hash(repo: &Repo, root: &Hash, paths: &[PathBuf]) -> Result<Hash> {
    let selection = Selection::from_paths(paths)?;
    if selection.whole {
        return Ok(*root);
    }
    select_subtree(repo, root, root, &selection, &selection, "")
}

fn select_subtree(
    repo: &Repo,
    root: &Hash,
    source: &Hash,
    selection: &Selection,
    all: &Selection,
    prefix: &str,
) -> Result<Hash> {
    let tree = read_tree(repo, source)?;
    let mut selected = Vec::new();

    if selection.whole {
        for entry in tree.entries() {
            selected.push(select_entry(repo, root, entry, selection, all, prefix)?);
        }
    } else {
        for (name, child) in &selection.children {
            let entry = tree
                .get(name)
                .ok_or_else(|| Error::PathNotFound(join_path(prefix, name)))?;
            selected.push(select_entry(repo, root, entry, child, all, prefix)?);
        }
    }

    if selected == tree.entries() {
        Ok(*source)
    } else {
        write_tree(repo, &Tree::new(selected)?)
    }
}

fn select_entry(
    repo: &Repo,
    root: &Hash,
    entry: &TreeEntry,
    selection: &Selection,
    all: &Selection,
    prefix: &str,
) -> Result<TreeEntry> {
    let logical_path = join_path(prefix, &entry.name);
    let kind = match &entry.kind {
        EntryKind::Directory {
            hash,
            uid,
            gid,
            mode,
            xattrs,
        } => {
            let hash = select_subtree(repo, root, hash, selection, all, &logical_path)?;
            EntryKind::Directory {
                hash,
                uid: *uid,
                gid: *gid,
                mode: *mode,
                xattrs: xattrs.clone(),
            }
        }
        EntryKind::Hardlink { target_path } if selection.whole => {
            if all.includes(target_path) {
                entry.kind.clone()
            } else {
                resolve_materialized_kind(repo, root, target_path, &mut BTreeSet::new())?
            }
        }
        _ if selection.whole => entry.kind.clone(),
        _ => return Err(Error::PathNotFound(logical_path)),
    };
    Ok(TreeEntry::new(entry.name.clone(), kind))
}

fn resolve_materialized_kind(
    repo: &Repo,
    root: &Hash,
    path: &str,
    followed: &mut BTreeSet<String>,
) -> Result<EntryKind> {
    if !followed.insert(path.to_string()) {
        return Err(Error::HardlinkTargetNotFound(path.to_string()));
    }
    let kind = resolve_entry(repo, root, path)?;
    match kind {
        EntryKind::Hardlink { target_path } => {
            resolve_materialized_kind(repo, root, &target_path, followed)
        }
        EntryKind::Regular { .. } | EntryKind::Symlink { .. } => Ok(kind),
        _ => Err(Error::HardlinkTargetNotFound(path.to_string())),
    }
}

fn resolve_entry(repo: &Repo, root: &Hash, path: &str) -> Result<EntryKind> {
    let mut tree = read_tree(repo, root)?;
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if components.is_empty() {
        return Err(Error::PathNotFound(path.to_string()));
    }
    for (index, component) in components.iter().enumerate() {
        let entry = tree
            .get(component)
            .ok_or_else(|| Error::PathNotFound(path.to_string()))?;
        if index + 1 == components.len() {
            return Ok(entry.kind.clone());
        }
        match &entry.kind {
            EntryKind::Directory { hash, .. } => tree = read_tree(repo, hash)?,
            _ => return Err(Error::PathNotFound(path.to_string())),
        }
    }
    Err(Error::PathNotFound(path.to_string()))
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{checkout_from_tree_hash, commit, CheckoutOptions};
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, Repo, Hash) {
        let directory = tempdir().unwrap();
        let repo = Repo::init(&directory.path().join("repo")).unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("usr/bin")).unwrap();
        fs::create_dir_all(source.join("usr/share/doc")).unwrap();
        fs::write(source.join("usr/bin/one"), b"one").unwrap();
        fs::write(source.join("usr/bin/two"), b"two").unwrap();
        fs::write(source.join("usr/share/doc/readme"), b"readme").unwrap();
        let commit_hash = commit(&repo, &source, "test/tree", None, None).unwrap();
        let tree = read_commit(&repo, &commit_hash).unwrap().tree;
        (directory, repo, tree)
    }

    #[test]
    fn selects_files_and_directories_without_copying_unselected_entries() {
        let (directory, repo, root) = fixture();
        let selected = select_tree_from_hash(
            &repo,
            &root,
            &[PathBuf::from("/usr/bin/one"), PathBuf::from("usr/share")],
        )
        .unwrap();
        let target = directory.path().join("target");
        checkout_from_tree_hash(&repo, &selected, &target, CheckoutOptions::default()).unwrap();

        assert_eq!(fs::read(target.join("usr/bin/one")).unwrap(), b"one");
        assert!(!target.join("usr/bin/two").exists());
        assert_eq!(
            fs::read(target.join("usr/share/doc/readme")).unwrap(),
            b"readme"
        );
    }

    #[test]
    fn rejects_missing_and_escaping_paths() {
        let (_directory, repo, root) = fixture();
        assert!(matches!(
            select_tree_from_hash(&repo, &root, &[PathBuf::from("/missing")]),
            Err(Error::PathNotFound(_))
        ));
        assert!(matches!(
            select_tree_from_hash(&repo, &root, &[PathBuf::from("../outside")]),
            Err(Error::InvalidPath(_))
        ));
    }

    #[test]
    fn materializes_a_hardlink_when_its_target_is_not_selected() {
        let directory = tempdir().unwrap();
        let repo = Repo::init(&directory.path().join("repo")).unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a"), b"linked").unwrap();
        fs::hard_link(source.join("a"), source.join("b")).unwrap();
        let commit_hash = commit(&repo, &source, "test/hardlinks", None, None).unwrap();
        let root = read_commit(&repo, &commit_hash).unwrap().tree;
        let selected = select_tree_from_hash(&repo, &root, &[PathBuf::from("b")]).unwrap();
        let target = directory.path().join("target");
        checkout_from_tree_hash(&repo, &selected, &target, CheckoutOptions::default()).unwrap();

        assert_eq!(fs::read(target.join("b")).unwrap(), b"linked");
        assert!(!target.join("a").exists());
    }

    #[test]
    fn preserves_a_hardlink_when_its_target_is_selected() {
        let directory = tempdir().unwrap();
        let repo = Repo::init(&directory.path().join("repo")).unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a"), b"linked").unwrap();
        fs::hard_link(source.join("a"), source.join("b")).unwrap();
        let commit_hash = commit(&repo, &source, "test/hardlinks", None, None).unwrap();
        let root = read_commit(&repo, &commit_hash).unwrap().tree;
        let selected =
            select_tree_from_hash(&repo, &root, &[PathBuf::from("a"), PathBuf::from("b")]).unwrap();
        let target = directory.path().join("target");
        checkout_from_tree_hash(&repo, &selected, &target, CheckoutOptions::default()).unwrap();

        assert_eq!(
            fs::metadata(target.join("a")).unwrap().ino(),
            fs::metadata(target.join("b")).unwrap().ino()
        );
    }
}
