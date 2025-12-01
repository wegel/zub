use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::types::{SparseRegion, Xattr};

/// a directory tree - collection of entries sorted by name
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    /// create a new tree, validating and sorting entries
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self> {
        // validate entry names
        for entry in &entries {
            validate_entry_name(&entry.name)?;
        }

        // sort by name (byte-wise)
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        // check for duplicates
        for window in entries.windows(2) {
            if window[0].name == window[1].name {
                return Err(Error::DuplicateEntryName(window[0].name.clone()));
            }
        }

        Ok(Self { entries })
    }

    /// create an empty tree
    pub fn empty() -> Self {
        Self { entries: vec![] }
    }

    /// get entries slice
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// consume and return entries
    pub fn into_entries(self) -> Vec<TreeEntry> {
        self.entries
    }

    /// look up entry by name
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_bytes().cmp(name.as_bytes()))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// is tree empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// validate an entry name
fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidEntryName("empty name".to_string()));
    }
    if name.contains('/') {
        return Err(Error::InvalidEntryName(format!(
            "name contains '/': {}",
            name
        )));
    }
    if name.contains('\0') {
        return Err(Error::InvalidEntryName(format!(
            "name contains null byte: {}",
            name
        )));
    }
    if name == "." || name == ".." {
        return Err(Error::InvalidEntryName(format!("reserved name: {}", name)));
    }
    Ok(())
}

/// a single entry in a tree
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    pub kind: EntryKind,
}

impl TreeEntry {
    pub fn new(name: impl Into<String>, kind: EntryKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }

    /// get the type name for error messages
    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }
}

/// kind of tree entry with associated metadata
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    /// regular file
    Regular {
        hash: Hash,
        size: u64,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        sparse_map: Option<Vec<SparseRegion>>,
    },

    /// symbolic link
    Symlink { hash: Hash },

    /// directory
    Directory {
        hash: Hash,
        uid: u32,
        gid: u32,
        mode: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        xattrs: Vec<Xattr>,
    },

    /// block device
    BlockDevice {
        major: u32,
        minor: u32,
        uid: u32,
        gid: u32,
        mode: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        xattrs: Vec<Xattr>,
    },

    /// character device
    CharDevice {
        major: u32,
        minor: u32,
        uid: u32,
        gid: u32,
        mode: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        xattrs: Vec<Xattr>,
    },

    /// named pipe (fifo)
    Fifo {
        uid: u32,
        gid: u32,
        mode: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        xattrs: Vec<Xattr>,
    },

    /// unix socket
    Socket {
        uid: u32,
        gid: u32,
        mode: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        xattrs: Vec<Xattr>,
    },

    /// hardlink to another file in the same tree
    Hardlink {
        /// path relative to tree root
        target_path: String,
    },
}

