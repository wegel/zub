use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::types::Xattr;
use crate::Error;

/// SHA-256 hash used for content addressing
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash([u8; 32]);

impl Hash {
    /// zero hash (useful as sentinel)
    pub const ZERO: Hash = Hash([0u8; 32]);

    /// create from raw bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// parse from hex string
    pub fn from_hex(s: &str) -> crate::Result<Self> {
        let bytes = hex::decode(s).map_err(|_| Error::InvalidHashHex(s.to_string()))?;
        if bytes.len() != 32 {
            return Err(Error::InvalidHashHex(s.to_string()));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }

    /// get raw bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// split into path components for object store
    /// returns (first 2 hex chars, remaining 62 hex chars)
    pub fn to_path_components(&self) -> (String, String) {
        let hex = self.to_hex();
        (hex[..2].to_string(), hex[2..].to_string())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", &self.to_hex()[..12])
    }
}

impl Serialize for Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// symlink mode constant for deterministic hashing
pub const SYMLINK_MODE: u32 = 0o120777;

/// compute blob hash over (uid, gid, mode, xattrs, content)
///
/// uid/gid are INSIDE namespace values (logical), NOT on-disk values.
/// xattrs are sorted by name for determinism.
/// format:
///   uid: 4 bytes LE
///   gid: 4 bytes LE
///   mode: 4 bytes LE
///   xattr_count: 4 bytes LE
///   for each xattr (sorted by name):
///     name_len: 4 bytes LE
///     name: bytes
///     value_len: 4 bytes LE
///     value: bytes
///   content: bytes
pub fn compute_blob_hash(
    inside_uid: u32,
    inside_gid: u32,
    mode: u32,
    xattrs: &[Xattr],
    content: &[u8],
) -> Hash {
    let mut hasher = Sha256::new();

    // fixed header
    hasher.update(&inside_uid.to_le_bytes());
    hasher.update(&inside_gid.to_le_bytes());
    hasher.update(&mode.to_le_bytes());

    // xattrs: count + sorted entries
    let mut sorted: Vec<_> = xattrs.iter().collect();
    sorted.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

    hasher.update(&(sorted.len() as u32).to_le_bytes());
    for xattr in sorted {
        hasher.update(&(xattr.name.len() as u32).to_le_bytes());
        hasher.update(xattr.name.as_bytes());
        hasher.update(&(xattr.value.len() as u32).to_le_bytes());
        hasher.update(&xattr.value);
    }

    // content
    hasher.update(content);

    Hash(hasher.finalize().into())
}

/// compute hash for symlink (target is the "content")
/// always uses SYMLINK_MODE for determinism
pub fn compute_symlink_hash(inside_uid: u32, inside_gid: u32, xattrs: &[Xattr], target: &str) -> Hash {
    compute_blob_hash(inside_uid, inside_gid, SYMLINK_MODE, xattrs, target.as_bytes())
}

/// compute hash of compressed bytes (for trees and commits)
#[allow(dead_code)]
pub fn compute_compressed_hash(compressed: &[u8]) -> Hash {
    let digest = Sha256::digest(compressed);
    Hash(digest.into())
}

/// streaming blob hasher for large files
#[allow(dead_code)]
pub struct BlobHasher {
    hasher: Sha256,
}

impl BlobHasher {
    /// create new hasher, writing header and xattrs immediately
    pub fn new(inside_uid: u32, inside_gid: u32, mode: u32, xattrs: &[Xattr]) -> Self {
        let mut hasher = Sha256::new();

        hasher.update(&inside_uid.to_le_bytes());
        hasher.update(&inside_gid.to_le_bytes());
        hasher.update(&mode.to_le_bytes());

        let mut sorted: Vec<_> = xattrs.iter().collect();
        sorted.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

        hasher.update(&(sorted.len() as u32).to_le_bytes());
        for xattr in sorted {
            hasher.update(&(xattr.name.len() as u32).to_le_bytes());
            hasher.update(xattr.name.as_bytes());
            hasher.update(&(xattr.value.len() as u32).to_le_bytes());
            hasher.update(&xattr.value);
        }

        Self { hasher }
    }

