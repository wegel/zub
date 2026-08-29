//! high-level operations on zub repositories

use crate::error::{Error, Result};
use crate::namespace::inside_to_outside;
use crate::repo::Repo;

mod checkout;
mod commit;
mod diff;
mod export;
mod fsck;
mod gc;
mod log;
mod ls_tree;
mod map;
mod select;
mod stats;
mod truncate;
mod union;
mod union_checkout;

pub use checkout::{checkout, checkout_from_tree_hash, checkout_paths, CheckoutOptions};
pub use commit::{commit, commit_tree, commit_tree_with_metadata, commit_with_metadata};
pub use diff::{diff, diff_trees};
pub use export::{export_path, ExportOptions};
pub use fsck::{fsck, CorruptObject, FsckReport, MissingObject, ObjectType};
pub use gc::{gc, GcStats};
pub use log::{log, LogEntry};
pub use ls_tree::{ls_tree, ls_tree_recursive, LsTreeEntry, LsTreeOptions};
pub use map::{map, MapOptions, MapStats};
pub use select::{select_tree, select_tree_from_hash};
pub use stats::{du, du_tree, stats, PathSize, RefSize, RepoStats};
pub use truncate::{truncate_history, TruncateStats};
pub use union::{union as union_trees, ConflictResolution, UnionOptions};
pub use union_checkout::{checkout_union as union_checkout, UnionCheckoutOptions};

/// map logical tree-entry ownership to the repository's on-disk namespace.
fn map_entry_ownership(repo: &Repo, uid: u32, gid: u32) -> Result<(u32, u32)> {
    let namespace = &repo.config().namespace;
    let outside_uid = inside_to_outside(uid, &namespace.uid_map).ok_or(Error::UnmappedUid(uid))?;
    let outside_gid = inside_to_outside(gid, &namespace.gid_map).ok_or(Error::UnmappedGid(gid))?;

    Ok((outside_uid, outside_gid))
}
