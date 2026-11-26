//! bash-screen: operate GUI apps on a remote Linux host over SSH.

mod calls;
mod config;
mod herdr;
mod input;
mod preview;
mod remote;
mod web;

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use remote::{HelperError, Remote};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "bash-screen",
    version,
    about = "Operate GUI apps on a remote Linux host over SSH",
    long_about = "Operate GUI apps on a remote Linux host over SSH.\n\n\
Typical loop:\n  \
bash-screen launch -- firefox https://example.org\n  \
bash-screen observe            # elements of the current window, each with a ref like e12\n  \
bash-screen click e12          # or: bash-screen click 640,400, or: bash-screen click --name Save\n  \
bash-screen type --ref e7 hello\n  \
bash-screen text               # the text of the page, for reading\n  \
bash-screen look               # screenshot, when the element list is not enough\n  \
bash-screen wait --text Saved  # until an element contains the text, instead of observing again\n  \
bash-screen do click e7 then type hello then key return   # several steps, one observation\n\n\
Every action prints what changed in the window, or the whole list when much did.\n\
A ref stays with its element for as long as the element is in the window.\n\
Coordinates are screen pixels, the same as in `look` images at scale 1.\n\n\
Commands act on the default host and its session \"main\". Use --host and --session\n\
(or BASH_SCREEN_HOST and BASH_SCREEN_SESSION) for another host, or for a second,\n\
separate screen on the same host."
)]
struct Cli {
    /// Host to act on instead of the default: an SSH host, or "local" for this machine
    #[arg(long, global = true)]
    host: Option<String>,
    /// Session on that host; each session is a separate screen with its own apps
    #[arg(long, global = true)]
    session: Option<String>,
    /// Print the reply as one line of JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the config, or change the default host (--host) and its settings
    Config {
        /// Remote directory holding the session tools
        #[arg(long)]
        remote_dir: Option<String>,
        /// Virtual screen size, for example 1280x800
        #[arg(long)]
        geometry: Option<String>,
    },
    /// Connect a host in one step: write the config, install the remote tools,
    /// start a session and check that it works. The first host becomes the
    /// default; later ones are addressed with --host
    Init {
        /// SSH host to connect to, or "local" for this machine
        #[arg(value_name = "HOST")]
        init_host: String,
        /// Remote directory holding the session tools
        #[arg(long)]
        remote_dir: Option<String>,
        /// Virtual screen size, for example 1280x800
        #[arg(long)]
        geometry: Option<String>,
    },
    /// Install the remote tools on the host (no root needed)
    Setup,
    /// Start the session (other commands start it on demand)
    Start,
    /// Stop the session and everything running in it
    Stop {
        /// Also delete what the session saved, such as its browser profile
        #[arg(long)]
        forget: bool,
    },
    /// Show whether the session is running and which windows are open
    Status,
    /// List the sessions on the host
    Sessions,
    /// List the elements of the current window
    Observe {
        /// Also list non-interactive named elements such as headings and text
        #[arg(long)]
        all: bool,
    },
    /// Print the text of the current window as a person reads it, without
    /// refs: for reading a page. In a browser, only the page
    Text,
    /// Take a screenshot and save it locally as PNG
    Look {
        /// Only this region: x,y,width,height in screen pixels
        #[arg(long)]
        region: Option<String>,
        /// Shrink factor, for example 0.5 (image coordinates must then be divided by it)
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// Output file (default: a file under the local state directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// For a pane that shows the screen to a person: say that it is
        /// open, and add the recent actions, host and session to the reply
        #[arg(long, hide = true)]
        viewer: bool,
    },
    /// Click an element ref (e12), a screen position (x,y), or the element
    /// with a name (--name Save)
    Click {
        #[arg(required_unless_present = "name", conflicts_with = "name")]
        target: Option<String>,
        /// The element with this name instead of a ref; it must be the only one
        #[arg(long)]
        name: Option<String>,
        /// left, right or middle
        #[arg(long)]
        button: Option<String>,
        /// Number of clicks, 2 for a double click
        #[arg(long)]
        count: Option<u32>,
    },
    /// Press the mouse at one screen position and release it at another
    Drag {
        /// Where to press: x,y
        from: String,
        /// Where to release: x,y
        to: String,
    },
    /// Type text into an element, or into whatever has keyboard focus
    Type {
        /// Element ref to type into; without it the text is pasted at the focus
        #[arg(long = "ref", conflicts_with = "name")]
        target: Option<String>,
        /// The element with this name instead of a ref; it must be the only one
        #[arg(long)]
        name: Option<String>,
        text: String,
    },
    /// Press key combos in order, for example: ctrl+l  return  down
    Key { keys: Vec<String> },
    /// Scroll an element into view (--ref or --name), or turn the wheel (--dy, negative is down)
    Scroll {
        #[arg(long = "ref", conflicts_with = "name")]
        target: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        dy: Option<i32>,
    },
    /// Launch an app in the session: bash-screen launch -- firefox https://example.org
    Launch {
        /// Seconds to wait for the window before observing
        #[arg(long, default_value_t = 4.0)]
        wait: f64,
        /// Keep the app off the accessibility channel: `observe` will not see
        /// it, so work from `look` and positions. For apps that misbehave
        /// when their elements are read
        #[arg(long)]
        no_a11y: bool,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Wait until the window is ready, then observe. Without a condition:
    /// until the screen stops changing
    Wait {
        /// Until some element's name or value contains this (case-insensitive)
        #[arg(long)]
        text: Option<String>,
        /// Until no element contains this any more
        #[arg(long)]
        gone: Option<String>,
        /// Until the window title contains this
        #[arg(long)]
        title: Option<String>,
        /// Also until the screen stops changing
        #[arg(long)]
        stable: bool,
        /// Give up with an error after this many seconds
        #[arg(long, default_value_t = 15.0)]
        timeout: f64,
    },
    /// Run several steps in one go, separated by the word "then", and observe
    /// once at the end: bash-screen do click e7 then type hello then key return
    /// then wait --text Saved. Steps: click, type, key, scroll, drag, wait;
    /// the last step can also be text or look, to read the result in the same
    /// call. Refs keep the meaning they had in the observation you last saw.
    /// When a step fails, the error comes with the window as it is then
    Do {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        steps: Vec<String>,
    },
    /// List the windows in the session
    Windows,
    /// Show where the time of the recent calls to the session went: inside
    /// bash-screen, or between calls, in the agent. Covers the latest stretch
    /// of calls without a 10-minute pause
    Stats {
        /// The calls of the last this many minutes instead
        #[arg(long)]
        since: Option<f64>,
    },
    /// Print the commands a person needs to take over the session with a VNC client
    Vnc,
    /// Serve a full-resolution HTML viewer on localhost. Open the printed URL
    /// in a browser; Ctrl+C stops the viewer and leaves the remote apps running
    Web {
        /// Local port (0 chooses an available port)
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Seconds between screenshots
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Open the viewer in the default browser
        #[arg(long)]
        open: bool,
    },
    /// Show or change the preview resolution: a pixel cap, "full", "up" or "down"
    Res { value: Option<String> },
    /// A person's own input, for a viewer such as the Herdr app's pane: inject
    /// it and return at once, without the observation an agent's action gets.
    /// ACTIONS is a JSON list, for example
    /// [{"kind":"click","x":10,"y":20},{"kind":"text","text":"hi"},{"kind":"key","key":"return"},{"kind":"wheel","x":10,"y":20,"dy":-3}]
    #[command(hide = true)]
    Input { actions: String },
    /// Print the Agent Skill that teaches a coding agent to use bash-screen,
    /// or install it with --install
    Skill {
        /// Write the skill where coding agents look for skills:
        /// ~/.claude/skills and ~/.agents/skills
        #[arg(long)]
        install: bool,
        /// Install into this skills directory instead (repeatable), for
        /// example .claude/skills in a project
        #[arg(long, value_name = "DIR")]
        dir: Vec<PathBuf>,
    },
    /// Live preview of the screen, in this terminal. Uses quadrant blocks when
    /// images are unavailable; smaller terminal fonts give more detail. Inside herdr it opens by
    /// itself beside the pane that runs commands
    Preview {
        /// Seconds between refreshes
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Set by bash-screen when it opens the preview beside a pane: show
        /// whichever session that pane used last
        #[arg(long, hide = true)]
        follow: Option<String>,
    },
}

/// After an action the helper also says what changed, when that is shorter
/// than the whole list; that is what an agent reads then.
fn print_observation(reply: &Value) {
    if let Some(text) = reply["brief"].as_str().or(reply["text"].as_str()) {
        println!("{text}");
    }
    if let Some(windows) = reply["windows"].as_array() {
        if windows.len() > 1 {
            let titles: Vec<String> = windows
                .iter()
                .map(|w| {
                    let mark = if w["focused"] == json!(true) { "*" } else { "" };
                    format!("{}{mark}", w["title"].as_str().unwrap_or("?"))
                })
                .collect();
            println!("windows: {}", titles.join(" | "));
        }
    }
}

fn parse_numbers(text: &str, count: usize, what: &str) -> Result<Vec<i64>> {
    let numbers: Vec<i64> = text
        .split(',')
        .map(|part| part.trim().parse::<i64>())
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("{what} must be {count} comma-separated integers"))?;
    if numbers.len() != count {
        bail!("{what} must be {count} comma-separated integers");
    }
    Ok(numbers)
}

