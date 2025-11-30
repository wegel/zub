use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoResultExt, Result};
use crate::namespace::NsConfig;

/// repository configuration stored in config.toml
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// namespace mapping for this repository
    pub namespace: NsConfig,
    /// configured remotes
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<Remote>,
}

impl Config {
    /// create a new config with given namespace
    pub fn new(namespace: NsConfig) -> Self {
        Self {
            namespace,
            remotes: vec![],
        }
    }

    /// load config from file
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).with_path(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// save config to file
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content).with_path(path)?;
        Ok(())
    }

    /// add a remote
    pub fn add_remote(&mut self, name: impl Into<String>, url: impl Into<String>) -> Result<()> {
        let name = name.into();
        if self.remotes.iter().any(|r| r.name == name) {
            return Err(Error::RemoteNotFound(format!(
                "remote '{}' already exists",
                name
            )));
        }
        self.remotes.push(Remote {
            name,
            url: url.into(),
        });
        Ok(())
    }

    /// remove a remote
    pub fn remove_remote(&mut self, name: &str) -> Result<()> {
        let pos = self
            .remotes
            .iter()
            .position(|r| r.name == name)
            .ok_or_else(|| Error::RemoteNotFound(name.to_string()))?;
        self.remotes.remove(pos);
        Ok(())
    }

    /// get remote by name
    pub fn get_remote(&self, name: &str) -> Option<&Remote> {
        self.remotes.iter().find(|r| r.name == name)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            namespace: NsConfig::default(),
            remotes: vec![],
        }
    }
}

/// a configured remote repository
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

impl Remote {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MapEntry;

    #[test]
    fn test_config_toml_roundtrip() {
        let config = Config {
            namespace: NsConfig {
                uid_map: vec![
                    MapEntry::new(0, 1000, 1),
                    MapEntry::new(1, 100000, 65536),
                ],
                gid_map: vec![
                    MapEntry::new(0, 1000, 1),
                    MapEntry::new(1, 100000, 65536),
                ],
            },
            remotes: vec![
                Remote::new("origin", "ssh://server/var/zub"),
                Remote::new("backup", "/mnt/backup/zub"),
            ],
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.namespace.uid_map, parsed.namespace.uid_map);
        assert_eq!(config.namespace.gid_map, parsed.namespace.gid_map);
        assert_eq!(config.remotes, parsed.remotes);
    }

    #[test]
    fn test_config_add_remove_remote() {
        let mut config = Config::default();

        config.add_remote("origin", "ssh://foo/bar").unwrap();
        assert_eq!(config.remotes.len(), 1);

        // duplicate should fail
        assert!(config.add_remote("origin", "ssh://other").is_err());

        // get remote
        let r = config.get_remote("origin").unwrap();
        assert_eq!(r.url, "ssh://foo/bar");

        // remove
        config.remove_remote("origin").unwrap();
        assert!(config.remotes.is_empty());

        // remove non-existent should fail
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
}
