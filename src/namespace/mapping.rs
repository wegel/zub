use serde::{Deserialize, Serialize};

/// a single range in a uid/gid mapping
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapEntry {
    /// start of range inside namespace (logical value)
    pub inside_start: u32,
    /// start of range outside namespace (on-disk value)
    pub outside_start: u32,
    /// number of ids in this range
    pub count: u32,
}

impl MapEntry {
    pub fn new(inside_start: u32, outside_start: u32, count: u32) -> Self {
        Self {
            inside_start,
            outside_start,
            count,
        }
    }

    /// identity mapping for a single id
    pub fn identity_single(id: u32) -> Self {
        Self {
            inside_start: id,
            outside_start: id,
            count: 1,
        }
    }

    /// check if an inside id falls within this range
    pub fn contains_inside(&self, id: u32) -> bool {
        id >= self.inside_start && id < self.inside_start.saturating_add(self.count)
    }

    /// check if an outside id falls within this range
    pub fn contains_outside(&self, id: u32) -> bool {
        id >= self.outside_start && id < self.outside_start.saturating_add(self.count)
    }
}

/// namespace configuration with uid and gid mappings
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NsConfig {
    pub uid_map: Vec<MapEntry>,
    pub gid_map: Vec<MapEntry>,
}

impl NsConfig {
    /// create identity mapping (outside == inside for all ids)
    /// this is what you get when running as real root outside any namespace
    pub fn identity() -> Self {
        Self {
            uid_map: vec![MapEntry::new(0, 0, u32::MAX)],
            gid_map: vec![MapEntry::new(0, 0, u32::MAX)],
        }
    }

    /// check if this is an identity mapping
    pub fn is_identity(&self) -> bool {
        self.uid_map.len() == 1
            && self.uid_map[0].inside_start == 0
            && self.uid_map[0].outside_start == 0
            && self.uid_map[0].count == u32::MAX
            && self.gid_map.len() == 1
            && self.gid_map[0].inside_start == 0
            && self.gid_map[0].outside_start == 0
            && self.gid_map[0].count == u32::MAX
    }
}

/// convert outside (on-disk) id to inside (logical namespace) id
pub fn outside_to_inside(outside: u32, map: &[MapEntry]) -> Option<u32> {
    for entry in map {
        if entry.contains_outside(outside) {
            return Some(entry.inside_start + (outside - entry.outside_start));
        }
    }
    None
}

/// convert inside (logical namespace) id to outside (on-disk) id
pub fn inside_to_outside(inside: u32, map: &[MapEntry]) -> Option<u32> {
    for entry in map {
        if entry.contains_inside(inside) {
            return Some(entry.outside_start + (inside - entry.inside_start));
        }
    }
    None
}

/// remap an id from one namespace to another
/// old_outside -> inside (via old_map) -> new_outside (via new_map)
pub fn remap(old_outside: u32, old_map: &[MapEntry], new_map: &[MapEntry]) -> Option<u32> {
    let inside = outside_to_inside(old_outside, old_map)?;
    inside_to_outside(inside, new_map)
}

