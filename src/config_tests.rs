use std::path::PathBuf;

use super::*;
use crate::namespace::MapEntry;

#[test]
fn test_config_toml_roundtrip() {
    let config = Config {
        namespace: NsConfig {
            uid_map: vec![MapEntry::new(0, 1000, 1), MapEntry::new(1, 100000, 65536)],
            gid_map: vec![MapEntry::new(0, 1000, 1), MapEntry::new(1, 100000, 65536)],
        },
        remotes: vec![
            Remote::new("origin", "ssh://server/var/zub"),
            Remote::new("backup", "/mnt/backup/zub"),
        ],
        gc_roots: vec![PathBuf::from("/nex/deployments")],
    };

    let toml_str = toml::to_string_pretty(&config).unwrap();
    let parsed: Config = toml::from_str(&toml_str).unwrap();

    assert_eq!(config.namespace.uid_map, parsed.namespace.uid_map);
    assert_eq!(config.namespace.gid_map, parsed.namespace.gid_map);
    assert_eq!(config.remotes, parsed.remotes);
    assert_eq!(config.gc_roots, parsed.gc_roots);
}

#[test]
fn test_config_add_remove_remote() {
    let mut config = Config::default();

    config.add_remote("origin", "ssh://foo/bar").unwrap();
    assert_eq!(config.remotes.len(), 1);
    assert!(config.add_remote("origin", "ssh://other").is_err());

    let remote = config.get_remote("origin").unwrap();
    assert_eq!(remote.url, "ssh://foo/bar");

    config.remove_remote("origin").unwrap();
    assert!(config.remotes.is_empty());
    assert!(config.gc_roots.is_empty());
    assert!(config.remove_remote("origin").is_err());
}

#[test]
fn test_config_minimal_toml() {
    let toml_str = r#"
[namespace]
uid_map = []
gid_map = []
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.namespace.uid_map.is_empty());
    assert!(config.remotes.is_empty());
}
