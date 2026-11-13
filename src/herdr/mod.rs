//! herdr integration: open the preview as a split next to the calling pane,
//! in the herdr terminal multiplexer or in the Herdr desktop app (`app`).

use crate::config::{self, Target, PLUGIN_ID};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

mod app;

/// Find the first "pane_id" anywhere in herdr's JSON reply.
fn find_pane_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(id)) = map.get("pane_id") {
                return Some(id.clone());
            }
            map.values().find_map(find_pane_id)
        }
        Value::Array(items) => items.iter().find_map(find_pane_id),
        _ => None,
    }
}

fn pane_exists(herdr: &str, pane_id: &str) -> bool {
    Command::new(herdr)
        .args(["pane", "get", pane_id])
        .output()
        .map(|out| {
            out.status.success() && !String::from_utf8_lossy(&out.stdout).contains("\"error\"")
        })
        .unwrap_or(false)
}

/// Size of a pane in terminal cells, from herdr's layout of its tab.
fn pane_size(herdr: &str, pane: &str) -> Option<(f64, f64)> {
    let out = Command::new(herdr)
        .args(["pane", "layout", "--pane", pane])
        .output()
        .ok()?;
    let layout: Value = serde_json::from_slice(&out.stdout).ok()?;
    let rect = layout["result"]["layout"]["panes"]
        .as_array()?
        .iter()
        .find(|p| p["pane_id"] == pane)?["rect"]
        .clone();
    Some((rect["width"].as_f64()?, rect["height"].as_f64()?))
}

/// The pane that has keyboard focus in the tab that contains `pane`.
fn focused_pane(herdr: &str, pane: &str) -> Option<String> {
    let out = Command::new(herdr)
        .args(["pane", "layout", "--pane", pane])
        .output()
        .ok()?;
    let layout: Value = serde_json::from_slice(&out.stdout).ok()?;
    layout["result"]["layout"]["focused_pane_id"]
        .as_str()
        .map(str::to_string)
}

/// Narrowest the calling pane may become when the preview takes half its width.
const MIN_COLS_AFTER_SPLIT: f64 = 70.0;

/// Where to put the preview for a pane of this size: "right" when the pane is
/// wide, "up" when it is narrow. The choice is the one that leaves the larger
/// picture, for a 16:10 screen and cells twice as tall as wide.
fn choose_direction(cols: f64, rows: f64) -> &'static str {
    // Work in units of cell widths: a row is two units tall.
    let (width, height, aspect) = (cols, rows * 2.0, 1.6);
    let picture_right = height.min(width / 2.0 / aspect);
    let picture_up = (height / 2.0).min(width / aspect);
    if cols / 2.0 >= MIN_COLS_AFTER_SPLIT && picture_right >= picture_up {
        "right"
    } else {
        "up"
    }
}

fn auto_direction(herdr: &str, pane: &str) -> &'static str {
    match pane_size(herdr, pane) {
        Some((cols, rows)) => choose_direction(cols, rows),
        None => "up",
    }
}

/// File that tells the preview in `preview_pane` which session to show.
fn preview_target_file(preview_pane: &str) -> Result<PathBuf> {
    Ok(config::state_dir()?.join(format!(
        "preview-target-{}",
        preview_pane.replace([':', '/'], "_")
    )))
}

fn set_preview_target(preview_pane: &str, target: &Target) -> Result<()> {
    let path = preview_target_file(preview_pane)?;
    let wanted = format!("{}\n{}\n", target.host, target.session);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(wanted.as_str()) {
        std::fs::write(&path, wanted)?;
    }
    Ok(())
}

/// Key given with `preview --follow`, when this process is such a preview.
static FOLLOW: OnceLock<String> = OnceLock::new();

/// Make this preview show whichever session the pane behind `key` used last.
pub fn follow(key: String) {
    let _ = FOLLOW.set(key);
}

/// The file this preview takes its session from: named after its own pane in
/// herdr, after the key it was started with in the Herdr app.
fn own_target_key() -> Option<String> {
    FOLLOW
        .get()
        .cloned()
        .or_else(|| std::env::var("HERDR_PANE_ID").ok())
}