    /// feed content bytes
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    /// finalize and return hash
    pub fn finalize(self) -> Hash {
        Hash(self.hasher.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_hex_roundtrip() {
        let original =
            Hash::from_hex("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
                .unwrap();
        let hex = original.to_hex();
        let parsed = Hash::from_hex(&hex).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn test_hash_invalid_hex() {
        assert!(Hash::from_hex("not valid hex").is_err());
        assert!(Hash::from_hex("abcd").is_err()); // too short
        assert!(Hash::from_hex(
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789ff"
        )
        .is_err()); // too long
    }

    #[test]
    fn test_hash_path_components() {
        let h =
            Hash::from_hex("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
                .unwrap();
        let (dir, file) = h.to_path_components();
        assert_eq!(dir, "ab");
        assert_eq!(file, "cdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789");
    }

    #[test]
    fn test_hash_ordering() {
        let h1 =
            Hash::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let h2 =
            Hash::from_hex("0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap();
        assert!(h1 < h2);
    }

    #[test]
    fn test_blob_hash_determinism() {
        let h1 = compute_blob_hash(0, 0, 0o644, &[], b"hello");
        let h2 = compute_blob_hash(0, 0, 0o644, &[], b"hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_blob_hash_different_uid() {
        let h1 = compute_blob_hash(0, 0, 0o644, &[], b"hello");
        let h2 = compute_blob_hash(1, 0, 0o644, &[], b"hello");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_blob_hash_different_gid() {
        let h1 = compute_blob_hash(0, 0, 0o644, &[], b"hello");
        let h2 = compute_blob_hash(0, 1, 0o644, &[], b"hello");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_blob_hash_different_mode() {
        let h1 = compute_blob_hash(0, 0, 0o644, &[], b"hello");
        let h2 = compute_blob_hash(0, 0, 0o755, &[], b"hello");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_blob_hash_different_content() {
        let h1 = compute_blob_hash(0, 0, 0o644, &[], b"hello");
        let h2 = compute_blob_hash(0, 0, 0o644, &[], b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_blob_hash_different_xattr() {
        let x1 = vec![Xattr::new("user.test", vec![1, 2, 3])];
        let x2 = vec![Xattr::new("user.test", vec![4, 5, 6])];

        let h1 = compute_blob_hash(0, 0, 0o644, &x1, b"hello");
        let h2 = compute_blob_hash(0, 0, 0o644, &x2, b"hello");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_blob_hash_xattr_ordering() {
        // different order should produce same hash
        let x1 = vec![
            Xattr::new("user.a", vec![1]),
            Xattr::new("user.b", vec![2]),
        ];
        let x2 = vec![
            Xattr::new("user.b", vec![2]),
            Xattr::new("user.a", vec![1]),
        ];

        let h1 = compute_blob_hash(0, 0, 0o644, &x1, b"hello");
        let h2 = compute_blob_hash(0, 0, 0o644, &x2, b"hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_blob_hash_empty_content() {
        let h = compute_blob_hash(0, 0, 0o644, &[], b"");
        assert_ne!(h, Hash::ZERO);
    }

    #[test]
    fn test_blob_hash_empty_xattr_value() {
        let x = vec![Xattr::new("user.empty", vec![])];
        let h = compute_blob_hash(0, 0, 0o644, &x, b"hello");
        assert_ne!(h, Hash::ZERO);
    }

    #[test]
    fn test_symlink_hash() {
        let h1 = compute_symlink_hash(0, 0, &[], "/target/path");
        let h2 = compute_symlink_hash(0, 0, &[], "/target/path");
        assert_eq!(h1, h2);

        // different target = different hash
        let h3 = compute_symlink_hash(0, 0, &[], "/other/path");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_streaming_hasher() {
        let direct = compute_blob_hash(0, 0, 0o644, &[], b"helloworld");

        let mut streaming = BlobHasher::new(0, 0, 0o644, &[]);
        streaming.update(b"hello");
        streaming.update(b"world");
        let streamed = streaming.finalize();

        assert_eq!(direct, streamed);
    }

    #[test]
    fn test_hash_serde_json() {
        let h =
            Hash::from_hex("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
                .unwrap();
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("abcdef"));
        let parsed: Hash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, parsed);
    }
}
