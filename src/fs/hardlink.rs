use std::collections::HashMap;

/// tracker for detecting hardlinks during commit
///
/// when committing a directory, files with the same (dev, ino) pair
/// are hardlinks to each other. we store the first occurrence's path
/// and emit Hardlink entries for subsequent occurrences.
pub struct HardlinkTracker {
    /// maps (dev, ino) to the first path that referenced this inode
    seen: HashMap<(u64, u64), String>,
}

impl HardlinkTracker {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// check if we've seen this inode before
    ///
    /// if this is the first time seeing (dev, ino), records the path and returns None.
    /// if we've seen it before, returns Some with the original path.
    ///
    /// the path should be relative to the tree root.
    pub fn check(&mut self, dev: u64, ino: u64, path: &str) -> Option<String> {
        let key = (dev, ino);
        if let Some(existing) = self.seen.get(&key) {
            Some(existing.clone())
        } else {
            self.seen.insert(key, path.to_string());
            None
        }
    }

    /// check if we've seen this inode without recording
    pub fn get(&self, dev: u64, ino: u64) -> Option<&str> {
        self.seen.get(&(dev, ino)).map(|s| s.as_str())
    }

    /// number of unique inodes tracked
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// is the tracker empty
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// clear all tracked inodes
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

impl Default for HardlinkTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// tracker for recreating hardlinks during checkout
///
/// during checkout, we may encounter Hardlink entries before
/// their targets have been checked out. this tracks the mapping
/// from logical paths to filesystem paths.
pub struct CheckoutHardlinkTracker {
    /// maps logical path (in tree) to filesystem path
    paths: HashMap<String, std::path::PathBuf>,
}

impl CheckoutHardlinkTracker {
    pub fn new() -> Self {
        Self {
            paths: HashMap::new(),
        }
    }

    /// record that a file at logical path was checked out to fs_path
    pub fn record(&mut self, logical_path: &str, fs_path: std::path::PathBuf) {
        self.paths.insert(logical_path.to_string(), fs_path);
    }

    /// get the filesystem path for a logical path
    pub fn get(&self, logical_path: &str) -> Option<&std::path::Path> {
        self.paths.get(logical_path).map(|p| p.as_path())
    }
}

impl Default for CheckoutHardlinkTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_occurrence() {
        let mut tracker = HardlinkTracker::new();

        // first time seeing this inode
        let result = tracker.check(1, 12345, "path/to/file");
        assert!(result.is_none());
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn test_second_occurrence() {
        let mut tracker = HardlinkTracker::new();

        // first time
        tracker.check(1, 12345, "path/to/first");

        // second time - same inode
        let result = tracker.check(1, 12345, "path/to/second");
        assert_eq!(result, Some("path/to/first".to_string()));
    }

    #[test]
    fn test_different_inodes() {
        let mut tracker = HardlinkTracker::new();

        tracker.check(1, 12345, "file1");
        tracker.check(1, 67890, "file2");

        assert_eq!(tracker.len(), 2);

        // neither should return a hardlink target
        assert!(tracker.get(1, 11111).is_none());
    }

    #[test]
    fn test_same_ino_different_dev() {
        let mut tracker = HardlinkTracker::new();

        // same inode number but different device = different file
        tracker.check(1, 12345, "file1");
        let result = tracker.check(2, 12345, "file2");

        assert!(result.is_none());
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn test_clear() {
        let mut tracker = HardlinkTracker::new();

        tracker.check(1, 12345, "file1");
        assert!(!tracker.is_empty());

        tracker.clear();
        assert!(tracker.is_empty());
    }

    #[test]
    fn test_checkout_tracker() {
        let mut tracker = CheckoutHardlinkTracker::new();

        tracker.record("usr/bin/foo", "/mnt/rootfs/usr/bin/foo".into());

        let path = tracker.get("usr/bin/foo");
        assert_eq!(
            path,
            Some(std::path::Path::new("/mnt/rootfs/usr/bin/foo"))
        );

        assert!(tracker.get("nonexistent").is_none());
    }
}
