//! server-side remote helper for SSH transport
//!
//! implements the protocol that responds to pull/push requests from remote clients

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};

use crate::hash::Hash;
use crate::object::{read_commit, read_tree, verify_commit};
use crate::refs::{list_refs, read_ref, write_ref};
use crate::repo::Repo;
use crate::repo::RepoLock;
use crate::transport::local::{
    install_transfer_object, read_transfer_object, remove_objects, ObjectKind, ObjectSet,
    TransferObject,
};
use crate::transport::ssh::PROTOCOL_VERSION;
use crate::types::EntryKind;
use crate::Result;

/// serve the remote helper protocol on stdin/stdout.
/// used by SSH transport when `zub zub-remote` or similar is invoked.
pub fn serve_remote(repo: &Repo) -> Result<()> {
    let stdin = std::io::stdin();
    let reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    serve_remote_io(repo, reader, stdout)
}

fn serve_remote_io(repo: &Repo, mut reader: impl BufRead, mut stdout: impl Write) -> Result<()> {
    // track the last requested ref for have-objects
    let mut last_ref_hash: Option<Hash> = None;
    let mut received = ReceiveSession::default();

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
            "protocol-version" => {
                writeln!(stdout, "{PROTOCOL_VERSION}").map_err(io_err)?;
                write_end(&mut stdout)?;
            }

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
                received.lock(repo)?;
                match receive_object(args, &mut reader).and_then(|object| {
                    install_transfer_object(repo, &object).map(|new| (object, new))
                }) {
                    Ok((object, true)) => {
                        received.objects.push(object.kind, object.hash);
                        write_ok(&mut stdout)?;
                    }
                    Ok((_, false)) => write_ok(&mut stdout)?,
                    Err(error) => {
                        received.rollback(repo)?;
                        write_error(&mut stdout, &error.to_string())?;
                    }
                }
            }

            "update-ref" => {
                received.lock(repo)?;
                match update_ref(repo, args) {
                    Ok(()) => {
                        received.commit();
                        write_ok(&mut stdout)?;
                    }
                    Err(error) => {
                        received.rollback(repo)?;
                        write_error(&mut stdout, &error.to_string())?;
                    }
                }
            }

            "quit" => {
                received.rollback(repo)?;
                break;
            }

            _ => {
                write_error(&mut stdout, &format!("unknown command: {}", cmd))?;
            }
        }
    }

    received.rollback(repo)?;
    Ok(())
}

#[derive(Default)]
struct ReceiveSession {
    objects: ObjectSet,
    lock: Option<RepoLock>,
}

impl ReceiveSession {
    fn lock(&mut self, repo: &Repo) -> Result<()> {
        if self.lock.is_none() {
            self.lock = Some(repo.lock()?);
        }
        Ok(())
    }

    fn commit(&mut self) {
        self.objects = ObjectSet::new();
        self.lock = None;
    }

    fn rollback(&mut self, repo: &Repo) -> Result<()> {
        remove_objects(repo, &self.objects)?;
        self.commit();
        Ok(())
    }
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
        verify_commit(repo, commit_hash)?;
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
        let object = read_transfer_object(repo, ObjectKind::parse(obj_type)?, hash)?;
        let metadata = object
            .metadata
            .unwrap_or(crate::transport::local::BlobMetadata {
                uid: 0,
                gid: 0,
                mode: 0,
            });
        writeln!(
            stdout,
            "object {} {} {} {} {} {}",
            object.kind,
            object.hash,
            object.data.len(),
            metadata.uid,
            metadata.gid,
            metadata.mode
        )
        .map_err(io_err)?;
        stdout.write_all(&object.data).map_err(io_err)?;
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

fn receive_object(args: &str, reader: &mut impl BufRead) -> Result<TransferObject> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 6 {
        return Err(crate::Error::Transport {
            message: "invalid object header".to_string(),
        });
    }

    let kind = ObjectKind::parse(parts[0])?;
    let hash = Hash::from_hex(parts[1])?;
    let size = parse_number(parts[2], "size")? as usize;
    let uid = parse_number(parts[3], "uid")? as u32;
    let gid = parse_number(parts[4], "gid")? as u32;
    let mode = parse_number(parts[5], "mode")? as u32;

    let mut data = vec![0u8; size];
    reader.read_exact(&mut data).map_err(|e| crate::Error::Io {
        path: "stdin".into(),
        source: e,
    })?;

    Ok(TransferObject {
        kind,
        hash,
        data,
        metadata: (kind == ObjectKind::Blob).then_some(crate::transport::local::BlobMetadata {
            uid,
            gid,
            mode,
        }),
    })
}

