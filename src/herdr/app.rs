//! The Herdr desktop app: `herdr-app` arranges the window, `hc` (herdr-core)
//! runs the terminals. A terminal there knows its own address from
//! HC_TERMINAL_ID.

use super::{choose_direction, set_preview_target, viewer_of_target, viewer_open};
use crate::config::{self, Target};
use anyhow::Result;
use serde_json::{json, Value};
use std::process::Command;
use std::time::Duration;

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

const MOD: &str = "bash-screen";

/// How long a pane that was just opened is given to show its first picture.
const OPENING: Duration = Duration::from_secs(15);

/// Whether the app has a version of the bash-screen mod that says when it is
/// open. Without that, nothing tells a closed pane from an open one.
fn mod_installed() -> bool {
    let home = std::env::var_os("HERDR_APP_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config::home().join(".config").join("herdr-app"));
    std::fs::read_to_string(home.join("mods").join(MOD).join("main.js"))
        .is_ok_and(|source| source.contains("--viewer"))
}

/// Make sure the screen is shown beside the pane of `terminal`: by the
/// bash-screen mod when the app has it, else by the preview in a terminal.
/// Returns a note for the user when a pane was opened or could not be opened;
/// does nothing when the app's window does not show this terminal.
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
    // The pane shows whichever session its calling pane used last.
    set_preview_target(&key, target)?;
    if let Ok(existing) = std::fs::read_to_string(&binding) {
        if is_preview(existing.trim(), &key) {
            return Ok(None);
        }
    }
    // A mod pane says by itself that it is open: one that follows this
    // terminal, or one that a person opened on this very session.
    let opened = config::state_dir()?.join(format!("preview-mod-{key}"));
    let opening = std::fs::metadata(&opened)
        .and_then(|file| file.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age < OPENING));
    if opening || viewer_open(&key) || viewer_open(&viewer_of_target(target)) {
        return Ok(None);
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
    // A split gives the new pane the keyboard. It goes back to whichever pane
    // had it, which need not be the calling one.
    let focused = json_from("herdr-app", &["status"])
        .and_then(|status| status["focused_pane"].as_str().map(str::to_string));
    // The app splits only to the right or downwards; "up" is a downward
    // split followed by swapping the two panes.
    let split = if direction == "right" {
        "right"
    } else {
        "down"
    };
    let with_mod = mod_installed();
    let made = if with_mod {
        app_do(
            "pane.split",
            json!({
                "direction": split,
                "pane": pane,
                "mod": MOD,
                "props": {"follow": key, "host": target.host, "session": target.session},
            }),
        )
    } else {
        let program = std::env::current_exe()?;
        app_do(
            "pane.split",
            json!({
                "direction": split,
                "pane": pane,
                "argv": [program.to_string_lossy(), "preview", "--follow", key],
            }),
        )
    };
    let reply = match &made {
        Ok(reply) => reply,
        Err(error) => return Ok(Some(format!("preview pane not opened: {error}"))),
    };
    let Some(new_pane) = reply["pane"].as_str() else {
        return Ok(Some(format!("preview pane not opened: {reply}")));
    };
    let name = if with_mod {
        // Its age is what counts; the pane is noted for whoever looks into it.
        std::fs::write(&opened, new_pane)?;
        "the bash-screen mod"
    } else {
        let Some(new_terminal) = reply["terminal"].as_str() else {
            return Ok(Some(format!("preview pane not opened: {reply}")));
        };
        std::fs::write(&binding, new_terminal)?;
        new_terminal
    };
    if direction == "up" {
        let _ = app_do("pane.swap", json!({"pane": new_pane, "target": pane}));
    }
    if let Some(focused) = focused {
        let _ = app_do("pane.focus", json!({"pane": focused}));
    }
    Ok(Some(format!("preview pane opened ({name}, {direction})")))
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
