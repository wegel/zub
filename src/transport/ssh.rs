//! SSH transport for remote repository operations
//!
//! uses the `zub-remote` helper on the remote side (similar to git-receive-pack)

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::error::Result;
use crate::hash::Hash;
use crate::transport::local::{BlobMetadata, ObjectKind, ObjectSet, TransferObject};

pub(crate) const PROTOCOL_VERSION: u32 = 2;

/// SSH connection to a remote repository
pub struct SshConnection {
    child: Child,
    reader: BufReader<ChildStdout>,
    writer: ChildStdin,
}

impl SshConnection {
    /// connect to a remote repository via SSH
    pub fn connect(remote: &str, repo_path: &Path) -> Result<Self> {
        // parse remote in format user@host or just host
        let (host, user) = parse_remote(remote);

        // first, check if zub exists on the remote
        if !check_remote_zub(&host, user.as_deref())? {
            deploy_zub_to_remote(&host, user.as_deref())?;
        }

        let child = spawn_remote(&host, user.as_deref(), repo_path, false)?;
        let mut connection = Self::from_child(child)?;
        if connection.protocol_version().ok() == Some(PROTOCOL_VERSION) {
            return Ok(connection);
        }

        drop(connection);
        deploy_zub_to_remote(&host, user.as_deref())?;
        let child = spawn_remote(&host, user.as_deref(), repo_path, true)?;
        let mut connection = Self::from_child(child)?;
        let actual = connection.protocol_version()?;
        if actual != PROTOCOL_VERSION {
            return Err(crate::Error::Transport {
                message: format!(
                    "remote protocol version {actual} does not match required {PROTOCOL_VERSION}"
                ),
            });
        }
        Ok(connection)
    }

    fn from_child(mut child: Child) -> Result<Self> {
        let stdout = child.stdout.take().ok_or_else(|| crate::Error::Transport {
            message: "stdout not available".to_string(),
        })?;
        let stdin = child.stdin.take().ok_or_else(|| crate::Error::Transport {
            message: "stdin not available".to_string(),
        })?;

        Ok(Self {
            child,
            reader: BufReader::new(stdout),
            writer: stdin,
        })
    }

    fn protocol_version(&mut self) -> Result<u32> {
        self.send_command("protocol-version")?;
        let response = self.read_response()?;
        response
            .trim()
            .parse()
            .map_err(|_| crate::Error::Transport {
                message: format!("invalid remote protocol version {response:?}"),
            })
    }

    /// list refs on the remote
    pub fn list_refs(&mut self) -> Result<Vec<(String, Hash)>> {
        self.send_command("list-refs")?;
        let response = self.read_response()?;

        let mut refs = Vec::new();
        for line in response.lines() {
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() == 2 {
                if let Ok(hash) = Hash::from_hex(parts[0]) {
                    refs.push((parts[1].to_string(), hash));
                }
            }
        }

        Ok(refs)
    }

    /// check which objects the remote needs
    pub fn want_objects(&mut self, objects: &ObjectSet) -> Result<ObjectSet> {
        let mut request = String::from("want-objects\n");

        for hash in &objects.blobs {
            request.push_str(&format!("blob {}\n", hash));
        }
        for hash in &objects.trees {
            request.push_str(&format!("tree {}\n", hash));
        }
        for hash in &objects.commits {
            request.push_str(&format!("commit {}\n", hash));
        }
        request.push_str("end\n");

        self.send_raw(&request)?;
        let response = self.read_response()?;

        let mut needed = ObjectSet::new();
        for line in response.lines() {
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() == 2 {
                if let Ok(hash) = Hash::from_hex(parts[1]) {
                    match parts[0] {
                        "blob" => needed.blobs.push(hash),
                        "tree" => needed.trees.push(hash),
                        "commit" => needed.commits.push(hash),
                        _ => {}
                    }
                }
            }
        }

        Ok(needed)
    }

    /// send an object to the remote
    pub(crate) fn send_object(&mut self, object: &TransferObject) -> Result<()> {
        let metadata = object.metadata.unwrap_or(BlobMetadata {
            uid: 0,
            gid: 0,
            mode: 0,
        });
        let header = format!(
            "object {} {} {} {} {} {}\n",
            object.kind,
            object.hash,
            object.data.len(),
            metadata.uid,
            metadata.gid,
            metadata.mode
        );
        self.send_raw(&header)?;

        self.writer
            .write_all(&object.data)
            .map_err(|e| crate::Error::Transport {
                message: format!("failed to write object: {}", e),
            })?;

        self.expect_ok()
    }

    /// update a ref on the remote
    pub fn update_ref(&mut self, name: &str, hash: &Hash) -> Result<()> {
        self.send_command(&format!("update-ref {} {}", name, hash))?;
        self.expect_ok()
    }