/// Whether this preview was opened beside a pane, and so is told its session.
pub fn preview_is_bound() -> bool {
    own_target_key().is_some()
}

/// For a preview opened beside a pane: the host and session it was opened
/// for, or was last pointed at. None for a preview started by hand.
pub fn own_preview_target() -> Option<(String, String)> {
    let key = own_target_key()?;
    let text = std::fs::read_to_string(preview_target_file(&key).ok()?).ok()?;
    let mut lines = text.lines();
    Some((lines.next()?.to_string(), lines.next()?.to_string()))
}

/// When running inside a herdr pane, make sure a preview pane sits beside it.
/// `direction` is "auto", "right", "down" or "up". Returns a note for the user
/// when a pane was opened or could not be opened. Does nothing outside herdr,
/// or when BASH_SCREEN_NO_PREVIEW is set.
pub fn ensure_preview_pane(direction: &str, target: &Target) -> Result<Option<String>> {
    if std::env::var_os("BASH_SCREEN_NO_PREVIEW").is_some() {
        return Ok(None);
    }
    let (Ok(herdr), Ok(pane)) = (
        std::env::var("HERDR_BIN_PATH"),
        std::env::var("HERDR_PANE_ID"),
    ) else {
        // Not the herdr terminal multiplexer; perhaps the Herdr desktop app.
        return match std::env::var("HC_TERMINAL_ID") {
            Ok(terminal) => app::ensure_preview_pane(&terminal, direction, target),
            Err(_) => Ok(None),
        };
    };
    // One preview per calling pane; the binding survives across CLI invocations.
    let binding = config::state_dir()?.join(format!("preview-{}", pane.replace([':', '/'], "_")));
    if let Ok(existing) = std::fs::read_to_string(&binding) {
        if pane_exists(&herdr, existing.trim()) {
            // The pane shows whichever session its calling pane used last.
            set_preview_target(existing.trim(), target)?;
            return Ok(None);
        }
    }
    let direction = if direction == "auto" {
        auto_direction(&herdr, &pane)
    } else {
        direction
    };
    let had_focus = focused_pane(&herdr, &pane).as_deref() == Some(pane.as_str());
    // herdr splits only to the right or downwards; "up" is a downward split
    // followed by swapping the two panes.
    let split = if direction == "right" {
        "right"
    } else {
        "down"
    };
    let out = Command::new(&herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            PLUGIN_ID,
            "--entrypoint",
            "preview",
        ])
        .args([
            "--placement",
            "split",
            "--direction",
            split,
            "--target-pane",
            &pane,
            "--no-focus",
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let opened = serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|v| find_pane_id(&v));
    match opened {
        Some(id) if out.status.success() => {
            set_preview_target(&id, target)?;
            std::fs::write(&binding, &id)?;
            if direction == "up" {
                let _ = Command::new(&herdr)
                    .args(["pane", "swap", "--source-pane", &id, "--target-pane", &pane])
                    .output();
                // The swap leaves focus on the preview; hand it back to the
                // calling pane, which now sits directly below.
                if had_focus {
                    let _ = Command::new(&herdr)
                        .args(["pane", "focus", "--direction", "down", "--pane", &id])
                        .output();
                }
            }
            Ok(Some(format!("preview pane opened ({id}, {direction})")))
        }
        _ => {
            let detail = format!(
                "{}{}",
                stdout.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(Some(format!(
                "preview pane not opened: {detail}. Link the plugin with: herdr plugin link <bash-screen directory>"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::choose_direction;

    #[test]
    fn wide_panes_get_the_preview_on_the_right() {
        assert_eq!(choose_direction(263.0, 70.0), "right");
        assert_eq!(choose_direction(200.0, 40.0), "right");
    }

    #[test]
    fn narrow_or_squarish_panes_get_the_preview_above() {
        assert_eq!(choose_direction(170.0, 70.0), "up");
        assert_eq!(choose_direction(90.0, 60.0), "up");
    }

    #[test]
    fn a_pane_too_narrow_to_halve_never_splits_sideways() {
        assert_eq!(choose_direction(120.0, 20.0), "up");
    }
}