/// How many recent actions a viewer pane is sent with each picture.
const VIEWER_EVENTS: u32 = 60;

/// Take a screenshot and save it as PNG. The reply comes back without the
/// picture and with its `path`, and with how many bytes travelled.
fn take_look(
    remote: &Remote,
    region: Option<Vec<i64>>,
    scale: f64,
    output: Option<PathBuf>,
    events: bool,
) -> Result<(Value, usize)> {
    let mut request = json!({"cmd": "look", "scale": scale});
    if events {
        request["events"] = json!(VIEWER_EVENTS);
    }
    if let Some(region) = region {
        request["region"] = json!(region);
    }
    let mut reply = remote.call(request)?;
    let webp = base64::engine::general_purpose::STANDARD
        .decode(reply["image_webp"].as_str().unwrap_or(""))
        .context("bad image data")?;
    // The image travels as WebP (small) and is stored as PNG (readable everywhere).
    let image = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP)?;
    let path = match output {
        Some(path) => path,
        None => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis();
            config::state_dir()?.join(format!("look-{stamp}.png"))
        }
    };
    image.save_with_format(&path, image::ImageFormat::Png)?;
    // The picture is in the file; the reply says where it is.
    reply.as_object_mut().unwrap().remove("image_webp");
    reply["path"] = json!(path.display().to_string());
    Ok((reply, webp.len()))
}