    /// request objects from remote (for pull)
    pub fn have_objects(&mut self, objects: &ObjectSet) -> Result<ObjectSet> {
        let mut request = String::from("have-objects\n");

        for hash in &objects.blobs {
            request.push_str(&format!("blob {}\n", hash));
        }
        for hash in &objects.trees {
            request.push_str(&format!("tree {}\n", hash));
        }
        for hash in &objects.commits {
            request.push_str(&format!("commit {}\n", hash));
        }
        request.push_str("end\n");

        self.send_raw(&request)?;
        let response = self.read_response()?;

        let mut missing = ObjectSet::new();
        for line in response.lines() {
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() == 2 {
                if let Ok(hash) = Hash::from_hex(parts[1]) {
                    match parts[0] {
                        "blob" => missing.blobs.push(hash),
                        "tree" => missing.trees.push(hash),
                        "commit" => missing.commits.push(hash),
                        _ => {}
                    }
                }
            }
        }

        Ok(missing)
    }

    /// receive an object from the remote
    pub(crate) fn receive_object(&mut self) -> Result<Option<TransferObject>> {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .map_err(|e| crate::Error::Transport {
                message: format!("failed to read: {}", e),
            })?;

        let line = line.trim();
        if line == "end" {
            return Ok(None);
        }

        // parse "object TYPE HASH SIZE UID GID MODE"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 7 || parts[0] != "object" {
            return Err(crate::Error::Transport {
                message: format!("unexpected response: {}", line),
            });
        }

        let kind = ObjectKind::parse(parts[1])?;
        let hash = Hash::from_hex(parts[2])?;
        let size: usize = parts[3].parse().map_err(|_| crate::Error::Transport {
            message: format!("invalid size: {}", parts[3]),
        })?;
        let uid = parse_metadata(parts[4], "uid")?;
        let gid = parse_metadata(parts[5], "gid")?;
        let mode = parse_metadata(parts[6], "mode")?;

        let mut data = vec![0u8; size];
        self.reader
            .read_exact(&mut data)
            .map_err(|e| crate::Error::Transport {
                message: format!("failed to read object data: {}", e),
            })?;

        Ok(Some(TransferObject {
            kind,
            hash,
            data,
            metadata: (kind == ObjectKind::Blob).then_some(BlobMetadata { uid, gid, mode }),
        }))
    }

    /// request ref value from remote
    pub fn get_ref(&mut self, name: &str) -> Result<Option<Hash>> {
        self.send_command(&format!("get-ref {}", name))?;
        let response = self.read_response()?;

        if response.trim().is_empty() || response.trim() == "not-found" {
            return Ok(None);
        }

        Hash::from_hex(response.trim()).map(Some)
    }

    /// close the connection
    pub fn close(mut self) -> Result<()> {
        let _ = self.send_command("quit");
        let _ = self.child.wait();
        Ok(())
    }

    fn send_command(&mut self, cmd: &str) -> Result<()> {
        self.send_raw(&format!("{}\n", cmd))
    }

    fn send_raw(&mut self, data: &str) -> Result<()> {
        self.writer
            .write_all(data.as_bytes())
            .map_err(|e| crate::Error::Transport {
                message: format!("failed to write: {}", e),
            })?;

        self.writer.flush().map_err(|e| crate::Error::Transport {
            message: format!("failed to flush: {}", e),
        })
    }

    fn read_response(&mut self) -> Result<String> {
        let mut response = String::new();

        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| crate::Error::Transport {
                    message: format!("failed to read: {}", e),
                })?;

            if n == 0 {
                break;
            }

            if line.trim() == "end" {
                break;
            }

            if let Some(message) = line.strip_prefix("error:") {
                return Err(crate::Error::Transport {
                    message: message.trim().to_string(),
                });
            }

            response.push_str(&line);
        }

        Ok(response)
    }

    fn expect_ok(&mut self) -> Result<()> {
        let response = self.read_response()?;
        if response.trim() == "ok" {
            Ok(())
        } else {
            Err(crate::Error::Transport {
                message: format!("expected 'ok', got: {}", response),
            })
        }
    }
}

fn parse_metadata(value: &str, field: &str) -> Result<u32> {
    value.parse().map_err(|_| crate::Error::Transport {
        message: format!("invalid object {field}: {value}"),
    })
}

impl Drop for SshConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn parse_remote(remote: &str) -> (String, Option<String>) {
    if remote.contains('@') {
        let parts: Vec<&str> = remote.splitn(2, '@').collect();
        (parts[1].to_string(), Some(parts[0].to_string()))
    } else {
        (remote.to_string(), None)
    }
}

// deployed binary path: use $TMPDIR if set, otherwise ~/.cache
const REMOTE_ZUB_PATH: &str = "${TMPDIR:-$HOME/.cache}/zub_auto_deployed";

