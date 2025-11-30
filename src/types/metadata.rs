use serde::{Deserialize, Serialize};

/// extended attribute (name + value)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Xattr {
    pub name: String,
    pub value: Vec<u8>,
}

impl Xattr {
    pub fn new(name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// a data region in a sparse file
/// holes are implicit (gaps between regions)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SparseRegion {
    /// offset where this data region starts in the logical file
    pub offset: u64,
    /// length of this data region
    pub length: u64,
}

impl SparseRegion {
    pub fn new(offset: u64, length: u64) -> Self {
        Self { offset, length }
    }

    /// end offset of this region (exclusive)
    pub fn end(&self) -> u64 {
        self.offset + self.length
    }
}

/// diff entry change kind
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    MetadataOnly,
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeKind::Added => write!(f, "A"),
            ChangeKind::Modified => write!(f, "M"),
            ChangeKind::Deleted => write!(f, "D"),
            ChangeKind::MetadataOnly => write!(f, "m"),
        }
    }
}

/// entry in a diff result
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: String,
    pub kind: ChangeKind,
}

impl std::fmt::Display for DiffEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.kind, self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xattr_creation() {
        let x = Xattr::new("security.selinux", vec![1, 2, 3]);
        assert_eq!(x.name, "security.selinux");
        assert_eq!(x.value, vec![1, 2, 3]);
    }

    #[test]
    fn test_sparse_region() {
        let r = SparseRegion::new(100, 50);
        assert_eq!(r.offset, 100);
        assert_eq!(r.length, 50);
        assert_eq!(r.end(), 150);
    }

    #[test]
    fn test_change_kind_display() {
        assert_eq!(format!("{}", ChangeKind::Added), "A");
        assert_eq!(format!("{}", ChangeKind::Modified), "M");
        assert_eq!(format!("{}", ChangeKind::Deleted), "D");
        assert_eq!(format!("{}", ChangeKind::MetadataOnly), "m");
    }
}