fn parse_region(region: Option<String>) -> Result<Option<Vec<i64>>> {
    region
        .map(|region| parse_numbers(&region, 4, "--region"))
        .transpose()
}

/// Fail now, rather than after a batch has acted, if a picture cannot be saved there.
fn check_output(output: &Option<PathBuf>) -> Result<()> {
    let Some(dir) = output.as_deref().and_then(|path| path.parent()) else {
        return Ok(());
    };
    if !dir.as_os_str().is_empty() && !dir.is_dir() {
        bail!("-o: there is no directory {}", dir.display());
    }
    Ok(())
}

fn print_look(reply: &Value, bytes: usize, scale: f64) {
    let origin = &reply["origin"];
    println!("{}", reply["path"].as_str().unwrap_or("?"));
    println!(
        "{}x{} px, transferred {bytes} bytes; screen position = image position / {scale} + ({},{})",
        reply["size"][0], reply["size"][1], origin[0], origin[1]
    );
}

fn look(
    remote: &Remote,
    region: Option<String>,
    scale: f64,
    output: Option<PathBuf>,
    json_output: bool,
    viewer: Option<(&str, &str)>,
) -> Result<()> {
    let region = parse_region(region)?;
    let (mut reply, bytes) = take_look(remote, region, scale, output, viewer.is_some())?;
    if json_output {
        if let Some((host, session)) = viewer {
            reply["host"] = json!(host);
            reply["session"] = json!(session);
        }
        println!("{reply}");
    } else {
        print_look(&reply, bytes, scale);
    }
    Ok(())
}

fn print_text(reply: &Value) {
    println!(
        "window: {} ({})",
        reply["title"].as_str().unwrap_or("?"),
        reply["app"].as_str().unwrap_or("?")
    );
    if let Some(note) = reply["note"].as_str() {
        println!("({note})");
    }
    println!("{}", reply["text"].as_str().unwrap_or(""));
}

/// What the last step of a batch reads, after the batch has run.
enum Reading {
    Text,
    Look {
        region: Option<Vec<i64>>,
        scale: f64,
        output: Option<PathBuf>,
    },
}

/// An error whose message has already been printed, with what came with it.
#[derive(Debug)]
struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed")
    }
}

impl std::error::Error for Reported {}

