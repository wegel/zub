use std::path::PathBuf;

use crate::Hash;

/// error type for zuboperations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("repository not found at {0}")]
    NoRepo(PathBuf),

    #[error("repository already exists at {0}")]
    RepoExists(PathBuf),

    #[error("ref not found: {0}")]
    RefNotFound(String),

    #[error("invalid ref name: {0}")]
    InvalidRef(String),

    #[error("path not found in tree: {0}")]
    PathNotFound(String),

    #[error("object not found: {0}")]
    ObjectNotFound(Hash),

    #[error("corrupt object: hash mismatch for {0}")]
    CorruptObject(Hash),

    #[error("path conflict during union: {0}")]
    UnionConflict(PathBuf),

    #[error("type conflict during union at {path}: cannot merge {first_type} with {second_type}")]
    UnionTypeConflict {
        path: PathBuf,
        first_type: &'static str,
        second_type: &'static str,
    },

    #[error("checkout target not empty: {0}")]
    TargetNotEmpty(PathBuf),

    #[error("lock contention on repository")]
    LockContention,

    #[error("uid {0} not mapped in namespace")]
    UnmappedUid(u32),

    #[error("gid {0} not mapped in namespace")]
    UnmappedGid(u32),

    #[error("failed to parse namespace mapping from {0}")]
    NamespaceParseError(PathBuf),

    #[error("remote not found: {0}")]
    RemoteNotFound(String),

    #[error("remote connection failed: {0}")]
    RemoteConnection(String),

    #[error("remote config missing or invalid")]
    RemoteConfigError,

    #[error("invalid tree entry name: {0}")]
    InvalidEntryName(String),

    #[error("duplicate tree entry name: {0}")]
    DuplicateEntryName(String),

    #[error("hardlink target not found: {0}")]
    HardlinkTargetNotFound(String),

    #[error("cannot create device node without privileges: {0}")]
    DeviceNodePermission(PathBuf),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cbor serialization error: {0}")]
    CborEncode(#[from] ciborium::ser::Error<std::io::Error>),

    #[error("cbor deserialization error: {0}")]
    CborDecode(#[from] ciborium::de::Error<std::io::Error>),

    #[error("config error: {0}")]
    Config(#[from] toml::de::Error),

    #[error("config serialization error: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    #[error("invalid hash hex: {0}")]
    InvalidHashHex(String),

    #[error("xattr error on {path}: {message}")]
    Xattr { path: PathBuf, message: String },

    #[error("transport error: {message}")]
    Transport { message: String },

    #[error("invalid conflict resolution strategy: {0}")]
    InvalidConflictResolution(String),

    #[error("corrupt object: {0}")]
    CorruptObjectMessage(String),

    #[error("invalid object type: {0}")]
    InvalidObjectType(String),

    #[error("metadata key not found: {0}")]
    MetadataKeyNotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// helper to wrap io errors with path context
pub trait IoResultExt<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: path.into(),
            source,
        })
    }
}
