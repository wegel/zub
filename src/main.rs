//! zubCLI - git-like object tree command line interface

mod cli_output;
mod commands;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zub")]
#[command(about = "git-like object tree - content-addressed filesystem store")]
#[command(version)]
struct Cli {
    /// repository path (default: ZUB_REPO env, .zub symlink/dir, or current directory)
    #[arg(short, long, env = "ZUB_REPO")]
    repo: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

/// resolve the repository path from CLI arg, .zub symlink, or .zub directory
fn resolve_repo_path(repo_arg: Option<PathBuf>) -> PathBuf {
    if let Some(path) = repo_arg {
        return path;
    }

    let zub_path = Path::new(".zub");

    // check for .zub symlink
    if zub_path.is_symlink() {
        if let Ok(target) = std::fs::read_link(zub_path) {
            return target;
        }
    }

    // check for .zub directory
    if zub_path.is_dir() {
        return zub_path.to_path_buf();
    }

    // default to current directory
    PathBuf::from(".")
}

#[derive(Subcommand)]
enum Commands {
    /// initialize a new repository
    Init {
        /// path to create repository at
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// commit a directory to a ref
    Commit {
        /// source directory to commit
        source: PathBuf,

        /// ref name to commit to
        #[arg(short = 'r', long)]
        ref_name: String,

        /// commit message
        #[arg(short, long)]
        message: Option<String>,

        /// author name
        #[arg(short, long)]
        author: Option<String>,
    },

    /// checkout a ref to a directory
    Checkout {
        /// ref to checkout
        ref_name: String,

        /// destination directory
        destination: PathBuf,

        /// allow checkout into non-empty directory (overwrites existing files)
        #[arg(short, long)]
        force: bool,

        /// share immutable files with the object store using hardlinks
        #[arg(long, conflicts_with = "copy")]
        hardlink: bool,

        /// use copies (the default; retained for command compatibility)
        #[arg(long, hide = true, conflicts_with = "hardlink")]
        copy: bool,

        /// preserve sparse file holes
        #[arg(long)]
        sparse: bool,

        /// path inside the tree to checkout; may be repeated
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
    },

    /// show commit log for a ref
    Log {
        /// ref to show log for
        ref_name: String,

        /// maximum number of commits to show
        #[arg(short = 'n', long)]
        max_count: Option<usize>,
    },

    /// list tree contents
    LsTree {
        /// ref to list
        ref_name: String,

        /// path within tree
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// list recursively
        #[arg(short, long)]
        recursive: bool,

        /// long format (permissions, uid, gid, size)
        #[arg(short, long)]
        long: bool,

        /// human-readable sizes (with -l)
        #[arg(short = 'H', long)]
        human: bool,
    },

    /// show differences between two refs
    Diff {
        /// first ref
        ref1: String,

        /// second ref
        ref2: String,
    },

    /// merge multiple refs into one
    Union {
        /// refs to merge
        #[arg(required = true)]
        refs: Vec<String>,

        /// output ref name
        #[arg(short, long)]
        output: String,

        /// conflict resolution: error, first, last
        #[arg(long, default_value = "error")]
        on_conflict: String,

        /// commit message
        #[arg(short, long)]
        message: Option<String>,
    },

    /// checkout union of multiple refs
    UnionCheckout {
        /// refs to merge
        #[arg(required = true)]
        refs: Vec<String>,

        /// destination directory
        #[arg(short, long)]
        destination: PathBuf,

        /// allow checkout into non-empty directory (overwrites existing files)
        #[arg(short, long)]
        force: bool,

        /// conflict resolution: error, first, last
        #[arg(long, default_value = "error")]
        on_conflict: String,

        /// share immutable files with the object store using hardlinks
        #[arg(long, conflicts_with = "copy")]
        hardlink: bool,

        /// use copies (the default; retained for command compatibility)
        #[arg(long, hide = true, conflicts_with = "hardlink")]
        copy: bool,
    },

    /// verify repository integrity
    Fsck {
        /// rebuild derived indexes after a successful check
        #[arg(long)]
        reindex: bool,
    },

    /// garbage collect unreachable objects
    Gc {
        /// only show what would be removed
        #[arg(long)]
        dry_run: bool,
    },

    /// show repository statistics
    Stats,

    /// show disk usage per ref (or within a ref with --depth)
    Du {
        /// ref name or glob pattern to filter refs
        pattern: Option<String>,

        /// number of top entries to show (default: 20)
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// show breakdown within ref at this depth (e.g. 1=top dirs, 2=two levels)
        #[arg(short, long)]
        depth: Option<usize>,
    },

    /// truncate history, keeping only latest commit per ref
    TruncateHistory {
        /// only show what would be done
        #[arg(long)]
        dry_run: bool,
    },

    /// remap blob ownership to current user namespace
    Remap {
        /// skip blobs that can't be remapped instead of erroring
        #[arg(long)]
        force: bool,

        /// only show what would be done
        #[arg(long)]
        dry_run: bool,
    },

    /// push a ref to another repository
    Push {
        /// destination repository path
        destination: PathBuf,

        /// ref to push
        ref_name: String,

        /// force non-fast-forward update
        #[arg(short, long)]
        force: bool,

        /// dry run - show what would be transferred without doing it
        #[arg(long)]
        dry_run: bool,
    },

    /// pull a ref from another repository
    Pull {
        /// source repository path
        source: PathBuf,

        /// ref to pull
        ref_name: String,

        /// only fetch objects, don't update ref
        #[arg(long)]
        fetch_only: bool,

        /// dry run - show what would be transferred without doing it
        #[arg(long)]
        dry_run: bool,
    },

    /// list refs
    Refs {
        /// list only refs whose current tree contains this blob
        #[arg(long)]
        blob: Option<String>,
    },

    /// show ref hash
    ShowRef {
        /// ref name
        ref_name: String,
    },

    /// inspect one plan's current outputs and builder witnesses
    Plan {
        /// plan BLAKE3 hash
        plan: String,
    },

    /// inspect one named derived artifact
    Artifact {
        /// artifact key, such as elf/<blob> or interface/<tree>
        key: String,
    },

    /// delete a ref
    DeleteRef {
        /// ref name
        ref_name: String,
    },

    /// delete refs matching a glob pattern
    DeleteRefs {
        /// glob pattern (e.g. "x86_64/pkg/*/neovim/*")
        pattern: String,
    },

    /// delete artifact refs matching a glob pattern
    DeleteArtifacts {
        /// glob pattern (e.g. "x86_64/*/foo/*")
        pattern: String,
    },

    /// show contents of an object
    ///
    /// Examples:
    ///   zub cat-file myref:path/to/file     # show file contents
    ///   zub cat-file myref:path/to/dir      # list directory
    ///   zub cat-file myref                  # show commit info
    ///   zub cat-file -t blob HASH           # raw hash access
    CatFile {
        /// object spec: ref:path, ref, or hash (with -t)
        spec: String,

        /// object type for raw hash access (blob, tree, commit)
        #[arg(short = 't', long = "type")]
        object_type: Option<String>,
    },

    /// resolve a ref to a hash
    RevParse {
        /// ref or hash to resolve
        rev: String,

        /// output short hash (first 12 chars)
        #[arg(long)]
        short: bool,
    },

    /// show commit information
    Show {
        /// ref or commit hash to show
        rev: String,

        /// print specific metadata key
        #[arg(long = "print-metadata-key")]
        metadata_key: Option<String>,
    },

    /// remote helper (used by SSH transport)
    #[command(name = "zub-remote")]
    Remote {
        /// repository path
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Err(e) = commands::run(cli) {
        eprintln!("error: {}", e);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
