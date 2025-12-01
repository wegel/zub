//! zubCLI - git-like object tree command line interface

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use std::io::{self, Write};

use zub::ops::{
    checkout, commit, diff, fsck, gc, log, ls_tree, ls_tree_recursive, map, union_checkout,
    union_trees, CheckoutOptions, ConflictResolution, MapOptions, UnionCheckoutOptions,
    UnionOptions,
};
use zub::transport::{pull_local, push_local, PullOptions, PushOptions};
use zub::{read_blob, read_commit, read_tree, Hash, Repo};

#[derive(Parser)]
#[command(name = "zub")]
#[command(about = "git-like object tree - content-addressed filesystem store")]
#[command(version)]
struct Cli {
    /// repository path
    #[arg(short, long, default_value = ".")]
    repo: PathBuf,

    #[command(subcommand)]
    command: Commands,
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

        /// use copy instead of hardlinks
        #[arg(long)]
        copy: bool,

        /// preserve sparse file holes
        #[arg(long)]
        sparse: bool,
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

        /// conflict resolution: error, first, last
        #[arg(long, default_value = "error")]
        on_conflict: String,

        /// use copy instead of hardlinks
        #[arg(long)]
        copy: bool,
    },

    /// verify repository integrity
    Fsck,

    /// garbage collect unreachable objects
    Gc {
        /// only show what would be removed
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
    Refs,

    /// show ref hash
    ShowRef {
        /// ref name
        ref_name: String,
    },

    /// delete a ref
    DeleteRef {
        /// ref name
        ref_name: String,
    },

    /// show contents of an object
    CatFile {
        /// object type (blob, tree, commit)
        object_type: String,

        /// object hash
        object: String,
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

    if let Err(e) = run(cli) {
        eprintln!("error: {}", e);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run(cli: Cli) -> zub::Result<()> {
    match cli.command {
        Commands::Init { path } => {
            Repo::init(&path)?;
            println!("initialized zub repository at {}", path.display());
        }

        Commands::Commit {
            source,
            ref_name,
            message,
            author,
        } => {
            let repo = Repo::open(&cli.repo)?;
            let hash = commit(
                &repo,
                &source,
                &ref_name,
                message.as_deref(),
                author.as_deref(),
            )?;
            println!("{}", hash);
        }

        Commands::Checkout {
            ref_name,
            destination,
            copy,
            sparse,
        } => {
            let repo = Repo::open(&cli.repo)?;
            let options = CheckoutOptions {
                force: false,
                hardlink: !copy,
                preserve_sparse: sparse,
            };
            checkout(&repo, &ref_name, &destination, options)?;
            println!("checked out {} to {}", ref_name, destination.display());
        }

        Commands::Log {
            ref_name,
            max_count,
        } => {
            let repo = Repo::open(&cli.repo)?;
            let entries = log(&repo, &ref_name, max_count)?;

            for entry in entries {
                println!("{}", entry);
            }
        }

        Commands::LsTree {
            ref_name,
            path,
            recursive,
        } => {
            let repo = Repo::open(&cli.repo)?;

            let entries = if recursive {
                ls_tree_recursive(&repo, &ref_name)?
            } else {
                ls_tree(&repo, &ref_name, path.as_deref())?
            };

            for entry in entries {
                println!("{}", entry);
            }
        }

        Commands::Diff { ref1, ref2 } => {
            let repo = Repo::open(&cli.repo)?;
            let changes = diff(&repo, &ref1, &ref2)?;

            for change in changes {
                let prefix = match change.kind {
                    zub::ChangeKind::Added => "+",
                    zub::ChangeKind::Deleted => "-",
                    zub::ChangeKind::Modified => "M",
                    zub::ChangeKind::MetadataOnly => "m",
                };
                println!("{} {}", prefix, change.path);
            }
        }

        Commands::Union {
            refs,
            output,
            on_conflict,
            message,
        } => {
            let repo = Repo::open(&cli.repo)?;
            let resolution = parse_conflict_resolution(&on_conflict)?;
            let ref_strs: Vec<&str> = refs.iter().map(|s| s.as_str()).collect();

            let opts = UnionOptions {
                message,
                author: None,
                on_conflict: resolution,
            };
            let hash = union_trees(&repo, &ref_strs, &output, opts)?;
            println!("{}", hash);
        }

        Commands::UnionCheckout {
            refs,
            destination,
            on_conflict,
            copy,
        } => {
            let repo = Repo::open(&cli.repo)?;
            let resolution = parse_conflict_resolution(&on_conflict)?;
            let ref_strs: Vec<&str> = refs.iter().map(|s| s.as_str()).collect();

            let options = UnionCheckoutOptions {
                force: false,
                on_conflict: resolution,
                hardlink: !copy,
            };
            union_checkout(&repo, &ref_strs, &destination, options)?;
            println!(
                "checked out union of {} refs to {}",
                refs.len(),
                destination.display()
            );
        }

        Commands::Fsck => {
            let repo = Repo::open(&cli.repo)?;
            let report = fsck(&repo)?;

            println!("objects checked: {}", report.objects_checked);

            if !report.corrupt_objects.is_empty() {
                println!("\ncorrupt objects:");
                for obj in &report.corrupt_objects {
                    println!("  {} {}: {}", obj.object_type, obj.hash, obj.message);
                }
            }

            if !report.missing_objects.is_empty() {
                println!("\nmissing objects:");
                for obj in &report.missing_objects {
                    println!(
                        "  {} {} (referenced by {})",
                        obj.object_type, obj.hash, obj.referenced_by
                    );
                }
            }

            if !report.dangling_objects.is_empty() {
                println!("\ndangling objects: {}", report.dangling_objects.len());
            }

            if report.is_ok() {
                println!("\nrepository is healthy");
            } else {
                println!("\nrepository has issues");
                return Err(zub::Error::CorruptObjectMessage(
                    "repository integrity check failed".to_string(),
                ));
            }
        }

        Commands::Gc { dry_run } => {
            let repo = Repo::open(&cli.repo)?;
            let stats = gc(&repo, dry_run)?;

            let action = if dry_run { "would remove" } else { "removed" };
            println!(
                "{} {} blobs, {} trees, {} commits",
                action, stats.blobs_removed, stats.trees_removed, stats.commits_removed
            );
            println!("freed {} bytes", stats.bytes_freed);
        }

        Commands::Remap { force, dry_run } => {
            let mut repo = Repo::open(&cli.repo)?;
            let options = MapOptions { force, dry_run };
            let stats = map(&mut repo, &options)?;

            if stats.total == 0 && stats.remapped == 0 {
                println!("namespace mappings match, nothing to do");
            } else {
                let action = if dry_run { "would remap" } else { "remapped" };
                println!("{} {} of {} blobs", action, stats.remapped, stats.total);
                if stats.skipped_unmapped_source > 0 {
                    println!(
                        "skipped {} blobs (uid/gid not in source namespace)",
                        stats.skipped_unmapped_source
                    );
                }
                if stats.skipped_unmapped_target > 0 {
                    println!(
                        "skipped {} blobs (uid/gid not mappable to current namespace)",
                        stats.skipped_unmapped_target
                    );
                }
            }
        }

        Commands::Push {
            destination,
            ref_name,
            force,
            dry_run,
        } => {
            let src = Repo::open(&cli.repo)?;
            let dst = Repo::open(&destination)?;

            let options = PushOptions { force, dry_run };
            let result = push_local(&src, &dst, &ref_name, &options)?;

            if dry_run {
                println!("would push {} to {}", result.hash, destination.display());
                println!("would transfer {} objects", result.objects_to_transfer);
            } else {
                println!("pushed {} to {}", result.hash, destination.display());
                println!(
                    "transferred: {} copied, {} hardlinked, {} skipped, {} bytes",
                    result.stats.copied,
                    result.stats.hardlinked,
                    result.stats.skipped,
                    result.stats.bytes_transferred
                );
            }
        }

        Commands::Pull {
            source,
            ref_name,
            fetch_only,
            dry_run,
        } => {
            let src = Repo::open(&source)?;
            let dst = Repo::open(&cli.repo)?;

            let options = PullOptions {
                fetch_only,
                dry_run,
            };
            let result = pull_local(&src, &dst, &ref_name, &options)?;

            if dry_run {
                println!("would pull {} from {}", result.hash, source.display());
                println!("would transfer {} objects", result.objects_to_transfer);
            } else {
                println!("pulled {} from {}", result.hash, source.display());
                println!(
                    "transferred: {} copied, {} hardlinked, {} skipped, {} bytes",
                    result.stats.copied,
                    result.stats.hardlinked,
                    result.stats.skipped,
                    result.stats.bytes_transferred
                );
            }
        }

        Commands::Refs => {
            let repo = Repo::open(&cli.repo)?;
            let refs = zub::list_refs(&repo)?;

            for ref_name in refs {
                let hash = zub::read_ref(&repo, &ref_name)?;
                println!("{} {}", hash, ref_name);
            }
        }

        Commands::ShowRef { ref_name } => {
            let repo = Repo::open(&cli.repo)?;
            let hash = zub::resolve_ref(&repo, &ref_name)?;
            println!("{}", hash);
        }

        Commands::DeleteRef { ref_name } => {
            let repo = Repo::open(&cli.repo)?;
            zub::delete_ref(&repo, &ref_name)?;
            println!("deleted ref {}", ref_name);
        }

        Commands::CatFile {
            object_type,
            object,
        } => {
            let repo = Repo::open(&cli.repo)?;
            let hash = Hash::from_hex(&object)?;

            match object_type.as_str() {
                "blob" => {
                    let data = read_blob(&repo, &hash)?;
                    io::stdout().write_all(&data).map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                }
                "tree" => {
                    let tree = read_tree(&repo, &hash)?;
                    for entry in tree.entries() {
                        println!("{} {}", entry.kind.type_name(), entry.name);
                    }
                }
                "commit" => {
                    let commit = read_commit(&repo, &hash)?;
                    println!("tree {}", commit.tree);
                    for parent in &commit.parents {
                        println!("parent {}", parent);
                    }
                    println!("author {}", commit.author);
                    println!("timestamp {}", commit.timestamp);
                    println!();
                    println!("{}", commit.message);
                }
                _ => {
                    return Err(zub::Error::InvalidObjectType(object_type));
                }
            }
        }

        Commands::RevParse { rev, short } => {
            let repo = Repo::open(&cli.repo)?;
            let hash = zub::resolve_ref(&repo, &rev)?;
            if short {
                println!("{}", &hash.to_hex()[..12]);
            } else {
                println!("{}", hash);
            }
        }

        Commands::Show { rev, metadata_key } => {
            let repo = Repo::open(&cli.repo)?;
            let hash = zub::resolve_ref(&repo, &rev)?;
            let commit = read_commit(&repo, &hash)?;

            match metadata_key {
                Some(key) => {
                    // print specific metadata key
                    match commit.metadata.get(&key) {
                        Some(value) => println!("{}", value),
                        None => {
                            return Err(zub::Error::MetadataKeyNotFound(key));
                        }
                    }
                }
                None => {
                    // print full commit info
                    println!("commit {}", hash);
                    println!("tree {}", commit.tree);
                    for parent in &commit.parents {
                        println!("parent {}", parent);
                    }
                    println!("author {}", commit.author);
                    println!("timestamp {}", commit.timestamp);
                    if !commit.metadata.is_empty() {
                        println!();
                        println!("metadata:");
                        for (k, v) in &commit.metadata {
                            println!("  {}: {}", k, v);
                        }
                    }
                    println!();
                    println!("{}", commit.message);
                }
            }
        }

        Commands::Remote { path } => {
            run_remote_helper(&path)?;
        }
    }

    Ok(())
}

fn parse_conflict_resolution(s: &str) -> zub::Result<ConflictResolution> {
    match s.to_lowercase().as_str() {
        "error" => Ok(ConflictResolution::Error),
        "first" => Ok(ConflictResolution::First),
        "last" => Ok(ConflictResolution::Last),
        _ => Err(zub::Error::InvalidConflictResolution(s.to_string())),
    }
}

/// run the remote helper protocol (server side of SSH transport)
fn run_remote_helper(repo_path: &Path) -> zub::Result<()> {
    use std::fs;
    use std::io::{BufRead, BufReader, Read};

    let repo = Repo::open(repo_path)?;

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout();

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        let cmd = parts[0];
        let args = parts.get(1).copied().unwrap_or("");

        match cmd {
            "list-refs" => {
                let refs = zub::list_refs(&repo)?;
                for ref_name in refs {
                    let hash = zub::read_ref(&repo, &ref_name)?;
                    writeln!(stdout, "{} {}", hash, ref_name).map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                }
                writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                stdout.flush().map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
            }

            "get-ref" => {
                match zub::read_ref(&repo, args) {
                    Ok(hash) => writeln!(stdout, "{}", hash),
                    Err(_) => writeln!(stdout, "not-found"),
                }
                .map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                stdout.flush().map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
            }

            "want-objects" => {
                // read object list until "end"
                let mut needed = Vec::new();
                loop {
                    let mut obj_line = String::new();
                    reader.read_line(&mut obj_line).unwrap_or(0);
                    let obj_line = obj_line.trim();
                    if obj_line == "end" {
                        break;
                    }
                    let obj_parts: Vec<&str> = obj_line.splitn(2, ' ').collect();
                    if obj_parts.len() == 2 {
                        let obj_type = obj_parts[0];
                        if let Ok(hash) = Hash::from_hex(obj_parts[1]) {
                            // check if we need this object
                            let exists = match obj_type {
                                "blob" => zub::blob_exists(&repo, &hash),
                                "tree" => repo
                                    .trees_path()
                                    .join(&hash.to_hex()[..2])
                                    .join(&hash.to_hex()[2..])
                                    .exists(),
                                "commit" => repo
                                    .commits_path()
                                    .join(&hash.to_hex()[..2])
                                    .join(&hash.to_hex()[2..])
                                    .exists(),
                                _ => false,
                            };
                            if !exists {
                                needed.push((obj_type.to_string(), hash));
                            }
                        }
                    }
                }
                // output what we need
                for (obj_type, hash) in needed {
                    writeln!(stdout, "{} {}", obj_type, hash).map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                }
                writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                stdout.flush().map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
            }

            "have-objects" => {
                // read object list until "end", report what we don't have
                let mut missing = Vec::new();
                loop {
                    let mut obj_line = String::new();
                    reader.read_line(&mut obj_line).unwrap_or(0);
                    let obj_line = obj_line.trim();
                    if obj_line == "end" {
                        break;
                    }
                    let obj_parts: Vec<&str> = obj_line.splitn(2, ' ').collect();
                    if obj_parts.len() == 2 {
                        let obj_type = obj_parts[0];
                        if let Ok(hash) = Hash::from_hex(obj_parts[1]) {
                            let exists = match obj_type {
                                "blob" => zub::blob_exists(&repo, &hash),
                                "tree" => repo
                                    .trees_path()
                                    .join(&hash.to_hex()[..2])
                                    .join(&hash.to_hex()[2..])
                                    .exists(),
                                "commit" => repo
                                    .commits_path()
                                    .join(&hash.to_hex()[..2])
                                    .join(&hash.to_hex()[2..])
                                    .exists(),
                                _ => false,
                            };
                            if !exists {
                                missing.push((obj_type.to_string(), hash));
                            }
                        }
                    }
                }
                for (obj_type, hash) in missing {
                    writeln!(stdout, "{} {}", obj_type, hash).map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                }
                writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                stdout.flush().map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
            }

            "object" => {
                // parse "TYPE HASH SIZE"
                let obj_parts: Vec<&str> = args.splitn(3, ' ').collect();
                if obj_parts.len() == 3 {
                    let obj_type = obj_parts[0];
                    let hash = Hash::from_hex(obj_parts[1])?;
                    let size: usize = obj_parts[2].parse().unwrap_or(0);

                    let mut data = vec![0u8; size];
                    reader.read_exact(&mut data).map_err(|e| zub::Error::Io {
                        path: "stdin".into(),
                        source: e,
                    })?;

                    // write object to appropriate location
                    let dest = match obj_type {
                        "blob" => {
                            let hex = hash.to_hex();
                            repo.blobs_path().join(&hex[..2]).join(&hex[2..])
                        }
                        "tree" => {
                            let hex = hash.to_hex();
                            repo.trees_path().join(&hex[..2]).join(&hex[2..])
                        }
                        "commit" => {
                            let hex = hash.to_hex();
                            repo.commits_path().join(&hex[..2]).join(&hex[2..])
                        }
                        _ => {
                            writeln!(stdout, "error: unknown object type").map_err(|e| {
                                zub::Error::Io {
                                    path: "stdout".into(),
                                    source: e,
                                }
                            })?;
                            continue;
                        }
                    };

                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|e| zub::Error::Io {
                            path: parent.to_path_buf(),
                            source: e,
                        })?;
                    }
                    fs::write(&dest, &data).map_err(|e| zub::Error::Io {
                        path: dest.clone(),
                        source: e,
                    })?;

                    writeln!(stdout, "ok").map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                    writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                    stdout.flush().map_err(|e| zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    })?;
                }
            }

            "update-ref" => {
                let ref_parts: Vec<&str> = args.splitn(2, ' ').collect();
                if ref_parts.len() == 2 {
                    let ref_name = ref_parts[0];
                    if let Ok(hash) = Hash::from_hex(ref_parts[1]) {
                        zub::write_ref(&repo, ref_name, &hash)?;
                        writeln!(stdout, "ok").map_err(|e| zub::Error::Io {
                            path: "stdout".into(),
                            source: e,
                        })?;
                    } else {
                        writeln!(stdout, "error: invalid hash").map_err(|e| zub::Error::Io {
                            path: "stdout".into(),
                            source: e,
                        })?;
                    }
                } else {
                    writeln!(stdout, "error: invalid update-ref args").map_err(|e| {
                        zub::Error::Io {
                            path: "stdout".into(),
                            source: e,
                        }
                    })?;
                }
                writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                stdout.flush().map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
            }

            "quit" => {
                break;
            }

            _ => {
                writeln!(stdout, "error: unknown command: {}", cmd).map_err(|e| {
                    zub::Error::Io {
                        path: "stdout".into(),
                        source: e,
                    }
                })?;
                writeln!(stdout, "end").map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
                stdout.flush().map_err(|e| zub::Error::Io {
                    path: "stdout".into(),
                    source: e,
                })?;
            }
        }
    }

    Ok(())
}
