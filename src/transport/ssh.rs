//! SSH transport for remote repository operations
//!
//! uses the `zub-remote` helper on the remote side (similar to git-receive-pack)

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::error::Result;
use crate::hash::Hash;
use crate::transport::local::ObjectSet;

/// SSH connection to a remote repository
pub struct SshConnection {
    child: Child,
}

impl SshConnection {
    /// connect to a remote repository via SSH
    pub fn connect(remote: &str, repo_path: &Path) -> Result<Self> {
        // parse remote in format user@host or just host
        let (host, user) = if remote.contains('@') {
            let parts: Vec<&str> = remote.splitn(2, '@').collect();
            (parts[1].to_string(), Some(parts[0].to_string()))
        } else {
            (remote.to_string(), None)
        };

        let mut cmd = Command::new("ssh");

        if let Some(user) = user {
            cmd.arg("-l").arg(user);
        }

        cmd.arg(&host);
        cmd.arg("zub-remote");
        cmd.arg(repo_path);

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());

        let child = cmd.spawn().map_err(|e| crate::Error::Transport {
            message: format!("failed to spawn ssh: {}", e),
        })?;

        Ok(Self { child })
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

        let stdin = self.child.stdin.as_mut().ok_or_else(|| crate::Error::Transport {
            message: "stdin not available".to_string(),
        })?;

        stdin.write_all(data).map_err(|e| crate::Error::Transport {
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
    pub fn receive_object(&mut self) -> Result<Option<(String, Hash, Vec<u8>)>> {
        let stdout = self.child.stdout.as_mut().ok_or_else(|| crate::Error::Transport {
            message: "stdout not available".to_string(),
        })?;

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| crate::Error::Transport {
            message: format!("failed to read: {}", e),
        })?;

        let line = line.trim();
        if line == "end" {
            return Ok(None);
        }

        // parse "object TYPE HASH SIZE"
        let parts: Vec<&str> = line.splitn(4, ' ').collect();
        if parts.len() != 4 || parts[0] != "object" {
            return Err(crate::Error::Transport {
                message: format!("unexpected response: {}", line),
            });
        }

        let obj_type = parts[1].to_string();
        let hash = Hash::from_hex(parts[2])?;
        let size: usize = parts[3].parse().map_err(|_| crate::Error::Transport {
            message: format!("invalid size: {}", parts[3]),
        })?;

        let mut data = vec![0u8; size];
        reader.read_exact(&mut data).map_err(|e| crate::Error::Transport {
            message: format!("failed to read object data: {}", e),
        })?;

        Ok(Some((obj_type, hash, data)))
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
        let stdin = self.child.stdin.as_mut().ok_or_else(|| crate::Error::Transport {
            message: "stdin not available".to_string(),
        })?;

        stdin.write_all(data.as_bytes()).map_err(|e| crate::Error::Transport {
            message: format!("failed to write: {}", e),
        })?;

        stdin.flush().map_err(|e| crate::Error::Transport {
            message: format!("failed to flush: {}", e),
        })
    }

    fn read_response(&mut self) -> Result<String> {
        let stdout = self.child.stdout.as_mut().ok_or_else(|| crate::Error::Transport {
            message: "stdout not available".to_string(),
        })?;

        let mut reader = BufReader::new(stdout);
        let mut response = String::new();

        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).map_err(|e| crate::Error::Transport {
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

// note: SSH transport tests require a remote server, so they're integration tests