impl EntryKind {
    /// get the type name for error messages
    pub fn type_name(&self) -> &'static str {
        match self {
            EntryKind::Regular { .. } => "regular",
            EntryKind::Symlink { .. } => "symlink",
            EntryKind::Directory { .. } => "directory",
            EntryKind::BlockDevice { .. } => "block_device",
            EntryKind::CharDevice { .. } => "char_device",
            EntryKind::Fifo { .. } => "fifo",
            EntryKind::Socket { .. } => "socket",
            EntryKind::Hardlink { .. } => "hardlink",
        }
    }

    /// is this a directory entry
    pub fn is_directory(&self) -> bool {
        matches!(self, EntryKind::Directory { .. })
    }

    /// is this a regular file entry
    pub fn is_regular(&self) -> bool {
        matches!(self, EntryKind::Regular { .. })
    }

    /// is this a symlink entry
    pub fn is_symlink(&self) -> bool {
        matches!(self, EntryKind::Symlink { .. })
    }

    /// get the hash if this entry has one (files, symlinks, directories)
    pub fn hash(&self) -> Option<&Hash> {
        match self {
            EntryKind::Regular { hash, .. } => Some(hash),
            EntryKind::Symlink { hash } => Some(hash),
            EntryKind::Directory { hash, .. } => Some(hash),
            _ => None,
        }
    }

    /// create a regular file entry
    pub fn regular(hash: Hash, size: u64) -> Self {
        Self::Regular {
            hash,
            size,
            sparse_map: None,
        }
    }

    /// create a sparse regular file entry
    pub fn sparse(hash: Hash, size: u64, sparse_map: Vec<SparseRegion>) -> Self {
        Self::Regular {
            hash,
            size,
            sparse_map: Some(sparse_map),
        }
    }

    /// create a symlink entry
    pub fn symlink(hash: Hash) -> Self {
        Self::Symlink { hash }
    }

    /// create a directory entry
    pub fn directory(hash: Hash, uid: u32, gid: u32, mode: u32) -> Self {
        Self::Directory {
            hash,
            uid,
            gid,
            mode,
            xattrs: vec![],
        }
    }

    /// create a directory entry with xattrs
    pub fn directory_with_xattrs(
        hash: Hash,
        uid: u32,
        gid: u32,
        mode: u32,
        xattrs: Vec<Xattr>,
    ) -> Self {
        Self::Directory {
            hash,
            uid,
            gid,
            mode,
            xattrs,
        }
    }

    /// create a hardlink entry
    pub fn hardlink(target_path: impl Into<String>) -> Self {
        Self::Hardlink {
            target_path: target_path.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tree_empty() {
        let t = Tree::empty();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_tree_sorting() {
        let entries = vec![
            TreeEntry::new("zebra", EntryKind::regular(Hash::ZERO, 0)),
            TreeEntry::new("alpha", EntryKind::regular(Hash::ZERO, 0)),
            TreeEntry::new("beta", EntryKind::regular(Hash::ZERO, 0)),
        ];
        let tree = Tree::new(entries).unwrap();
        let names: Vec<_> = tree.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "zebra"]);
    }

    #[test]
    fn test_tree_get() {
        let entries = vec![
            TreeEntry::new("alpha", EntryKind::regular(Hash::ZERO, 10)),
            TreeEntry::new("beta", EntryKind::regular(Hash::ZERO, 20)),
        ];
        let tree = Tree::new(entries).unwrap();

        assert!(tree.get("alpha").is_some());
        assert!(tree.get("beta").is_some());
        assert!(tree.get("gamma").is_none());
    }

    #[test]
    fn test_tree_rejects_empty_name() {
        let entries = vec![TreeEntry::new("", EntryKind::regular(Hash::ZERO, 0))];
        assert!(Tree::new(entries).is_err());
    }

    #[test]
    fn test_tree_rejects_slash_in_name() {
        let entries = vec![TreeEntry::new("foo/bar", EntryKind::regular(Hash::ZERO, 0))];
        assert!(Tree::new(entries).is_err());
    }

    #[test]
    fn test_tree_rejects_null_in_name() {
        let entries = vec![TreeEntry::new(
            "foo\0bar",
            EntryKind::regular(Hash::ZERO, 0),
        )];
        assert!(Tree::new(entries).is_err());
    }

    #[test]
    fn test_tree_rejects_dot() {
        let entries = vec![TreeEntry::new(".", EntryKind::regular(Hash::ZERO, 0))];
        assert!(Tree::new(entries).is_err());
    }

    #[test]
    fn test_tree_rejects_dotdot() {
        let entries = vec![TreeEntry::new("..", EntryKind::regular(Hash::ZERO, 0))];
        assert!(Tree::new(entries).is_err());
    }

    #[test]
    fn test_tree_rejects_duplicates() {
        let entries = vec![
            TreeEntry::new("same", EntryKind::regular(Hash::ZERO, 0)),
            TreeEntry::new("same", EntryKind::regular(Hash::ZERO, 0)),
        ];
        assert!(Tree::new(entries).is_err());
    }

    #[test]
    fn test_entry_kind_type_names() {
        assert_eq!(EntryKind::regular(Hash::ZERO, 0).type_name(), "regular");
        assert_eq!(EntryKind::symlink(Hash::ZERO).type_name(), "symlink");
        assert_eq!(
            EntryKind::directory(Hash::ZERO, 0, 0, 0o755).type_name(),
            "directory"
        );
        assert_eq!(EntryKind::hardlink("foo").type_name(), "hardlink");
    }

    #[test]
    fn test_entry_kind_predicates() {
        assert!(EntryKind::directory(Hash::ZERO, 0, 0, 0o755).is_directory());
        assert!(!EntryKind::regular(Hash::ZERO, 0).is_directory());

        assert!(EntryKind::regular(Hash::ZERO, 0).is_regular());
        assert!(!EntryKind::symlink(Hash::ZERO).is_regular());

        assert!(EntryKind::symlink(Hash::ZERO).is_symlink());
        assert!(!EntryKind::regular(Hash::ZERO, 0).is_symlink());
    }

    #[test]
    fn test_entry_kind_hash() {
        let h = Hash::from_hex("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
            .unwrap();

        assert_eq!(EntryKind::regular(h, 0).hash(), Some(&h));
        assert_eq!(EntryKind::symlink(h).hash(), Some(&h));
        assert_eq!(EntryKind::directory(h, 0, 0, 0o755).hash(), Some(&h));

        // these don't have hashes
        assert!(EntryKind::hardlink("foo").hash().is_none());
        assert!(EntryKind::Fifo {
            uid: 0,
            gid: 0,
            mode: 0o644,
            xattrs: vec![]
        }
        .hash()
        .is_none());
    }

    #[test]
    fn test_tree_cbor_roundtrip() {
        let entries = vec![
            TreeEntry::new("file.txt", EntryKind::regular(Hash::ZERO, 100)),
            TreeEntry::new("link", EntryKind::symlink(Hash::ZERO)),
            TreeEntry::new("dir", EntryKind::directory(Hash::ZERO, 1000, 1000, 0o755)),
            TreeEntry::new(
                "dev",
                EntryKind::BlockDevice {
                    major: 8,
                    minor: 0,
                    uid: 0,
                    gid: 6,
                    mode: 0o660,
                    xattrs: vec![],
                },
            ),
            TreeEntry::new("hardlink", EntryKind::hardlink("file.txt")),
        ];

        let tree = Tree::new(entries).unwrap();

        // serialize to cbor
        let mut cbor_bytes = Vec::new();
        ciborium::into_writer(&tree, &mut cbor_bytes).unwrap();

        // deserialize
        let parsed: Tree = ciborium::from_reader(&cbor_bytes[..]).unwrap();

        assert_eq!(tree, parsed);
    }

    #[test]
    fn test_tree_cbor_determinism() {
        // same tree should produce identical cbor bytes
        let entries1 = vec![
            TreeEntry::new("b", EntryKind::regular(Hash::ZERO, 0)),
            TreeEntry::new("a", EntryKind::regular(Hash::ZERO, 0)),
        ];
        let entries2 = vec![
            TreeEntry::new("a", EntryKind::regular(Hash::ZERO, 0)),
            TreeEntry::new("b", EntryKind::regular(Hash::ZERO, 0)),
        ];

        let tree1 = Tree::new(entries1).unwrap();
        let tree2 = Tree::new(entries2).unwrap();

        let mut bytes1 = Vec::new();
        let mut bytes2 = Vec::new();
        ciborium::into_writer(&tree1, &mut bytes1).unwrap();
        ciborium::into_writer(&tree2, &mut bytes2).unwrap();

        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn test_sparse_entry() {
        let regions = vec![SparseRegion::new(0, 100), SparseRegion::new(1000, 200)];
        let kind = EntryKind::sparse(Hash::ZERO, 2000, regions.clone());

        if let EntryKind::Regular {
            sparse_map, size, ..
        } = kind
        {
            assert_eq!(size, 2000);
            assert_eq!(sparse_map, Some(regions));
        } else {
            panic!("expected regular");
        }
    }
}
