//! Local HTML viewer. Python's standard library serves HTTP; the existing Rust
//! transport owns SSH. Newline-delimited JSON on private pipes joins the two.

use crate::{config::Target, remote::Remote};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

pub fn run(target: Target, port: u16, interval: f64, open: bool, json_output: bool) -> Result<()> {
    if !interval.is_finite() || !(0.2..=60.0).contains(&interval) {
        bail!("--interval must be between 0.2 and 60 seconds");
    }
    let mut child = Command::new("python3")
        .args(["-u", "-c", include_str!("../web/server.py")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("the HTML viewer needs python3 on this machine")?;
    let result = (|| -> Result<()> {
        let mut input = child.stdin.take().context("viewer input pipe is missing")?;
        writeln!(
            input,
            "{}",
            json!({
                "port": port, "interval": interval, "open": open, "label": target.label(),
                "html": include_str!("../web/index.html"),
                "js": include_str!("../web/viewer.js"),
                "css": include_str!("../web/style.css"),
            })
        )?;
        input.flush()?;
        let remote = Remote::new(target);
        let output = child
            .stdout
            .take()
            .context("viewer output pipe is missing")?;
        for line in BufReader::new(output).lines() {
            let message: Value = serde_json::from_str(&line?)?;
            if message["event"] == "ready" {
                if json_output {
                    println!("{message}");
                } else {
                    println!("HTML viewer: {}", message["url"].as_str().unwrap_or(""));
                    println!(
                        "Open this URL in a browser. Ctrl+C stops the viewer, not the remote apps."
                    );
                }
                std::io::stdout().flush()?;
            } else if let Some(request) = message.get("request") {
                let response = match remote.call(request.clone()) {
                    Ok(reply) => json!({"reply": reply}),
                    Err(error) => json!({"error": format!("{error:#}")}),
                };
                writeln!(input, "{response}")?;
                input.flush()?;
            } else {
                bail!("invalid viewer bridge message");
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    result?;
    if !status.success() {
        bail!("HTML viewer exited with {status}");
    }
    Ok(())
}
