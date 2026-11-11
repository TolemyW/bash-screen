//! Live preview of the remote screen plus the most recent actions. Meant to
//! run in a herdr pane beside the agent.
//!
//! The picture is a real image when the terminal supports the Kitty graphics
//! protocol (herdr renders it natively), and block characters otherwise.

use crate::config;
use crate::input::{self, Input};
use crate::remote::Remote;
use anyhow::{bail, Result};
use base64::Engine;
use serde_json::json;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::time::Duration;

/// Kitty image id used for the preview; re-sending it replaces the picture in place.
const IMAGE_ID: u32 = 7;

/// How the picture is drawn.
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// A real image through the Kitty graphics protocol.
    Image,
    /// Block characters with the given sub-pixel grid per cell.
    Blocks(Blocks),
}

/// How many sub-pixels one terminal cell is divided into. More sub-pixels give
/// a sharper picture, but each cell can still show only two colours.
#[derive(Clone, Copy, PartialEq)]
pub enum Blocks {
    /// 1x2: one pixel per colour, the most faithful colours, the coarsest shapes.
    Half,
    /// 2x2: quadrant characters, supported by practically every terminal font.
    Quad,
    /// 2x3: sextant characters (Unicode 13). The sharpest, needs font support.
    Sextant,
}

impl Mode {
    /// Resolve the configured name; "auto" asks the terminal whether it can show images.
    pub fn resolve(name: &str) -> Result<Self> {
        Ok(match name {
            "auto" => {
                if terminal_shows_images() {
                    Mode::Image
                } else {
                    Mode::Blocks(Blocks::Sextant)
                }
            }
            "image" => Mode::Image,
            "half" => Mode::Blocks(Blocks::Half),
            "quad" => Mode::Blocks(Blocks::Quad),
            "sextant" => Mode::Blocks(Blocks::Sextant),
            _ => bail!("preview_blocks must be auto, image, sextant, quad or half"),
        })
    }
}

impl Blocks {
    /// Sub-pixel columns and rows per cell.
    fn grid(self) -> (usize, usize) {
        match self {
            Blocks::Half => (1, 2),
            Blocks::Quad => (2, 2),
            Blocks::Sextant => (2, 3),
        }
    }

    /// The character whose lit sub-pixels match `mask` (bit i = sub-pixel i,
    /// counted left to right, top to bottom).
    fn glyph(self, mask: usize) -> char {
        match self {
            Blocks::Half => [' ', '\u{2580}', '\u{2584}', '\u{2588}'][mask],
            Blocks::Quad => [
                ' ', '\u{2598}', '\u{259D}', '\u{2580}', '\u{2596}', '\u{258C}', '\u{259E}',
                '\u{259B}', '\u{2597}', '\u{259A}', '\u{2590}', '\u{259C}', '\u{2584}', '\u{2599}',
                '\u{259F}', '\u{2588}',
            ][mask],
            Blocks::Sextant => match mask {
                0 => ' ',
                21 => '\u{258C}',
                42 => '\u{2590}',
                63 => '\u{2588}',
                // The sextant block skips the two patterns that already exist
                // as left and right half blocks.
                n => char::from_u32(0x1FB00 + n as u32 - 1 - (n > 21) as u32 - (n > 42) as u32)
                    .unwrap_or(' '),
            },
        }
    }
}

/// Terminal size in cells and the pixel size of one cell.
struct Geometry {
    cols: usize,
    rows: usize,
    cell_w: usize,
    cell_h: usize,
}

fn geometry() -> Geometry {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    let (cols, rows) = if ok && size.ws_col > 0 && size.ws_row > 0 {
        (size.ws_col as usize, size.ws_row as usize)
    } else {
        (80, 24)
    };
    // Terminals that do not report pixels get a typical cell size.
    let cell_w = if size.ws_xpixel > 0 {
        size.ws_xpixel as usize / cols
    } else {
        9
    };
    let cell_h = if size.ws_ypixel > 0 {
        size.ws_ypixel as usize / rows
    } else {
        18
    };
    Geometry {
        cols,
        rows,
        cell_w: cell_w.max(1),
        cell_h: cell_h.max(1),
    }
}

