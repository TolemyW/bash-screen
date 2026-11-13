//! The Herdr desktop app: `herdr-app` arranges the window, `hc` (herdr-core)
//! runs the terminals. A terminal there knows its own address from
//! HC_TERMINAL_ID.

use super::{choose_direction, set_preview_target};
use crate::config::{self, Target};
use anyhow::Result;
use serde_json::{json, Value};
use std::process::Command;

/// Run a program that prints JSON; None when it is missing, fails or prints
/// something else.
fn json_from(program: &str, args: &[&str]) -> Option<Value> {
    let out = Command::new(program).args(args).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Run one app command; the app prints what it made, or `{"error": ...}`.
fn app_do(command: &str, args: Value) -> Result<Value, String> {
    let out = Command::new("herdr-app")
        .args(["do", command, &args.to_string()])
        .output()
        .map_err(|error| format!("herdr-app could not be run ({error})"))?;
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    match reply["error"].as_str() {
        Some(error) => Err(error.to_string()),
        None if out.status.success() => Ok(reply),
        None => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// The pane that shows `terminal`, from the app's listing of its window.
fn pane_of(listing: &Value, terminal: &str) -> Option<String> {
    listing["spaces"]
        .as_array()?
        .iter()
        .flat_map(|space| space["tabs"].as_array().into_iter().flatten())
        .flat_map(|tab| tab["terminals"].as_array().into_iter().flatten())
        .find(|entry| entry["terminal"] == terminal)?["pane"]
        .as_str()
        .map(str::to_string)
}

/// Whether `terminal` is still the preview that was started with `key`.
/// Addresses are reused, so being alive is not enough.
fn is_preview(terminal: &str, key: &str) -> bool {
    json_from("hc", &["get", terminal, "--json"]).is_some_and(|facts| {
        facts["state"] == "live"
            && facts["argv"]
                .as_array()
                .is_some_and(|argv| argv.iter().any(|arg| arg.as_str() == Some(key)))
    })
}

/// Make sure a preview pane sits beside the pane of `terminal`. Returns a note
/// for the user when a pane was opened or could not be opened; does nothing
/// when the app's window does not show this terminal.
pub fn ensure_preview_pane(
    terminal: &str,
    direction: &str,
    target: &Target,
) -> Result<Option<String>> {
    // Names this terminal for as long as it lives: its address alone is
    // given to a later terminal once it is gone.
    let scope = std::env::var("HC_SCOPE").unwrap_or_default();
    let key = format!("{terminal}-{scope}").replace([':', '/'], "_");
    let binding = config::state_dir()?.join(format!("preview-{key}"));
    // The preview shows whichever session its calling pane used last.
    set_preview_target(&key, target)?;
    if let Ok(existing) = std::fs::read_to_string(&binding) {
        if is_preview(existing.trim(), &key) {
            return Ok(None);
        }
    }
    let Some(pane) = json_from("herdr-app", &["ls"]).and_then(|ls| pane_of(&ls, terminal)) else {
        return Ok(None);
    };
    let direction = match direction {
        "auto" => json_from("hc", &["get", terminal, "--json"])
            .and_then(|facts| Some((facts["cols"].as_f64()?, facts["rows"].as_f64()?)))
            .map_or("up", |(cols, rows)| choose_direction(cols, rows)),
        other => other,
    };
    let had_focus =
        json_from("herdr-app", &["status"]).is_some_and(|status| status["focused_pane"] == pane);
    let program = std::env::current_exe()?;
    // The app splits only to the right or downwards; "up" is a downward
    // split followed by swapping the two panes.
    let split = if direction == "right" {
        "right"
    } else {
        "down"
    };
    let made = app_do(
        "pane.split",
        json!({
            "direction": split,
            "pane": pane,
            "argv": [program.to_string_lossy(), "preview", "--follow", key],
        }),
    );
    let (new_pane, new_terminal) = match &made {
        Ok(reply) => match (reply["pane"].as_str(), reply["terminal"].as_str()) {
            (Some(new_pane), Some(new_terminal)) => (new_pane, new_terminal),
            _ => return Ok(Some(format!("preview pane not opened: {reply}"))),
        },
        Err(error) => return Ok(Some(format!("preview pane not opened: {error}"))),
    };
    std::fs::write(&binding, new_terminal)?;
    if direction == "up" {
        let _ = app_do("pane.swap", json!({"pane": new_pane, "target": pane}));
    }
    if had_focus {
        let _ = app_do("pane.focus", json!({"pane": pane}));
    }
    Ok(Some(format!(
        "preview pane opened ({new_terminal}, {direction})"
    )))
}

#[cfg(test)]
mod tests {
    use super::pane_of;
    use serde_json::json;

    #[test]
    fn the_pane_of_a_terminal_is_found_in_any_space_and_tab() {
        let listing = json!({"spaces": [
            {"tabs": [{"terminals": [{"terminal": "t1", "pane": "p1"}]}]},
            {"tabs": [
                {"terminals": []},
                {"terminals": [
                    {"terminal": "t8", "pane": "p8"},
                    {"terminal": "t9", "pane": "p9"},
                ]},
            ]},
        ]});
        assert_eq!(pane_of(&listing, "t9").as_deref(), Some("p9"));
        assert_eq!(pane_of(&listing, "t10"), None);
    }
}
