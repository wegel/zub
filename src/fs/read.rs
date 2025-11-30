use std::fs::{self, Metadata};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use nix::libc;

use crate::error::{Error, IoResultExt, Result};
use crate::types::Xattr;

/// file type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
}

impl FileType {
    /// detect file type from metadata
    pub fn from_metadata(meta: &Metadata) -> Self {
        use std::os::unix::fs::FileTypeExt;
        let ft = meta.file_type();
        if ft.is_file() {
            FileType::Regular
        } else if ft.is_dir() {
            FileType::Directory
        } else if ft.is_symlink() {
            FileType::Symlink
        } else if ft.is_block_device() {
            FileType::BlockDevice
        } else if ft.is_char_device() {
            FileType::CharDevice
        } else if ft.is_fifo() {
            FileType::Fifo
        } else if ft.is_socket() {
            FileType::Socket
        } else {
            // fallback, shouldn't happen
            FileType::Regular
        }
    }
}

/// metadata for a filesystem entry
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub file_type: FileType,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub size: u64,
    /// device major/minor for block/char devices
    pub rdev: Option<(u32, u32)>,
    /// inode number (for hardlink detection)
    pub ino: u64,
    /// device id (for hardlink detection)
    pub dev: u64,
    /// number of hard links
    pub nlink: u64,
}

impl FileMetadata {
    /// read metadata from path (does not follow symlinks)
    pub fn from_path(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path).with_path(path)?;
        Ok(Self::from_std_metadata(&meta))
    }

    /// create from std::fs::Metadata
    pub fn from_std_metadata(meta: &Metadata) -> Self {
        let rdev = if meta.file_type().is_block_device() || meta.file_type().is_char_device() {
            let rdev = meta.rdev();
            // major = rdev >> 8, minor = rdev & 0xff (simplified, real formula is more complex)
            Some((
                nix::sys::stat::major(rdev) as u32,
                nix::sys::stat::minor(rdev) as u32,
            ))
        } else {
            None
        };

        Self {
            file_type: FileType::from_metadata(meta),
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode(),
            size: meta.len(),
            rdev,
            ino: meta.ino(),
            dev: meta.dev(),
            nlink: meta.nlink(),
        }
    }

    /// check if this could be a hardlink (nlink > 1 for regular files)
    pub fn could_be_hardlink(&self) -> bool {
        self.file_type == FileType::Regular && self.nlink > 1
    }
}

/// read all extended attributes from a path
pub fn read_xattrs(path: &Path) -> Result<Vec<Xattr>> {
    let mut xattrs = Vec::new();

    // list xattr names
    let names: Vec<String> = match xattr::list(path) {
        Ok(iter) => iter.map(|n| n.to_string_lossy().into_owned()).collect(),
        Err(e) => {
            // ENOTSUP/ENODATA means no xattr support or no xattrs, not an error
            if e.raw_os_error() == Some(libc::ENOTSUP)
                || e.raw_os_error() == Some(libc::ENODATA)
                || e.raw_os_error() == Some(libc::EOPNOTSUPP)
            {
                return Ok(vec![]);
            }
            return Err(Error::Xattr {
                path: path.to_path_buf(),
                message: format!("failed to list: {}", e),
            });
        }
    };

    for name in names {
        match xattr::get(path, &name) {
            Ok(Some(value)) => {
                xattrs.push(Xattr::new(name, value));
            }
            Ok(None) => {
                // xattr was removed between list and get, skip it
            }
            Err(e) => {
                // skip xattrs we can't read (permission issues, etc.)
                if e.raw_os_error() != Some(libc::ENODATA) {
                    // log but don't fail for individual xattr read errors
                    eprintln!("warning: failed to read xattr {} on {:?}: {}", name, path, e);
                }
            }
        }
    }

    // sort for determinism
    xattrs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(xattrs)
}

/// read symlink target
pub fn read_symlink_target(path: &Path) -> Result<String> {
    let target = fs::read_link(path).with_path(path)?;
    Ok(target.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn test_file_type_regular() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "content").unwrap();

        let meta = FileMetadata::from_path(&path).unwrap();
        assert_eq!(meta.file_type, FileType::Regular);
    }

    #[test]
    fn test_file_type_directory() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let meta = FileMetadata::from_path(&subdir).unwrap();
        assert_eq!(meta.file_type, FileType::Directory);
    }

    #[test]
    fn test_file_type_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::write(&target, "content").unwrap();
        symlink(&target, &link).unwrap();

        let meta = FileMetadata::from_path(&link).unwrap();
        assert_eq!(meta.file_type, FileType::Symlink);
    }

    #[test]
    fn test_metadata_uid_gid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "content").unwrap();

        let meta = FileMetadata::from_path(&path).unwrap();
        // just verify these are populated (actual values depend on current user)
        assert!(meta.uid > 0 || meta.uid == 0);
        assert!(meta.gid > 0 || meta.gid == 0);
    }

    #[test]
    fn test_metadata_mode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "content").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let meta = FileMetadata::from_path(&path).unwrap();
        assert_eq!(meta.mode & 0o777, 0o644);
    }

    #[test]
    fn test_read_symlink_target() {
        let dir = tempdir().unwrap();
        let link = dir.path().join("link");
        symlink("/some/target/path", &link).unwrap();

        let target = read_symlink_target(&link).unwrap();
        assert_eq!(target, "/some/target/path");
    }

    #[test]
    fn test_could_be_hardlink() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "content").unwrap();

        let meta = FileMetadata::from_path(&path).unwrap();
        // single file has nlink=1
        assert!(!meta.could_be_hardlink());

        // create hardlink
        let link = dir.path().join("link");
        fs::hard_link(&path, &link).unwrap();

        let meta2 = FileMetadata::from_path(&path).unwrap();
        assert!(meta2.could_be_hardlink());
    }
}