/// check if two namespace configs are equivalent (same mappings)
pub fn mappings_equal(a: &NsConfig, b: &NsConfig) -> bool {
    a.uid_map == b.uid_map && a.gid_map == b.gid_map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_entry_contains() {
        let entry = MapEntry::new(1, 100000, 65536);

        // inside range
        assert!(entry.contains_inside(1));
        assert!(entry.contains_inside(100));
        assert!(entry.contains_inside(65536));
        assert!(!entry.contains_inside(0));
        assert!(!entry.contains_inside(65537));

        // outside range
        assert!(entry.contains_outside(100000));
        assert!(entry.contains_outside(100100));
        assert!(entry.contains_outside(165535));
        assert!(!entry.contains_outside(99999));
        assert!(!entry.contains_outside(165536));
    }

    #[test]
    fn test_outside_to_inside_simple() {
        // typical podman unshare mapping:
        // 0 -> 1000 (root inside maps to user outside)
        // 1-65536 -> 100000-165535
        let map = vec![
            MapEntry::new(0, 1000, 1),
            MapEntry::new(1, 100000, 65536),
        ];

        // root
        assert_eq!(outside_to_inside(1000, &map), Some(0));

        // regular user
        assert_eq!(outside_to_inside(100000, &map), Some(1));
        assert_eq!(outside_to_inside(100189, &map), Some(190));

        // unmapped
        assert_eq!(outside_to_inside(999, &map), None);
        assert_eq!(outside_to_inside(200000, &map), None);
    }

    #[test]
    fn test_inside_to_outside_simple() {
        let map = vec![
            MapEntry::new(0, 1000, 1),
            MapEntry::new(1, 100000, 65536),
        ];

        assert_eq!(inside_to_outside(0, &map), Some(1000));
        assert_eq!(inside_to_outside(1, &map), Some(100000));
        assert_eq!(inside_to_outside(190, &map), Some(100189));

        // unmapped (beyond range)
        assert_eq!(inside_to_outside(70000, &map), None);
    }

    #[test]
    fn test_remap_between_namespaces() {
        // machine A: 0->1000, 1-65536->100000-165535
        let map_a = vec![
            MapEntry::new(0, 1000, 1),
            MapEntry::new(1, 100000, 65536),
        ];

        // machine B: 0->2000, 1-65536->200000-265535
        let map_b = vec![
            MapEntry::new(0, 2000, 1),
            MapEntry::new(1, 200000, 65536),
        ];

        // file owned by uid 100189 on machine A (inside uid 190)
        // should become uid 200189 on machine B
        assert_eq!(remap(100189, &map_a, &map_b), Some(200189));

        // root file: 1000 on A -> 2000 on B
        assert_eq!(remap(1000, &map_a, &map_b), Some(2000));
    }

    #[test]
    fn test_identity_mapping() {
        let id = NsConfig::identity();
        assert!(id.is_identity());

        // everything maps to itself
        assert_eq!(outside_to_inside(0, &id.uid_map), Some(0));
        assert_eq!(outside_to_inside(1000, &id.uid_map), Some(1000));
        assert_eq!(outside_to_inside(u32::MAX - 1, &id.uid_map), Some(u32::MAX - 1));

        assert_eq!(inside_to_outside(0, &id.uid_map), Some(0));
        assert_eq!(inside_to_outside(65535, &id.uid_map), Some(65535));
    }

    #[test]
    fn test_non_identity() {
        let ns = NsConfig {
            uid_map: vec![MapEntry::new(0, 1000, 1)],
            gid_map: vec![MapEntry::new(0, 1000, 1)],
        };
        assert!(!ns.is_identity());
    }

    #[test]
    fn test_mappings_equal() {
        let a = NsConfig {
            uid_map: vec![MapEntry::new(0, 1000, 1)],
            gid_map: vec![MapEntry::new(0, 1000, 1)],
        };
        let b = NsConfig {
            uid_map: vec![MapEntry::new(0, 1000, 1)],
            gid_map: vec![MapEntry::new(0, 1000, 1)],
        };
        let c = NsConfig {
            uid_map: vec![MapEntry::new(0, 2000, 1)],
            gid_map: vec![MapEntry::new(0, 1000, 1)],
        };

        assert!(mappings_equal(&a, &b));
        assert!(!mappings_equal(&a, &c));
    }

    #[test]
    fn test_overflow_safety() {
        // ensure we don't panic on edge cases
        let entry = MapEntry::new(u32::MAX - 10, 0, 20);
        assert!(entry.contains_inside(u32::MAX - 10));
        // saturating_add caps at MAX, so range becomes [MAX-10, MAX) - exclusive upper bound
        // so MAX-1 is included but MAX is not (it would require checking < MAX which is true for MAX-1)
        assert!(entry.contains_inside(u32::MAX - 1));
        // MAX is NOT in the range because saturating_add(MAX-10, 20) = MAX, and MAX < MAX is false
        assert!(!entry.contains_inside(u32::MAX));

        let entry2 = MapEntry::new(0, u32::MAX - 10, 20);
        assert!(entry2.contains_outside(u32::MAX - 1));
        assert!(!entry2.contains_outside(u32::MAX));
    }
}
