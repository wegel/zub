//! SSH transport for remote repository operations
//!
//! uses the `zub-remote` helper on the remote side (similar to git-receive-pack)

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::error::Result;
use crate::hash::Hash;
use crate::transport::local::ObjectSet;

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

        let mut child = spawn_remote(&host, user.as_deref(), repo_path)?;

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
    pub fn send_object(&mut self, obj_type: &str, hash: &Hash, data: &[u8]) -> Result<()> {
        let header = format!("object {} {} {}\n", obj_type, hash, data.len());
        self.send_raw(&header)?;

        self.writer.write_all(data).map_err(|e| crate::Error::Transport {
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
    /// returns (type, hash, data, mode) where mode is file permissions for blobs
    pub fn receive_object(&mut self) -> Result<Option<(String, Hash, Vec<u8>, u32)>> {
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

        // parse "object TYPE HASH SIZE MODE"
        let parts: Vec<&str> = line.splitn(5, ' ').collect();
        if parts.len() < 4 || parts[0] != "object" {
            return Err(crate::Error::Transport {
                message: format!("unexpected response: {}", line),
            });
        }

        let obj_type = parts[1].to_string();
        let hash = Hash::from_hex(parts[2])?;
        let size: usize = parts[3].parse().map_err(|_| crate::Error::Transport {
            message: format!("invalid size: {}", parts[3]),
        })?;
        // mode is optional for backwards compat, default to 0644
        let mode: u32 = parts
            .get(4)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0o644);

        let mut data = vec![0u8; size];
        self.reader
            .read_exact(&mut data)
            .map_err(|e| crate::Error::Transport {
                message: format!("failed to read object data: {}", e),
            })?;

        Ok(Some((obj_type, hash, data, mode)))
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

            if line.starts_with("error:") {
                return Err(crate::Error::Transport {
                    message: line[6..].trim().to_string(),
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
    // TODO: this assumes the remote has the same architecture as the local machine.
    // in the future, we could detect the remote arch and either:
    // - download the correct binary from a release
    // - refuse with a helpful error message
    let local_exe = std::env::current_exe().map_err(|e| crate::Error::Transport {
        message: format!("failed to get current executable path: {}", e),
    })?;

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

fn spawn_remote(host: &str, user: Option<&str>, repo_path: &Path) -> Result<std::process::Child> {
    let mut cmd = Command::new("ssh");

    if let Some(u) = user {
        cmd.arg("-l").arg(u);
    }

    cmd.arg(host);
    // try zub in PATH first, fall back to deployed location
    cmd.arg(format!(
        "$(command -v zub || echo {}) zub-remote {}",
        REMOTE_ZUB_PATH,
        repo_path.display()
    ));

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    cmd.spawn().map_err(|e| crate::Error::Transport {
        message: format!("failed to spawn ssh: {}", e),
    })
}

// note: SSH transport tests require a remote server, so they're integration tests
