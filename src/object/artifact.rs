use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::marker::PhantomData;
use std::path::PathBuf;

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::error::{Error, IoResultExt, Result};
use crate::hash::Hash;
use crate::refs::{read_artifact_ref, write_artifact_ref};
use crate::repo::Repo;
use crate::types::Artifact;

/// Write an artifact object and atomically point a semantic key at it.
pub fn write_named_artifact(repo: &Repo, key: &str, artifact: &Artifact) -> Result<Hash> {
    let hash = write_artifact(repo, artifact)?;
    write_artifact_ref(repo, key, &hash)?;
    Ok(hash)
}

/// Read and verify the artifact object named by a semantic key.
pub fn read_named_artifact(repo: &Repo, key: &str) -> Result<Artifact> {
    let hash = read_artifact_ref(repo, key)?;
    read_artifact(repo, &hash)
}

/// write an artifact to the object store
///
/// artifacts are serialized as CBOR. the hash is computed from the serialized
/// content (deterministic: same artifact = same hash).
///
/// note: unlike commits, we don't compress artifacts since they're small and
/// the hash must match Artifact::compute_hash() for verification.
pub fn write_artifact(repo: &Repo, artifact: &Artifact) -> Result<Hash> {
    // compute hash (deterministic)
    let hash = artifact.compute_hash();

    let (dir, file) = hash.to_path_components();
    let artifact_dir = repo.artifacts_path().join(&dir);
    let artifact_path = artifact_dir.join(&file);

    let mut cbor_bytes = Vec::new();
    ciborium::into_writer(artifact, &mut cbor_bytes)?;

    if artifact_path.exists() {
        let existing = fs::read(&artifact_path).with_path(&artifact_path)?;
        if existing == cbor_bytes {
            return Ok(hash);
        }
    }

    // ensure directory exists
    fs::create_dir_all(&artifact_dir).with_path(&artifact_dir)?;

    // atomic write: temp -> fsync -> rename
    let tmp_path = repo.tmp_path().join(uuid::Uuid::new_v4().to_string());
    {
        let mut tmp_file = File::create(&tmp_path).with_path(&tmp_path)?;
        tmp_file.write_all(&cbor_bytes).with_path(&tmp_path)?;
        tmp_file.sync_all().with_path(&tmp_path)?;
    }

    // rename to final location
    fs::rename(&tmp_path, &artifact_path).with_path(&artifact_path)?;

    // fsync parent directory
    let dir_file = File::open(&artifact_dir).with_path(&artifact_dir)?;
    dir_file.sync_all().with_path(&artifact_dir)?;

    Ok(hash)
}

/// read an artifact from the object store
pub fn read_artifact(repo: &Repo, hash: &Hash) -> Result<Artifact> {
    let path = artifact_path(repo, hash);

    let cbor_bytes = fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*hash)
        } else {
            Error::Io {
                path: path.clone(),
                source: e,
            }
        }
    })?;

    let actual_hash = Hash::from_bytes(*blake3::hash(&cbor_bytes).as_bytes());
    if actual_hash != *hash {
        return Err(Error::CorruptObject(*hash));
    }

    // deserialize
    let artifact: Artifact = ciborium::from_reader(&cbor_bytes[..])?;

    Ok(artifact)
}

/// Verify one artifact without retaining its serialized bytes or list entries.
pub(crate) fn verify_artifact(repo: &Repo, hash: &Hash) -> Result<()> {
    const BUFFER_BYTES: usize = 64 * 1024;

    let path = artifact_path(repo, hash);
    let mut file = File::open(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::ObjectNotFound(*hash)
        } else {
            Error::Io {
                path: path.clone(),
                source,
            }
        }
    })?;
    let mut buffer = [0_u8; BUFFER_BYTES];
    let mut hasher = blake3::Hasher::new();
    loop {
        let read = file.read(&mut buffer).with_path(&path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if Hash::from_bytes(*hasher.finalize().as_bytes()) != *hash {
        return Err(Error::CorruptObject(*hash));
    }

    let file = File::open(&path).with_path(&path)?;
    let mut scratch = [0_u8; BUFFER_BYTES];
    let _: CheckedArtifact = ciborium::from_reader_with_buffer(BufReader::new(file), &mut scratch)?;
    Ok(())
}

struct CheckedArtifact;

impl<'de> Deserialize<'de> for CheckedArtifact {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CheckedArtifactVisitor)
    }
}

struct CheckedArtifactVisitor;

