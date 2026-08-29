use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoResultExt, Result};
use crate::namespace::NsConfig;

/// repository configuration stored in config.toml
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    /// namespace mapping for this repository
    pub namespace: NsConfig,
    /// configured remotes
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<Remote>,
    /// directories whose `<tree>.<serial>` children protect deployed trees
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gc_roots: Vec<PathBuf>,
}

impl Config {
    /// create a new config with given namespace
    pub fn new(namespace: NsConfig) -> Self {
        Self {
            namespace,
            remotes: vec![],
            gc_roots: vec![],
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
#[path = "config_tests.rs"]
mod tests;
