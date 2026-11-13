//! Talks to the helper on the host: one request, one JSON response. The host
//! is reached over SSH, or directly when it is this machine.

use crate::config::{self, Target, LOCAL_HOST};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// The remote side is shipped inside this binary and copied over on first use.
const REMOTE_FILES: &[(&str, &str)] = &[
    ("bs_remote.py", include_str!("../remote/bs_remote.py")),
    ("session.sh", include_str!("../remote/session.sh")),
    ("setup.sh", include_str!("../remote/setup.sh")),
];

pub struct Remote {
    target: Target,
}

impl Remote {
    pub fn new(target: Target) -> Self {
        Self { target }
    }

    /// A command that runs `shell_command` on the host. Over SSH one master
    /// connection is reused by every call, which phase 0 measured as cutting
    /// the per-command overhead to about a fifth.
    fn shell(&self, shell_command: &str) -> Result<Command> {
        if self.target.host == LOCAL_HOST {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(shell_command);
            return Ok(cmd);
        }
        let control = config::state_dir()?.join("cm-%C");
        let mut cmd = Command::new("ssh");
        cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
            .args(["-o", "ControlMaster=auto", "-o", "ControlPersist=600"])
            .arg("-o")
            .arg(format!("ControlPath={}", control.display()))
            .arg(&self.target.host)
            .arg(shell_command);
        Ok(cmd)
    }

    fn run(&self, remote_command: &str, stdin: &[u8]) -> Result<(bool, Vec<u8>, String)> {
        let mut child = self
            .shell(remote_command)?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run the command on the host")?;
        child.stdin.take().unwrap().write_all(stdin)?;
        let out = child.wait_with_output()?;
        Ok((
            out.status.success(),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }

    /// Marker file recording that this build's remote files are on this host.
    fn deployed_marker(&self) -> Result<std::path::PathBuf> {
        let mut hash: u64 = 0xcbf29ce484222325;
        for (_, content) in REMOTE_FILES {
            for byte in content.bytes() {
                hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
            }
        }
        let dir = self.target.remote_dir.replace(['/', '~'], "_");
        Ok(config::state_dir()?.join(format!(
            "deployed-{}-{}-{:016x}",
            self.target.host, dir, hash
        )))
    }

    /// Copy the helper scripts to the remote directory.
    pub fn deploy(&self) -> Result<()> {
        let dir = &self.target.remote_dir;
        for (name, content) in REMOTE_FILES {
            let command = format!("mkdir -p {dir} && cat > {dir}/{name} && chmod 700 {dir}/{name}");
            let (ok, _, stderr) = self.run(&command, content.as_bytes())?;
            if !ok {
                bail!("cannot copy {name} to {}:{dir}: {stderr}", self.target.host);
            }
        }
        std::fs::write(self.deployed_marker()?, b"")?;
        Ok(())
    }

    /// Install the remote tools, showing the progress of the installer as it runs.
    pub fn setup(&self) -> Result<bool> {
        self.deploy()?;
        let status = self
            .shell(&format!("bash {}/setup.sh", self.target.remote_dir))?
            .stdin(Stdio::null())
            .status()
            .context("cannot run the command on the host")?;
        Ok(status.success())
    }

    /// Send one request to the helper and return its response object.
    pub fn call(&self, mut request: Value) -> Result<Value> {
        if !self.deployed_marker()?.exists() {
            self.deploy()?;
        }
        request["geometry"] = Value::String(self.target.geometry.clone());
        request["session"] = Value::String(self.target.session.clone());
        let command = format!("python3 {}/bs_remote.py", self.target.remote_dir);
        let payload = serde_json::to_vec(&request)?;
        let (_, stdout, stderr) = self.run(&command, &payload)?;
        let response: Value = serde_json::from_slice(&stdout).map_err(|_| {
            anyhow!(
                "no valid response from {}: {}",
                self.target.host,
                if stderr.is_empty() {
                    "empty output".to_string()
                } else {
                    stderr
                }
            )
        })?;
        if response["ok"].as_bool() != Some(true) {
            bail!("{}", response["error"].as_str().unwrap_or("remote error"));
        }
        Ok(response)
    }
}
