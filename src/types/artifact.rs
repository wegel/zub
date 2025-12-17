use serde::{Deserialize, Serialize};

use crate::hash::Hash;

/// a reproducible build artifact - deterministically content-addressed
///
/// the artifact hash is computed from (tree, manifest_hash, output), making it
/// deterministic: same inputs always produce the same artifact hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// root tree hash (the actual build output)
    pub tree: Hash,
    /// SHA256 of manifest content that produced this build
    pub manifest_hash: Hash,
    /// output identifier (e.g., "bundles/dev", "outputs/bin")
    pub output: String,
}

impl Artifact {
    /// create a new artifact
    pub fn new(tree: Hash, manifest_hash: Hash, output: impl Into<String>) -> Self {
        Self {
            tree,
            manifest_hash,
            output: output.into(),
        }
    }

    /// compute the artifact hash - this is DETERMINISTIC
    /// same (tree, manifest_hash, output) = same hash, always
    pub fn compute_hash(&self) -> Hash {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf).expect("cbor serialization failed");
        Hash::from_bytes(*blake3::hash(&buf).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifact_new() {
        let tree = Hash::from_hex(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        let manifest_hash = Hash::from_hex(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap();

        let a = Artifact::new(tree, manifest_hash, "bundles/dev");
        assert_eq!(a.tree, tree);
        assert_eq!(a.manifest_hash, manifest_hash);
        assert_eq!(a.output, "bundles/dev");
    }

    #[test]
    fn test_artifact_hash_deterministic() {
        let tree = Hash::from_hex(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        let manifest_hash = Hash::from_hex(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap();

        let a1 = Artifact::new(tree, manifest_hash, "bundles/dev");
        let a2 = Artifact::new(tree, manifest_hash, "bundles/dev");

        assert_eq!(a1.compute_hash(), a2.compute_hash());
    }

    #[test]
    fn test_artifact_hash_differs_by_output() {
        let tree = Hash::from_hex(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        let manifest_hash = Hash::from_hex(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap();

        let a1 = Artifact::new(tree, manifest_hash, "bundles/dev");
        let a2 = Artifact::new(tree, manifest_hash, "bundles/full");

        assert_ne!(a1.compute_hash(), a2.compute_hash());
    }

    #[test]
    fn test_artifact_cbor_roundtrip() {
        let tree = Hash::from_hex(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        let manifest_hash = Hash::from_hex(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap();

        let a = Artifact::new(tree, manifest_hash, "outputs/bin");

        let mut bytes = Vec::new();
        ciborium::into_writer(&a, &mut bytes).unwrap();

        let parsed: Artifact = ciborium::from_reader(&bytes[..]).unwrap();
        assert_eq!(a, parsed);
    }
}
