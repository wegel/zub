use std::path::Path;

use zub::ops::{fsck, gc, map, MapOptions};
use zub::Repo;

use crate::Commands;

pub(super) fn run(command: Commands, repo_path: &Path) -> zub::Result<()> {
    match command {
        Commands::Fsck { reindex } => check(repo_path, reindex),
        Commands::Gc { dry_run } => collect(repo_path, dry_run),
        Commands::Stats => stats(repo_path),
        Commands::Du {
            pattern,
            limit,
            depth,
        } => disk_usage(repo_path, pattern, limit, depth),
        Commands::TruncateHistory { dry_run } => truncate(repo_path, dry_run),
        Commands::Remap { force, dry_run } => remap(repo_path, force, dry_run),
        _ => unreachable!("maintenance router received another command group"),
    }
}

fn check(repo_path: &Path, reindex: bool) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let report = fsck(&repo)?;
    println!("objects checked: {}", report.objects_checked);
    if !report.corrupt_objects.is_empty() {
        println!("\ncorrupt objects:");
        for object in &report.corrupt_objects {
            println!(
                "  {} {}: {}",
                object.object_type, object.hash, object.message
            );
        }
    }
    if !report.missing_objects.is_empty() {
        println!("\nmissing objects:");
        for object in &report.missing_objects {
            println!(
                "  {} {} (referenced by {})",
                object.object_type, object.hash, object.referenced_by
            );
        }
    }
    if !report.dangling_objects.is_empty() {
        println!("\ndangling objects: {}", report.dangling_objects.len());
    }
    finish_check(&repo, report.is_ok(), reindex)
}

fn finish_check(repo: &Repo, healthy: bool, reindex: bool) -> zub::Result<()> {
    if !healthy {
        println!("\nrepository has issues");
        return Err(zub::Error::CorruptObjectMessage(
            "repository integrity check failed".to_owned(),
        ));
    }
    println!("\nrepository is healthy");
    if reindex {
        let markers = zub::rebuild_index(repo)?;
        println!("rebuilt reverse index: {markers} blob/tree links");
    }
    Ok(())
}

fn collect(repo_path: &Path, dry_run: bool) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let stats = gc(&repo, dry_run)?;
    let action = if dry_run { "would remove" } else { "removed" };
    println!(
        "{} {} blobs, {} trees, {} commits, {} artifacts",
        action,
        stats.blobs_removed,
        stats.trees_removed,
        stats.commits_removed,
        stats.artifacts_removed
    );
    println!("freed {} bytes", stats.bytes_freed);
    Ok(())
}

fn stats(repo_path: &Path) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let stats = zub::stats(&repo)?;
    println!("refs: {}\n\nobjects:", stats.total_refs);
    print_object_stats(
        "blobs:",
        stats.total_blobs,
        stats.reachable_blobs,
        stats.total_blobs_bytes,
    );
    print_object_stats(
        "trees:",
        stats.total_trees,
        stats.reachable_trees,
        stats.total_trees_bytes,
    );
    print_object_stats(
        "commits:",
        stats.total_commits,
        stats.reachable_commits,
        stats.total_commits_bytes,
    );
    println!();
    if stats.unreachable_blobs_bytes > 0 {
        println!(
            "unreachable blob data: {:.1} MB (run gc to free)",
            stats.unreachable_blobs_bytes as f64 / 1_000_000.0
        );
    }
    Ok(())
}

fn print_object_stats(label: &str, total: usize, reachable: usize, bytes: u64) {
    println!(
        "  {label:<9}{total:>8} total, {reachable:>8} reachable ({:.1} MB on disk)",
        bytes as f64 / 1_000_000.0
    );
}

fn disk_usage(
    repo_path: &Path,
    pattern: Option<String>,
    limit: usize,
    depth: Option<usize>,
) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    if let Some(depth) = depth {
        let reference = pattern
            .as_deref()
            .ok_or_else(|| zub::Error::RefNotFound("ref name required with --depth".to_owned()))?;
        let sizes = zub::du_tree(&repo, reference, depth)?;
        for entry in sizes.iter().take(limit) {
            println!(
                "{:>10.1} MB  {}",
                entry.bytes as f64 / 1_000_000.0,
                entry.path
            );
        }
        print_remainder(sizes.len(), limit, "paths");
    } else {
        let sizes = zub::du(&repo, pattern.as_deref())?;
        for entry in sizes.iter().take(limit) {
            println!(
                "{:>10.1} MB  {}",
                entry.bytes as f64 / 1_000_000.0,
                entry.ref_name
            );
        }
        print_remainder(sizes.len(), limit, "refs");
    }
    Ok(())
}

fn print_remainder(count: usize, limit: usize, kind: &str) {
    if count > limit {
        println!("... and {} more {kind}", count - limit);
    }
}

fn truncate(repo_path: &Path, dry_run: bool) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let stats = zub::truncate_history(&repo, dry_run)?;
    let action = if dry_run {
        "would truncate"
    } else {
        "truncated"
    };
    println!(
        "{} {}/{} refs",
        action, stats.refs_truncated, stats.refs_processed
    );
    if !dry_run && stats.refs_truncated > 0 {
        println!("run gc to free unreachable objects");
    }
    Ok(())
}

fn remap(repo_path: &Path, force: bool, dry_run: bool) -> zub::Result<()> {
    let mut repo = Repo::open(repo_path)?;
    let stats = map(&mut repo, &MapOptions { force, dry_run })?;
    if stats.total == 0 && stats.remapped == 0 {
        println!("namespace mappings match, nothing to do");
        return Ok(());
    }
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
    Ok(())
}
