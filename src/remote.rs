//! Talks to the helper on the host: one request, one JSON response. The host
//! is reached over SSH, or directly when it is this machine.

use crate::config::{self, Target, LOCAL_HOST};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The remote side is shipped inside this binary and copied over on first use.
const REMOTE_FILES: &[(&str, &str)] = &[
    ("bs_remote.py", include_str!("../remote/bs_remote.py")),
    ("session.sh", include_str!("../remote/session.sh")),
    ("setup.sh", include_str!("../remote/setup.sh")),
];

/// Where the helper programs of each version are published.
const RELEASES: &str = "https://github.com/TolemyW/bash-screen/releases/download";

/// The program that answers requests on the host.
#[derive(Clone, Copy, PartialEq)]
enum Helper {
    /// `bs_remote.py`, run by the host's Python.
    Python,
    /// `bs-helper`, a program of its own, built for the host's processor.
    Native,
}

/// The helper chosen with BASH_SCREEN_HELPER (`python` or `rust`).
fn helper() -> Helper {
    match std::env::var("BASH_SCREEN_HELPER").as_deref() {
        Ok("python") => Helper::Python,
        _ => Helper::Native,
    }
}

pub struct Remote {
    target: Target,
}

impl Remote {
    pub fn new(target: Target) -> Self {
        Self { target }
    }

    /// A command that runs `shell_command` on the host. Over SSH one master
    /// connection is reused by every call, which cuts the overhead of each
    /// command to about a fifth.
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

    /// Marker file recording that this build's remote files, and the helper
    /// in use, are on this host.
    fn deployed_marker(&self) -> Result<PathBuf> {
        let mut hash: u64 = 0xcbf29ce484222325;
        let mut add = |bytes: &[u8]| {
            for byte in bytes {
                hash = (hash ^ *byte as u64).wrapping_mul(0x100000001b3);
            }
        };
        for (_, content) in REMOTE_FILES {
            add(content.as_bytes());
        }
        if helper() == Helper::Native {
            add(native_identity().as_bytes());
        }
        let dir = self.target.remote_dir.replace(['/', '~'], "_");
        Ok(config::state_dir()?.join(format!(
            "deployed-{}-{}-{:016x}",
            self.target.host, dir, hash
        )))
    }

    /// Copy the helper scripts, and the native helper when it is in use, to
    /// the remote directory.
    pub fn deploy(&self) -> Result<()> {
        let dir = &self.target.remote_dir;
        for (name, content) in REMOTE_FILES {
            let command = format!("mkdir -p {dir} && cat > {dir}/{name} && chmod 700 {dir}/{name}");
            let (ok, _, stderr) = self.run(&command, content.as_bytes())?;
            if !ok {
                bail!("cannot copy {name} to {}:{dir}: {stderr}", self.target.host);
            }
        }
        if helper() == Helper::Native {
            let program = std::fs::read(native_helper(&self.processor()?)?)?;
            // Replaced in one step, so that a request running meanwhile still
            // finds a whole program.
            let command = format!(
                "cat > {dir}/bs-helper.new && chmod 700 {dir}/bs-helper.new && mv {dir}/bs-helper.new {dir}/bs-helper"
            );
            let (ok, _, stderr) = self.run(&command, &program)?;
            if !ok {
                bail!(
                    "cannot copy bs-helper to {}:{dir}: {stderr}",
                    self.target.host
                );
            }
        }
        std::fs::write(self.deployed_marker()?, b"")?;
        Ok(())
    }

    /// The host's processor, as release files name it: `x86_64` or `arm64`.
    fn processor(&self) -> Result<String> {
        let (ok, out, stderr) = self.run("uname -sm", b"")?;
        let found = String::from_utf8_lossy(&out).trim().to_string();
        if !ok {
            bail!(
                "cannot ask {} for its processor: {stderr}",
                self.target.host
            );
        }
        match found.as_str() {
            "Linux x86_64" => Ok("x86_64".into()),
            "Linux aarch64" | "Linux arm64" => Ok("arm64".into()),
            other => bail!(
                "{} runs {other}; bash-screen's helper is built for Linux on x86_64 and arm64",
                self.target.host
            ),
        }
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
        let dir = &self.target.remote_dir;
        let command = match helper() {
            Helper::Python => format!("python3 {dir}/bs_remote.py"),
            Helper::Native => format!("{dir}/bs-helper"),
        };
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

/// What tells one native helper from another: its file when given by
/// BASH_SCREEN_HELPER_FILE, else this version.
fn native_identity() -> String {
    match std::env::var_os("BASH_SCREEN_HELPER_FILE") {
        Some(path) => {
            let facts = std::fs::metadata(&path).ok();
            let size = facts.as_ref().map_or(0, |f| f.len());
            let modified = facts
                .and_then(|f| f.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{size}:{modified}", PathBuf::from(path).display())
        }
        None => env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// A local copy of the native helper for a host with this processor: the
/// file named by BASH_SCREEN_HELPER_FILE, or the one published with this
/// version, downloaded once and checked against the release's checksums.
fn native_helper(processor: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("BASH_SCREEN_HELPER_FILE") {
        return Ok(PathBuf::from(path));
    }
    let version = env!("CARGO_PKG_VERSION");
    let name = format!("bs-helper-linux-{processor}");
    let dir = config::state_dir()?.join("helpers").join(version);
    let path = dir.join(&name);
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let base = format!("{RELEASES}/v{version}");
    let sums = download(&format!("{base}/SHA256SUMS"))?;
    let wanted = String::from_utf8_lossy(&sums)
        .lines()
        .find_map(|line| {
            let (sum, file) = line.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| sum.to_lowercase())
        })
        .ok_or_else(|| anyhow!("the release v{version} has no {name}"))?;
    let program = download(&format!("{base}/{name}"))?;
    let found = sha256_hex(&program);
    if found != wanted {
        bail!("{name} from the release v{version} does not match its checksum");
    }
    let partial = dir.join(format!("{name}.part"));
    std::fs::write(&partial, &program)?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

fn download(url: &str) -> Result<Vec<u8>> {
    let out = Command::new("curl")
        .args(["-fsSL", "--retry", "2", url])
        .output()
        .context("cannot run curl to download the helper")?;
    if !out.status.success() {
        bail!(
            "cannot download {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// SHA-256 of `data`, in lower-case hex.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = vec![b'a'; 1000];
        assert_eq!(
            sha256_hex(&long),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }
}
