use std::path::{Path, PathBuf};

use zub::ops::{
    checkout, checkout_paths, commit, diff, log, ls_tree, ls_tree_recursive, union_checkout,
    union_trees, CheckoutOptions, ConflictResolution, LsTreeOptions, UnionCheckoutOptions,
    UnionOptions,
};
use zub::Repo;

use crate::Commands;

pub(super) fn run(command: Commands, repo_path: &Path) -> zub::Result<()> {
    if matches!(
        &command,
        Commands::Union { .. } | Commands::UnionCheckout { .. }
    ) {
        return run_union(command, repo_path);
    }
    run_basic(command, repo_path)
}

fn run_basic(command: Commands, repo_path: &Path) -> zub::Result<()> {
    match command {
        Commands::Init { path } => init(path),
        Commands::Commit {
            source,
            ref_name,
            message,
            author,
        } => commit_command(repo_path, source, ref_name, message, author),
        Commands::Checkout {
            ref_name,
            destination,
            force,
            hardlink,
            copy,
            sparse,
            paths,
        } => checkout_command(
            repo_path,
            ref_name,
            destination,
            paths,
            CheckoutOptions {
                force,
                hardlink: hardlink && !copy,
                preserve_sparse: sparse,
            },
        ),
        Commands::Log {
            ref_name,
            max_count,
        } => show_log(repo_path, ref_name, max_count),
        Commands::LsTree {
            ref_name,
            path,
            recursive,
            long,
            human,
        } => show_tree(repo_path, ref_name, path, recursive, long, human),
        Commands::Diff { ref1, ref2 } => show_diff(repo_path, ref1, ref2),
        _ => unreachable!("basic store router received another command group"),
    }
}

fn run_union(command: Commands, repo_path: &Path) -> zub::Result<()> {
    match command {
        Commands::Union {
            refs,
            output,
            on_conflict,
            message,
        } => union(repo_path, refs, output, on_conflict, message),
        Commands::UnionCheckout {
            refs,
            destination,
            force,
            on_conflict,
            hardlink,
            copy,
        } => union_to_directory(
            repo_path,
            refs,
            destination,
            force,
            on_conflict,
            hardlink && !copy,
        ),
        _ => unreachable!("union router received another command group"),
    }
}

fn init(path: PathBuf) -> zub::Result<()> {
    Repo::init(&path)?;
    println!("initialized zub repository at {}", path.display());
    Ok(())
}

fn commit_command(
    repo_path: &Path,
    source: PathBuf,
    reference: String,
    message: Option<String>,
    author: Option<String>,
) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let hash = commit(
        &repo,
        &source,
        &reference,
        message.as_deref(),
        author.as_deref(),
    )?;
    println!("{hash}");
    Ok(())
}

fn checkout_command(
    repo_path: &Path,
    reference: String,
    destination: PathBuf,
    paths: Vec<PathBuf>,
    options: CheckoutOptions,
) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    if paths.is_empty() {
        checkout(&repo, &reference, &destination, options)?;
    } else {
        checkout_paths(&repo, &reference, &paths, &destination, options)?;
    }
    println!("checked out {} to {}", reference, destination.display());
    Ok(())
}

fn show_log(repo_path: &Path, reference: String, limit: Option<usize>) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    for entry in log(&repo, &reference, limit)? {
        println!("{entry}");
    }
    Ok(())
}

fn show_tree(
    repo_path: &Path,
    reference: String,
    path: Option<PathBuf>,
    recursive: bool,
    long: bool,
    human: bool,
) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let options = LsTreeOptions { long, human };
    let entries = if recursive {
        ls_tree_recursive(&repo, &reference, &options)?
    } else {
        ls_tree(&repo, &reference, path.as_deref(), &options)?
    };
    for entry in entries {
        println!("{}", entry.format(&options));
    }
    Ok(())
}

fn show_diff(repo_path: &Path, first: String, second: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    for change in diff(&repo, &first, &second)? {
        let prefix = match change.kind {
            zub::ChangeKind::Added => "+",
            zub::ChangeKind::Deleted => "-",
            zub::ChangeKind::Modified => "M",
            zub::ChangeKind::MetadataOnly => "m",
        };
        println!("{} {}", prefix, change.path);
    }
    Ok(())
}

fn union(
    repo_path: &Path,
    refs: Vec<String>,
    output: String,
    conflict: String,
    message: Option<String>,
) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let references = refs.iter().map(String::as_str).collect::<Vec<_>>();
    let options = UnionOptions {
        message,
        author: None,
        on_conflict: parse_conflict_resolution(&conflict)?,
    };
    println!("{}", union_trees(&repo, &references, &output, options)?);
    Ok(())
}

fn union_to_directory(
    repo_path: &Path,
    refs: Vec<String>,
    destination: PathBuf,
    force: bool,
    conflict: String,
    hardlink: bool,
) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let references = refs.iter().map(String::as_str).collect::<Vec<_>>();
    let options = UnionCheckoutOptions {
        force,
        on_conflict: parse_conflict_resolution(&conflict)?,
        hardlink,
    };
    union_checkout(&repo, &references, &destination, options)?;
    println!(
        "checked out union of {} refs to {}",
        refs.len(),
        destination.display()
    );
    Ok(())
}

fn parse_conflict_resolution(value: &str) -> zub::Result<ConflictResolution> {
    match value.to_lowercase().as_str() {
        "error" => Ok(ConflictResolution::Error),
        "first" => Ok(ConflictResolution::First),
        "last" => Ok(ConflictResolution::Last),
        _ => Err(zub::Error::InvalidConflictResolution(value.to_owned())),
    }
}