/// How a command is called in the record of calls, and how many steps it
/// carried; None for commands that are not an agent's.
fn recorded_as(command: &Cmd) -> Option<(&'static str, u64)> {
    Some(match command {
        Cmd::Start => ("start", 1),
        Cmd::Stop { .. } => ("stop", 1),
        Cmd::Status => ("status", 1),
        Cmd::Sessions => ("sessions", 1),
        Cmd::Observe { .. } => ("observe", 1),
        Cmd::Text => ("text", 1),
        Cmd::Look { viewer: false, .. } => ("look", 1),
        Cmd::Click { .. } => ("click", 1),
        Cmd::Drag { .. } => ("drag", 1),
        Cmd::Type { .. } => ("type", 1),
        Cmd::Key { .. } => ("key", 1),
        Cmd::Scroll { .. } => ("scroll", 1),
        Cmd::Launch { .. } => ("launch", 1),
        Cmd::Wait { .. } => ("wait", 1),
        Cmd::Windows => ("windows", 1),
        Cmd::Do { steps } => (
            "do",
            steps.iter().filter(|w| *w == "then").count() as u64 + 1,
        ),
        _ => return None,
    })
}

/// Change the default host and its settings, keeping what is not given.
fn write_config(
    host: Option<&String>,
    remote_dir: Option<&String>,
    geometry: Option<&String>,
) -> Result<()> {
    let mut config = match (config::load(), host) {
        (Ok(config), _) => config,
        (Err(_), Some(host)) if !config::config_path().exists() => config::Config::for_host(host),
        (Err(error), _) => return Err(error),
    };
    if let Some(host) = host {
        config.host = host.clone();
    }
    if let Some(dir) = remote_dir {
        config.remote_dir = dir.clone();
    }
    if let Some(geometry) = geometry {
        config.geometry = geometry.clone();
    }
    println!("wrote {}", config::save(&config)?.display());
    Ok(())
}

/// Record a host for `init`. The first host becomes the default; a later one
/// leaves the default alone and gets its own entry.
fn register_host(
    host: &String,
    remote_dir: Option<&String>,
    geometry: Option<&String>,
) -> Result<()> {
    let existing = if config::config_path().exists() {
        Some(config::load()?)
    } else {
        None
    };
    match existing {
        Some(mut config) if &config.host != host => {
            let entry = config.hosts.entry(host.clone()).or_default();
            if let Some(dir) = remote_dir {
                entry.remote_dir = Some(dir.clone());
            }
            if let Some(geometry) = geometry {
                entry.geometry = Some(geometry.clone());
            }
            let default = config.host.clone();
            println!("wrote {}", config::save(&config)?.display());
            println!(
                "the default host stays {default}; reach this one with --host {host} \
(or make it the default: bash-screen config --host {host})"
            );
            Ok(())
        }
        _ => write_config(Some(host), remote_dir, geometry),
    }
}

