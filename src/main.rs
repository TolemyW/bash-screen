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
Coordinates are screen pixels, the same as in `look` images at scale 1."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show or write the connection config
    Config {
        /// SSH host to connect to
        #[arg(long)]
        host: Option<String>,
        /// Remote directory holding the session tools
        #[arg(long)]
        remote_dir: Option<String>,
        /// Virtual screen size, for example 1280x800
        #[arg(long)]
        geometry: Option<String>,
    },
    /// Install the remote tools on the configured host (no root needed)
    Setup,
    /// Start the remote GUI session (other commands start it on demand)
    Start,
    /// Stop the remote GUI session and everything running in it
    Stop,
    /// Show whether the session is running and which windows are open
    Status,
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
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// List the windows in the session
    Windows,
    /// Print the commands a person needs to take over the session with a VNC client
    Vnc,
    /// Show or change the preview resolution: a pixel cap, "full", "up" or "down"
    Res { value: Option<String> },
    /// Live preview of the remote screen (runs in the herdr preview pane)
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
) -> Result<()> {
    let mut request = json!({"cmd": "look", "scale": scale});
    if let Some(region) = region {
        request["region"] = json!(parse_numbers(&region, 4, "--region")?);
    }
    let reply = remote.call(request)?;
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

fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Cmd::Config {
        host,
        remote_dir,
        geometry,
    } = &cli.command
    {
        if host.is_none() && remote_dir.is_none() && geometry.is_none() {
            let config = config::load()?;
            println!("{}", config::config_path().display());
            print!("{}", toml::to_string_pretty(&config)?);
            return Ok(());
        }
        let mut config = config::load().unwrap_or(config::Config {
            host: String::new(),
            remote_dir: "~/.bash-screen".into(),
            geometry: "1280x800".into(),
            preview_direction: "auto".into(),
            preview_blocks: "auto".into(),
            preview_max_px: 640,
        });
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
        return Ok(());
    }

    let config = match (config::load(), &cli.command) {
        (Ok(config), _) => config,
        // In the preview pane, keep the error on screen instead of closing the pane.
        (Err(error), Cmd::Preview { .. }) => {
            eprintln!("bash-screen: {error:#}");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        (Err(error), _) => return Err(error),
    };
    let host = config.host.clone();
    let preview_direction = config.preview_direction.clone();
    // The preview pane can be opened with overrides, to compare settings side by side.
    let preview_blocks =
        std::env::var("BASH_SCREEN_PREVIEW_BLOCKS").unwrap_or(config.preview_blocks.clone());
    // A fixed resolution from the environment pins the pane; otherwise it
    // follows the shared run-time setting.
    let pinned_max_px: Option<u32> = std::env::var("BASH_SCREEN_PREVIEW_MAX_PX")
        .ok()
        .and_then(|v| v.parse().ok());
    let config_max_px = config.preview_max_px;
    let remote = Remote::new(config);

    if let Cmd::Preview { interval } = cli.command {
        let mode = preview::Mode::resolve(&preview_blocks)?;
        return preview::run(
            &remote,
            &host,
            Duration::from_secs_f64(interval.max(0.2)),
            mode,
            config_max_px,
            pinned_max_px,
        );
    }

    // Any agent-facing command brings up the preview beside the calling pane.
    if !matches!(
        cli.command,
        Cmd::Stop | Cmd::Vnc | Cmd::Setup | Cmd::Res { .. }
    ) {
        if let Some(note) = herdr::ensure_preview_pane(&preview_direction)? {
            eprintln!("bash-screen: {note}");
        }
    }

    match cli.command {
        Cmd::Config { .. } | Cmd::Preview { .. } => unreachable!(),
        Cmd::Setup => {
            if !remote.setup()? {
                bail!("setup on {host} did not finish; see the messages above");
            }
        }
        Cmd::Start => {
            let reply = remote.call(json!({"cmd": "start"}))?;
            let state = if reply["started"] == json!(true) {
                "started"
            } else {
                "already running"
            };
            println!(
                "session {state} on {host} (display {})",
                reply["display"].as_str().unwrap_or("?")
            );
        }
        Cmd::Stop => {
            remote.call(json!({"cmd": "stop"}))?;
            println!("session stopped on {host}");
        }
        Cmd::Status => {
            let reply = remote.call(json!({"cmd": "status"}))?;
            if reply["running"] != json!(true) {
                println!("no session on {host}");
            } else {
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
            }
        }
        Cmd::Observe { all } => {
            print_observation(&remote.call(json!({"cmd": "observe", "all": all}))?)
        }
        Cmd::Look {
            region,
            scale,
            output,
        } => look(&remote, region, scale, output)?,
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
            print_observation(&remote.call(request)?);
        }
        Cmd::Type { target, text } => {
            let mut request = json!({"cmd": "type", "text": text});
            if let Some(target) = target {
                request["ref"] = json!(target);
            }
            print_observation(&remote.call(request)?);
        }
        Cmd::Key { keys } => {
            if keys.is_empty() {
                bail!("give at least one key combo, for example: bash-screen key ctrl+l");
            }
            print_observation(&remote.call(json!({"cmd": "key", "keys": keys}))?);
        }
        Cmd::Scroll { target, dy } => {
            let mut request = json!({"cmd": "scroll"});
            match (target, dy) {
                (Some(target), _) => request["ref"] = json!(target),
                (None, dy) => request["dy"] = json!(dy.unwrap_or(-5)),
            }
            print_observation(&remote.call(request)?);
        }
        Cmd::Launch { wait, argv } => {
            print_observation(&remote.call(json!({"cmd": "launch", "argv": argv, "wait": wait}))?)
        }
        Cmd::Windows => {
            let reply = remote.call(json!({"cmd": "windows"}))?;
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
            println!("ssh -N -L 15901:{socket} {host}");
            println!("then connect a VNC client to localhost:15901 (no password)");
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
