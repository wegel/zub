# zub API Documentation

A content-addressed filesystem store with git-like semantics for Linux rootfs fragments.

## Overview

`zub` (git-like object tree) provides:
- Content-addressed storage using SHA-256 hashes
- CBOR + zstd compression for trees and commits
- User namespace UID/GID remapping
- Sparse file detection and preservation
- Hardlink tracking across commits
- Extended attribute (xattr) support
- Union/merge operations for multiple trees
- Local and SSH transport for repository synchronization

## Table of Contents

- [Core Types](#core-types)
- [Repository Management](#repository-management)
- [Configuration](#configuration)
- [Namespace Mapping](#namespace-mapping)
- [Object Storage](#object-storage)
- [References](#references)
- [High-Level Operations](#high-level-operations)
- [Filesystem Operations](#filesystem-operations)
- [Transport](#transport)
- [Error Handling](#error-handling)
- [CLI Reference](#cli-reference)

---

## Core Types

### Hash

Content address for all objects. 32-byte SHA-256 hash.

```rust
pub struct Hash([u8; 32]);

impl Hash {
    /// zero hash sentinel value
    pub const ZERO: Hash;

    /// create from raw bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Self;

    /// parse from 64-character hex string
    pub fn from_hex(s: &str) -> Result<Self>;

    /// get raw bytes
    pub fn as_bytes(&self) -> &[u8; 32];

    /// convert to 64-character hex string
    pub fn to_hex(&self) -> String;

    /// split hash for object store path (first 2 chars, remaining 62)
    pub fn to_path_components(&self) -> (String, String);
}
```

**Hash computation:**

```rust
/// compute blob hash: SHA256(uid | gid | mode | xattr_count | xattrs... | content)
pub fn compute_blob_hash(
    inside_uid: u32,
    inside_gid: u32,
    mode: u32,
    xattrs: &[Xattr],
    content: &[u8],
) -> Hash;

/// compute symlink hash (uses SYMLINK_MODE = 0o120777)
pub fn compute_symlink_hash(
    inside_uid: u32,
    inside_gid: u32,
    xattrs: &[Xattr],
    target: &str,
) -> Hash;
```

### Commit

A snapshot of a tree with metadata.

```rust
pub struct Commit {
    /// root tree hash
    pub tree: Hash,
    /// parent commits (empty for initial, 1 for linear, 2+ for merge)
    pub parents: Vec<Hash>,
    /// author identity
    pub author: String,
    /// unix timestamp (seconds since epoch)
    pub timestamp: i64,
    /// commit message
    pub message: String,
    /// optional key-value metadata
    pub metadata: BTreeMap<String, String>,
}

impl Commit {
    pub fn new(
        tree: Hash,
        parents: Vec<Hash>,
        author: impl Into<String>,
        message: impl Into<String>,
    ) -> Self;

    pub fn with_timestamp(
        tree: Hash,
        parents: Vec<Hash>,
        author: impl Into<String>,
        timestamp: i64,
        message: impl Into<String>,
    ) -> Self;

    pub fn with_metadata(self, key: impl Into<String>, value: impl Into<String>) -> Self;

    pub fn is_root(&self) -> bool;   // no parents
    pub fn is_merge(&self) -> bool;  // multiple parents
}
```

### Tree

A directory structure - sorted collection of entries.

```rust
pub struct Tree { /* entries sorted by name */ }

impl Tree {
    /// create new tree, validates and sorts entries
    pub fn new(entries: Vec<TreeEntry>) -> Result<Self>;

    /// create empty tree
    pub fn empty() -> Self;

    /// get entries slice
    pub fn entries(&self) -> &[TreeEntry];

    /// consume and return entries
    pub fn into_entries(self) -> Vec<TreeEntry>;

    /// lookup entry by name (binary search)
    pub fn get(&self, name: &str) -> Option<&TreeEntry>;

    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

### TreeEntry

A single entry in a tree.

```rust
pub struct TreeEntry {
    pub name: String,
    pub kind: EntryKind,
}

impl TreeEntry {
    pub fn new(name: impl Into<String>, kind: EntryKind) -> Self;
    pub fn type_name(&self) -> &'static str;
}
```

### EntryKind

The type and metadata of a tree entry.

```rust
pub enum EntryKind {
    /// regular file
    Regular {
        hash: Hash,
        size: u64,
        sparse_map: Option<Vec<SparseRegion>>,
    },

    /// symbolic link
    Symlink { hash: Hash },

    /// directory (subtree)
    Directory {
        hash: Hash,
        uid: u32,
        gid: u32,
        mode: u32,
        xattrs: Vec<Xattr>,
    },

    /// block device
    BlockDevice {
        major: u32,
        minor: u32,
        uid: u32,
        gid: u32,
        mode: u32,
        xattrs: Vec<Xattr>,
    },

    /// character device
    CharDevice {
        major: u32,
        minor: u32,
        uid: u32,
        gid: u32,
        mode: u32,
        xattrs: Vec<Xattr>,
    },

    /// named pipe (FIFO)
    Fifo {
        uid: u32,
        gid: u32,
        mode: u32,
        xattrs: Vec<Xattr>,
    },

    /// unix socket (placeholder only)
    Socket {
        uid: u32,
        gid: u32,
        mode: u32,
        xattrs: Vec<Xattr>,
    },

    /// hardlink to another file in same tree
    Hardlink {
        target_path: String,  // path relative to tree root
    },
}

impl EntryKind {
    pub fn type_name(&self) -> &'static str;
    pub fn is_directory(&self) -> bool;
    pub fn is_regular(&self) -> bool;
    pub fn is_symlink(&self) -> bool;
    pub fn hash(&self) -> Option<&Hash>;

    // constructors
    pub fn regular(hash: Hash, size: u64) -> Self;
    pub fn sparse(hash: Hash, size: u64, sparse_map: Vec<SparseRegion>) -> Self;
    pub fn symlink(hash: Hash) -> Self;
    pub fn directory(hash: Hash, uid: u32, gid: u32, mode: u32) -> Self;
    pub fn directory_with_xattrs(hash: Hash, uid: u32, gid: u32, mode: u32, xattrs: Vec<Xattr>) -> Self;
    pub fn hardlink(target_path: impl Into<String>) -> Self;
}
```

### Xattr

Extended attribute (name + binary value).

```rust
pub struct Xattr {
    pub name: String,
    pub value: Vec<u8>,
}

impl Xattr {
    pub fn new(name: impl Into<String>, value: impl Into<Vec<u8>>) -> Self;
}
```

### SparseRegion

Data region in a sparse file.

```rust
pub struct SparseRegion {
    pub offset: u64,  // where region starts in logical file
    pub length: u64,  // length of data region
}

impl SparseRegion {
    pub fn new(offset: u64, length: u64) -> Self;
    pub fn end(&self) -> u64;  // exclusive end offset
}
```

---

## Repository Management

### Repo

Handle to an open repository.

```rust
pub struct Repo { /* ... */ }

impl Repo {
    /// initialize a new repository at path
    pub fn init(path: &Path) -> Result<Self>;

    /// open an existing repository
    pub fn open(path: &Path) -> Result<Self>;

    /// repository root path
    pub fn path(&self) -> &Path;

    /// repository configuration
    pub fn config(&self) -> &Config;
    pub fn config_mut(&mut self) -> &mut Config;
    pub fn save_config(&self) -> Result<()>;

    // paths
    pub fn config_path(&self) -> PathBuf;    // config.toml
    pub fn objects_path(&self) -> PathBuf;   // objects/
    pub fn blobs_path(&self) -> PathBuf;     // objects/blobs/
    pub fn trees_path(&self) -> PathBuf;     // objects/trees/
    pub fn commits_path(&self) -> PathBuf;   // objects/commits/
    pub fn refs_path(&self) -> PathBuf;      // refs/heads/
    pub fn tags_path(&self) -> PathBuf;      // refs/tags/
    pub fn tmp_path(&self) -> PathBuf;       // tmp/
    pub fn lock_path(&self) -> PathBuf;      // .lock

    /// acquire exclusive repository lock
    pub fn lock(&self) -> Result<RepoLock>;

    /// try to acquire lock (returns None if locked)
    pub fn try_lock(&self) -> Result<Option<RepoLock>>;
}

/// guard that holds repository lock until dropped
pub struct RepoLock { /* ... */ }
```

**Repository layout:**

```
<repo>/
├── config.toml          # repository configuration
├── objects/
│   ├── blobs/           # content-addressed file data
│   │   └── ab/cdef...   # organized by first 2 hex chars
│   ├── trees/           # serialized directory structures
│   └── commits/         # commit objects
├── refs/
│   ├── heads/           # branch refs (hierarchical)
│   └── tags/            # tag refs
└── tmp/                 # temporary files during writes
```

---

## Configuration

### Config

Repository configuration stored in `config.toml`.

```rust
pub struct Config {
    /// namespace mapping for uid/gid translation
    pub namespace: NsConfig,
    /// configured remotes
    pub remotes: Vec<Remote>,
}

impl Config {
    pub fn new(namespace: NsConfig) -> Self;
    pub fn load(path: &Path) -> Result<Self>;
    pub fn save(&self, path: &Path) -> Result<()>;

    pub fn add_remote(&mut self, name: impl Into<String>, url: impl Into<String>) -> Result<()>;
    pub fn remove_remote(&mut self, name: &str) -> Result<()>;
    pub fn get_remote(&self, name: &str) -> Option<&Remote>;
}

pub struct Remote {
    pub name: String,
    pub url: String,
}
```

---

## Namespace Mapping

User namespace support for uid/gid translation between container and host.

### NsConfig

```rust
pub struct NsConfig {
    pub uid_map: Vec<MapEntry>,
    pub gid_map: Vec<MapEntry>,
}

impl NsConfig {
    /// create identity mapping (no translation)
    pub fn identity() -> Self;

    pub fn is_identity(&self) -> bool;
}

pub struct MapEntry {
    pub inside_start: u32,   // logical id (inside namespace)
    pub outside_start: u32,  // on-disk id (outside namespace)
    pub count: u32,          // number of ids in range
}

impl MapEntry {
    pub fn new(inside_start: u32, outside_start: u32, count: u32) -> Self;
    pub fn identity_single(id: u32) -> Self;
    pub fn contains_inside(&self, id: u32) -> bool;
    pub fn contains_outside(&self, id: u32) -> bool;
}
```

### Mapping Functions

```rust
/// parse /proc/self/{uid,gid}_map format
pub fn parse_id_map(content: &str) -> Result<Vec<MapEntry>>;

/// read current process mappings
pub fn current_uid_map() -> Result<Vec<MapEntry>>;
pub fn current_gid_map() -> Result<Vec<MapEntry>>;

/// convert on-disk id to logical id
pub fn outside_to_inside(outside: u32, map: &[MapEntry]) -> Option<u32>;

/// convert logical id to on-disk id
pub fn inside_to_outside(inside: u32, map: &[MapEntry]) -> Option<u32>;

/// remap id from one namespace to another
pub fn remap(old_outside: u32, old_map: &[MapEntry], new_map: &[MapEntry]) -> Option<u32>;

/// check if two namespace configs are equivalent
pub fn mappings_equal(a: &NsConfig, b: &NsConfig) -> bool;
```

---

## Object Storage

Low-level object read/write operations.

### Blobs

```rust
/// write blob to object store (compressed with zstd)
pub fn write_blob(
    repo: &Repo,
    content: &[u8],
    inside_uid: u32,
    inside_gid: u32,
    mode: u32,
    xattrs: &[Xattr],
) -> Result<Hash>;

/// read blob from object store
pub fn read_blob(repo: &Repo, hash: &Hash) -> Result<Vec<u8>>;

/// check if blob exists
pub fn blob_exists(repo: &Repo, hash: &Hash) -> bool;

/// get filesystem path to blob
pub fn blob_path(repo: &Repo, hash: &Hash) -> PathBuf;
```

### Trees

```rust
/// write tree to object store (CBOR + zstd)
pub fn write_tree(repo: &Repo, tree: &Tree) -> Result<Hash>;

/// read tree from object store
pub fn read_tree(repo: &Repo, hash: &Hash) -> Result<Tree>;

/// get filesystem path to tree
pub fn tree_path(repo: &Repo, hash: &Hash) -> PathBuf;
```

### Commits

```rust
/// write commit to object store (CBOR + zstd)
pub fn write_commit(repo: &Repo, commit: &Commit) -> Result<Hash>;

/// read commit from object store
pub fn read_commit(repo: &Repo, hash: &Hash) -> Result<Commit>;

/// get filesystem path to commit
pub fn commit_path(repo: &Repo, hash: &Hash) -> PathBuf;
```

---

## References

Named pointers to commits. Hierarchical names like `heads/main` or `tags/v1.0`.

```rust
/// write/update a ref
pub fn write_ref(repo: &Repo, ref_name: &str, hash: &Hash) -> Result<()>;

/// read a ref
pub fn read_ref(repo: &Repo, ref_name: &str) -> Result<Hash>;

/// delete a ref
pub fn delete_ref(repo: &Repo, ref_name: &str) -> Result<()>;

/// resolve ref name or hash string to hash
pub fn resolve_ref(repo: &Repo, ref_or_hash: &str) -> Result<Hash>;

/// list all refs
pub fn list_refs(repo: &Repo) -> Result<Vec<String>>;

/// check if ref exists
pub fn ref_exists(repo: &Repo, ref_name: &str) -> bool;
```

---

## High-Level Operations

### Commit

Commit a directory tree to a ref.

```rust
pub fn commit(
    repo: &Repo,
    source: &Path,       // directory to commit
    ref_name: &str,      // target ref
    message: Option<&str>,
    author: Option<&str>,
) -> Result<Hash>;
```

### Checkout

Checkout a ref to a target directory.

```rust
pub struct CheckoutOptions {
    pub force: bool,           // overwrite existing files
    pub hardlink: bool,        // use hardlinks (default: true)
    pub preserve_sparse: bool, // preserve sparse file holes
}

impl Default for CheckoutOptions {
    fn default() -> Self {
        Self {
            force: false,
            hardlink: true,
            preserve_sparse: false,
        }
    }
}

pub fn checkout(
    repo: &Repo,
    ref_name: &str,
    target: &Path,
    opts: CheckoutOptions,
) -> Result<()>;
```

### Diff

Compare two refs.

```rust
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    MetadataOnly,
}

pub struct DiffEntry {
    pub path: String,
    pub kind: ChangeKind,
}

pub fn diff(repo: &Repo, ref1: &str, ref2: &str) -> Result<Vec<DiffEntry>>;
```

### Log

Get commit history.

```rust
pub struct LogEntry {
    pub hash: Hash,
    pub commit: Commit,
}

pub fn log(
    repo: &Repo,
    ref_name: &str,
    max_count: Option<usize>,
) -> Result<Vec<LogEntry>>;
```

### List Tree

List tree contents.

```rust
pub struct LsTreeEntry {
    pub path: String,
    pub entry: TreeEntry,
}

/// list tree contents at path
pub fn ls_tree(
    repo: &Repo,
    ref_name: &str,
    path: Option<&Path>,
) -> Result<Vec<LsTreeEntry>>;

/// list tree contents recursively
pub fn ls_tree_recursive(
    repo: &Repo,
    ref_name: &str,
) -> Result<Vec<LsTreeEntry>>;
```

### Union

Merge multiple refs into one.

```rust
pub enum ConflictResolution {
    Error,  // error on any conflict (default)
    First,  // use entry from first tree
    Last,   // use entry from last tree
}

pub struct UnionOptions {
    pub message: Option<String>,
    pub author: Option<String>,
    pub on_conflict: ConflictResolution,
}

/// merge multiple refs into a new commit
pub fn union_trees(
    repo: &Repo,
    refs: &[&str],
    output_ref: &str,
    opts: UnionOptions,
) -> Result<Hash>;

/// checkout union of multiple refs directly
pub fn union_checkout(
    repo: &Repo,
    refs: &[&str],
    destination: &Path,
    opts: UnionCheckoutOptions,
) -> Result<()>;
```

### Fsck

Verify repository integrity.

```rust
pub struct FsckReport {
    pub objects_checked: usize,
    pub corrupt_objects: Vec<CorruptObject>,
    pub missing_objects: Vec<MissingObject>,
    pub dangling_objects: Vec<Hash>,
}

impl FsckReport {
    pub fn is_ok(&self) -> bool;
}

pub fn fsck(repo: &Repo) -> Result<FsckReport>;
```

### Garbage Collection

Remove unreachable objects.

```rust
pub struct GcStats {
    pub blobs_removed: usize,
    pub trees_removed: usize,
    pub commits_removed: usize,
    pub bytes_freed: u64,
}

pub fn gc(repo: &Repo, dry_run: bool) -> Result<GcStats>;
```

---

## Filesystem Operations

Low-level filesystem utilities in `zub::fs`.

### File Type Detection

```rust
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
}

pub struct FileMetadata {
    pub file_type: FileType,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub size: u64,
    pub rdev: Option<(u32, u32)>,  // device major/minor
    pub ino: u64,
    pub dev: u64,
    pub nlink: u64,
}
```

### Creation Functions

```rust
pub fn create_directory(path: &Path, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()>;
pub fn create_symlink(path: &Path, target: &str, uid: u32, gid: u32, xattrs: &[Xattr]) -> Result<()>;
pub fn create_block_device(path: &Path, major: u32, minor: u32, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()>;
pub fn create_char_device(path: &Path, major: u32, minor: u32, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()>;
pub fn create_fifo(path: &Path, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()>;
pub fn create_socket_placeholder(path: &Path, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()>;
pub fn create_hardlink(path: &Path, target: &Path) -> Result<()>;
pub fn apply_metadata(path: &Path, uid: u32, gid: u32, mode: u32, xattrs: &[Xattr]) -> Result<()>;
```

### Sparse File Support

```rust
/// detect sparse regions using SEEK_HOLE/SEEK_DATA
pub fn detect_sparse_regions(file: &File) -> Result<Option<Vec<SparseRegion>>>;

/// read only data regions from sparse file
pub fn read_data_regions(file: &mut File, regions: &[SparseRegion]) -> Result<Vec<u8>>;

/// write sparse file recreating holes
pub fn write_sparse_file(path: &Path, data: &[u8], regions: &[SparseRegion], total_size: u64) -> Result<()>;
```

### Hardlink Tracking

```rust
/// track hardlinks during commit (by device + inode)
pub struct HardlinkTracker { /* ... */ }

impl HardlinkTracker {
    pub fn new() -> Self;
    pub fn check(&mut self, dev: u64, ino: u64, path: &str) -> Option<String>;
}

/// track hardlinks during checkout (logical path -> filesystem path)
pub struct CheckoutHardlinkTracker { /* ... */ }
```

---

## Transport

Repository synchronization operations.

### Push

```rust
pub struct PushOptions {
    pub force: bool,    // force non-fast-forward
    pub dry_run: bool,  // show what would be transferred
}

pub struct PushResult {
    pub hash: Hash,
    pub stats: TransferStats,
    pub objects_to_transfer: usize,  // for dry_run
}

pub struct TransferStats {
    pub copied: usize,
    pub hardlinked: usize,
    pub skipped: usize,
    pub bytes_transferred: u64,
}

/// push to local repository
pub fn push_local(
    src: &Repo,
    dst: &Repo,
    ref_name: &str,
    options: &PushOptions,
) -> Result<PushResult>;

/// push to remote via SSH
pub fn push_ssh(
    local: &Repo,
    remote: &str,          // user@host or host
    remote_path: &Path,    // path to repo on remote
    ref_name: &str,
    options: &PushOptions,
) -> Result<PushResult>;
```

### Pull

```rust
pub struct PullOptions {
    pub fetch_only: bool,  // only fetch, don't update ref
    pub dry_run: bool,     // show what would be transferred
}

pub struct PullResult {
    pub hash: Hash,
    pub stats: TransferStats,
    pub objects_to_transfer: usize,  // for dry_run
}

/// pull from local repository
pub fn pull_local(
    src: &Repo,
    dst: &Repo,
    ref_name: &str,
    options: &PullOptions,
) -> Result<PullResult>;

/// pull from remote via SSH
pub fn pull_ssh(
    remote: &str,
    remote_path: &Path,
    local: &Repo,
    ref_name: &str,
    options: &PullOptions,
) -> Result<PullResult>;
```

### SSH Protocol

The SSH transport uses a line-based protocol with the `zub-remote` helper:

| Command | Response |
|---------|----------|
| `list-refs` | `<hash> <ref>\n...` then `end\n` |
| `get-ref <name>` | `<hash>\n` or `not-found\n`, then `end\n` |
| `want-objects\n<type> <hash>\n...end\n` | `<type> <hash>\n...` (needed objects), then `end\n` |
| `have-objects\n<type> <hash>\n...end\n` | `<type> <hash>\n...` (missing objects), then `end\n` |
| `object <type> <hash> <size>\n<data>` | `ok\nend\n` |
| `update-ref <name> <hash>` | `ok\nend\n` |
| `quit` | (closes connection) |

---

## Error Handling

```rust
pub enum Error {
    NoRepo(PathBuf),                    // repository not found
    RepoExists(PathBuf),                // repository already exists
    RefNotFound(String),                // ref not found
    InvalidRef(String),                 // invalid ref name
    ObjectNotFound(Hash),               // object not found
    CorruptObject(Hash),                // hash mismatch
    UnionConflict(PathBuf),             // path conflict during union
    UnionTypeConflict { path, first_type, second_type }, // type mismatch in union
    TargetNotEmpty(PathBuf),            // checkout target not empty
    LockContention,                     // repository locked
    UnmappedUid(u32),                   // uid not in namespace map
    UnmappedGid(u32),                   // gid not in namespace map
    NamespaceParseError(PathBuf),       // bad namespace mapping
    RemoteNotFound(String),             // remote not configured
    RemoteConnection(String),           // connection failed
    RemoteConfigError,                  // remote config invalid
    InvalidEntryName(String),           // bad tree entry name
    DuplicateEntryName(String),         // duplicate in tree
    HardlinkTargetNotFound(String),     // hardlink target missing
    DeviceNodePermission(PathBuf),      // need privileges for device
    Io { path: PathBuf, source: std::io::Error },
    CborEncode(ciborium::ser::Error<std::io::Error>),
    CborDecode(ciborium::de::Error<std::io::Error>),
    Config(toml::de::Error),
    ConfigSerialize(toml::ser::Error),
    InvalidHashHex(String),
    Xattr { path: PathBuf, message: String },
    Transport { message: String },
    InvalidConflictResolution(String),
    CorruptObjectMessage(String),
    InvalidObjectType(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// helper trait for wrapping io errors with path context
pub trait IoResultExt<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T>;
}
```

---

## CLI Reference

### Commands

```
zub init [PATH]                              # initialize repository
zub commit SOURCE -r REF [-m MSG] [-a AUTHOR] # commit directory
zub checkout REF DEST [--copy] [--sparse]    # checkout to directory
zub log REF [-n COUNT]                       # show commit history
zub ls-tree REF [-p PATH] [-r]               # list tree contents
zub diff REF1 REF2                           # compare refs
zub union REFS... -o OUTPUT [--on-conflict]  # merge refs
zub union-checkout REFS... -d DEST           # checkout merged refs
zub fsck                                     # verify integrity
zub gc [--dry-run]                           # garbage collect
zub push DEST REF [-f] [--dry-run]           # push to repository
zub pull SOURCE REF [--fetch-only] [--dry-run] # pull from repository
zub refs                                     # list refs
zub show-ref REF                             # show ref hash
zub delete-ref REF                           # delete ref
zub cat-file TYPE HASH                       # show object contents
zub rev-parse REF [--short]                  # resolve ref to hash
zub zub-remote PATH                          # SSH remote helper
```

### Examples

```bash
# initialize and commit
zub init /path/to/repo
zub -r /path/to/repo commit /source/dir -r main -m "initial commit"

# checkout
zub -r /path/to/repo checkout main /dest/dir
zub -r /path/to/repo checkout --copy main /dest/dir  # copy instead of hardlink

# push/pull between repos
zub -r src push /path/to/dst main
zub -r dst pull /path/to/src main

# dry run
zub -r src push --dry-run /path/to/dst main

# merge multiple refs
zub -r repo union base overlay1 overlay2 -o merged -m "merge layers"

# inspect objects
zub -r repo cat-file commit $(zub -r repo rev-parse main)
zub -r repo cat-file tree HASH
zub -r repo cat-file blob HASH

# maintenance
zub -r repo fsck
zub -r repo gc --dry-run
zub -r repo gc
```

---

## Example Usage

```rust
use zub::{Repo, ops};
use std::path::Path;

// initialize repository
let repo = Repo::init(Path::new("/path/to/repo"))?;

// commit a directory
let hash = ops::commit(
    &repo,
    Path::new("/source"),
    "heads/main",
    Some("initial commit"),
    None,
)?;
println!("committed: {}", hash);

// checkout to directory
ops::checkout(
    &repo,
    "heads/main",
    Path::new("/destination"),
    ops::CheckoutOptions::default(),
)?;

// show differences
let changes = ops::diff(&repo, "heads/main", "heads/feature")?;
for change in changes {
    println!("{}", change);
}

// merge multiple refs
let merged = ops::union_trees(
    &repo,
    &["base", "overlay1", "overlay2"],
    "merged",
    ops::UnionOptions::default(),
)?;
```