/// The config file, or the defaults when there is none but a host was named.
fn load_config(host: Option<&String>) -> Result<config::Config> {
    match host {
        Some(host) if !config::config_path().exists() => Ok(config::Config::for_host(host)),
        _ => config::load(),
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Print a reply: as JSON when asked, else through `text`.
fn show(reply: &Value, json_output: bool, text: impl FnOnce(&Value)) {
    if json_output {
        println!("{reply}");
    } else {
        text(reply);
    }
}

/// Run the preview until the terminal closes. Inside herdr, and when no
/// target was given, the pane follows the session its calling pane last used.
fn run_preview(host: Option<String>, session: Option<String>, interval: f64) -> Result<()> {
    let explicit = host.is_some() || session.is_some();
    // A pane opened by a command is told its session a moment after it starts.
    if !explicit && herdr::preview_is_bound() {
        for _ in 0..10 {
            if herdr::own_preview_target().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    loop {
        let (mut host, mut session) = (host.clone(), session.clone());
        if !explicit {
            if let Some((bound_host, bound_session)) = herdr::own_preview_target() {
                host = Some(bound_host);
                session = Some(bound_session);
            }
        }
        let bound = (host.clone(), session.clone());
        let loaded = load_config(host.as_ref())
            .and_then(|config| Ok((config.target(host.as_deref(), session.as_deref())?, config)));
        let (target, config) = match loaded {
            Ok(loaded) => loaded,
            // Keep the error on screen instead of closing the pane.
            Err(error) => {
                eprintln!("bash-screen: {error:#}");
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
        };
        // The pane can be opened with overrides, to compare settings side by side.
        let preview_blocks =
            env_value("BASH_SCREEN_PREVIEW_BLOCKS").unwrap_or(config.preview_blocks.clone());
        // A fixed resolution from the environment pins the pane; otherwise it
        // follows the shared run-time setting.
        let pinned_max_px: Option<u32> =
            env_value("BASH_SCREEN_PREVIEW_MAX_PX").and_then(|v| v.parse().ok());
        let mode = preview::Mode::resolve(&preview_blocks)?;
        let label = target.label();
        let remote = Remote::new(target);
        let retargeted = move || {
            !explicit
                && herdr::own_preview_target().is_some_and(|(h, s)| (Some(h), Some(s)) != bound)
        };
        preview::run(
            &remote,
            &label,
            Duration::from_secs_f64(interval.max(0.2)),
            mode,
            config.preview_max_px,
            pinned_max_px,
            &retargeted,
        )?;
    }
}

/// The request the helper on the host gets for one action or wait.
fn step_request(command: Cmd) -> Result<Value> {
    Ok(match command {
        Cmd::Click {
            target,
            name,
            button,
            count,
        } => {
            let mut request = json!({"cmd": "click", "button": button, "count": count});
            match (target, name) {
                (Some(target), _) if target.contains(',') => {
                    let xy = parse_numbers(&target, 2, "a position")?;
                    request["x"] = json!(xy[0]);
                    request["y"] = json!(xy[1]);
                }
                (Some(target), _) => request["ref"] = json!(target),
                (None, name) => request["name"] = json!(name),
            }
            request
        }
        Cmd::Drag { from, to } => {
            let from = parse_numbers(&from, 2, "the start position")?;
            let to = parse_numbers(&to, 2, "the end position")?;
            json!({"cmd": "drag", "from": from, "to": to})
        }
        Cmd::Type { target, name, text } => {
            let mut request = json!({"cmd": "type", "text": text});
            if let Some(target) = target {
                request["ref"] = json!(target);
            }
            if let Some(name) = name {
                request["name"] = json!(name);
            }
            request
        }
        Cmd::Key { keys } => {
            if keys.is_empty() {
                bail!("give at least one key combo, for example: bash-screen key ctrl+l");
            }
            json!({"cmd": "key", "keys": keys})
        }
        Cmd::Scroll { target, name, dy } => {
            let mut request = json!({"cmd": "scroll"});
            match (target, name, dy) {
                (Some(target), _, _) => request["ref"] = json!(target),
                (None, Some(name), _) => request["name"] = json!(name),
                (None, None, dy) => request["dy"] = json!(dy.unwrap_or(-5)),
            }
            request
        }
        Cmd::Wait {
            text,
            gone,
            title,
            stable,
            timeout,
        } => json!({
            "cmd": "wait", "text": text, "gone": gone, "title": title,
            "stable": stable, "timeout": timeout,
        }),
        _ => bail!("not an action"),
    })
}

/// The skill is part of the binary, so it always describes this version.
const SKILL: &str = include_str!("../skills/bash-screen/SKILL.md");

/// Print the skill, or write it as `bash-screen/SKILL.md` into each skills
/// directory.
fn skill(install: bool, dirs: &[PathBuf]) -> Result<()> {
    if !install && dirs.is_empty() {
        print!("{SKILL}");
        return Ok(());
    }
    let home = config::home();
    let defaults = [home.join(".claude/skills"), home.join(".agents/skills")];
    let dirs = if dirs.is_empty() { &defaults[..] } else { dirs };
    for dir in dirs {
        let folder = dir.join("bash-screen");
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("cannot create {}", folder.display()))?;
        let file = folder.join("SKILL.md");
        std::fs::write(&file, SKILL).with_context(|| format!("cannot write {}", file.display()))?;
        println!("installed {}", file.display());
    }
    println!("Agents that are already running see the skill after a restart.");
    Ok(())
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let json_output = cli.json;

    if let Cmd::Skill { install, dir } = &cli.command {
        return skill(*install, dir);
    }

    if let Cmd::Config {
        remote_dir,
        geometry,
    } = &cli.command
    {
        if cli.host.is_none() && remote_dir.is_none() && geometry.is_none() {
            let config = config::load()?;
            println!("{}", config::config_path().display());
            print!("{}", toml::to_string_pretty(&config)?);
            return Ok(());
        }
        write_config(cli.host.as_ref(), remote_dir.as_ref(), geometry.as_ref())?;
        return Ok(());
    }

    // Which host and session: the command line, then the environment, then
    // the defaults.
    let mut host = cli.host.clone().or_else(|| env_value("BASH_SCREEN_HOST"));
    let mut session = cli
        .session
        .clone()
        .or_else(|| env_value("BASH_SCREEN_SESSION"));
    if let Cmd::Init {
        init_host,
        remote_dir,
        geometry,
    } = &cli.command
    {
        register_host(init_host, remote_dir.as_ref(), geometry.as_ref())?;
        host = Some(init_host.clone());
    }
    // A viewer pane opened beside a pane shows what that pane used last.
    let follow = env_value("BASH_SCREEN_FOLLOW");
    if let Some((bound_host, bound_session)) = follow.as_deref().and_then(herdr::followed_target) {
        host = Some(bound_host);
        session = Some(bound_session);
    }
    if let Cmd::Preview { interval, follow } = cli.command {
        if let Some(key) = follow {
            herdr::follow(key);
        }
        return run_preview(host, session, interval);
    }

    let config = load_config(host.as_ref())?;
    let target = config.target(host.as_deref(), session.as_deref())?;
    if let Cmd::Web {
        port,
        interval,
        open,
    } = cli.command
    {
        return web::run(target, port, interval, open, json_output);
    }
    let config_max_px = config.preview_max_px;
    let record_key = format!("{}/{}", target.host, target.session);
    if let Cmd::Stats { since } = cli.command {
        return calls::show(&record_key, &target.label(), since, json_output);
    }

    if let Cmd::Look { viewer: true, .. } = cli.command {
        herdr::viewer_seen(follow.as_deref(), &target)?;
    }
    // Any agent-facing command brings up the preview beside the calling pane.
    // A viewer pane is not an agent.
    if follow.is_none()
        && !matches!(
            cli.command,
            Cmd::Stop { .. }
                | Cmd::Vnc
                | Cmd::Setup
                | Cmd::Init { .. }
                | Cmd::Res { .. }
                | Cmd::Input { .. }
                | Cmd::Look { viewer: true, .. }
                | Cmd::Sessions
                | Cmd::Stats { .. }
        )
    {
        if let Some(note) = herdr::ensure_preview_pane(&config.preview_direction, &target)? {
            eprintln!("bash-screen: {note}");
        }
    }
    // "host" in messages names the session too, when it is not the default one.
    let host = target.label();
    let ssh_host = target.host.clone();
    let session_name = target.session.clone();
    let remote = Remote::new(target);
    let observation = |reply: Value| show(&reply, json_output, print_observation);
    // A viewer pane's calls are not an agent's.
    if let Some((command, steps)) = recorded_as(&cli.command).filter(|_| follow.is_none()) {
        calls::begin(&record_key, command, steps);
    }

    match cli.command {
        Cmd::Config { .. }
        | Cmd::Preview { .. }
        | Cmd::Web { .. }
        | Cmd::Skill { .. }
        | Cmd::Stats { .. } => {
            unreachable!()
        }
        Cmd::Init { .. } => {
            if !remote.setup()? {
                bail!("setup on {host} did not finish; see the messages above");
            }
            let reply = remote.call(json!({"cmd": "start"}))?;
            println!(
                "session running on {host} (display {})",
                reply["display"].as_str().unwrap_or("?")
            );
            // Proof that the screen can be read back: a small screenshot.
            let reply = remote.call(json!({"cmd": "look", "scale": 0.25}))?;
            let picture = base64::engine::general_purpose::STANDARD
                .decode(reply["image_webp"].as_str().unwrap_or(""))
                .context("bad image data")?;
            image::load_from_memory_with_format(&picture, image::ImageFormat::WebP)
                .context("the screen could not be read")?;
            println!("screen check passed");
            println!("ready. Try: bash-screen launch -- firefox https://example.org");
        }
        Cmd::Setup => {
            if !remote.setup()? {
                bail!("setup on {host} did not finish; see the messages above");
            }
        }
        Cmd::Start => {
            let reply = remote.call(json!({"cmd": "start"}))?;
            show(&reply, json_output, |reply| {
                let state = if reply["started"] == json!(true) {
                    "started"
                } else {
                    "already running"
                };
                println!(
                    "session {state} on {host} (display {})",
                    reply["display"].as_str().unwrap_or("?")
                );
            });
        }
        Cmd::Stop { forget } => {
            let reply = remote.call(json!({"cmd": "stop", "forget": forget}))?;
            show(&reply, json_output, |_| {
                println!("session stopped on {host}")
            });
        }
        Cmd::Status => {
            let reply = remote.call(json!({"cmd": "status"}))?;
            show(&reply, json_output, |reply| {
                if reply["running"] != json!(true) {
                    println!("no session on {host}");
                    return;
                }
                println!(
                    "session running on {host} (display {})",
                    reply["display"].as_str().unwrap_or("?")
                );
                for window in reply["windows"].as_array().into_iter().flatten() {
                    println!(
                        "  {} ({})",
                        window["title"].as_str().unwrap_or("?"),
                        window["app"].as_str().unwrap_or("?")
                    );
                }
            });
        }
        Cmd::Sessions => {
            let reply = remote.call(json!({"cmd": "sessions"}))?;
            show(&reply, json_output, |reply| {
                let sessions = reply["sessions"].as_array().cloned().unwrap_or_default();
                if sessions.is_empty() {
                    println!("no session on {ssh_host}");
                }
                for session in sessions {
                    let name = session["name"].as_str().unwrap_or("?");
                    match session["display"].as_str() {
                        Some(display) => println!("{name}  running (display {display})"),
                        None => println!("{name}  stopped"),
                    }
                }
            });
        }
        Cmd::Observe { all } => observation(remote.call(json!({"cmd": "observe", "all": all}))?),
        Cmd::Text => {
            let reply = remote.call(json!({"cmd": "text"}))?;
            show(&reply, json_output, print_text);
        }
        Cmd::Look {
            region,
            scale,
            output,
            viewer,
        } => {
            let shows = viewer.then_some((ssh_host.as_str(), session_name.as_str()));
            look(&remote, region, scale, output, json_output, shows)?
        }
        command @ (Cmd::Click { .. }
        | Cmd::Drag { .. }
        | Cmd::Type { .. }
        | Cmd::Key { .. }
        | Cmd::Scroll { .. }
        | Cmd::Wait { .. }) => observation(remote.call(step_request(command)?)?),
        Cmd::Do { steps } => {
            let mut requests = Vec::new();
            let mut reading = None;
            let count = steps.split(|word| word == "then").count();
            for (index, words) in steps.split(|word| word == "then").enumerate() {
                let number = index + 1;
                if words.is_empty() {
                    bail!("step {number} is empty: put a step on each side of \"then\"");
                }
                let parsed = Cli::try_parse_from(
                    std::iter::once("bash-screen").chain(words.iter().map(String::as_str)),
                )
                .map_err(|error| {
                    anyhow::anyhow!("step {number} ({}): {}", words.join(" "), error.kind())
                })?;
                match parsed.command {
                    command @ (Cmd::Click { .. }
                    | Cmd::Drag { .. }
                    | Cmd::Type { .. }
                    | Cmd::Key { .. }
                    | Cmd::Scroll { .. }
                    | Cmd::Wait { .. }) => requests
                        .push(step_request(command).with_context(|| format!("step {number}"))?),
                    Cmd::Text | Cmd::Look { viewer: false, .. } if number < count => bail!(
                        "step {number} ({}): text and look can only be the last step",
                        words[0]
                    ),
                    Cmd::Text => reading = Some(Reading::Text),
                    Cmd::Look {
                        region,
                        scale,
                        output,
                        viewer: false,
                    } => {
                        let region =
                            parse_region(region).with_context(|| format!("step {number}"))?;
                        check_output(&output).with_context(|| format!("step {number}"))?;
                        reading = Some(Reading::Look {
                            region,
                            scale,
                            output,
                        })
                    }
                    _ => bail!(
                        "step {number} ({}): only click, type, key, scroll, drag and wait can be \
steps, and text or look the last one",
                        words[0]
                    ),
                }
            }
            if requests.is_empty() {
                bail!("give at least one step to do before {}", steps[0]);
            }
            let request = json!({"cmd": "do", "steps": requests, "show_on_failure": true});
            let mut reply = match remote.call(request) {
                Ok(reply) => reply,
                Err(error) => {
                    // A failed batch says where things stand; show it with the error.
                    let Some(now) = error
                        .downcast_ref::<HelperError>()
                        .map(|failed| failed.0.clone())
                        .filter(|reply| reply.get("text").is_some())
                    else {
                        return Err(error);
                    };
                    if json_output {
                        println!("{now}");
                    } else {
                        eprintln!("bash-screen: {error:#}");
                        print_observation(&now);
                    }
                    return Err(Reported.into());
                }
            };
            let Some(reading) = reading else {
                observation(reply);
                return Ok(());
            };
            if !json_output {
                print_observation(&reply);
            }
            let read = match reading {
                Reading::Text => remote.call(json!({"cmd": "text"})).map(|text| (text, None)),
                Reading::Look {
                    region,
                    scale,
                    output,
                } => take_look(&remote, region, scale, output, false)
                    .map(|(picture, bytes)| (picture, Some((bytes, scale)))),
            };
            match read {
                Ok((then, look)) => {
                    if json_output {
                        reply["then"] = then;
                        println!("{reply}");
                    } else if let Some((bytes, scale)) = look {
                        print_look(&then, bytes, scale);
                    } else {
                        print_text(&then);
                    }
                }
                Err(error) => {
                    // The steps took effect; say so, or the caller may run them again.
                    let done = reply["steps"].as_u64().unwrap_or(0);
                    let told = format!(
                        "all {done} step(s) were done, but reading the result failed: {error:#}"
                    );
                    if json_output {
                        reply["then"] = json!({"ok": false, "error": told});
                        println!("{reply}");
                    } else {
                        eprintln!("bash-screen: {told}");
                    }
                    return Err(Reported.into());
                }
            }
        }
        Cmd::Launch {
            wait,
            no_a11y,
            argv,
        } => observation(
            remote.call(json!({"cmd": "launch", "argv": argv, "wait": wait, "a11y": !no_a11y}))?,
        ),
        Cmd::Windows => {
            let reply = remote.call(json!({"cmd": "windows"}))?;
            show(&reply, json_output, |reply| {
                for window in reply["windows"].as_array().into_iter().flatten() {
                    let mark = if window["focused"] == json!(true) {
                        "*"
                    } else {
                        " "
                    };
                    println!(
                        "{mark} {} ({})",
                        window["title"].as_str().unwrap_or("?"),
                        window["app"].as_str().unwrap_or("?")
                    );
                }
            });
        }
        Cmd::Res { value } => {
            let current = config::read_res_override().unwrap_or(config_max_px);
            let chosen = match value.as_deref() {
                None => current,
                Some("up") | Some("+") => config::step_res(current, true),
                Some("down") | Some("-") => config::step_res(current, false),
                Some("full") => 0,
                Some(number) => number
                    .parse()
                    .context("give a pixel cap, full, up or down")?,
            };
            if value.is_some() {
                config::write_res_override(chosen)?;
            }
            let describe = |v: u32| {
                if v == 0 {
                    "full".to_string()
                } else {
                    format!("{v} px")
                }
            };
            println!("preview resolution: {}", describe(chosen));
            let levels: Vec<String> = config::RES_LEVELS.iter().map(|&v| describe(v)).collect();
            println!("steps: {}", levels.join(", "));
        }
        Cmd::Input { actions } => {
            let actions: Value =
                serde_json::from_str(&actions).context("ACTIONS must be a JSON list of actions")?;
            let reply = remote.call(json!({"cmd": "input", "actions": actions}))?;
            show(&reply, json_output, |reply| {
                println!("done: {}", reply["done"])
            });
        }
        Cmd::Vnc => {
            let reply = remote.call(json!({"cmd": "status"}))?;
            if reply["running"] != json!(true) {
                bail!("no session on {host}; start one first");
            }
            let socket = reply["vnc_socket"]
                .as_str()
                .unwrap_or("")
                .replace("~", "$HOME");
            if ssh_host == config::LOCAL_HOST {
                println!("connect a VNC client to the socket {socket} (no password)");
            } else {
                println!("ssh -N -L 15901:{socket} {ssh_host}");
                println!("then connect a VNC client to localhost:15901 (no password)");
            }
        }
    }
    Ok(())
}

fn main() {
    let result = run();
    calls::finish(result.is_ok());
    if let Err(error) = result {
        if !error.is::<Reported>() {
            eprintln!("bash-screen: {error:#}");
        }
        std::process::exit(1);
    }
}