/// Ask the terminal whether it implements the Kitty graphics protocol: send a
/// graphics query followed by a device-attributes request, and see whether the
/// graphics answer arrives before the device-attributes one.
fn terminal_shows_images() -> bool {
    unsafe {
        let fd = libc::STDIN_FILENO;
        let mut saved: libc::termios = std::mem::zeroed();
        if libc::isatty(fd) == 0 || libc::tcgetattr(fd, &mut saved) != 0 {
            return false;
        }
        let mut raw = saved;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(fd, libc::TCSANOW, &raw);
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
        let _ = out.flush();
        let mut reply = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if libc::poll(&mut poll, 1, 700) <= 0 {
                break;
            }
            match std::io::stdin().read(&mut buffer) {
                Ok(n) if n > 0 => reply.extend_from_slice(&buffer[..n]),
                _ => break,
            }
            // The device-attributes reply ends with 'c' and always comes last.
            if reply.windows(2).any(|w| w == b"\x1b[") && reply.last() == Some(&b'c') {
                break;
            }
        }
        libc::tcsetattr(fd, libc::TCSANOW, &saved);
        String::from_utf8_lossy(&reply).contains("_Gi=31;OK")
    }
}

/// What is currently on screen, so unchanged frames are not redrawn.
#[derive(Default)]
struct Shown {
    hash: String,
    host_line: String,
    title: String,
    detail: String,
    /// Size of the last picture received, for display.
    sent: String,
    sent_line: String,
    /// Recent actions, oldest first.
    events: Vec<String>,
    /// Picture area in cells.
    cells_w: usize,
    cells_h: usize,
    /// Character rows of the picture in block mode, ready to print.
    block_rows: Vec<String>,
    /// Kitty transmission of the picture in image mode, ready to print.
    image: String,
}

fn luminance(p: [u8; 3]) -> u32 {
    299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32
}

/// Reduce one cell's sub-pixels to two colours and the mask of which sub-pixels
/// take the foreground colour. The split is by brightness around the cell mean.
fn two_colours(pixels: &[[u8; 3]]) -> (usize, [u8; 3], [u8; 3]) {
    let mean = pixels.iter().map(|p| luminance(*p)).sum::<u32>() / pixels.len() as u32;
    let mut mask = 0;
    let mut sums = [[0u32; 3]; 2];
    let mut counts = [0u32; 2];
    for (i, p) in pixels.iter().enumerate() {
        let bright = (luminance(*p) > mean) as usize;
        mask |= bright << i;
        counts[bright] += 1;
        for c in 0..3 {
            sums[bright][c] += p[c] as u32;
        }
    }
    let average = |side: usize| {
        let n = counts[side].max(1);
        [
            (sums[side][0] / n) as u8,
            (sums[side][1] / n) as u8,
            (sums[side][2] / n) as u8,
        ]
    };
    let (dark, bright) = (average(0), average(1));
    // A flat cell has no bright pixels: both colours are the same.
    (mask, if counts[1] == 0 { dark } else { bright }, dark)
}

/// Turn raw RGB sub-pixels into one string of coloured block characters per row.
fn block_rows(rgb: &[u8], width: usize, height: usize, blocks: Blocks) -> Vec<String> {
    let (sx, sy) = blocks.grid();
    let mut rows = Vec::with_capacity(height / sy);
    let mut pixels = Vec::with_capacity(sx * sy);
    for cy in 0..height / sy {
        let mut row = String::new();
        for cx in 0..width / sx {
            pixels.clear();
            for dy in 0..sy {
                for dx in 0..sx {
                    let i = ((cy * sy + dy) * width + cx * sx + dx) * 3;
                    pixels.push([rgb[i], rgb[i + 1], rgb[i + 2]]);
                }
            }
            let (mask, fg, bg) = two_colours(&pixels);
            let _ = write!(
                row,
                "\x1b[38;2;{};{};{};48;2;{};{};{}m{}",
                fg[0],
                fg[1],
                fg[2],
                bg[0],
                bg[1],
                bg[2],
                blocks.glyph(mask)
            );
        }
        row.push_str("\x1b[0m");
        rows.push(row);
    }
    rows
}

/// Kitty graphics transmission that shows `png` scaled to the given cell area
/// at the cursor. Re-using the image id replaces the previous picture.
fn kitty_image(png: &[u8], cells_w: usize, cells_h: usize) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(png);
    let mut out = String::with_capacity(data.len() + 256);
    let mut chunks = data.as_bytes().chunks(4096).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = chunks.peek().is_some() as u8;
        if first {
            // q=2 keeps the terminal from answering; C=1 leaves the cursor in place.
            let _ = write!(
                out,
                "\x1b_Ga=T,f=100,i={IMAGE_ID},p=1,c={cells_w},r={cells_h},C=1,q=2,m={more};"
            );
            first = false;
        } else {
            let _ = write!(out, "\x1b_Gm={more};");
        }
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        out.push_str("\x1b\\");
    }
    out
}