fn check_remote_zub(host: &str, user: Option<&str>) -> Result<bool> {
    let mut cmd = Command::new("ssh");
    if let Some(u) = user {
        cmd.arg("-l").arg(u);
    }
    cmd.arg(host);
    // check both PATH and our deploy location
    cmd.arg(format!(
        "command -v zub >/dev/null 2>&1 || test -x {}",
        REMOTE_ZUB_PATH
    ));

    let status = cmd.status().map_err(|e| crate::Error::Transport {
        message: format!("failed to check remote zub: {}", e),
    })?;

    Ok(status.success())
}

fn deploy_zub_to_remote(host: &str, user: Option<&str>) -> Result<()> {
    let local_exe = local_zub_binary()?;

    // get the resolved remote path
    let resolved_path = get_resolved_remote_path(host, user)?;

    let remote_target = if let Some(u) = user {
        format!("{}@{}:{}", u, host, resolved_path)
    } else {
        format!("{}:{}", host, resolved_path)
    };

    // ensure parent directory exists on remote
    let mut mkdir_cmd = Command::new("ssh");
    if let Some(u) = user {
        mkdir_cmd.arg("-l").arg(u);
    }
    mkdir_cmd.arg(host);
    mkdir_cmd.arg(format!("mkdir -p \"$(dirname {})\"", REMOTE_ZUB_PATH));

    let status = mkdir_cmd.status().map_err(|e| crate::Error::Transport {
        message: format!("failed to create remote directory: {}", e),
    })?;

    if !status.success() {
        return Err(crate::Error::Transport {
            message: "failed to create directory on remote".to_string(),
        });
    }

    // copy the binary
    let status = Command::new("scp")
        .arg(&local_exe)
        .arg(&remote_target)
        .status()
        .map_err(|e| crate::Error::Transport {
            message: format!("failed to copy zub to remote: {}", e),
        })?;

    if !status.success() {
        return Err(crate::Error::Transport {
            message: "failed to copy zub binary to remote".to_string(),
        });
    }

    // make it executable
    let mut chmod_cmd = Command::new("ssh");
    if let Some(u) = user {
        chmod_cmd.arg("-l").arg(u);
    }
    chmod_cmd.arg(host);
    chmod_cmd.arg(format!("chmod +x {}", REMOTE_ZUB_PATH));

    let status = chmod_cmd.status().map_err(|e| crate::Error::Transport {
        message: format!("failed to chmod zub on remote: {}", e),
    })?;

    if !status.success() {
        return Err(crate::Error::Transport {
            message: "failed to make zub executable on remote".to_string(),
        });
    }

    eprintln!("deployed zub to remote {}", resolved_path);
    Ok(())
}

fn local_zub_binary() -> Result<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("ZUB_BINARY") {
        let path = std::path::PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(crate::Error::Transport {
            message: format!("ZUB_BINARY does not name a file: {}", path.display()),
        });
    }

    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("zub");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    let current = std::env::current_exe().map_err(|error| crate::Error::Transport {
        message: format!("failed to locate the current executable: {error}"),
    })?;
    if current.file_name().is_some_and(|name| name == "zub") {
        return Ok(current);
    }

    Err(crate::Error::Transport {
        message: "cannot deploy a compatible remote helper; put zub in PATH or set ZUB_BINARY"
            .to_string(),
    })
}

fn get_resolved_remote_path(host: &str, user: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("ssh");
    if let Some(u) = user {
        cmd.arg("-l").arg(u);
    }
    cmd.arg(host);
    cmd.arg(format!("echo {}", REMOTE_ZUB_PATH));

    let output = cmd.output().map_err(|e| crate::Error::Transport {
        message: format!("failed to resolve remote path: {}", e),
    })?;

    if !output.status.success() {
        return Err(crate::Error::Transport {
            message: "failed to resolve remote path".to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn spawn_remote(
    host: &str,
    user: Option<&str>,
    repo_path: &Path,
    force_deployed: bool,
) -> Result<std::process::Child> {
    let mut cmd = Command::new("ssh");

    if let Some(u) = user {
        cmd.arg("-l").arg(u);
    }

    cmd.arg(host);
    // try zub in PATH first, fall back to deployed location
    let executable = if force_deployed {
        REMOTE_ZUB_PATH.to_string()
    } else {
        format!("$(command -v zub || echo {REMOTE_ZUB_PATH})")
    };
    cmd.arg(format!("{executable} zub-remote {}", repo_path.display()));

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    cmd.spawn().map_err(|e| crate::Error::Transport {
        message: format!("failed to spawn ssh: {}", e),
    })
}

// note: SSH transport tests require a remote server, so they're integration tests
