use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hash::Hash;

/// a commit object pointing to a tree with metadata
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// root tree hash
    pub tree: Hash,
    /// parent commit hashes (empty for initial, 1 for linear, 2+ for merge)
    pub parents: Vec<Hash>,
    /// author identity
    pub author: String,
    /// unix timestamp (seconds since epoch)
    pub timestamp: i64,
    /// commit message
    pub message: String,
    /// optional key-value metadata (uses BTreeMap for deterministic serialization)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl Commit {
    /// create a new commit
    pub fn new(
        tree: Hash,
        parents: Vec<Hash>,
        author: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            tree,
            parents,
            author: author.into(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            message: message.into(),
            metadata: BTreeMap::new(),
        }
    }

    /// create a new commit with explicit timestamp
    pub fn with_timestamp(
        tree: Hash,
        parents: Vec<Hash>,
        author: impl Into<String>,
        timestamp: i64,
        message: impl Into<String>,
    ) -> Self {
        Self {
            tree,
            parents,
            author: author.into(),
            timestamp,
            message: message.into(),
            metadata: BTreeMap::new(),
        }
    }

    /// add metadata key-value pair
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// is this an initial commit (no parents)
    pub fn is_root(&self) -> bool {
        self.parents.is_empty()
    }

    /// is this a merge commit (multiple parents)
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commit_new() {
        let c = Commit::new(Hash::ZERO, vec![], "author", "message");
        assert_eq!(c.tree, Hash::ZERO);
        assert!(c.parents.is_empty());
        assert_eq!(c.author, "author");
        assert_eq!(c.message, "message");
        assert!(c.is_root());
        assert!(!c.is_merge());
    }

    #[test]
    fn test_commit_with_parents() {
        let parent = Hash::from_hex(
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        )
        .unwrap();
        let c = Commit::new(Hash::ZERO, vec![parent], "author", "message");
        assert!(!c.is_root());
        assert!(!c.is_merge());
    }

    #[test]
    fn test_commit_merge() {
        let p1 =
            Hash::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let p2 =
            Hash::from_hex("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();
        let c = Commit::new(Hash::ZERO, vec![p1, p2], "author", "merge");
        assert!(c.is_merge());
    }

    #[test]
    fn test_commit_with_metadata() {
        let c = Commit::new(Hash::ZERO, vec![], "author", "message")
            .with_metadata("key1", "value1")
            .with_metadata("key2", "value2");
        assert_eq!(c.metadata.get("key1"), Some(&"value1".to_string()));
        assert_eq!(c.metadata.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_commit_cbor_roundtrip() {
        let c = Commit::with_timestamp(Hash::ZERO, vec![], "author", 1234567890, "message")
            .with_metadata("foo", "bar");

        let mut bytes = Vec::new();
        ciborium::into_writer(&c, &mut bytes).unwrap();

        let parsed: Commit = ciborium::from_reader(&bytes[..]).unwrap();
        assert_eq!(c, parsed);
    }

    #[test]
    fn test_commit_cbor_determinism() {
        // metadata insertion order shouldn't affect output (BTreeMap)
        let mut c1 = Commit::with_timestamp(Hash::ZERO, vec![], "a", 0, "m");
        c1.metadata.insert("z".to_string(), "1".to_string());
        c1.metadata.insert("a".to_string(), "2".to_string());

        let mut c2 = Commit::with_timestamp(Hash::ZERO, vec![], "a", 0, "m");
        c2.metadata.insert("a".to_string(), "2".to_string());
        c2.metadata.insert("z".to_string(), "1".to_string());

        let mut bytes1 = Vec::new();
        let mut bytes2 = Vec::new();
        ciborium::into_writer(&c1, &mut bytes1).unwrap();
        ciborium::into_writer(&c2, &mut bytes2).unwrap();

        assert_eq!(bytes1, bytes2);
    }
}
