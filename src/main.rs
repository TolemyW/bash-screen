//! bash-screen: operate GUI apps on a remote Linux host over SSH.

mod config;
mod herdr;
mod input;
mod preview;
mod remote;

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use remote::Remote;
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
bash-screen click e12          # or: bash-screen click 640,400\n  \
bash-screen type --ref e7 hello\n  \
bash-screen look               # screenshot, when the element list is not enough\n\n\
Every action prints a fresh observation. Refs are only valid for the latest observation.\n\
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
    },
    /// Click an element ref (e12) or a screen position (x,y)
    Click {
        target: String,
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
        #[arg(long = "ref")]
        target: Option<String>,
        text: String,
    },
    /// Press key combos in order, for example: ctrl+l  return  down
    Key { keys: Vec<String> },
    /// Scroll an element into view (--ref), or turn the wheel (--dy, negative is down)
    Scroll {
        #[arg(long = "ref")]
        target: Option<String>,
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
    /// List the windows in the session
    Windows,
    /// Print the commands a person needs to take over the session with a VNC client
    Vnc,
    /// Show or change the preview resolution: a pixel cap, "full", "up" or "down"
    Res { value: Option<String> },
    /// Live preview of the screen, in this terminal. Inside herdr it opens by
    /// itself beside the pane that runs commands
    Preview {
        /// Seconds between refreshes
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
    },
}

fn print_observation(reply: &Value) {
    if let Some(text) = reply["text"].as_str() {
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

fn look(
    remote: &Remote,
    region: Option<String>,
    scale: f64,
    output: Option<PathBuf>,
    json_output: bool,
) -> Result<()> {
    let mut request = json!({"cmd": "look", "scale": scale});
    if let Some(region) = region {
        request["region"] = json!(parse_numbers(&region, 4, "--region")?);
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
    if json_output {
        // The picture is in the file; the reply says where it is.
        reply.as_object_mut().unwrap().remove("image_webp");
        reply["path"] = json!(path.display().to_string());
        println!("{reply}");
        return Ok(());
    }
    let origin = &reply["origin"];
    println!("{}", path.display());
    println!(
        "{}x{} px, transferred {} bytes; screen position = image position / {} + ({},{})",
        reply["size"][0],
        reply["size"][1],
        webp.len(),
        scale,
        origin[0],
        origin[1]
    );
    Ok(())
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
    if !explicit && std::env::var_os("HERDR_PANE_ID").is_some() {
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    let json_output = cli.json;

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
    let session = cli
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
    if let Cmd::Preview { interval } = cli.command {
        return run_preview(host, session, interval);
    }

    let config = load_config(host.as_ref())?;
    let target = config.target(host.as_deref(), session.as_deref())?;
    let config_max_px = config.preview_max_px;

    // Any agent-facing command brings up the preview beside the calling pane.
    if !matches!(
        cli.command,
        Cmd::Stop { .. }
            | Cmd::Vnc
            | Cmd::Setup
            | Cmd::Init { .. }
            | Cmd::Res { .. }
            | Cmd::Sessions
    ) {
        if let Some(note) = herdr::ensure_preview_pane(&config.preview_direction, &target)? {
            eprintln!("bash-screen: {note}");
        }
    }
    // "host" in messages names the session too, when it is not the default one.
    let host = target.label();
    let ssh_host = target.host.clone();
    let remote = Remote::new(target);
    let observation = |reply: Value| show(&reply, json_output, print_observation);

    match cli.command {
        Cmd::Config { .. } | Cmd::Preview { .. } => unreachable!(),
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
        Cmd::Look {
            region,
            scale,
            output,
        } => look(&remote, region, scale, output, json_output)?,
        Cmd::Click {
            target,
            button,
            count,
        } => {
            let mut request = json!({"cmd": "click", "button": button, "count": count});
            if target.contains(',') {
                let xy = parse_numbers(&target, 2, "a position")?;
                request["x"] = json!(xy[0]);
                request["y"] = json!(xy[1]);
            } else {
                request["ref"] = json!(target);
            }
            observation(remote.call(request)?);
        }
        Cmd::Drag { from, to } => {
            let from = parse_numbers(&from, 2, "the start position")?;
            let to = parse_numbers(&to, 2, "the end position")?;
            observation(remote.call(json!({"cmd": "drag", "from": from, "to": to}))?);
        }
        Cmd::Type { target, text } => {
            let mut request = json!({"cmd": "type", "text": text});
            if let Some(target) = target {
                request["ref"] = json!(target);
            }
            observation(remote.call(request)?);
        }
        Cmd::Key { keys } => {
            if keys.is_empty() {
                bail!("give at least one key combo, for example: bash-screen key ctrl+l");
            }
            observation(remote.call(json!({"cmd": "key", "keys": keys}))?);
        }
        Cmd::Scroll { target, dy } => {
            let mut request = json!({"cmd": "scroll"});
            match (target, dy) {
                (Some(target), _) => request["ref"] = json!(target),
                (None, dy) => request["dy"] = json!(dy.unwrap_or(-5)),
            }
            observation(remote.call(request)?);
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
    if let Err(error) = run() {
        eprintln!("bash-screen: {error:#}");
        std::process::exit(1);
    }
}
