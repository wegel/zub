//! Metadata derived from immutable store objects.

mod elf;

use std::fs::File;
use std::io::Read;

use crate::error::IoResultExt;
use crate::{read_blob, read_named_artifact, write_named_artifact, Artifact, Hash, Repo, Result};

pub use elf::parse_elf;

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
