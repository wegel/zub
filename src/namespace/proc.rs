use std::path::Path;

use crate::error::{Error, Result};
use crate::namespace::MapEntry;

/// parse /proc/self/uid_map or gid_map format
/// format: "inside_start outside_start count" per line, whitespace separated
pub fn parse_id_map(content: &str) -> Result<Vec<MapEntry>> {
    let mut entries = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue; // skip malformed lines
        }

        let inside_start: u32 = parts[0].parse().map_err(|_| {
            Error::NamespaceParseError(Path::new("/proc/self/uid_map").to_path_buf())
        })?;
        let outside_start: u32 = parts[1].parse().map_err(|_| {
            Error::NamespaceParseError(Path::new("/proc/self/uid_map").to_path_buf())
        })?;
        let count: u32 = parts[2].parse().map_err(|_| {
            Error::NamespaceParseError(Path::new("/proc/self/uid_map").to_path_buf())
        })?;

        entries.push(MapEntry::new(inside_start, outside_start, count));
    }

    Ok(entries)
}

/// read current process uid map from /proc/self/uid_map
pub fn current_uid_map() -> Result<Vec<MapEntry>> {
    let path = Path::new("/proc/self/uid_map");
    let content = std::fs::read_to_string(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    parse_id_map(&content)
}

/// read current process gid map from /proc/self/gid_map
pub fn current_gid_map() -> Result<Vec<MapEntry>> {
    let path = Path::new("/proc/self/gid_map");
    let content = std::fs::read_to_string(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    parse_id_map(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_identity_mapping() {
        // real root outside any namespace
        let content = "         0          0 4294967295\n";
        let entries = parse_id_map(content).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].inside_start, 0);
        assert_eq!(entries[0].outside_start, 0);
        assert_eq!(entries[0].count, 4294967295);
    }

    #[test]
    fn test_parse_podman_mapping() {
        // typical podman unshare mapping
        let content = "         0       1000          1\n         1     100000      65536\n";
        let entries = parse_id_map(content).unwrap();
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].inside_start, 0);
        assert_eq!(entries[0].outside_start, 1000);
        assert_eq!(entries[0].count, 1);

        assert_eq!(entries[1].inside_start, 1);
        assert_eq!(entries[1].outside_start, 100000);
        assert_eq!(entries[1].count, 65536);
    }

    #[test]
    fn test_parse_empty() {
        let entries = parse_id_map("").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_whitespace_only() {
        let entries = parse_id_map("   \n\n  \n").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_multiple_ranges() {
        let content = "0 1000 1\n1 100000 10000\n10001 200000 50000\n";
        let entries = parse_id_map(content).unwrap();
        assert_eq!(entries.len(), 3);
    }
}
