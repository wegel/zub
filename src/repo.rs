use std::fs::File;
use std::path::{Path, PathBuf};

use nix::fcntl::{Flock, FlockArg};

use crate::config::Config;
use crate::error::{Error, IoResultExt, Result};
use crate::namespace::{current_gid_map, current_uid_map, NsConfig};

/// a zub repository
pub struct Repo {
    path: PathBuf,
    config: Config,
}

impl Repo {
    /// initialize a new repository at the given path
    pub fn init(path: &Path) -> Result<Self> {
        let config_path = path.join("config.toml");
        if config_path.exists() {
            return Err(Error::RepoExists(path.to_path_buf()));
        }

        // create directory structure
        std::fs::create_dir_all(path.join("objects/blobs")).with_path(path)?;
        std::fs::create_dir_all(path.join("objects/trees")).with_path(path)?;
        std::fs::create_dir_all(path.join("objects/commits")).with_path(path)?;
        std::fs::create_dir_all(path.join("refs/heads")).with_path(path)?;
        std::fs::create_dir_all(path.join("refs/tags")).with_path(path)?;
        std::fs::create_dir_all(path.join("tmp")).with_path(path)?;

        // capture current namespace mapping
        let uid_map = current_uid_map()?;
        let gid_map = current_gid_map()?;

        let config = Config::new(NsConfig { uid_map, gid_map });
        config.save(&config_path)?;

        Ok(Self {
            path: path.to_path_buf(),
            config,
        })
    }

    /// open an existing repository
    pub fn open(path: &Path) -> Result<Self> {
        let config_path = path.join("config.toml");
        if !config_path.exists() {
            return Err(Error::NoRepo(path.to_path_buf()));
        }

        let config = Config::load(&config_path)?;

        Ok(Self {
            path: path.to_path_buf(),
            config,
        })
    }

    /// repository root path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// repository configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// mutable access to configuration
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// save configuration changes
    pub fn save_config(&self) -> Result<()> {
        self.config.save(&self.config_path())
    }

    /// path to config.toml
    pub fn config_path(&self) -> PathBuf {
        self.path.join("config.toml")
    }

    /// path to objects directory
    pub fn objects_path(&self) -> PathBuf {
        self.path.join("objects")
    }

    /// path to blobs directory
    pub fn blobs_path(&self) -> PathBuf {
        self.objects_path().join("blobs")
    }

    /// path to trees directory
    pub fn trees_path(&self) -> PathBuf {
        self.objects_path().join("trees")
    }

    /// path to commits directory
    pub fn commits_path(&self) -> PathBuf {
        self.objects_path().join("commits")
    }

    /// path to refs directory
    pub fn refs_path(&self) -> PathBuf {
        self.path.join("refs/heads")
    }

    /// path to tags directory
    pub fn tags_path(&self) -> PathBuf {
        self.path.join("refs/tags")
    }

    /// path to tmp directory (for atomic writes)
    pub fn tmp_path(&self) -> PathBuf {
        self.path.join("tmp")
    }

    /// path to lock file
    pub fn lock_path(&self) -> PathBuf {
        self.path.join(".lock")
    }

    /// acquire exclusive lock on repository
    /// returns a guard that releases the lock on drop
    pub fn lock(&self) -> Result<RepoLock> {
        let lock_path = self.lock_path();
        let file = File::create(&lock_path).with_path(&lock_path)?;

        let flock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|_| Error::LockContention)?;

        Ok(RepoLock { flock })
    }

    /// try to acquire exclusive lock, returning None if already locked
    pub fn try_lock(&self) -> Result<Option<RepoLock>> {
        let lock_path = self.lock_path();
        let file = File::create(&lock_path).with_path(&lock_path)?;

        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(flock) => Ok(Some(RepoLock { flock })),
            Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
            Err(_) => Err(Error::LockContention),
        }
    }
}

/// guard that holds repository lock until dropped
pub struct RepoLock {
    #[allow(dead_code)]
    flock: Flock<File>,
}
// lock is released automatically when Flock is dropped

/// helper to run a function while holding the repository lock
#[allow(dead_code)]
pub fn with_lock<T, F>(repo: &Repo, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let _lock = repo.lock()?;
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_repo_init() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("test-repo");

        let repo = Repo::init(&repo_path).unwrap();

        // verify structure
        assert!(repo_path.join("objects/blobs").is_dir());
        assert!(repo_path.join("objects/trees").is_dir());
        assert!(repo_path.join("objects/commits").is_dir());
        assert!(repo_path.join("refs/heads").is_dir());
        assert!(repo_path.join("refs/tags").is_dir());
        assert!(repo_path.join("tmp").is_dir());
        assert!(repo_path.join("config.toml").is_file());

        // verify namespace was captured
        assert!(!repo.config().namespace.uid_map.is_empty());
    }

    #[test]
    fn test_repo_init_already_exists() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("test-repo");

        Repo::init(&repo_path).unwrap();
        let result = Repo::init(&repo_path);

        assert!(matches!(result, Err(Error::RepoExists(_))));
    }

    #[test]
    fn test_repo_open() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("test-repo");

        Repo::init(&repo_path).unwrap();
        let repo = Repo::open(&repo_path).unwrap();

        assert_eq!(repo.path(), repo_path);
    }

    #[test]
    fn test_repo_open_not_found() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("nonexistent");

        let result = Repo::open(&repo_path);
        assert!(matches!(result, Err(Error::NoRepo(_))));
    }

    #[test]
    fn test_repo_paths() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("test-repo");
        let repo = Repo::init(&repo_path).unwrap();

        assert_eq!(repo.blobs_path(), repo_path.join("objects/blobs"));
        assert_eq!(repo.trees_path(), repo_path.join("objects/trees"));
        assert_eq!(repo.commits_path(), repo_path.join("objects/commits"));
        assert_eq!(repo.refs_path(), repo_path.join("refs/heads"));
        assert_eq!(repo.tmp_path(), repo_path.join("tmp"));
    }

    #[test]
    fn test_repo_lock() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("test-repo");
        let repo = Repo::init(&repo_path).unwrap();

        // acquire lock
        let lock = repo.lock().unwrap();

        // try to acquire again should fail
        let result = repo.try_lock().unwrap();
        assert!(result.is_none());

        // drop lock
        drop(lock);

        // now should succeed
        let lock2 = repo.try_lock().unwrap();
        assert!(lock2.is_some());
    }

    #[test]
    fn test_config_modification() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("test-repo");
        let mut repo = Repo::init(&repo_path).unwrap();

        repo.config_mut()
            .add_remote("origin", "ssh://server/repo")
            .unwrap();
        repo.save_config().unwrap();

        // reopen and verify
        let repo2 = Repo::open(&repo_path).unwrap();
        assert_eq!(repo2.config().remotes.len(), 1);
        assert_eq!(repo2.config().remotes[0].name, "origin");
    }
}
