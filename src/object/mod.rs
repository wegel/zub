pub mod artifact;
pub mod blob;
pub mod commit;
pub mod tree;
mod verify;

use std::fs::File;

use crate::error::{Error, IoResultExt, Result};
use crate::repo::Repo;

pub use artifact::{
    artifact_exists, artifact_path, read_artifact, read_named_artifact, write_artifact,
    write_named_artifact,
};
pub use blob::{blob_exists, blob_path, read_blob, write_blob};
pub(crate) use blob::{write_blob_streaming_with_durability, write_blob_with_durability};
pub(crate) use commit::write_commit_with_durability;
pub use commit::{commit_path, read_commit, write_commit};
pub(crate) use tree::write_tree_with_durability;
pub use tree::{read_tree, tree_path, write_tree};
pub(crate) use verify::verify_blob;
pub use verify::{verify_commit, verify_commits};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ObjectDurability {
    Immediate,
    Deferred,
}

impl ObjectDurability {
    fn sync_file(self, file: &File, path: &std::path::Path) -> Result<()> {
        if self == Self::Immediate {
            file.sync_all().with_path(path)?;
        }
        Ok(())
    }

    fn sync_directory(self, path: &std::path::Path) -> Result<()> {
        if self == Self::Immediate {
            let directory = File::open(path).with_path(path)?;
            directory.sync_all().with_path(path)?;
        }
        Ok(())
    }
}

pub(crate) fn sync_repository(repo: &Repo) -> Result<()> {
    let path = repo.path();
    let directory = File::open(path).with_path(path)?;
    nix::unistd::syncfs(&directory).map_err(|error| Error::Io {
        path: path.to_path_buf(),
        source: error.into(),
    })
}
