//! Typed metadata derived from immutable store objects.

use serde::{Deserialize, Serialize};

use crate::Hash;

/// Current schema for every typed artifact.
pub const ARTIFACT_SCHEMA: u32 = 1;

/// Metadata that Zub can recreate from immutable objects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Artifact {
    /// Dynamic-linking facts from one ELF blob.
    Elf(ElfArtifact),
    /// A package output's exported interface and development headers.
    Interface(InterfaceArtifact),
}

impl Artifact {
    /// Compute the content hash of the canonical CBOR representation.
    pub fn compute_hash(&self) -> Hash {
        let mut bytes = Vec::new();
        ciborium::into_writer(self, &mut bytes).expect("artifact serialization failed");
        Hash::from_bytes(*blake3::hash(&bytes).as_bytes())
    }
}

/// Dynamic-linking facts derived from one ELF file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ElfArtifact {
    /// Artifact schema version.
    pub schema: u32,
    /// Blob whose bytes supplied these facts.
    pub blob: Hash,
    /// Dynamic object name from `DT_SONAME`.
    pub soname: Option<Vec<u8>>,
    /// Libraries requested by `DT_NEEDED`, in loader order.
    pub needed: Vec<Vec<u8>>,
    /// Search paths requested by `DT_RPATH`, in table order.
    pub rpath: Vec<Vec<u8>>,
    /// Search paths requested by `DT_RUNPATH`, in table order.
    pub runpath: Vec<Vec<u8>>,
    /// Undefined dynamic symbols, sorted by their complete identity.
    pub imports: Vec<ElfImport>,
    /// Defined dynamic symbols, sorted by their complete identity.
    pub exports: Vec<ElfExport>,
}

/// One undefined dynamic symbol.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ElfImport {
    /// Library named by the GNU version requirement, when present.
    pub library: Vec<u8>,
    /// Symbol name.
    pub name: Vec<u8>,
    /// GNU symbol version.
    pub version: Option<Vec<u8>>,
    /// Whether the symbol has weak binding.
    pub weak: bool,
}

/// One defined dynamic symbol.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ElfExport {
    /// Symbol name.
    pub name: Vec<u8>,
    /// GNU symbol version.
    pub version: Option<Vec<u8>>,
    /// ELF `STB_*` binding value.
    pub binding: u8,
    /// Whether the GNU version is hidden.
    pub version_hidden: bool,
}

/// One development header contributing to an interface hash.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct InterfaceHeader {
    /// Absolute package path, such as `/usr/include/example.h`.
    pub path: String,
    /// Blob containing the header bytes.
    pub blob: Hash,
}

/// Recomputable interface identity for one package output tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InterfaceArtifact {
    /// Artifact schema version.
    pub schema: u32,
    /// Package output tree described by this artifact.
    pub output: Hash,
    /// BLAKE3 of the canonical interface stream.
    pub interface: Hash,
    /// ELF blobs whose exports contributed to the interface.
    pub elf_blobs: Vec<Hash>,
    /// Development headers whose paths and bytes contributed to the interface.
    pub headers: Vec<InterfaceHeader>,
}
