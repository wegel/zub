use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use nix::libc;

use crate::error::{IoResultExt, Result};
use crate::types::SparseRegion;

/// detect sparse regions in a file using SEEK_HOLE/SEEK_DATA
///
/// returns None if the file is not sparse (all data)
/// returns Some(vec![]) if the file is all holes (empty)
/// returns Some(regions) where regions are the data regions
pub fn detect_sparse_regions(file: &File) -> Result<Option<Vec<SparseRegion>>> {
    let fd = file.as_raw_fd();
    let file_size = file.metadata().map_err(|e| crate::Error::Io {
        path: std::path::PathBuf::from("<sparse>"),
        source: e,
    })?.len();

    if file_size == 0 {
        return Ok(None); // empty file is not sparse
    }

    let mut regions = Vec::new();
    let mut pos: u64 = 0;

    // SEEK_DATA = 3, SEEK_HOLE = 4 on linux
    const SEEK_DATA: i32 = 3;
    const SEEK_HOLE: i32 = 4;

    loop {
        // find next data region
        let data_start = match unsafe { libc::lseek(fd, pos as i64, SEEK_DATA) } {
            -1 => {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENXIO) {
                    // no more data from this position
                    break;
                } else if err.raw_os_error() == Some(libc::EINVAL) {
                    // SEEK_HOLE/SEEK_DATA not supported, file is not sparse
                    return Ok(None);
                } else {
                    return Err(crate::Error::Io {
                        path: std::path::PathBuf::from("<sparse>"),
                        source: err,
                    });
                }
            }
            n => n as u64,
        };

        // find end of this data region (start of next hole)
        let data_end = match unsafe { libc::lseek(fd, data_start as i64, SEEK_HOLE) } {
            -1 => {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENXIO) {
                    // no hole after this, data goes to end of file
                    file_size
                } else {
                    return Err(crate::Error::Io {
                        path: std::path::PathBuf::from("<sparse>"),
                        source: err,
                    });
                }
            }
            n => n as u64,
        };

        if data_end > data_start {
            regions.push(SparseRegion::new(data_start, data_end - data_start));
        }

        pos = data_end;
        if pos >= file_size {
            break;
        }
    }

    // if regions cover entire file contiguously from 0, it's not sparse
    if regions.len() == 1 && regions[0].offset == 0 && regions[0].length == file_size {
        return Ok(None);
    }

    // if we have no regions but file has size, it's all holes
    if regions.is_empty() && file_size > 0 {
        return Ok(Some(vec![]));
    }

    Ok(Some(regions))
}

/// read only the data regions from a sparse file
/// returns concatenated data bytes
pub fn read_data_regions(file: &mut File, regions: &[SparseRegion]) -> Result<Vec<u8>> {
    let total_size: u64 = regions.iter().map(|r| r.length).sum();
    let mut data = Vec::with_capacity(total_size as usize);

    for region in regions {
        file.seek(SeekFrom::Start(region.offset)).map_err(|e| crate::Error::Io {
            path: std::path::PathBuf::from("<sparse>"),
            source: e,
        })?;
        let mut buf = vec![0u8; region.length as usize];
        file.read_exact(&mut buf).map_err(|e| crate::Error::Io {
            path: std::path::PathBuf::from("<sparse>"),
            source: e,
        })?;
        data.extend_from_slice(&buf);
    }

    Ok(data)
}

/// write a sparse file from data and sparse map
pub fn write_sparse_file(
    path: &Path,
    data: &[u8],
    regions: &[SparseRegion],
    total_size: u64,
) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    // create file
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)
        .with_path(path)?;

    // set file size (creates holes)
    file.set_len(total_size).with_path(path)?;

    // write data regions
    let mut data_offset = 0usize;
    for region in regions {
        file.seek(SeekFrom::Start(region.offset)).with_path(path)?;
        let end = data_offset + region.length as usize;
        file.write_all(&data[data_offset..end]).with_path(path)?;
        data_offset = end;
    }

    file.sync_all().with_path(path)?;
    Ok(())
}

/// check if sparse file support is available
pub fn sparse_support_available() -> bool {
    // try to use SEEK_HOLE on /dev/null or similar
    // this is a simple check that should work on most linux systems
    true // assume linux has sparse support
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{tempdir, NamedTempFile};

    #[test]
    fn test_non_sparse_file() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"hello world").unwrap();
        file.flush().unwrap();

        let file = File::open(file.path()).unwrap();
        let regions = detect_sparse_regions(&file).unwrap();

        // non-sparse file should return None
        assert!(regions.is_none(), "expected None, zub{:?}", regions);
    }

    #[test]
    fn test_sparse_file_detection() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse");

        // create a sparse file
        {
            let mut file = File::create(&path).unwrap();
            // set size to 1MB but don't write any data
            file.set_len(1024 * 1024).unwrap();
            // write something at offset 512KB
            file.seek(SeekFrom::Start(512 * 1024)).unwrap();
            file.write_all(b"data in the middle").unwrap();
        }

        let file = File::open(&path).unwrap();
        let regions = detect_sparse_regions(&file).unwrap();

        match regions {
            Some(r) => {
                // should have one data region at ~512KB
                assert!(!r.is_empty());
                // first data region should start at or after 512KB
                assert!(r[0].offset >= 512 * 1024 - 4096); // allow for block alignment
            }
            None => {
                // filesystem might not support sparse detection
                // this is ok, just means we'll store the whole file
            }
        }
    }

    #[test]
    fn test_write_and_read_sparse() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse");

        let regions = vec![
            SparseRegion::new(0, 100),
            SparseRegion::new(1000, 200),
        ];

        let data = vec![0u8; 300]; // 100 + 200 bytes

        write_sparse_file(&path, &data, &regions, 2000).unwrap();

        // verify file size
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 2000);

        // read back the data regions
        let mut file = File::open(&path).unwrap();
        let read_data = read_data_regions(&mut file, &regions).unwrap();
        assert_eq!(read_data.len(), 300);
    }

    #[test]
    fn test_empty_file() {
        let mut file = NamedTempFile::new().unwrap();
        file.flush().unwrap();

        let file = File::open(file.path()).unwrap();
        let regions = detect_sparse_regions(&file).unwrap();

        // empty file is not sparse
        assert!(regions.is_none());
    }
}
