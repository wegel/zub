//! server-side remote helper for SSH transport
//!
//! implements the protocol that responds to pull/push requests from remote clients

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::hash::Hash;
use crate::object::{read_commit, read_tree};
use crate::refs::{list_refs, read_ref, write_ref};
use crate::repo::Repo;
use crate::types::EntryKind;
use crate::Result;

/// serve the remote helper protocol on stdin/stdout.
/// used by SSH transport when `zub zub-remote` or similar is invoked.
pub fn serve_remote(repo: &Repo) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();

    // track the last requested ref for have-objects
    let mut last_ref_hash: Option<Hash> = None;

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
                handle_list_refs(repo, &mut stdout)?;
            }

            "get-ref" => {
                last_ref_hash = handle_get_ref(repo, args, &mut stdout)?;
            }

            "have-objects" => {
                handle_have_objects(repo, &mut reader, &mut stdout, last_ref_hash.as_ref())?;
            }

            "want-objects" => {
                handle_want_objects(repo, &mut reader, &mut stdout)?;
            }

            "object" => {
                handle_receive_object(repo, args, &mut reader, &mut stdout)?;
            }

            "update-ref" => {
                handle_update_ref(repo, args, &mut stdout)?;
            }

            "quit" => {
                break;
            }

            _ => {
                write_error(&mut stdout, &format!("unknown command: {}", cmd))?;
            }
        }
    }

    Ok(())
}

fn handle_list_refs(repo: &Repo, stdout: &mut impl Write) -> Result<()> {
    let refs = list_refs(repo)?;
    for ref_name in refs {
        let hash = read_ref(repo, &ref_name)?;
        writeln!(stdout, "{} {}", hash, ref_name).map_err(io_err)?;
    }
    write_end(stdout)
}

fn handle_get_ref(repo: &Repo, ref_name: &str, stdout: &mut impl Write) -> Result<Option<Hash>> {
    match read_ref(repo, ref_name) {
        Ok(hash) => {
            writeln!(stdout, "{}", hash).map_err(io_err)?;
            write_end(stdout)?;
            Ok(Some(hash))
        }
        Err(_) => {
            writeln!(stdout, "not-found").map_err(io_err)?;
            write_end(stdout)?;
            Ok(None)
        }
    }
}

fn handle_have_objects(
    repo: &Repo,
    reader: &mut impl BufRead,
    stdout: &mut impl Write,
    last_ref_hash: Option<&Hash>,
) -> Result<()> {
    // read what client has
    let mut client_has: HashSet<Hash> = HashSet::new();
    loop {
        let mut obj_line = String::new();
        reader.read_line(&mut obj_line).unwrap_or(0);
        let obj_line = obj_line.trim();
        if obj_line == "end" {
            break;
        }
        let obj_parts: Vec<&str> = obj_line.splitn(2, ' ').collect();
        if obj_parts.len() == 2 {
            if let Ok(hash) = Hash::from_hex(obj_parts[1]) {
                client_has.insert(hash);
            }
        }
    }

    // find what client needs from the last requested ref
    let mut to_send: Vec<(String, Hash)> = Vec::new();

    if let Some(commit_hash) = last_ref_hash {
        // walk the commit tree to find all needed objects
        let mut needed = Vec::new();
        let mut visited = HashSet::new();
        collect_commit_objects(repo, commit_hash, &mut needed, &mut visited)?;

        // filter to only what client doesn't have
        for (obj_type, hash) in needed {
            if !client_has.contains(&hash) {
                to_send.push((obj_type, hash));
            }
        }
    }

    // report what client is missing
    for (obj_type, hash) in &to_send {
        writeln!(stdout, "{} {}", obj_type, hash).map_err(io_err)?;
    }
    write_end(stdout)?;

    // now send the actual objects
    for (obj_type, hash) in &to_send {
        let (data, mode) = read_object_data_with_mode(repo, obj_type, hash)?;
        writeln!(stdout, "object {} {} {} {}", obj_type, hash, data.len(), mode).map_err(io_err)?;
        stdout.write_all(&data).map_err(io_err)?;
    }
    write_end(stdout)?;

    Ok(())
}

fn handle_want_objects(
    repo: &Repo,
    reader: &mut impl BufRead,
    stdout: &mut impl Write,
) -> Result<()> {
    // read object list, report what we don't have (for push)
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
                if !object_exists(repo, obj_type, &hash) {
                    needed.push((obj_type.to_string(), hash));
                }
            }
        }
    }

    for (obj_type, hash) in needed {
        writeln!(stdout, "{} {}", obj_type, hash).map_err(io_err)?;
    }
    write_end(stdout)
}

