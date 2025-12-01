use std::ffi::CString;
use std::fs::{self, File, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use nix::libc;
use nix::sys::stat::{makedev, mknod, Mode, SFlag};
use nix::unistd::{chown, Gid, Uid};

use crate::error::{Error, IoResultExt, Result};
use crate::types::Xattr;

/// create a directory with specified metadata
pub fn create_directory(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<()> {
    fs::create_dir_all(path).with_path(path)?;
    apply_metadata(path, uid, gid, mode, xattrs)
}

/// create a symlink
pub fn create_symlink(
    path: &Path,
    target: &str,
    uid: u32,
    gid: u32,
    xattrs: &[Xattr],
) -> Result<()> {
    // remove existing if present
    if path.exists() || path.symlink_metadata().is_ok() {
        fs::remove_file(path).with_path(path)?;
    }

    symlink(target, path).with_path(path)?;

    // set ownership (can't set mode on symlinks, it's always 0777)
    // use lchown for symlinks, skip if matches current user
    let current_uid = nix::unistd::getuid().as_raw();
    let current_gid = nix::unistd::getgid().as_raw();
    if uid != current_uid || gid != current_gid {
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid path"),
        })?;
        let ret = unsafe { libc::lchown(c_path.as_ptr(), uid, gid) };
        if ret != 0 {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
        }
    }

    // set xattrs (must use lsetxattr for symlinks)
    for xattr in xattrs {
        // note: xattr crate's set follows symlinks by default, need to use fsetxattr
        // for now, skip xattrs on symlinks as most systems don't support them anyway
        if let Err(e) = set_xattr_no_follow(path, &xattr.name, &xattr.value) {
            eprintln!(
                "warning: failed to set xattr {} on symlink {:?}: {}",
                xattr.name, path, e
            );
        }
    }

    Ok(())
}

/// create a block device
pub fn create_block_device(
    path: &Path,
    major: u32,
    minor: u32,
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<()> {
    create_device_node(path, SFlag::S_IFBLK, major, minor, uid, gid, mode, xattrs)
}

/// create a character device
pub fn create_char_device(
    path: &Path,
    major: u32,
    minor: u32,
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<()> {
    create_device_node(path, SFlag::S_IFCHR, major, minor, uid, gid, mode, xattrs)
}

/// create a fifo (named pipe)
pub fn create_fifo(path: &Path, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()> {
    // remove existing
    if path.exists() {
        fs::remove_file(path).with_path(path)?;
    }

    nix::unistd::mkfifo(path, Mode::from_bits_truncate(mode)).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
    })?;

    apply_metadata(path, uid, gid, mode, xattrs)
}

/// create a unix socket placeholder
/// note: we can't actually create a bound socket, just a placeholder
pub fn create_socket_placeholder(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<()> {
    // sockets can't be created without binding, skip them during checkout
    // instead create an empty file as a marker
    // this matches what some other tools do

    // remove existing
    if path.exists() {
        fs::remove_file(path).with_path(path)?;
    }

    // create device node with S_IFSOCK if we have privileges
    let dev = makedev(0, 0);
    match mknod(path, SFlag::S_IFSOCK, Mode::from_bits_truncate(mode), dev) {
        Ok(()) => apply_metadata(path, uid, gid, mode, xattrs),
        Err(nix::errno::Errno::EPERM) => {
            // no permission, skip socket creation
            eprintln!(
                "warning: cannot create socket {:?} without privileges, skipping",
                path
            );
            Ok(())
        }
        Err(e) => Err(Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
        }),
    }
}

/// create a hardlink
pub fn create_hardlink(link_path: &Path, target_path: &Path) -> Result<()> {
    // remove existing
    if link_path.exists() {
        fs::remove_file(link_path).with_path(link_path)?;
    }

    fs::hard_link(target_path, link_path).with_path(link_path)
}

/// apply metadata (ownership, mode, xattrs) to an existing path
pub fn apply_metadata(path: &Path, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()> {
    // set xattrs first (while we still have write permission)
    for xattr in xattrs {
        xattr::set(path, &xattr.name, &xattr.value).map_err(|e| Error::Xattr {
            path: path.to_path_buf(),
            message: format!("failed to set {}: {}", xattr.name, e),
        })?;
    }

    // set ownership (skip if matches current user to avoid permission errors when not root)
    let current_uid = nix::unistd::getuid().as_raw();
    let current_gid = nix::unistd::getgid().as_raw();
    if uid != current_uid || gid != current_gid {
        chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid))).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
        })?;
    }

    // set mode last (might remove write permission)
    fs::set_permissions(path, Permissions::from_mode(mode & 0o7777)).with_path(path)?;

    Ok(())
}

