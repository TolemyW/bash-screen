//! Running other programs: agent-desktop, with a time limit, and the apps
//! that `launch` starts.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The environment of the GUI session.
pub type Env = BTreeMap<String, String>;

/// Run one agent-desktop command and return its parsed JSON envelope.
pub fn ad(env: &Env, args: &[&str], timeout: Duration) -> Value {
    let child = Command::new("agent-desktop")
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return internal(&format!("cannot run agent-desktop: {error}")),
    };
    // Read both pipes while waiting, so that a full pipe cannot stall it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut data = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut data);
            }
            data
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return json!({"ok": false, "error": {"code": "TIMEOUT", "message": "agent-desktop did not return"}});
            }
        }
    }
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    match serde_json::from_slice::<Value>(&out) {
        Ok(envelope) => envelope,
        Err(_) => {
            let said = if err.is_empty() { out } else { err };
            internal(&crate::tail(&String::from_utf8_lossy(&said), 300))
        }
    }
}

fn internal(message: &str) -> Value {
    json!({"ok": false, "error": {"code": "INTERNAL", "message": message}})
}

/// "CODE: message" from an agent-desktop envelope that reports a failure.
pub fn ad_error(envelope: &Value) -> String {
    let error = &envelope["error"];
    let code = error["code"].as_str().unwrap_or("ERROR");
    let message = error["message"].as_str().unwrap_or("");
    format!("{code}: {message}")
}

/// Start an app in a session of its own, with its output appended to `log`,
/// and do not wait for it.
pub fn launch(argv: &[String], env: &Env, log: &Path) -> std::io::Result<()> {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let err = out.try_clone()?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn()?;
    Ok(())
}