fn handle_receive_object(
    repo: &Repo,
    args: &str,
    reader: &mut impl BufRead,
    stdout: &mut impl Write,
) -> Result<()> {
    let obj_parts: Vec<&str> = args.splitn(3, ' ').collect();
    if obj_parts.len() != 3 {
        return write_error(stdout, "invalid object args");
    }

    let obj_type = obj_parts[0];
    let hash = Hash::from_hex(obj_parts[1])?;
    let size: usize = obj_parts[2].parse().unwrap_or(0);

    let mut data = vec![0u8; size];
    reader.read_exact(&mut data).map_err(|e| crate::Error::Io {
        path: "stdin".into(),
        source: e,
    })?;

    let dest = object_path(repo, obj_type, &hash);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| crate::Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    fs::write(&dest, &data).map_err(|e| crate::Error::Io {
        path: dest,
        source: e,
    })?;

    writeln!(stdout, "ok").map_err(io_err)?;
    write_end(stdout)
}

fn handle_update_ref(repo: &Repo, args: &str, stdout: &mut impl Write) -> Result<()> {
    let ref_parts: Vec<&str> = args.splitn(2, ' ').collect();
    if ref_parts.len() != 2 {
        return write_error(stdout, "invalid update-ref args");
    }

    let ref_name = ref_parts[0];
    match Hash::from_hex(ref_parts[1]) {
        Ok(hash) => {
            write_ref(repo, ref_name, &hash)?;
            writeln!(stdout, "ok").map_err(io_err)?;
        }
        Err(_) => {
            writeln!(stdout, "error: invalid hash").map_err(io_err)?;
        }
    }
    write_end(stdout)
}

// helper: collect all objects reachable from a commit
fn collect_commit_objects(
    repo: &Repo,
    commit_hash: &Hash,
    objects: &mut Vec<(String, Hash)>,
    visited: &mut HashSet<Hash>,
) -> Result<()> {
    if visited.contains(commit_hash) {
        return Ok(());
    }
    visited.insert(*commit_hash);
    objects.push(("commit".to_string(), *commit_hash));

    let commit = read_commit(repo, commit_hash)?;
    collect_tree_objects(repo, &commit.tree, objects, visited)?;

    // don't recurse into parent commits - we only need the current tree
    Ok(())
}

fn collect_tree_objects(
    repo: &Repo,
    tree_hash: &Hash,
    objects: &mut Vec<(String, Hash)>,
    visited: &mut HashSet<Hash>,
) -> Result<()> {
    if visited.contains(tree_hash) {
        return Ok(());
    }
    visited.insert(*tree_hash);
    objects.push(("tree".to_string(), *tree_hash));

    let tree = read_tree(repo, tree_hash)?;
    for entry in tree.entries() {
        match &entry.kind {
            EntryKind::Regular { hash, .. } | EntryKind::Symlink { hash } => {
                if !visited.contains(hash) {
                    visited.insert(*hash);
                    objects.push(("blob".to_string(), *hash));
                }
            }
            EntryKind::Directory { hash, .. } => {
                collect_tree_objects(repo, hash, objects, visited)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn object_exists(repo: &Repo, obj_type: &str, hash: &Hash) -> bool {
    object_path(repo, obj_type, hash).exists()
}

fn object_path(repo: &Repo, obj_type: &str, hash: &Hash) -> PathBuf {
    let hex = hash.to_hex();
    let base = match obj_type {
        "blob" => repo.blobs_path(),
        "tree" => repo.trees_path(),
        "commit" => repo.commits_path(),
        _ => return PathBuf::new(),
    };
    base.join(&hex[..2]).join(&hex[2..])
}

fn read_object_data_with_mode(repo: &Repo, obj_type: &str, hash: &Hash) -> Result<(Vec<u8>, u32)> {
    use std::os::unix::fs::MetadataExt;
    let path = object_path(repo, obj_type, hash);
    let data = fs::read(&path).map_err(|e| crate::Error::Io {
        path: path.clone(),
        source: e,
    })?;
    let mode = if obj_type == "blob" {
        fs::metadata(&path)
            .map(|m| m.mode() & 0o7777)
            .unwrap_or(0o644)
    } else {
        0
    };
    Ok((data, mode))
}

fn write_end(stdout: &mut impl Write) -> Result<()> {
    writeln!(stdout, "end").map_err(io_err)?;
    stdout.flush().map_err(io_err)?;
    Ok(())
}

fn write_error(stdout: &mut impl Write, msg: &str) -> Result<()> {
    writeln!(stdout, "error: {}", msg).map_err(io_err)?;
    write_end(stdout)
}

fn io_err(e: std::io::Error) -> crate::Error {
    crate::Error::Io {
        path: "stdout".into(),
        source: e,
    }
}