fn clip(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// Narrowest side column worth having, and how many actions the stacked
/// layout lists under the picture.
const SIDE_MIN: usize = 26;
const STACKED_EVENTS: usize = 2;
/// Wheel lines scrolled on the remote screen per wheel report.
const WHEEL_LINES: i64 = 2;
/// Longest edge of the picture while a person is controlling the screen.
const CONTROL_MIN_PX: u32 = 960;

/// Where the picture and the text go. The pane is rarely the same shape as the
/// remote screen, so whichever direction has spare room holds the text:
/// a side column when the picture is limited by height, lines below it otherwise.
#[derive(Clone, Copy, PartialEq, Default)]
struct Layout {
    side: bool,
    /// Area the picture may use, in cells.
    box_cols: usize,
    box_rows: usize,
}

/// Pick the layout that gives the larger picture for a screen of this aspect.
fn choose_layout(geo: &Geometry, aspect: f64) -> Layout {
    // Height in pixels of the picture when fitted into a box of cells.
    let fitted = |cols: usize, rows: usize| {
        let (w, h) = ((cols * geo.cell_w) as f64, (rows * geo.cell_h) as f64);
        h.min(w / aspect)
    };
    let stacked = Layout {
        side: false,
        box_cols: geo.cols,
        box_rows: geo.rows.saturating_sub(1 + STACKED_EVENTS).max(1),
    };
    if geo.cols < SIDE_MIN + 20 {
        return stacked;
    }
    let side = Layout {
        side: true,
        box_cols: geo.cols - SIDE_MIN - 1,
        box_rows: geo.rows,
    };
    if fitted(side.box_cols, side.box_rows) >= fitted(stacked.box_cols, stacked.box_rows) {
        side
    } else {
        stacked
    }
}

/// Row and column (1-based) of the picture's top-left cell: at the left edge
/// in the side layout, centred under the status line otherwise.
fn picture_origin(shown: &Shown, geo: &Geometry, layout: Layout) -> (usize, usize) {
    if layout.side {
        (1, 1)
    } else {
        (2, 1 + geo.cols.saturating_sub(shown.cells_w) / 2)
    }
}

/// Remote screen position under a mouse report, or None outside the picture.
/// `x` and `y` are 1-based, in pixels when `pixels` is set and in cells otherwise.
fn to_screen(
    (x, y): (usize, usize),
    pixels: bool,
    shown: &Shown,
    geo: &Geometry,
    layout: Layout,
    screen: (f64, f64),
) -> Option<(i64, i64)> {
    let (cell_w, cell_h) = (geo.cell_w as f64, geo.cell_h as f64);
    // A cell report means "somewhere in this cell": take its centre.
    let (px, py) = if pixels {
        (x as f64 - 1.0, y as f64 - 1.0)
    } else {
        ((x as f64 - 0.5) * cell_w, (y as f64 - 0.5) * cell_h)
    };
    let (top, left) = picture_origin(shown, geo, layout);
    let across = (px - (left - 1) as f64 * cell_w) / (shown.cells_w as f64 * cell_w);
    let down = (py - (top - 1) as f64 * cell_h) / (shown.cells_h as f64 * cell_h);
    if shown.cells_w == 0 || !(0.0..1.0).contains(&across) || !(0.0..1.0).contains(&down) {
        return None;
    }
    Some(((across * screen.0) as i64, (down * screen.1) as i64))
}

/// Compose the whole screen. Every line is placed with an absolute cursor move
/// and nothing ends with a newline, so the pane never scrolls.
fn compose(
    shown: &Shown,
    mode: Mode,
    geo: &Geometry,
    layout: Layout,
    picture_changed: bool,
    control: bool,
) -> String {
    // The heading turns red while the pane is controlling the remote screen.
    let heading = if control { "\x1b[1;97;41m" } else { "\x1b[7m" };
    // Synchronised update: the terminal shows the frame only when it is complete.
    let mut out = String::from("\x1b[?2026h");
    let (top, left) = picture_origin(shown, geo, layout);
    match mode {
        Mode::Blocks(_) => {
            for (i, row) in shown.block_rows.iter().enumerate() {
                let _ = write!(out, "\x1b[{};{}H{}", top + i, left, row);
            }
        }
        Mode::Image => {
            if picture_changed && !shown.image.is_empty() {
                let _ = write!(out, "\x1b[{top};{left}H{}", shown.image);
            }
        }
    }

    if layout.side {
        // Text column to the right of the picture: where, what, then the actions.
        let column = shown.cells_w + 2;
        let width = geo.cols.saturating_sub(column).max(1);
        let mut lines: Vec<(bool, String)> = vec![(true, format!(" {}", shown.host_line))];
        lines.push((false, shown.title.clone()));
        lines.push((false, shown.detail.clone()));
        lines.push((false, shown.sent_line.clone()));
        lines.push((false, String::new()));
        let room = geo.rows.saturating_sub(lines.len());
        let skip = shown.events.len().saturating_sub(room);
        lines.extend(shown.events[skip..].iter().map(|e| (false, e.clone())));
        for row in 1..=geo.rows {
            let _ = write!(out, "\x1b[{row};{column}H");
            match lines.get(row - 1) {
                Some((true, text)) => {
                    let _ = write!(out, "{heading}{:<w$}\x1b[0m", clip(text, width), w = width);
                }
                Some((false, text)) => {
                    let _ = write!(out, "\x1b[2m{}\x1b[0m\x1b[K", clip(text, width));
                }
                None => out.push_str("\x1b[K"),
            }
        }
    } else {
        let status = format!(
            " {}  {}  {}  {}",
            shown.host_line, shown.title, shown.detail, shown.sent_line
        );
        let _ = write!(
            out,
            "\x1b[1;1H{heading}{:<w$}\x1b[0m",
            clip(&status, geo.cols),
            w = geo.cols
        );
        let mut row = 2 + shown.cells_h;
        let skip = shown.events.len().saturating_sub(STACKED_EVENTS);
        for event in &shown.events[skip..] {
            if row > geo.rows {
                break;
            }
            let _ = write!(
                out,
                "\x1b[{row};1H\x1b[2m{}\x1b[0m\x1b[K",
                clip(event, geo.cols)
            );
            row += 1;
        }
        while row <= geo.rows {
            let _ = write!(out, "\x1b[{row};1H\x1b[K");
            row += 1;
        }
    }
    out.push_str("\x1b[?2026l");
    out
}

fn clear_all(stdout: &mut std::io::Stdout, mode: Mode) -> Result<()> {
    if mode == Mode::Image {
        write!(stdout, "\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\")?;
    }
    write!(stdout, "\x1b[2J")?;
    Ok(())
}

/// `default_max_px` comes from the config. A `pinned` value fixes the
/// resolution; otherwise the pane follows the shared run-time setting, which
/// the + and - keys (and `bash-screen res`) change.
///
/// Pressing `i` starts controlling the remote screen: from then on clicks,
/// the wheel and every key go to it, until Ctrl+] is pressed.
pub fn run(
    remote: &Remote,
    host: &str,
    interval: Duration,
    mode: Mode,
    default_max_px: u32,
    pinned: Option<u32>,
) -> Result<()> {
    input::set_terminal(false);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b[?25l")?;
    clear_all(&mut stdout, mode)?;
    let mut shown = Shown::default();
    let mut last_size = (0, 0);
    let mut last_layout = Layout::default();
    // Width / height of the remote screen; refined by the first reply.
    let mut aspect = 1.6;
    let mut screen = (0.0, 0.0);
    let mut control = false;
    // Whether the terminal reports the mouse in pixels rather than cells.
    let mut pixel_mouse = false;
    // Refreshes still to be done quickly, to follow the effect of an input.
    let mut quick_refreshes = 0;
    let mut notice = String::new();
    loop {
        let mut max_px =
            pinned.unwrap_or_else(|| config::read_res_override().unwrap_or(default_max_px));
        if control && pinned.is_none() && (1..CONTROL_MIN_PX).contains(&max_px) {
            // Small targets are hard to hit in a coarse picture.
            max_px = CONTROL_MIN_PX;
        }
        let heading = match (control, notice.is_empty()) {
            (false, _) => format!("bash-screen  {host}"),
            (true, true) => format!("CONTROLLING {host}  Ctrl+] to leave"),
            (true, false) => format!("CONTROLLING {host}  ! {notice}"),
        };
        let geo = geometry();
        let layout = choose_layout(&geo, aspect);
        let mut redraw = (geo.cols, geo.rows) != last_size || layout != last_layout;
        if redraw {
            // The cached picture no longer fits.
            last_size = (geo.cols, geo.rows);
            last_layout = layout;
            shown = Shown::default();
            clear_all(&mut stdout, mode)?;
        }
        let mut request = json!({
            "cmd": "frame", "cols": layout.box_cols, "rows": layout.box_rows,
            "have": shown.hash, "events": geo.rows.max(STACKED_EVENTS),
        });
        match mode {
            Mode::Image => {
                request["px_w"] = json!(layout.box_cols * geo.cell_w);
                request["px_h"] = json!(layout.box_rows * geo.cell_h);
                request["max_px"] = json!(max_px);
            }
            Mode::Blocks(blocks) => {
                request["sx"] = json!(blocks.grid().0);
                request["sy"] = json!(blocks.grid().1);
            }
        }

        let before = (
            shown.host_line.clone(),
            shown.title.clone(),
            shown.detail.clone() + &shown.sent_line,
            shown.events.clone(),
        );
        let mut picture_changed = false;
        shown.host_line = heading.clone();
        let started = std::time::Instant::now();
        let mut transcode_ms = 0;
        let reply = remote.call(request);
        let call_ms = started.elapsed().as_millis();
        match reply {
            Ok(reply) if reply["running"] == json!(false) => {
                if !shown.hash.is_empty() || !shown.block_rows.is_empty() || !shown.image.is_empty()
                {
                    shown = Shown::default();
                    shown.host_line = heading.clone();
                    clear_all(&mut stdout, mode)?;
                    redraw = true;
                }
                shown.title = "(no session yet)".into();
                shown.detail.clear();
            }
            Ok(reply) => {
                let decode = |key: &str| {
                    reply[key]
                        .as_str()
                        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
                };
                let number = |key: &str| reply[key].as_u64().unwrap_or(0) as usize;
                let (w, h) = (number("w"), number("h"));
                if let (Some(sw), Some(sh)) =
                    (reply["screen"][0].as_f64(), reply["screen"][1].as_f64())
                {
                    if sh > 0.0 {
                        aspect = sw / sh;
                        screen = (sw, sh);
                    }
                }
                let (old_w, old_h) = (shown.cells_w, shown.cells_h);
                if let (Mode::Blocks(blocks), Some(png)) = (mode, decode("png")) {
                    let rgb = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?
                        .to_rgb8()
                        .into_raw();
                    shown.sent = format!(
                        "{}x{} sub-pixels, {:.1} KB/frame   [i] control",
                        w,
                        h,
                        png.len() as f64 / 1024.0
                    );
                    if w > 0 && rgb.len() == w * h * 3 {
                        let (sx, sy) = blocks.grid();
                        shown.block_rows = block_rows(&rgb, w, h, blocks);
                        shown.cells_w = w / sx;
                        shown.cells_h = h / sy;
                        picture_changed = true;
                    }
                } else if let (Mode::Image, Some(webp)) = (mode, decode("webp")) {
                    // The picture travels as WebP; Kitty graphics needs PNG.
                    let transcode = std::time::Instant::now();
                    let image =
                        image::load_from_memory_with_format(&webp, image::ImageFormat::WebP)?;
                    shown.sent = format!(
                        "{}x{} px, {:.1} KB/frame   [+/-] resolution  [i] control",
                        w,
                        h,
                        webp.len() as f64 / 1024.0
                    );
                    let mut png = Vec::new();
                    image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
                    shown.cells_w = (number("display_w") / geo.cell_w).clamp(1, layout.box_cols);
                    shown.cells_h = (number("display_h") / geo.cell_h).clamp(1, layout.box_rows);
                    shown.image = kitty_image(&png, shown.cells_w, shown.cells_h);
                    transcode_ms = transcode.elapsed().as_millis();
                    picture_changed = true;
                }
                if picture_changed && (old_w, old_h) != (shown.cells_w, shown.cells_h) && old_w > 0
                {
                    // The picture changed shape: wipe what the old one left behind.
                    clear_all(&mut stdout, mode)?;
                    redraw = true;
                }
                shown.hash = reply["hash"].as_str().unwrap_or("").to_string();
                shown.title = reply["title"].as_str().unwrap_or("-").to_string();
                shown.detail = format!(
                    "{} window(s), {}",
                    number("windows"),
                    if mode == Mode::Image {
                        "image"
                    } else {
                        "blocks"
                    }
                );
                shown.sent_line = shown.sent.clone();
                shown.events = reply["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        format!(
                            "{:>3} {}",
                            e["seq"].as_u64().unwrap_or(0),
                            e["what"].as_str().unwrap_or("")
                        )
                    })
                    .collect();
            }
            Err(error) => {
                shown.title = format!("! {error}");
                shown.detail.clear();
            }
        }

        // Redraw only when something on screen actually changed.
        let after = (
            shown.host_line.clone(),
            shown.title.clone(),
            shown.detail.clone() + &shown.sent_line,
            shown.events.clone(),
        );
        if redraw || picture_changed || before != after {
            let repaint_picture = picture_changed || redraw;
            stdout.write_all(
                compose(&shown, mode, &geo, layout, repaint_picture, control).as_bytes(),
            )?;
            stdout.flush()?;
            if std::env::var_os("BASH_SCREEN_DEBUG").is_some() {
                // Timing of one redraw, for diagnosing a slow preview.
                let line = format!(
                    "remote {call_ms} ms, transcode {transcode_ms} ms, total {} ms, {} bytes to terminal, picture {}{}",
                    started.elapsed().as_millis(), shown.image.len(), repaint_picture, char::from(10)
                );
                if let Ok(dir) = config::state_dir() {
                    use std::fs::OpenOptions;
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("preview-timing.log"))
                    {
                        let _ = file.write_all(line.as_bytes());
                    }
                }
            }
        }
        let wait = if quick_refreshes > 0 {
            quick_refreshes -= 1;
            Duration::from_millis(120)
        } else {
            interval
        };
        let mut pending = input::read(wait);
        while let Some(bytes) = pending.take() {
            let mut actions: Vec<serde_json::Value> = Vec::new();
            for item in input::parse(&bytes) {
                if !control {
                    // Not controlling: keys are commands for the preview itself.
                    let Input::Text(text) = item else { continue };
                    for key in text.chars() {
                        let chosen = match key {
                            '+' | '=' => Some(config::step_res(max_px, true)),
                            '-' | '_' => Some(config::step_res(max_px, false)),
                            '1'..='4' => Some(config::RES_LEVELS[key as usize - '1' as usize]),
                            _ => None,
                        };
                        if let (Some(chosen), None, Mode::Image) = (chosen, pinned, mode) {
                            config::write_res_override(chosen)?;
                        }
                        if key == 'i' {
                            control = true;
                            pixel_mouse = false;
                            notice.clear();
                            input::set_terminal(true);
                            stdout.write_all(input::ENTER_CONTROL.as_bytes())?;
                            stdout.flush()?;
                            break;
                        }
                    }
                    continue;
                }
                match item {
                    Input::Leave => {
                        control = false;
                        stdout.write_all(input::LEAVE_CONTROL.as_bytes())?;
                        stdout.flush()?;
                        input::set_terminal(false);
                    }
                    Input::PixelMouse(reported) => pixel_mouse = reported,
                    Input::Press { button, x, y } => {
                        // A position beyond the pane's cells can only be pixels.
                        pixel_mouse |= x > geo.cols || y > geo.rows;
                        if let Some((sx, sy)) =
                            to_screen((x, y), pixel_mouse, &shown, &geo, layout, screen)
                        {
                            actions
                                .push(json!({"kind": "click", "x": sx, "y": sy, "button": button}));
                        }
                    }
                    Input::Wheel { up, x, y } => {
                        pixel_mouse |= x > geo.cols || y > geo.rows;
                        let Some((sx, sy)) =
                            to_screen((x, y), pixel_mouse, &shown, &geo, layout, screen)
                        else {
                            continue;
                        };
                        let lines = if up { WHEEL_LINES } else { -WHEEL_LINES };
                        // A spin of the wheel arrives as many reports: add them up.
                        match actions.last_mut() {
                            Some(last) if last["kind"] == "wheel" => {
                                last["dy"] = json!(last["dy"].as_i64().unwrap_or(0) + lines);
                            }
                            _ => actions
                                .push(json!({"kind": "wheel", "x": sx, "y": sy, "dy": lines})),
                        }
                    }
                    Input::Text(text) => actions.push(json!({"kind": "text", "text": text})),
                    Input::Key(key) => actions.push(json!({"kind": "key", "key": key})),
                }
            }
            if !actions.is_empty() {
                match remote.call(json!({"cmd": "input", "actions": actions})) {
                    Ok(_) => notice.clear(),
                    Err(error) => notice = error.to_string(),
                }
                quick_refreshes = 4;
                // Input that arrived meanwhile goes out before the next frame.
                pending = input::read(Duration::ZERO);
            }
        }
    }
}
