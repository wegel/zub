use std::io::{self, Write};
use std::path::Path;

use zub::{read_blob, read_commit, read_tree, EntryKind, Hash, Repo, Tree};

use crate::{cli_output, Commands};

pub(super) fn run(command: Commands, repo_path: &Path) -> zub::Result<()> {
    match command {
        Commands::Refs { blob } => refs(repo_path, blob),
        Commands::ShowRef { ref_name } => show_ref(repo_path, ref_name),
        Commands::Plan { plan } => show_plan(repo_path, plan),
        Commands::Artifact { key } => artifact(repo_path, key),
        Commands::DeleteRef { ref_name } => delete_ref(repo_path, ref_name),
        Commands::DeleteRefs { pattern } => delete_refs(repo_path, pattern),
        Commands::DeleteArtifacts { pattern } => delete_artifacts(repo_path, pattern),
        Commands::CatFile { spec, object_type } => cat_file(repo_path, spec, object_type),
        Commands::RevParse { rev, short } => rev_parse(repo_path, rev, short),
        Commands::Show { rev, metadata_key } => show(repo_path, rev, metadata_key),
        _ => unreachable!("query router received another command group"),
    }
}

fn refs(repo_path: &Path, blob: Option<String>) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let refs = match blob {
        Some(blob) => zub::refs_containing_blob(&repo, Hash::from_hex(&blob)?)?,
        None => zub::list_refs(&repo)?,
    };
    for reference in refs {
        println!("{} {}", zub::read_ref(&repo, &reference)?, reference);
    }
    Ok(())
}

fn show_ref(repo_path: &Path, reference: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    println!("{}", zub::resolve_ref(&repo, &reference)?);
    Ok(())
}

fn show_plan(repo_path: &Path, value: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let report = zub::inspect_plan(&repo, Hash::from_hex(&value)?)?;
    cli_output::print_plan(&report);
    Ok(())
}

fn artifact(repo_path: &Path, key: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    cli_output::print_artifact(&zub::read_named_artifact(&repo, &key)?);
    Ok(())
}

fn delete_ref(repo_path: &Path, reference: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    zub::delete_ref(&repo, &reference)?;
    println!("deleted ref {reference}");
    Ok(())
}

fn delete_refs(repo_path: &Path, pattern: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let deleted = zub::delete_refs_matching(&repo, &pattern)?;
    print_deletions("ref", &pattern, &deleted);
    Ok(())
}

fn delete_artifacts(repo_path: &Path, pattern: String) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let deleted = zub::delete_artifact_refs_matching(&repo, &pattern)?;
    print_deletions("artifact ref", &pattern, &deleted);
    Ok(())
}

fn print_deletions(kind: &str, pattern: &str, deleted: &[String]) {
    if deleted.is_empty() {
        if kind == "ref" {
            println!("no refs matched pattern {pattern}");
        } else {
            println!("no artifact refs matched pattern {pattern}");
        }
    } else {
        for reference in deleted {
            println!("deleted {kind} {reference}");
        }
    }
}

fn cat_file(repo_path: &Path, spec: String, object_type: Option<String>) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    if let Some(object_type) = object_type {
        return cat_object(&repo, Hash::from_hex(&spec)?, &object_type);
    }
    if let Some((reference, path)) = spec.split_once(':') {
        let commit = read_commit(&repo, &zub::resolve_ref(&repo, reference)?)?;
        return cat_file_path(&repo, &read_tree(&repo, &commit.tree)?, path);
    }
    let commit_hash = zub::resolve_ref(&repo, &spec)?;
    print_commit(&read_commit(&repo, &commit_hash)?);
    Ok(())
}

fn cat_object(repo: &Repo, hash: Hash, object_type: &str) -> zub::Result<()> {
    match object_type {
        "blob" => write_bytes(read_blob(repo, &hash)?),
        "tree" => {
            for entry in read_tree(repo, &hash)?.entries() {
                println!("{} {}", entry.kind.type_name(), entry.name);
            }
            Ok(())
        }
        "commit" => {
            print_commit(&read_commit(repo, &hash)?);
            Ok(())
        }
        _ => Err(zub::Error::InvalidObjectType(object_type.to_owned())),
    }
}

fn write_bytes(bytes: Vec<u8>) -> zub::Result<()> {
    io::stdout()
        .write_all(&bytes)
        .map_err(|source| zub::Error::Io {
            path: "stdout".into(),
            source,
        })
}

fn print_commit(commit: &zub::Commit) {
    println!("tree {}", commit.tree);
    for parent in &commit.parents {
        println!("parent {parent}");
    }
    println!("author {}", commit.author);
    println!("timestamp {}\n", commit.timestamp);
    println!("{}", commit.message);
}

fn cat_file_path(repo: &Repo, tree: &Tree, path: &str) -> zub::Result<()> {
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if components.is_empty() {
        print_tree(tree);
        return Ok(());
    }
    let mut current = tree.clone();
    for (index, component) in components.iter().enumerate() {
        let entry = current
            .get(component)
            .ok_or_else(|| zub::Error::PathNotFound(path.to_owned()))?;
        let last = index == components.len() - 1;
        match &entry.kind {
            EntryKind::Directory { hash, .. } => {
                current = read_tree(repo, hash)?;
                if last {
                    print_tree(&current);
                    return Ok(());
                }
            }
            kind => return cat_tree_entry(repo, kind, path, last),
        }
    }
    Ok(())
}

fn cat_tree_entry(repo: &Repo, kind: &EntryKind, path: &str, last: bool) -> zub::Result<()> {
    if !last {
        return Err(zub::Error::PathNotFound(path.to_owned()));
    }
    match kind {
        EntryKind::Regular { hash, .. } => write_bytes(read_blob(repo, hash)?),
        EntryKind::Symlink { hash, .. } => {
            println!("{}", String::from_utf8_lossy(&read_blob(repo, hash)?));
            Ok(())
        }
        EntryKind::Hardlink { target_path } => {
            println!("-> {target_path}");
            Ok(())
        }
        _ => {
            println!("{}", kind.type_name());
            Ok(())
        }
    }
}

fn print_tree(tree: &Tree) {
    for entry in tree.entries() {
        println!("{} {}", entry.kind.type_name(), entry.name);
    }
}

fn rev_parse(repo_path: &Path, revision: String, short: bool) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let hash = zub::resolve_ref(&repo, &revision)?;
    if short {
        println!("{}", &hash.to_hex()[..12]);
    } else {
        println!("{hash}");
    }
    Ok(())
}

fn show(repo_path: &Path, revision: String, metadata_key: Option<String>) -> zub::Result<()> {
    let repo = Repo::open(repo_path)?;
    let hash = zub::resolve_ref(&repo, &revision)?;
    let commit = read_commit(&repo, &hash)?;
    if let Some(key) = metadata_key {
        let value = commit
            .metadata
            .get(&key)
            .ok_or(zub::Error::MetadataKeyNotFound(key))?;
        println!("{value}");
    } else {
        println!("commit {hash}");
        println!("tree {}", commit.tree);
        for parent in &commit.parents {
            println!("parent {parent}");
        }
        println!("author {}", commit.author);
        println!("timestamp {}", commit.timestamp);
        if !commit.metadata.is_empty() {
            println!("\nmetadata:");
            for (key, value) in &commit.metadata {
                println!("  {key}: {value}");
            }
        }
        println!("\n{}", commit.message);
    }
    Ok(())
}