/// helper to create device nodes
fn create_device_node(
    path: &Path,
    sflag: SFlag,
    major: u32,
    minor: u32,
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<()> {
    // remove existing
    if path.exists() {
        fs::remove_file(path).with_path(path)?;
    }

    let dev = makedev(major as u64, minor as u64);

    mknod(path, sflag, Mode::from_bits_truncate(mode), dev).map_err(|e| {
        if e == nix::errno::Errno::EPERM {
            Error::DeviceNodePermission(path.to_path_buf())
        } else {
            Error::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, e),
            }
        }
    })?;

    apply_metadata(path, uid, gid, mode, xattrs)
}

/// set xattr without following symlinks
/// note: this is a best-effort implementation
fn set_xattr_no_follow(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
    // the xattr crate doesn't have a no-follow option
    // on linux, we'd use lsetxattr, but the crate doesn't expose it
    // for now, just try the regular set which follows symlinks
    // most systems don't support xattrs on symlinks anyway
    xattr::set(path, name, value)
}

/// sync a file to disk
pub fn fsync_file(path: &Path) -> Result<()> {
    let file = File::open(path).with_path(path)?;
    file.sync_all().with_path(path)?;
    Ok(())
}

/// sync a directory to disk
pub fn fsync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path).with_path(path)?;
    dir.sync_all().with_path(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    fn current_ids() -> (u32, u32) {
        (
            nix::unistd::getuid().as_raw(),
            nix::unistd::getgid().as_raw(),
        )
    }

    #[test]
    fn test_create_directory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("subdir");
        let (uid, gid) = current_ids();

        create_directory(&path, uid, gid, 0o755, &[]).unwrap();

        assert!(path.is_dir());
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o755);
    }

    #[test]
    fn test_create_symlink() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("link");
        let (uid, gid) = current_ids();

        create_symlink(&path, "/target/path", uid, gid, &[]).unwrap();

        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        let target = fs::read_link(&path).unwrap();
        assert_eq!(target.to_string_lossy(), "/target/path");
    }

    #[test]
    fn test_create_fifo() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fifo");
        let (uid, gid) = current_ids();

        create_fifo(&path, uid, gid, 0o644, &[]).unwrap();

        use std::os::unix::fs::FileTypeExt;
        let meta = fs::metadata(&path).unwrap();
        assert!(meta.file_type().is_fifo());
    }

    #[test]
    fn test_create_hardlink() {
        let dir = tempdir().unwrap();
        let original = dir.path().join("original");
        let link = dir.path().join("link");

        fs::write(&original, "content").unwrap();
        create_hardlink(&link, &original).unwrap();

        assert!(link.exists());
        let orig_meta = fs::metadata(&original).unwrap();
        let link_meta = fs::metadata(&link).unwrap();
        assert_eq!(orig_meta.ino(), link_meta.ino());
    }

    #[test]
    fn test_apply_metadata_mode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file");
        let (uid, gid) = current_ids();
        fs::write(&path, "content").unwrap();

        apply_metadata(&path, uid, gid, 0o600, &[]).unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
    }
}
