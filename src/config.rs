//! Connection settings and local state locations.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

pub const PLUGIN_ID: &str = "bash-screen";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The default host: an SSH host as you would pass it to `ssh` (an alias
    /// from ~/.ssh/config works), or "local" for this machine.
    pub host: String,
    /// Directory on the remote host that holds the session scripts and tools.
    #[serde(default = "default_remote_dir")]
    pub remote_dir: String,
    /// Virtual screen size, fixed for the lifetime of a session.
    #[serde(default = "default_geometry")]
    pub geometry: String,
    /// Where the herdr preview pane opens relative to the calling pane: "auto" (right of a wide pane, above a narrow one), "right", "down" or "up".
    #[serde(default = "default_preview_direction")]
    pub preview_direction: String,
    /// How the preview draws the screen: "auto" (a real image when the terminal
    /// supports Kitty graphics, else sextants), "image", "sextant", "quad" or "half".
    #[serde(default = "default_preview_blocks")]
    pub preview_blocks: String,
    /// Longest edge, in pixels, of the picture sent to an image-mode preview;
    /// the terminal scales it up to fill the pane. 0 sends the full screen.
    #[serde(default = "default_preview_max_px")]
    pub preview_max_px: u32,
    /// Settings for individual hosts, used instead of the ones above when a
    /// command names that host.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<String, HostConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<String>,
}

/// The host that runs commands on this machine, without SSH.
pub const LOCAL_HOST: &str = "local";
pub const DEFAULT_SESSION: &str = "main";

/// One session on one host: what every command acts on.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub host: String,
    pub session: String,
    pub remote_dir: String,
    pub geometry: String,
}

impl Target {
    /// "host" for the default session, "host/session" otherwise.
    pub fn label(&self) -> String {
        if self.session == DEFAULT_SESSION {
            self.host.clone()
        } else {
            format!("{}/{}", self.host, self.session)
        }
    }
}

impl Config {
    /// A config with every setting at its default, for the given host.
    pub fn for_host(host: &str) -> Self {
        Self {
            host: host.to_string(),
            remote_dir: default_remote_dir(),
            geometry: default_geometry(),
            preview_direction: default_preview_direction(),
            preview_blocks: default_preview_blocks(),
            preview_max_px: default_preview_max_px(),
            hosts: BTreeMap::new(),
        }
    }

    /// Settings for `host` (the default host when None) and `session`.
    /// The top-level remote_dir and geometry belong to the default host; any
    /// other host uses its own entry under [hosts], or the defaults.
    pub fn target(&self, host: Option<&str>, session: Option<&str>) -> Result<Target> {
        let host = host.unwrap_or(&self.host).to_string();
        let entry = self.hosts.get(&host).cloned().unwrap_or_default();
        let is_default = host == self.host;
        let target = Target {
            remote_dir: entry.remote_dir.unwrap_or_else(|| {
                if is_default {
                    self.remote_dir.clone()
                } else {
                    default_remote_dir()
                }
            }),
            geometry: entry.geometry.unwrap_or_else(|| self.geometry.clone()),
            session: session.unwrap_or(DEFAULT_SESSION).to_string(),
            host,
        };
        validate_target(&target)?;
        Ok(target)
    }
}

fn default_preview_blocks() -> String {
    "auto".into()
}

fn default_preview_direction() -> String {
    "auto".into()
}

fn default_remote_dir() -> String {
    "~/.bash-screen".into()
}

fn default_geometry() -> String {
    "1280x800".into()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

/// Where the config file lives. An explicit override wins; otherwise the first
/// existing file among the herdr plugin config directory and
/// ~/.config/bash-screen, and the latter when neither exists yet.
pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("BASH_SCREEN_CONFIG") {
        return PathBuf::from(path);
    }
    let fallback = home().join(".config").join(PLUGIN_ID).join("config.toml");
    let plugin_dir = std::env::var("HERDR_PLUGIN_CONFIG_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(herdr_plugin_config_dir);
    match plugin_dir.map(|dir| dir.join("config.toml")) {
        Some(path) if path.exists() => path,
        _ => fallback,
    }
}

/// Ask herdr for the plugin's config directory; None outside herdr or when the
/// plugin is not linked.
fn herdr_plugin_config_dir() -> Option<PathBuf> {
    let herdr = std::env::var("HERDR_BIN_PATH").ok()?;
    let out = Command::new(herdr)
        .args(["plugin", "config-dir", PLUGIN_ID])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() || text.starts_with('{') {
        return None;
    }
    Some(PathBuf::from(text))
}

pub fn load() -> Result<Config> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no config at {}. Connect a host with: bash-screen init <ssh-host>",
            path.display()
        )
    })?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
    validate(&config)?;
    Ok(config)
}

pub fn save(config: &Config) -> Result<PathBuf> {
    validate(config)?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(config)?)?;
    Ok(path)
}

