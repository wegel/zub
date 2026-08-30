//! Metadata derived from immutable store objects.

mod elf;

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::IoResultExt;
use crate::{
    artifact_path, read_artifact_ref, read_blob, read_named_artifact, write_named_artifact,
    Artifact, Hash, Repo, Result,
};

pub use elf::parse_elf;

const ELF_INDEX_SCHEMA: u32 = 1;

pub(crate) fn ensure_elf(repo: &Repo, blob: Hash) -> Result<bool> {
    let key = format!("elf/{blob}");
    if matches!(
        read_named_artifact(repo, &key),
        Ok(Artifact::Elf(artifact)) if artifact.blob == blob
    ) {
        return Ok(true);
    }
    let path = crate::object::blob_path(repo, &blob);
    let mut file = File::open(&path).with_path(&path)?;
    let mut magic = [0; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(error) => return Err(error).with_path(&path),
    }
    if magic != *b"\x7fELF" {
        return Ok(false);
    }
    let bytes = read_blob(repo, &blob)?;
    if let Some(artifact) = parse_elf(blob, &bytes)? {
        write_named_artifact(repo, &key, &Artifact::Elf(artifact))?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) fn ensure_elf_set(repo: &Repo, blobs: impl IntoIterator<Item = Hash>) -> Result<()> {
    let path = elf_index_path(repo);
    let mut index = read_elf_index(&path);
    let mut changed = false;
    for blob in blobs {
        if index
            .records
            .get(&blob)
            .is_some_and(|record| record.matches(repo, blob))
        {
            continue;
        }
        if ensure_elf(repo, blob)? {
            index
                .records
                .insert(blob, ElfIndexRecord::read(repo, blob)?);
        } else {
            index.records.remove(&blob);
        }
        changed = true;
    }
    if changed {
        let _ = write_elf_index(repo, &path, &index);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct ElfIndex {
    schema: u32,
    records: BTreeMap<Hash, ElfIndexRecord>,
}

impl Default for ElfIndex {
    fn default() -> Self {
        Self {
            schema: ELF_INDEX_SCHEMA,
            records: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ElfIndexRecord {
    artifact: Hash,
    reference: FileFingerprint,
    object: FileFingerprint,
}

impl ElfIndexRecord {
    fn read(repo: &Repo, blob: Hash) -> Result<Self> {
        let key = format!("elf/{blob}");
        let reference_path = repo.artifact_refs_path().join(&key);
        let artifact = read_artifact_ref(repo, &key)?;
        let object_path = artifact_path(repo, &artifact);
        Ok(Self {
            artifact,
            reference: FileFingerprint::read(&reference_path)?,
            object: FileFingerprint::read(&object_path)?,
        })
    }

    fn matches(&self, repo: &Repo, blob: Hash) -> bool {
        let reference_path = repo.artifact_refs_path().join(format!("elf/{blob}"));
        let object_path = artifact_path(repo, &self.artifact);
        self.reference.matches(&reference_path) && self.object.matches(&object_path)
    }
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
struct FileFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    uid: u32,
    gid: u32,
    mode: u32,
}

impl FileFingerprint {
    fn read(path: &Path) -> Result<Self> {
        Ok(Self::from_metadata(&fs::metadata(path).with_path(path)?))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        fs::metadata(path)
            .map(|metadata| *self == Self::from_metadata(&metadata))
            .unwrap_or(false)
    }
}

fn elf_index_path(repo: &Repo) -> std::path::PathBuf {
    repo.index_path().join("elf-artifacts-v1.cbor")
}

fn read_elf_index(path: &Path) -> ElfIndex {
    let Ok(bytes) = fs::read(path) else {
        return ElfIndex::default();
    };
    let Ok(index) = ciborium::from_reader::<ElfIndex, _>(bytes.as_slice()) else {
        return ElfIndex::default();
    };
    if index.schema != ELF_INDEX_SCHEMA {
        return ElfIndex::default();
    }
    index
}

fn write_elf_index(repo: &Repo, path: &Path, index: &ElfIndex) -> Result<()> {
    fs::create_dir_all(repo.index_path()).with_path(repo.index_path())?;
    let temporary = repo
        .tmp_path()
        .join(format!("elf-artifacts-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut bytes = Vec::new();
        ciborium::into_writer(index, &mut bytes)?;
        let mut file = File::create(&temporary).with_path(&temporary)?;
        file.write_all(&bytes).with_path(&temporary)?;
        file.flush().with_path(&temporary)?;
        drop(file);
        fs::rename(&temporary, path).with_path(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
