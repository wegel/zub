//! Exact reusable root sizes guarded by current object inventories.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{IoResultExt, Result};
use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::repo::Repo;
use crate::types::EntryKind;

const SCHEMA: u32 = 1;

#[derive(Default)]
pub(super) struct DuCache {
    commits: HashMap<Hash, Hash>,
    trees: HashMap<Hash, TreeLinks>,
    sizes: HashMap<Hash, u64>,
    dirty: bool,
}

struct TreeLinks {
    blobs: Vec<Hash>,
    children: Vec<Hash>,
}

impl DuCache {
    pub(super) fn open(repo: &Repo, blobs: Hash, trees: Hash) -> Self {
        let sizes = read_index(&index_path(repo), blobs, trees);
        Self {
            sizes: sizes.into_iter().collect(),
            ..Self::default()
        }
    }

    pub(super) fn commit_tree(&mut self, repo: &Repo, commit: Hash) -> Result<Hash> {
        if let Some(tree) = self.commits.get(&commit) {
            return Ok(*tree);
        }
        let tree = read_commit(repo, &commit)?.tree;
        self.commits.insert(commit, tree);
        Ok(tree)
    }

    pub(super) fn tree_bytes(
        &mut self,
        repo: &Repo,
        root: Hash,
        blob_sizes: &HashMap<Hash, u64>,
    ) -> Result<u64> {
        if let Some(bytes) = self.sizes.get(&root) {
            return Ok(*bytes);
        }
        let mut pending = vec![root];
        let mut trees = HashSet::new();
        let mut blobs = HashSet::new();
        while let Some(hash) = pending.pop() {
            if !trees.insert(hash) {
                continue;
            }
            self.ensure_tree(repo, hash)?;
            let links = &self.trees[&hash];
            blobs.extend(links.blobs.iter().copied());
            pending.extend(links.children.iter().copied());
        }
        let bytes = blobs.iter().filter_map(|hash| blob_sizes.get(hash)).sum();
        self.sizes.insert(root, bytes);
        self.dirty = true;
        Ok(bytes)
    }

    pub(super) fn save(&self, repo: &Repo, blobs: Hash, trees: Hash) {
        if self.dirty {
            let _ = write_index(repo, &index_path(repo), blobs, trees, &self.sizes);
        }
    }

    fn ensure_tree(&mut self, repo: &Repo, hash: Hash) -> Result<()> {
        if self.trees.contains_key(&hash) {
            return Ok(());
        }
        let tree = read_tree(repo, &hash)?;
        let mut blobs = Vec::new();
        let mut children = Vec::new();
        for entry in tree.entries() {
            match &entry.kind {
                EntryKind::Regular { hash, .. } | EntryKind::Symlink { hash, .. } => {
                    blobs.push(*hash);
                }
                EntryKind::Directory { hash, .. } => children.push(*hash),
                _ => {}
            }
        }
        self.trees.insert(hash, TreeLinks { blobs, children });
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct DuIndex {
    schema: u32,
    blob_inventory: Hash,
    tree_inventory: Hash,
    sizes: BTreeMap<Hash, u64>,
}

fn index_path(repo: &Repo) -> PathBuf {
    repo.index_path().join("du-v1.cbor")
}

fn read_index(path: &Path, blobs: Hash, trees: Hash) -> BTreeMap<Hash, u64> {
    let Ok(bytes) = fs::read(path) else {
        return BTreeMap::new();
    };
    let Ok(index) = ciborium::from_reader::<DuIndex, _>(bytes.as_slice()) else {
        return BTreeMap::new();
    };
    if index.schema == SCHEMA && index.blob_inventory == blobs && index.tree_inventory == trees {
        index.sizes
    } else {
        BTreeMap::new()
    }
}

fn write_index(
    repo: &Repo,
    path: &Path,
    blobs: Hash,
    trees: Hash,
    sizes: &HashMap<Hash, u64>,
) -> Result<()> {
    fs::create_dir_all(repo.index_path()).with_path(repo.index_path())?;
    let temporary = repo
        .tmp_path()
        .join(format!("du-index-{}", uuid::Uuid::new_v4()));
    let index = DuIndex {
        schema: SCHEMA,
        blob_inventory: blobs,
        tree_inventory: trees,
        sizes: sizes.iter().map(|(hash, bytes)| (*hash, *bytes)).collect(),
    };
    let result = (|| {
        let mut bytes = Vec::new();
        ciborium::into_writer(&index, &mut bytes)?;
        let mut file = File::create(&temporary).with_path(&temporary)?;
        file.write_all(&bytes).with_path(&temporary)?;
        file.flush().with_path(&temporary)?;
        drop(file);
        fs::rename(&temporary, path).with_path(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