impl<'de> Visitor<'de> for CheckedArtifactVisitor {
    type Value = CheckedArtifact;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a typed artifact map")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut kind = None;
        let mut fields = 0_u16;
        while let Some(field) = map.next_key::<ArtifactField>()? {
            let bit = field.bit();
            if bit != 0 && fields & bit != 0 {
                return Err(serde::de::Error::custom(format_args!(
                    "duplicate artifact field {}",
                    field.name()
                )));
            }
            fields |= bit;
            match field {
                ArtifactField::Kind => kind = Some(map.next_value::<ArtifactKind>()?),
                ArtifactField::Schema => {
                    map.next_value::<u32>()?;
                }
                ArtifactField::Blob | ArtifactField::Output | ArtifactField::Interface => {
                    map.next_value::<CheckedHash>()?;
                }
                ArtifactField::Soname => {
                    map.next_value::<Option<CheckedBytes>>()?;
                }
                ArtifactField::Needed | ArtifactField::Rpath | ArtifactField::Runpath => {
                    map.next_value::<DiscardSequence<CheckedBytes>>()?;
                }
                ArtifactField::Imports => {
                    map.next_value::<DiscardSequence<CheckedElfImport>>()?;
                }
                ArtifactField::Exports => {
                    map.next_value::<DiscardSequence<CheckedElfExport>>()?;
                }
                ArtifactField::ElfBlobs => {
                    map.next_value::<DiscardSequence<CheckedHash>>()?;
                }
                ArtifactField::Headers => {
                    map.next_value::<DiscardSequence<CheckedInterfaceHeader>>()?;
                }
                ArtifactField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let kind = kind.ok_or_else(|| serde::de::Error::missing_field("kind"))?;
        let required = match kind {
            ArtifactKind::Elf => 0x01ff,
            ArtifactKind::Interface => 0x1e03,
        };
        if fields & required != required {
            return Err(serde::de::Error::custom(format_args!(
                "artifact {} is missing required fields",
                kind.name()
            )));
        }
        Ok(CheckedArtifact)
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactKind {
    Elf,
    Interface,
}

impl ArtifactKind {
    fn name(self) -> &'static str {
        match self {
            Self::Elf => "elf",
            Self::Interface => "interface",
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ArtifactField {
    Kind,
    Schema,
    Blob,
    Soname,
    Needed,
    Rpath,
    Runpath,
    Imports,
    Exports,
    Output,
    Interface,
    ElfBlobs,
    Headers,
    #[serde(other)]
    Other,
}

impl ArtifactField {
    fn bit(self) -> u16 {
        match self {
            Self::Kind => 1 << 0,
            Self::Schema => 1 << 1,
            Self::Blob => 1 << 2,
            Self::Soname => 1 << 3,
            Self::Needed => 1 << 4,
            Self::Rpath => 1 << 5,
            Self::Runpath => 1 << 6,
            Self::Imports => 1 << 7,
            Self::Exports => 1 << 8,
            Self::Output => 1 << 9,
            Self::Interface => 1 << 10,
            Self::ElfBlobs => 1 << 11,
            Self::Headers => 1 << 12,
            Self::Other => 0,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Schema => "schema",
            Self::Blob => "blob",
            Self::Soname => "soname",
            Self::Needed => "needed",
            Self::Rpath => "rpath",
            Self::Runpath => "runpath",
            Self::Imports => "imports",
            Self::Exports => "exports",
            Self::Output => "output",
            Self::Interface => "interface",
            Self::ElfBlobs => "elf_blobs",
            Self::Headers => "headers",
            Self::Other => "unknown",
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CheckedElfImport {
    library: CheckedBytes,
    name: CheckedBytes,
    version: Option<CheckedBytes>,
    weak: bool,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CheckedElfExport {
    name: CheckedBytes,
    version: Option<CheckedBytes>,
    binding: u8,
    version_hidden: bool,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct CheckedInterfaceHeader {
    path: CheckedText,
    blob: CheckedHash,
}

struct CheckedHash;

impl<'de> Deserialize<'de> for CheckedHash {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(CheckedHashVisitor)
    }
}

struct CheckedHashVisitor;

impl Visitor<'_> for CheckedHashVisitor {
    type Value = CheckedHash;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a 64-character lowercase hexadecimal hash")
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(CheckedHash)
        } else {
            Err(E::custom("invalid artifact hash"))
        }
    }
}

struct CheckedText;

impl<'de> Deserialize<'de> for CheckedText {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(CheckedTextVisitor)
    }
}

struct CheckedTextVisitor;

impl Visitor<'_> for CheckedTextVisitor {
    type Value = CheckedText;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("UTF-8 text")
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(CheckedText)
    }
}

struct CheckedBytes;

impl<'de> Deserialize<'de> for CheckedBytes {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_bytes(CheckedBytesVisitor)
    }
}

struct CheckedBytesVisitor;

impl<'de> Visitor<'de> for CheckedBytesVisitor {
    type Value = CheckedBytes;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a byte sequence")
    }

    fn visit_bytes<E>(self, _value: &[u8]) -> std::result::Result<Self::Value, E> {
        Ok(CheckedBytes)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<u8>()?.is_some() {}
        Ok(CheckedBytes)
    }
}

struct DiscardSequence<T>(PhantomData<T>);

impl<'de, T> Deserialize<'de> for DiscardSequence<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(DiscardSequenceVisitor(PhantomData))
    }
}

struct DiscardSequenceVisitor<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for DiscardSequenceVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = DiscardSequence<T>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a sequence")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<T>()?.is_some() {}
        Ok(DiscardSequence(PhantomData))
    }
}

/// get the filesystem path to an artifact object
pub fn artifact_path(repo: &Repo, hash: &Hash) -> PathBuf {
    let (dir, file) = hash.to_path_components();
    repo.artifacts_path().join(dir).join(file)
}

/// check if an artifact exists in the object store
pub fn artifact_exists(repo: &Repo, hash: &Hash) -> bool {
    artifact_path(repo, hash).exists()
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
