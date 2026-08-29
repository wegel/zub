use std::path::{Path, PathBuf};

use zub::transport::{pull_local, push_local, PullOptions, PushOptions};
use zub::Repo;

use crate::Commands;

pub(super) fn run(command: Commands, repo_path: &Path) -> zub::Result<()> {
    match command {
        Commands::Push {
            destination,
            ref_name,
            force,
            dry_run,
        } => push(repo_path, destination, ref_name, force, dry_run),
        Commands::Pull {
            source,
            ref_name,
            fetch_only,
            dry_run,
        } => pull(repo_path, source, ref_name, fetch_only, dry_run),
        Commands::Remote { path } => remote(path),
        _ => unreachable!("transfer router received another command group"),
    }
}

fn push(
    repo_path: &Path,
    destination: PathBuf,
    reference: String,
    force: bool,
    dry_run: bool,
) -> zub::Result<()> {
    let source = Repo::open(repo_path)?;
    let destination_repo = Repo::open(&destination)?;
    let result = push_local(
        &source,
        &destination_repo,
        &reference,
        &PushOptions { force, dry_run },
    )?;
    if dry_run {
        println!("would push {} to {}", result.hash, destination.display());
        println!("would transfer {} objects", result.objects_to_transfer);
    } else {
        println!("pushed {} to {}", result.hash, destination.display());
        print_transfer(&result.stats);
    }
    Ok(())
}

fn pull(
    repo_path: &Path,
    source: PathBuf,
    reference: String,
    fetch_only: bool,
    dry_run: bool,
) -> zub::Result<()> {
    let source_repo = Repo::open(&source)?;
    let destination = Repo::open(repo_path)?;
    let result = pull_local(
        &source_repo,
        &destination,
        &reference,
        &PullOptions {
            fetch_only,
            dry_run,
        },
    )?;
    if dry_run {
        println!("would pull {} from {}", result.hash, source.display());
        println!("would transfer {} objects", result.objects_to_transfer);
    } else {
        println!("pulled {} from {}", result.hash, source.display());
        print_transfer(&result.stats);
    }
    Ok(())
}

fn print_transfer(stats: &zub::transport::TransferStats) {
    println!(
        "transferred: {} copied, {} hardlinked, {} skipped, {} bytes",
        stats.copied, stats.hardlinked, stats.skipped, stats.bytes_transferred
    );
}

fn remote(path: PathBuf) -> zub::Result<()> {
    let repo = Repo::open(&path)?;
    zub::transport::serve_remote(&repo)
}
