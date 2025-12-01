//! zub - git-like object tree
//!
//! a content-addressed filesystem store for rootfs fragments with git-like semantics.
//! similar in spirit to ostree but simpler, designed for composing Linux system images.
//!
//! # Core concepts
//!
//! - **Blob**: content-addressed file data (compressed with zstd)
//! - **Tree**: a serialized directory structure (CBOR + zstd)
//! - **Commit**: a snapshot of a tree with metadata (CBOR + zstd)
//! - **Ref**: a named pointer to a commit (hierarchical, like git branches)
//!
//! # Hash format
//!
//! blob hash = SHA256(uid | gid | mode | xattr_count | xattrs... | content)
//!
//! where xattrs are sorted by name and each is: name_len | name | value_len | value
//!
//! # Example usage
//!
//! ```no_run
//! use zub::{Repo, ops};
//! use std::path::Path;
//!
//! // initialize a repository
//! let repo = Repo::init(Path::new("/path/to/repo")).unwrap();
//!
//! // commit a directory
//! let hash = ops::commit(&repo, Path::new("/source"), "my/ref", Some("initial commit"), None).unwrap();
//!
//! // checkout to a directory
//! ops::checkout(&repo, "my/ref", Path::new("/destination"), ops::CheckoutOptions::default()).unwrap();
//! ```

mod config;
mod error;
mod hash;
mod namespace;
mod object;
mod refs;
mod repo;

pub mod fs;
pub mod ops;
pub mod transport;
pub mod types;

pub use config::Config;
pub use error::{Error, Result};
pub use hash::{compute_blob_hash, Hash};
pub use namespace::{
    current_gid_map, current_uid_map, inside_to_outside, mappings_equal, outside_to_inside,
    parse_id_map, remap, MapEntry, NsConfig,
};
pub use object::{
    blob_exists, commit_path, read_blob, read_commit, read_tree, tree_path, write_blob,
    write_commit, write_tree,
};
pub use refs::{
    delete_ref, delete_refs_matching, list_refs, list_refs_matching, read_ref, ref_exists,
    resolve_ref, write_ref,
};
pub use repo::Repo;
pub use types::{ChangeKind, Commit, DiffEntry, EntryKind, SparseRegion, Tree, TreeEntry, Xattr};