fn safe(text: &str, extra: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// These values are interpolated into a remote shell command, so only allow
/// characters that cannot change its meaning.
fn validate_target(target: &Target) -> Result<()> {
    if !safe(&target.host, "._-@") {
        bail!("host may only contain letters, digits and . _ - @");
    }
    if !safe(&target.remote_dir, "._-/~") {
        bail!("remote_dir may only contain letters, digits and . _ - / ~");
    }
    if !safe(&target.geometry, "x") {
        bail!("geometry must look like 1280x800");
    }
    let name = &target.session;
    let well_formed = name.len() <= 32
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !well_formed {
        bail!("a session name is 1-32 characters: lower-case letters, digits, - and _");
    }
    Ok(())
}

fn validate(config: &Config) -> Result<()> {
    config.target(None, None)?;
    for host in config.hosts.keys() {
        config.target(Some(host), None)?;
    }
    if !matches!(
        config.preview_direction.as_str(),
        "auto" | "right" | "down" | "up"
    ) {
        bail!("preview_direction must be auto, right, down or up");
    }
    if !matches!(
        config.preview_blocks.as_str(),
        "auto" | "image" | "sextant" | "quad" | "half"
    ) {
        bail!("preview_blocks must be auto, image, sextant, quad or half");
    }
    Ok(())
}

/// Local state: SSH control sockets, deployment markers, preview pane bindings.
pub fn state_dir() -> Result<PathBuf> {
    let dir = home().join(".local").join("state").join(PLUGIN_ID);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

fn default_preview_max_px() -> u32 {
    640
}

/// Preview resolution steps, coarsest first. 0 means the full screen.
pub const RES_LEVELS: [u32; 4] = [320, 640, 960, 0];

/// The next coarser or sharper step from `current`.
pub fn step_res(current: u32, sharper: bool) -> u32 {
    // Rank by sharpness, treating 0 (full) as larger than any cap.
    let rank = |v: u32| if v == 0 { u32::MAX } else { v };
    let position = RES_LEVELS
        .iter()
        .filter(|&&level| rank(level) < rank(current))
        .count();
    let exact = RES_LEVELS.iter().any(|&level| level == current);
    let next = match (sharper, exact) {
        (true, true) => position + 1,
        (true, false) => position,
        (false, _) => position.saturating_sub(1),
    };
    RES_LEVELS[next.min(RES_LEVELS.len() - 1)]
}

/// The resolution chosen at run time (by key press or `bash-screen res`); it
/// overrides the config file and is shared by every preview pane.
pub fn read_res_override() -> Option<u32> {
    let path = state_dir().ok()?.join("preview-res");
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub fn write_res_override(max_px: u32) -> Result<()> {
    std::fs::write(state_dir()?.join("preview-res"), max_px.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{step_res, Config, HostConfig};

    #[test]
    fn the_default_host_uses_the_top_level_settings() {
        let mut config = Config::for_host("alpha");
        config.remote_dir = "~/tools".into();
        let target = config.target(None, None).unwrap();
        assert_eq!(
            (target.host.as_str(), target.remote_dir.as_str()),
            ("alpha", "~/tools")
        );
        assert_eq!(target.session, "main");
        assert_eq!(target.label(), "alpha");
    }

    #[test]
    fn another_host_uses_its_own_entry_or_the_defaults() {
        let mut config = Config::for_host("alpha");
        config.remote_dir = "~/tools".into();
        config.hosts.insert(
            "beta".into(),
            HostConfig {
                remote_dir: Some("~/b".into()),
                geometry: None,
            },
        );
        assert_eq!(config.target(Some("beta"), None).unwrap().remote_dir, "~/b");
        let gamma = config.target(Some("gamma"), Some("work")).unwrap();
        assert_eq!(gamma.remote_dir, "~/.bash-screen");
        assert_eq!(gamma.label(), "gamma/work");
    }

    #[test]
    fn names_that_could_change_a_shell_command_are_refused() {
        let config = Config::for_host("alpha");
        assert!(config.target(Some("a;b"), None).is_err());
        assert!(config.target(None, Some("x y")).is_err());
        assert!(config.target(None, Some("../up")).is_err());
        assert!(config.target(None, Some("")).is_err());
    }

    #[test]
    fn steps_walk_the_levels_and_stop_at_the_ends() {
        assert_eq!(step_res(640, true), 960);
        assert_eq!(step_res(960, true), 0);
        assert_eq!(step_res(0, true), 0);
        assert_eq!(step_res(0, false), 960);
        assert_eq!(step_res(640, false), 320);
        assert_eq!(step_res(320, false), 320);
    }

    #[test]
    fn a_custom_value_steps_to_its_neighbours() {
        assert_eq!(step_res(800, true), 960);
        assert_eq!(step_res(800, false), 640);
    }
}