fn update_ref(repo: &Repo, args: &str) -> Result<()> {
    let ref_parts: Vec<&str> = args.splitn(2, ' ').collect();
    if ref_parts.len() != 2 {
        return Err(crate::Error::Transport {
            message: "invalid update-ref arguments".to_string(),
        });
    }

    let ref_name = ref_parts[0];
    let hash = Hash::from_hex(ref_parts[1])?;
    verify_commit(repo, &hash)?;
    write_ref(repo, ref_name, &hash)
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
            EntryKind::Regular { hash, .. } | EntryKind::Symlink { hash, .. } => {
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
    ObjectKind::parse(obj_type)
        .map(|kind| crate::transport::local::object_path(repo, kind, hash).exists())
        .unwrap_or(false)
}

fn parse_number(value: &str, field: &str) -> Result<u64> {
    value.parse().map_err(|_| crate::Error::Transport {
        message: format!("invalid object {field}: {value}"),
    })
}

fn write_ok(stdout: &mut impl Write) -> Result<()> {
    writeln!(stdout, "ok").map_err(io_err)?;
    write_end(stdout)
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::ops::commit;
    use crate::transport::local::list_all_objects;
    use crate::{MapEntry, NsConfig};

    #[test]
    fn ssh_protocol_round_trip_preserves_blob_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let mut source = Repo::init(&temporary.path().join("source-repo")).unwrap();
        let mut destination = Repo::init(&temporary.path().join("destination-repo")).unwrap();
        configure_test_namespace(&mut source);
        configure_test_namespace(&mut destination);
        let source_path = temporary.path().join("source");
        fs::create_dir(&source_path).unwrap();
        let probe = source_path.join("probe");
        fs::write(&probe, b"payload").unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o751)).unwrap();
        xattr::set(&probe, "user.zub-test", b"metadata").unwrap();
        let commit_hash = commit(&source, &source_path, "test", Some("fixture"), None).unwrap();

        let objects = transfer_objects(&source, &commit_hash);
        let input = push_input(&objects, "test", commit_hash);
        let mut output = Vec::new();
        serve_remote_io(&destination, Cursor::new(input), &mut output).unwrap();

        assert_eq!(read_ref(&destination, "test").unwrap(), commit_hash);
        verify_commit(&destination, &commit_hash).unwrap();
        assert!(String::from_utf8_lossy(&output).contains(&format!("{PROTOCOL_VERSION}\nend")));
    }

    #[test]
    fn ssh_protocol_rejects_corrupt_blob_and_rolls_back_objects() {
        let temporary = tempfile::tempdir().unwrap();
        let source = Repo::init(&temporary.path().join("source-repo")).unwrap();
        let destination = Repo::init(&temporary.path().join("destination-repo")).unwrap();
        let source_path = temporary.path().join("source");
        fs::create_dir(&source_path).unwrap();
        fs::write(source_path.join("probe"), b"payload").unwrap();
        let commit_hash = commit(&source, &source_path, "test", Some("fixture"), None).unwrap();
        let mut objects = transfer_objects(&source, &commit_hash);
        let blob = objects
            .iter_mut()
            .find(|object| object.kind == ObjectKind::Blob)
            .unwrap();
        blob.data = b"corrupt".to_vec();

        let input = push_input(&objects, "test", commit_hash);
        let mut output = Vec::new();
        serve_remote_io(&destination, Cursor::new(input), &mut output).unwrap();

        assert!(matches!(
            read_ref(&destination, "test"),
            Err(crate::Error::RefNotFound(_))
        ));
        assert!(list_all_objects(&destination).unwrap().is_empty());
        assert!(String::from_utf8_lossy(&output).contains("corrupt object"));
    }

    #[test]
    fn ssh_pull_frames_include_logical_blob_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let mut source = Repo::init(&temporary.path().join("source-repo")).unwrap();
        configure_test_namespace(&mut source);
        let source_path = temporary.path().join("source");
        fs::create_dir(&source_path).unwrap();
        let probe = source_path.join("probe");
        fs::write(&probe, b"payload").unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o751)).unwrap();
        let commit_hash = commit(&source, &source_path, "test", Some("fixture"), None).unwrap();
        let blob = transfer_objects(&source, &commit_hash)
            .into_iter()
            .find(|object| object.kind == ObjectKind::Blob)
            .unwrap();
        let metadata = blob.metadata.unwrap();
        let expected = format!(
            "object blob {} {} {} {} {}\n",
            blob.hash,
            blob.data.len(),
            metadata.uid,
            metadata.gid,
            metadata.mode
        );
        let input = "protocol-version\nget-ref test\nhave-objects\nend\nquit\n".to_string();
        let mut output = Vec::new();
        serve_remote_io(&source, Cursor::new(input.into_bytes()), &mut output).unwrap();

        assert!(contains(&output, expected.as_bytes()));
        assert_eq!((metadata.uid, metadata.gid), (37, 43));
    }

    fn configure_test_namespace(repo: &mut Repo) {
        repo.config_mut().namespace = NsConfig {
            uid_map: vec![MapEntry::new(37, nix::unistd::getuid().as_raw(), 1)],
            gid_map: vec![MapEntry::new(43, nix::unistd::getgid().as_raw(), 1)],
        };
        repo.save_config().unwrap();
    }

    fn transfer_objects(repo: &Repo, commit_hash: &Hash) -> Vec<TransferObject> {
        let mut references = Vec::new();
        collect_commit_objects(repo, commit_hash, &mut references, &mut HashSet::new()).unwrap();
        references
            .into_iter()
            .map(|(kind, hash)| {
                read_transfer_object(repo, ObjectKind::parse(&kind).unwrap(), &hash).unwrap()
            })
            .collect()
    }

    fn push_input(objects: &[TransferObject], ref_name: &str, commit_hash: Hash) -> Vec<u8> {
        let mut input = b"protocol-version\n".to_vec();
        for object in objects {
            let metadata = object
                .metadata
                .unwrap_or(crate::transport::local::BlobMetadata {
                    uid: 0,
                    gid: 0,
                    mode: 0,
                });
            writeln!(
                input,
                "object {} {} {} {} {} {}",
                object.kind,
                object.hash,
                object.data.len(),
                metadata.uid,
                metadata.gid,
                metadata.mode
            )
            .unwrap();
            input.extend_from_slice(&object.data);
        }
        write!(input, "update-ref {ref_name} {commit_hash}\nquit\n").unwrap();
        input
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
