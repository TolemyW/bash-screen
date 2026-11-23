//! bash-screen remote helper.
//!
//! Runs on the remote host, once per request, over SSH. It reads one JSON
//! request from stdin and writes one JSON response to stdout:
//!
//!     request:  {"cmd": "<name>", ...arguments}
//!     response: {"ok": true, ...} or {"ok": false, "error": "<message>"}
//!
//! It drives one GUI session (see session.sh) through agent-desktop, and adds
//! what agent-desktop alone does not give:
//!
//!   * clicks go through "read the element bounds, click the centre", because
//!     agent-desktop's headed ref click is refused on web content and its
//!     semantic click can hang GTK3 apps that open a modal dialog;
//!   * refs are short ("e12") and stay with their element from one
//!     observation to the next, for as long as the element is in the window;
//!   * every action is followed by a short settle wait and a fresh
//!     observation, reported as what changed when little did.

mod atspi;
mod pictures;
mod run;

use run::{ad, ad_error, Env};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_SESSION: &str = "main";
const SETTLE: Duration = Duration::from_millis(400);

/// Roles that carry no information for an agent unless they have a name.
const NOISE_ROLES: &[&str] = &[
    "group",
    "unknown",
    "separator",
    "scrollarea",
    "menu",
    "toolbar",
    "tablist",
];
/// Roles that can be acted on even when agent-desktop lists no action for them.
const ACTING_ROLES: &[&str] = &[
    "button",
    "link",
    "menuitem",
    "checkbox",
    "radiobutton",
    "textfield",
    "combobox",
    "tab",
    "menubutton",
    "switch",
];
/// Elements whose content reads as one line of text.
const ONE_LINE_ROLES: &[&str] = &[
    "paragraph",
    "heading",
    "link",
    "button",
    "listitem",
    "cell",
    "label",
    "caption",
    "menuitem",
    "tab",
];

/// A failure reported to the caller as the response's `error`.
type R<T> = Result<T, String>;

macro_rules! fail {
    ($($arg:tt)*) => { return Err(format!($($arg)*)) };
}

// --- session -----------------------------------------------------------------

/// Where the tools are: the directory this program was copied to.
fn base() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// One session's files. Sessions on a host share the installed tools and
/// nothing else: each has its own display, session bus, browser profile and
/// event log.
struct Session {
    name: String,
    dir: PathBuf,
    run: PathBuf,
}

impl Session {
    fn new(base: &Path, name: &str) -> R<Self> {
        let ok = !name.is_empty()
            && name.len() <= 32
            && name.is_ascii()
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && name == name.to_lowercase();
        if !ok {
            fail!("a session name is 1-32 characters: lower-case letters, digits, - and _");
        }
        // The default session lives directly in the base directory, which is
        // where the only session lived before sessions had names.
        let dir = if name == DEFAULT_SESSION {
            base.to_path_buf()
        } else {
            base.join("sessions").join(name)
        };
        Ok(Self {
            name: name.to_string(),
            run: dir.join("run"),
            dir,
        })
    }

    fn envfile(&self) -> PathBuf {
        self.run.join("env.sh")
    }
    fn events(&self) -> PathBuf {
        self.run.join("events.jsonl")
    }
    fn refs(&self) -> PathBuf {
        self.run.join("refs.json")
    }
    fn ff_profile(&self) -> PathBuf {
        self.dir.join("ffprofile")
    }

    fn running(&self) -> bool {
        if !self.envfile().is_file() {
            return false;
        }
        std::fs::read_to_string(self.run.join("xvnc.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
            .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
    }

    fn script(&self, base: &Path, args: &[&str]) -> std::process::Output {
        let output = std::process::Command::new(base.join("session.sh"))
            .args(args)
            .env("BSCREEN_BASE", base)
            .env("BSCREEN_SESSION_DIR", &self.dir)
            .env("BSCREEN_MAXIMIZE", "1")
            .stdin(std::process::Stdio::null())
            .output();
        output.unwrap_or_else(|error| std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: error.to_string().into_bytes(),
        })
    }

    /// Environment of the GUI session, from the env file session.sh wrote.
    fn env(&self) -> R<Env> {
        let script = format!(". '{}' && env -0", self.envfile().display());
        let out = std::process::Command::new("bash")
            .args(["-c", &script])
            .output()
            .map_err(|error| format!("cannot read the session's environment: {error}"))?;
        if !out.status.success() {
            fail!("cannot read the session's environment");
        }
        Ok(out
            .stdout
            .split(|&b| b == 0)
            .filter_map(|item| {
                let at = item.iter().position(|&b| b == b'=')?;
                Some((
                    String::from_utf8_lossy(&item[..at]).into_owned(),
                    String::from_utf8_lossy(&item[at + 1..]).into_owned(),
                ))
            })
            .collect())
    }

    /// Start the session unless it runs; true when it was started now.
    fn ensure(&self, base: &Path, geometry: &str) -> R<bool> {
        if self.running() {
            return Ok(false);
        }
        for tool in [
            "root/usr/bin/Xtigervnc",
            "root/usr/bin/openbox",
            "ad/node_modules/.bin/agent-desktop",
        ] {
            if !base.join(tool).exists() {
                fail!("the remote tools are not installed here; run `bash-screen setup` first");
            }
        }
        if self.run.is_dir() {
            self.script(base, &["stop"]);
        }
        let done = self.script(base, &["start", geometry]);
        if !done.status.success() || !self.running() {
            let said = if done.stderr.is_empty() {
                &done.stdout
            } else {
                &done.stderr
            };
            let said = String::from_utf8_lossy(said);
            fail!("could not start the session: {}", tail(said.trim(), 400));
        }
        Ok(true)
    }
}

fn list_sessions(base: &Path) -> R<Value> {
    let mut names = vec![DEFAULT_SESSION.to_string()];
    if let Ok(entries) = std::fs::read_dir(base.join("sessions")) {
        let mut more: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        more.sort();
        names.extend(more);
    }
    let mut found = Vec::new();
    for name in names {
        let Ok(session) = Session::new(base, &name) else {
            continue;
        };
        if session.running() {
            let display = session
                .env()
                .ok()
                .and_then(|env| env.get("DISPLAY").cloned());
            found.push(json!({"name": name, "running": true, "display": display}));
        } else if name != DEFAULT_SESSION {
            found.push(json!({"name": name, "running": false}));
        }
    }
    Ok(Value::Array(found))
}

// --- small helpers -------------------------------------------------------------

/// The last `n` characters of `text`.
fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// The first `n` characters of `text`.
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// A string as a JSON literal, the way the log and the messages quote names.
fn quoted(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// Text shown in the log for typed text: whole when short.
fn shortened(text: &str) -> String {
    if text.chars().count() <= 40 {
        text.to_string()
    } else {
        format!("{}...", head(text, 37))
    }
}

/// A value as a person reads it.
fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A JSON value that Python would count as true.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn number(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn float(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Words of `text` joined by single spaces.
fn squeeze_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- the request in hand -------------------------------------------------------

struct Ctx {
    base: PathBuf,
    session: Session,
    env: Env,
    /// Set while a batch of steps runs (see `cmd_do`): single steps do not observe.
    in_batch: bool,
}

impl Ctx {
    fn ad(&self, args: &[&str]) -> Value {
        ad(&self.env, args, Duration::from_secs(30))
    }

    fn list_windows(&self) -> Vec<Value> {
        let envelope = self.ad(&["list-windows"]);
        if envelope["ok"] == json!(true) {
            envelope["data"].as_array().cloned().unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    fn current_window(&self) -> (Option<Value>, Vec<Value>) {
        let windows = self.list_windows();
        let focused = windows
            .iter()
            .find(|w| w["is_focused"] == json!(true))
            .cloned();
        (focused.or_else(|| windows.first().cloned()), windows)
    }

    fn log_event(&self, summary: &str, ok: bool) {
        let seq = std::fs::read(self.session.events())
            .map(|data| data.iter().filter(|&&b| b == b'\n').count())
            .unwrap_or(0);
        let line = json!({"seq": seq + 1, "t": now(), "ok": ok, "what": summary});
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.session.events())
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn screenshot_png(&self) -> R<Vec<u8>> {
        let path = self.session.run.join("shot.png");
        let _ = std::fs::remove_file(&path);
        let envelope = self.ad(&["screenshot", &path.to_string_lossy()]);
        if envelope["ok"] != json!(true) {
            fail!("screenshot failed: {}", ad_error(&envelope));
        }
        std::fs::read(&path).map_err(|error| format!("screenshot failed: {error}"))
    }
}

fn read_events(session: &Session, limit: usize) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(session.events()) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(limit)..]
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

// --- observation ---------------------------------------------------------------

/// Name of the web document that belongs to the window title, if any.
///
/// Browsers keep hidden documents in the tree (Firefox preloads a new-tab
/// page). The visible one is the document the window is titled after.
fn visible_document(tree: &Value, title: &str) -> Option<String> {
    fn walk(node: &Value, names: &mut Vec<String>) {
        if node["role"] == json!("webarea") {
            if let Some(name) = node["name"].as_str().filter(|n| !n.is_empty()) {
                names.push(name.to_string());
            }
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, names);
        }
    }
    let mut names = Vec::new();
    walk(tree, &mut names);
    let mut best: Option<String> = None;
    for name in names {
        if !title.is_empty()
            && title.starts_with(&name)
            && best.as_ref().is_none_or(|b| name.len() > b.len())
        {
            best = Some(name);
        }
    }
    best
}

/// Whether `node` is a web document other than the visible one.
fn hidden_document(node: &Value, document: Option<&str>) -> bool {
    node["role"] == json!("webarea") && document.is_some_and(|d| node["name"].as_str() != Some(d))
}

#[derive(Clone)]
struct Element {
    ad: Option<String>,
    role: String,
    name: String,
    line: String,
    bounds: Option<Value>,
    id: Option<String>,
    key: Option<(String, String, u64)>,
}

/// Flatten the accessibility tree into one entry per useful element.
fn listed_elements(node: &Value, found: &mut Vec<Element>, show_all: bool, document: Option<&str>) {
    let role = node["role"]
        .as_str()
        .filter(|r| !r.is_empty())
        .unwrap_or("?");
    let name = node["name"].as_str().unwrap_or("");
    if hidden_document(node, document) {
        return;
    }
    let reference = node["ref_id"].as_str().filter(|r| !r.is_empty());
    let actions: Vec<&str> = node["available_actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|a| *a != "SetFocus")
        .collect();
    let interactive = (reference.is_some() && actions.iter().any(|a| *a != "Click"))
        || ACTING_ROLES.contains(&role);
    let keep =
        (reference.is_some() && interactive && !(NOISE_ROLES.contains(&role) && name.is_empty()))
            || (show_all && !name.is_empty());
    if keep {
        let mut parts = vec![role.to_string()];
        if !name.is_empty() {
            parts.push(quoted(&head(name, 120)));
        }
        let value = &node["value"];
        if !value.is_null() && value != &json!("") {
            parts.push(format!("= {}", quoted(&head(&plain(value), 120))));
        }
        let states: Vec<&str> = node["states"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| ["checked", "selected", "disabled", "expanded", "focused"].contains(s))
            .collect();
        if !states.is_empty() {
            parts.push(format!("({})", states.join(",")));
        }
        found.push(Element {
            ad: reference.map(str::to_string),
            role: role.to_string(),
            name: name.to_string(),
            line: parts.join(" "),
            bounds: node.get("bounds").filter(|b| !b.is_null()).cloned(),
            id: None,
            key: None,
        });
    }
    for child in node["children"].as_array().into_iter().flatten() {
        listed_elements(child, found, show_all, document);
    }
}

/// The current window and its elements, read now. `elements` is a reason in
/// place of the list when the window cannot be read.
struct Reading {
    window: Option<Value>,
    windows: Vec<Value>,
    elements: Result<Vec<Element>, String>,
    partial: bool,
}

fn read_window(ctx: &Ctx, show_all: bool, bounds: bool) -> Reading {
    let (window, windows) = ctx.current_window();
    let Some(window) = window else {
        return Reading {
            window: None,
            windows,
            elements: Err("(no window is visible to the accessibility channel; use `look`)".into()),
            partial: false,
        };
    };
    let id = plain(&window["id"]);
    let mut args = vec!["snapshot", "--window-id", id.as_str()];
    if !show_all {
        args.extend(["-i", "--compact"]);
    }
    if bounds {
        args.push("--include-bounds");
    }
    let envelope = ad(&ctx.env, &args, Duration::from_secs(40));
    if envelope["ok"] != json!(true) {
        let reason = format!(
            "(accessibility channel failed: {}; use `look`)",
            ad_error(&envelope)
        );
        return Reading {
            window: Some(window),
            windows,
            elements: Err(reason),
            partial: false,
        };
    }
    let data = &envelope["data"];
    let document = visible_document(&data["tree"], str_of(&window, "title"));
    let mut found = Vec::new();
    listed_elements(&data["tree"], &mut found, show_all, document.as_deref());
    let partial = data["complete"] == json!(false);
    Reading {
        window: Some(window),
        windows,
        elements: Ok(found),
        partial,
    }
}

fn last_observation(session: &Session) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(session.refs()).ok()?).ok()
}

fn key_of(value: &Value) -> Option<(String, String, u64)> {
    let parts = value.as_array()?;
    Some((
        parts.first()?.as_str()?.to_string(),
        parts.get(1)?.as_str()?.to_string(),
        parts.get(2)?.as_u64()?,
    ))
}

/// Give each element that can be acted on a ref of our own.
///
/// An element keeps its ref from the observation before when it is still
/// there, which is judged by its role, its name and its place among the
/// elements sharing both. A ref is never given to a second element, so one
/// whose element has gone fails instead of landing somewhere else.
fn give_refs(elements: &mut [Element], before: Option<&Value>) -> u64 {
    let mut known = HashMap::new();
    for element in before
        .and_then(|b| b["elements"].as_array())
        .into_iter()
        .flatten()
    {
        if let (Some(key), Some(id)) = (key_of(&element["key"]), element["id"].as_str()) {
            known.insert(key, id.to_string());
        }
    }
    let mut next = before.and_then(|b| b["next"].as_u64()).unwrap_or(1);
    let mut seen: HashMap<(String, String), u64> = HashMap::new();
    for element in elements.iter_mut() {
        if element.ad.is_none() {
            continue;
        }
        let nth = seen
            .entry((element.role.clone(), element.name.clone()))
            .or_insert(0);
        let key = (element.role.clone(), element.name.clone(), *nth);
        *nth += 1;
        element.id = Some(match known.get(&key) {
            Some(id) => id.clone(),
            None => {
                next += 1;
                format!("e{}", next - 1)
            }
        });
        element.key = Some(key);
    }
    next
}

/// What differs from the observation before, as lines, or None when the two
/// cannot be compared or differ so much that the whole list reads better.
fn changes_since(
    before: Option<&Value>,
    window: &Value,
    elements: &[Element],
    show_all: bool,
) -> Option<Vec<String>> {
    let before = before?;
    if before["window"] != window["id"] || before["all"] != json!(show_all) {
        return None;
    }
    let old: Vec<(String, String)> = before["elements"]
        .as_array()?
        .iter()
        .map(|e| (str_of(e, "id").to_string(), str_of(e, "line").to_string()))
        .collect();
    let new: Vec<(String, String)> = elements
        .iter()
        .filter_map(|e| Some((e.id.clone()?, e.line.clone())))
        .collect();
    let old_line = |id: &str| old.iter().find(|(i, _)| i == id).map(|(_, l)| l.as_str());
    let has_new = |id: &str| new.iter().any(|(i, _)| i == id);
    let mut lines: Vec<String> = new
        .iter()
        .filter(|(i, _)| old_line(i).is_none())
        .map(|(i, l)| format!("+ [{i}] {l}"))
        .collect();
    lines.extend(
        new.iter()
            .filter(|(i, l)| old_line(i).is_some_and(|o| o != l))
            .map(|(i, l)| format!("~ [{i}] {l}")),
    );
    lines.extend(
        old.iter()
            .filter(|(i, _)| !has_new(i))
            .map(|(i, l)| format!("- [{i}] {l}")),
    );
    if lines.len() > 12.max(new.len() / 2) {
        return None;
    }
    Some(lines)
}

fn windows_list(windows: &[Value], with_focus: bool) -> Value {
    Value::Array(
        windows
            .iter()
            .map(|w| {
                let mut entry = json!({"title": w["title"], "app": w["app_name"]});
                if with_focus {
                    entry["focused"] = json!(w["is_focused"] == json!(true));
                }
                entry
            })
            .collect(),
    )
}

/// Read the current window and remember it as the latest observation. With
/// `brief`, also say what changed since the one before.
fn observe(ctx: &Ctx, show_all: bool, brief: bool) -> Map<String, Value> {
    let reading = read_window(ctx, show_all, false);
    let mut result = Map::new();
    result.insert("windows".into(), windows_list(&reading.windows, true));
    let mut elements = match reading.elements {
        Ok(elements) => elements,
        Err(reason) => {
            result.insert("a11y".into(), json!(false));
            result.insert("text".into(), json!(reason));
            return result;
        }
    };
    let window = reading.window.unwrap_or(Value::Null);
    let before = last_observation(&ctx.session);
    let next = give_refs(&mut elements, before.as_ref());
    let title = str_of(&window, "title").to_string();
    let kept: Vec<Value> = elements
        .iter()
        .filter_map(|e| {
            let (role, name, nth) = e.key.clone()?;
            Some(json!({"id": e.id, "key": [role, name, nth], "ad": e.ad, "line": e.line}))
        })
        .collect();
    let saved = json!({"next": next, "window": window["id"], "all": show_all, "title": title, "elements": kept});
    let _ = std::fs::write(ctx.session.refs(), saved.to_string());
    let mut header = format!("window: {title} ({})", plain(&window["app_name"]));
    if reading.partial {
        header.push_str("  [partial]");
    }
    let lines: Vec<String> = elements
        .iter()
        .map(|e| match &e.id {
            Some(id) => format!("[{id}] {}", e.line),
            None => format!("     {}", e.line),
        })
        .collect();
    let mut text = vec![header.clone()];
    text.extend(lines.iter().cloned());
    result.insert("a11y".into(), json!(true));
    result.insert("text".into(), json!(text.join("\n")));
    result.insert("elements".into(), json!(lines.len()));
    let changes = if brief {
        changes_since(before.as_ref(), &window, &elements, show_all)
    } else {
        None
    };
    if let (Some(changes), Some(before)) = (changes, before) {
        if str_of(&before, "title") != title {
            header.push_str(&format!("  [was: {}]", plain(&before["title"])));
        }
        let summary = if changes.is_empty() {
            format!(
                "nothing changed: the same {} elements, and their refs, as before",
                lines.len()
            )
        } else {
            let same = lines.len()
                - changes
                    .iter()
                    .filter(|l| l.starts_with('+') || l.starts_with('~'))
                    .count();
            format!(
                "{} changed (+ new, ~ different, - gone); the other {same} elements and their refs are as before",
                changes.len()
            )
        };
        let mut brief = vec![header, summary];
        brief.extend(changes);
        result.insert("brief".into(), json!(brief.join("\n")));
    }
    result
}

fn centre(bounds: Option<&Value>) -> Option<(i64, i64)> {
    let b = bounds?;
    let x = b["x"].as_f64()? + b["width"].as_f64()? / 2.0;
    let y = b["y"].as_f64()? + b["height"].as_f64()? / 2.0;
    Some((x as i64, y as i64))
}

/// The element a request is aimed at, by ref or by name.
struct Target {
    /// Its agent-desktop ref.
    ad: String,
    /// How the log names it.
    shown: String,
    /// Our own ref, and whether the observation it came from was made with --all.
    id: Option<(String, bool)>,
    /// Where it is, when already known from reading the window.
    centre: Option<Option<(i64, i64)>>,
}

fn target_of(ctx: &Ctx, req: &Value) -> R<Option<Target>> {
    if let Some(short) = req.get("ref").and_then(Value::as_str) {
        if short.starts_with('@') {
            return Ok(Some(Target {
                ad: short.into(),
                shown: short.into(),
                id: None,
                centre: None,
            }));
        }
        let Some(seen) = last_observation(&ctx.session) else {
            fail!("no observation yet: run `observe` before using a ref");
        };
        for element in seen["elements"].as_array().into_iter().flatten() {
            if element["id"] == json!(short) {
                return Ok(Some(Target {
                    ad: str_of(element, "ad").into(),
                    shown: short.into(),
                    id: Some((short.into(), seen["all"] == json!(true))),
                    centre: None,
                }));
            }
        }
        fail!("{short} is not in the window any more; run `observe` for the current refs");
    }
    match req
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
    {
        Some(name) => find_by_name(ctx, name).map(Some),
        None => Ok(None),
    }
}

/// The one element with this name, read now. The name must match whole, or
/// failing that be contained, without regard to case.
fn find_by_name(ctx: &Ctx, wanted: &str) -> R<Target> {
    let shown = quoted(wanted);
    let elements = match read_window(ctx, false, true).elements {
        Ok(elements) => elements,
        Err(reason) => fail!("cannot look for {shown}: {reason}"),
    };
    let elements: Vec<Element> = elements.into_iter().filter(|e| e.ad.is_some()).collect();
    let low = wanted.to_lowercase();
    let mut found: Vec<&Element> = elements
        .iter()
        .filter(|e| e.name.to_lowercase() == low)
        .collect();
    if found.is_empty() {
        found = elements
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&low))
            .collect();
    }
    match found.as_slice() {
        [] => fail!("no element is named {shown}; run `observe` to see the names"),
        [one] => Ok(Target {
            ad: one.ad.clone().unwrap_or_default(),
            shown,
            id: None,
            centre: Some(centre(one.bounds.as_ref())),
        }),
        many => {
            let some: Vec<&str> = many.iter().take(6).map(|e| e.line.as_str()).collect();
            let more = if many.len() > 6 { "; ..." } else { "" };
            fail!(
                "{} elements are named {shown} ({}{more}); run `observe` and use a ref",
                many.len(),
                some.join("; ")
            )
        }
    }
}

/// Where to click for this element, or None when it has no known position.
fn centre_of(ctx: &Ctx, target: &Target) -> R<Option<(i64, i64)>> {
    if let Some(known) = target.centre {
        return Ok(known);
    }
    let envelope = ctx.ad(&["get", &target.ad, "--property", "bounds"]);
    if envelope["ok"] == json!(true) {
        return Ok(centre(envelope["data"].get("value")));
    }
    if envelope["error"]["code"] == json!("AMBIGUOUS_TARGET") {
        if let Some((id, all)) = &target.id {
            // agent-desktop will not find an element again that looks like
            // its neighbours (three "Delete" buttons). Our own refs tell them
            // apart by their order, so read the window again and take that one.
            if let Ok(mut elements) = read_window(ctx, *all, true).elements {
                give_refs(&mut elements, last_observation(&ctx.session).as_ref());
                if let Some(element) = elements
                    .iter()
                    .find(|e| e.id.as_deref() == Some(id.as_str()))
                {
                    return Ok(centre(element.bounds.as_ref()));
                }
            }
        }
    }
    fail!(
        "{}: {} (the element may have gone; run `observe`)",
        target.shown,
        ad_error(&envelope)
    )
}

// --- commands ------------------------------------------------------------------

type Reply = R<Map<String, Value>>;

/// Settle, log, and return the fresh observation that every action reports.
fn after_action(ctx: &Ctx, summary: &str) -> Reply {
    std::thread::sleep(SETTLE);
    ctx.log_event(summary, true);
    Ok(if ctx.in_batch {
        Map::new()
    } else {
        observe(ctx, false, true)
    })
}

fn ok_or_fail(envelope: &Value) -> R<()> {
    if envelope["ok"] == json!(true) {
        Ok(())
    } else {
        Err(ad_error(envelope))
    }
}

fn cmd_click(ctx: &Ctx, req: &Value) -> Reply {
    let mut target = target_of(ctx, req)?;
    let shown = target.as_ref().map(|t| t.shown.clone()).unwrap_or_default();
    let (mut x, mut y) = match &target {
        Some(t) => match centre_of(ctx, t)? {
            Some(at) => at,
            None => {
                // No position known (for example GTK4 under some window
                // managers): fall back to the semantic click, without the
                // actionability wait.
                ok_or_fail(&ctx.ad(&["click", &t.ad, "--timeout-ms", "0"]))?;
                return after_action(ctx, &format!("click {shown} (semantic)"));
            }
        },
        None => (
            number(req.get("x")).unwrap_or(0),
            number(req.get("y")).unwrap_or(0),
        ),
    };
    let press = |x: i64, y: i64| {
        let at = format!("{x},{y}");
        let mut args = vec![
            "--headed".to_string(),
            "mouse-click".into(),
            "--xy".into(),
            at,
        ];
        if let Some(button) = req
            .get("button")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
        {
            args.extend(["--button".into(), button.into()]);
        }
        if let Some(count) = number(req.get("count")).filter(|c| *c != 0) {
            args.extend(["--count".into(), count.to_string()]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        ctx.ad(&args)
    };
    let mut envelope = press(x, y);
    if envelope["ok"] != json!(true) {
        if let Some(t) = &target {
            if ctx.ad(&["scroll-to", &t.ad])["ok"] == json!(true) {
                // Most likely scrolled out of view: bring the element in and
                // look again for where it is now.
                std::thread::sleep(SETTLE);
                if let Some(name) = req
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                {
                    target = Some(find_by_name(ctx, name)?);
                }
                if let Some(at) = centre_of(ctx, target.as_ref().unwrap())? {
                    (x, y) = at;
                    envelope = press(x, y);
                }
            }
        }
    }
    ok_or_fail(&envelope)?;
    after_action(ctx, &format!("click {shown} at {x},{y}").replace("  ", " "))
}

/// Paste text at the keyboard focus.
fn paste(ctx: &Ctx, text: &str) -> R<()> {
    ok_or_fail(&ctx.ad(&["clipboard-set", text]))?;
    ok_or_fail(&ctx.ad(&["press", "ctrl+v"]))
}

fn cmd_type(ctx: &Ctx, req: &Value) -> Reply {
    let text = str_of(req, "text");
    let target = target_of(ctx, req)?;
    let shown_target = target.as_ref().map(|t| t.shown.clone()).unwrap_or_default();
    let mut typed = false;
    if let Some(t) = &target {
        let envelope = ctx.ad(&["--headed", "type", &t.ad, text]);
        if envelope["ok"] == json!(true) {
            typed = true;
        } else {
            if envelope["error"]["code"] != json!("AMBIGUOUS_TARGET") {
                return Err(ad_error(&envelope));
            }
            // An element agent-desktop cannot tell from its neighbours: put
            // the keyboard focus on it with a click, then paste as below.
            let click = match centre_of(ctx, t)? {
                Some((x, y)) => ctx.ad(&["--headed", "mouse-click", "--xy", &format!("{x},{y}")]),
                None => envelope,
            };
            ok_or_fail(&click)?;
        }
    }
    if !typed {
        // No target element: paste into whatever has keyboard focus.
        paste(ctx, text)?;
    }
    after_action(
        ctx,
        &format!("type {shown_target} {}", quoted(&shortened(text))).replace("  ", " "),
    )
}

fn strings_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn cmd_key(ctx: &Ctx, req: &Value) -> Reply {
    let keys = strings_of(&req["keys"]);
    for combo in &keys {
        let envelope = ctx.ad(&["press", combo]);
        if envelope["ok"] != json!(true) {
            fail!("{combo}: {}", ad_error(&envelope));
        }
    }
    after_action(ctx, &format!("key {}", keys.join(" ")))
}

fn cmd_scroll(ctx: &Ctx, req: &Value) -> Reply {
    let (envelope, summary) = match target_of(ctx, req)? {
        Some(t) => (
            ctx.ad(&["scroll-to", &t.ad]),
            format!("scroll to {}", t.shown),
        ),
        None => {
            let x = number(req.get("x")).unwrap_or(640).to_string();
            let y = number(req.get("y")).unwrap_or(400).to_string();
            let dy = number(req.get("dy")).unwrap_or(-5);
            let envelope = ctx.ad(&[
                "--headed",
                "mouse-wheel",
                "--x",
                &x,
                "--y",
                &y,
                &format!("--dy={dy}"),
            ]);
            (envelope, format!("scroll dy={dy}"))
        }
    };
    ok_or_fail(&envelope)?;
    after_action(ctx, &summary)
}

fn cmd_drag(ctx: &Ctx, req: &Value) -> Reply {
    let point = |key: &str| -> (i64, i64) {
        let p = &req[key];
        (number(p.get(0)).unwrap_or(0), number(p.get(1)).unwrap_or(0))
    };
    let ((x1, y1), (x2, y2)) = (point("from"), point("to"));
    ok_or_fail(&ctx.ad(&[
        "--headed",
        "drag",
        &format!("--from-xy={x1},{y1}"),
        &format!("--to-xy={x2},{y2}"),
    ]))?;
    after_action(ctx, &format!("drag {x1},{y1} to {x2},{y2}"))
}

/// Input from a person using the preview: inject it and return at once.
///
/// Unlike the agent's actions this reads nothing back; the preview fetches the
/// next frame itself. Each action is logged, marked as a person's.
fn cmd_input(ctx: &Ctx, req: &Value) -> Reply {
    let mut done: Vec<String> = Vec::new();
    for action in req["actions"].as_array().into_iter().flatten() {
        let kind = str_of(action, "kind");
        let (steps, summary): (Vec<Vec<String>>, String) = match kind {
            "click" => {
                let (x, y) = (
                    number(action.get("x")).unwrap_or(0),
                    number(action.get("y")).unwrap_or(0),
                );
                let button = action
                    .get("button")
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .unwrap_or("left");
                let mut args: Vec<String> = vec![
                    "--headed".into(),
                    "mouse-click".into(),
                    "--xy".into(),
                    format!("{x},{y}"),
                ];
                if button != "left" {
                    args.extend(["--button".into(), button.into()]);
                }
                let what = if button == "left" {
                    "click".to_string()
                } else {
                    format!("{button}-click")
                };
                (vec![args], format!("{what} at {x},{y}"))
            }
            "wheel" => {
                let x = number(action.get("x")).unwrap_or(0);
                let y = number(action.get("y")).unwrap_or(0);
                let dy = number(action.get("dy")).unwrap_or(0);
                let args = vec![
                    "--headed".into(),
                    "mouse-wheel".into(),
                    "--x".into(),
                    x.to_string(),
                    "--y".into(),
                    y.to_string(),
                    format!("--dy={dy}"),
                ];
                (vec![args], format!("scroll dy={dy}"))
            }
            "text" => {
                let text = str_of(action, "text");
                let steps = vec![
                    vec!["clipboard-set".into(), text.into()],
                    vec!["press".into(), "ctrl+v".into()],
                ];
                (steps, format!("type {}", quoted(&shortened(text))))
            }
            "key" => {
                let key = str_of(action, "key");
                (vec![vec!["press".into(), key.into()]], format!("key {key}"))
            }
            other => fail!("unknown input: {other}"),
        };
        for args in &steps {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let envelope = ctx.ad(&args);
            if envelope["ok"] != json!(true) {
                ctx.log_event(&format!("you: {summary} failed"), false);
                fail!("{summary}: {}", ad_error(&envelope));
            }
        }
        done.push(summary);
    }
    // Runs of the same kind read better as one line.
    let mut merged: Vec<String> = Vec::new();
    for summary in &done {
        match merged.last_mut() {
            Some(last)
                if summary.starts_with("key ")
                    && last.starts_with("key ")
                    && last.chars().count() < 60 =>
            {
                last.push_str(&summary[3..]);
            }
            _ => merged.push(summary.clone()),
        }
    }
    for summary in merged {
        ctx.log_event(&format!("you: {summary}"), true);
    }
    let mut result = Map::new();
    result.insert("done".into(), json!(done.len()));
    Ok(result)
}

/// Title of the current window and every name and value in it, for `wait`.
fn window_strings(ctx: &Ctx) -> (String, Vec<String>) {
    let (window, _) = ctx.current_window();
    let Some(window) = window else {
        return (String::new(), Vec::new());
    };
    let title = str_of(&window, "title").to_string();
    let id = plain(&window["id"]);
    let envelope = ad(
        &ctx.env,
        &["snapshot", "--window-id", &id],
        Duration::from_secs(40),
    );
    if envelope["ok"] != json!(true) {
        return (title, Vec::new());
    }
    let tree = &envelope["data"]["tree"];
    let document = visible_document(tree, &title);
    fn walk(node: &Value, document: Option<&str>, found: &mut Vec<String>) {
        if hidden_document(node, document) {
            return;
        }
        for key in ["name", "value"] {
            let value = &node[key];
            if !value.is_null() && value != &json!("") {
                found.push(plain(value));
            }
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, document, found);
        }
    }
    let mut found = Vec::new();
    walk(tree, document.as_deref(), &mut found);
    // Running text, which the tree above does not carry.
    found.extend(atspi::read_text(&ctx.env, &title).unwrap_or_default());
    (title, found)
}

/// What a person would read in this part of the tree, top to bottom. A name
/// is left out when the text under its element already says it, as with a
/// link and the words inside it.
fn text_lines(node: &Value, document: Option<&str>) -> Vec<String> {
    let role = node["role"].as_str().unwrap_or("");
    if hidden_document(node, document) {
        return Vec::new();
    }
    let mut below: Vec<String> = Vec::new();
    for child in node["children"].as_array().into_iter().flatten() {
        below.extend(text_lines(child, document));
    }
    let one_line = ONE_LINE_ROLES.contains(&role);
    if one_line && !below.is_empty() {
        below = vec![below.join(" ")];
    }
    let squeeze =
        |text: &str| -> String { text.split_whitespace().collect::<String>().to_lowercase() };
    let name = node["name"].as_str().unwrap_or("");
    let value = &node["value"];
    let said = squeeze(&below.join(" "));
    let mut own = Vec::new();
    if !name.is_empty() && !said.contains(&squeeze(name)) {
        own.push(squeeze_spaces(name));
    }
    if !value.is_null() && value != &json!("") {
        let value = plain(value);
        if !said.contains(&squeeze(&value)) && squeeze(&value) != squeeze(name) {
            own.push(squeeze_spaces(&value));
        }
    }
    if !own.is_empty() && one_line && !below.is_empty() {
        return vec![format!("{} {}", own.join(": "), below[0])];
    }
    let mut lines = if own.is_empty() {
        Vec::new()
    } else {
        vec![own.join(": ")]
    };
    lines.extend(below);
    lines
}

/// The text of the current window, without refs: for reading a page.
fn cmd_text(ctx: &Ctx, _req: &Value) -> Reply {
    let (window, _) = ctx.current_window();
    let Some(window) = window else {
        fail!("no window is visible to the accessibility channel; use `look`");
    };
    let title = str_of(&window, "title").to_string();
    let mut result = Map::new();
    result.insert("title".into(), json!(title));
    result.insert("app".into(), window["app_name"].clone());
    let lines = match atspi::read_text(&ctx.env, &title) {
        Some(lines) => lines,
        None => {
            let id = plain(&window["id"]);
            let envelope = ad(
                &ctx.env,
                &["snapshot", "--window-id", &id],
                Duration::from_secs(40),
            );
            if envelope["ok"] != json!(true) {
                fail!(
                    "accessibility channel failed: {}; use `look`",
                    ad_error(&envelope)
                );
            }
            let tree = &envelope["data"]["tree"];
            let document = visible_document(tree, &title);
            fn page<'a>(node: &'a Value, document: &str) -> Option<&'a Value> {
                if node["role"] == json!("webarea") && node["name"].as_str() == Some(document) {
                    return Some(node);
                }
                node["children"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find_map(|child| page(child, document))
            }
            // In a browser, the page only, as above.
            let start = document
                .as_deref()
                .and_then(|d| page(tree, d))
                .unwrap_or(tree);
            result.insert(
                "note".into(),
                json!("only the names of elements: running text such as paragraphs is missing, because the accessibility bus could not be read"),
            );
            text_lines(start, document.as_deref())
                .into_iter()
                .filter(|l| !l.is_empty())
                .collect()
        }
    };
    result.insert("text".into(), json!(lines.join("\n")));
    result.insert("lines".into(), json!(lines.len()));
    Ok(result)
}

/// Wait until every given condition holds, then observe. Without a condition,
/// wait until the picture on the screen stops changing.
fn cmd_wait(ctx: &Ctx, req: &Value) -> Reply {
    let wanted_str = |key: &str| {
        req.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (text, gone, title) = (wanted_str("text"), wanted_str("gone"), wanted_str("title"));
    let any_text = text.is_some() || gone.is_some() || title.is_some();
    let stable = truthy(req.get("stable")) || !any_text;
    let timeout = float(req.get("timeout")).unwrap_or(15.0).min(300.0);
    let mut wanted: Vec<String> = [("text", &text), ("gone", &gone), ("title", &title)]
        .iter()
        .filter_map(|(label, value)| value.as_ref().map(|v| format!("{label} {}", quoted(v))))
        .collect();
    if stable {
        wanted.push("stable".into());
    }
    let summary = format!("wait {}", wanted.join(", "));
    let started = Instant::now();
    let mut last_picture: Option<u64> = None;
    let mut seen_title = String::new();
    loop {
        let mut unmet = Vec::new();
        if any_text {
            let (window_title, strings) = window_strings(ctx);
            seen_title = window_title;
            let has = |needle: &str| {
                let needle = needle.to_lowercase();
                strings.iter().any(|s| s.to_lowercase().contains(&needle))
            };
            if let Some(text) = &text {
                if !has(text) {
                    unmet.push(format!("no element contains {}", quoted(text)));
                }
            }
            if let Some(gone) = &gone {
                if has(gone) {
                    unmet.push(format!("an element still contains {}", quoted(gone)));
                }
            }
            if let Some(title) = &title {
                if !seen_title.to_lowercase().contains(&title.to_lowercase()) {
                    unmet.push(format!(
                        "the window title does not contain {}",
                        quoted(title)
                    ));
                }
            }
        }
        if stable {
            let picture = pictures::fingerprint(&ctx.screenshot_png()?);
            if Some(picture) != last_picture {
                unmet.push("the screen was still changing".to_string());
            }
            last_picture = Some(picture);
        }
        let waited = started.elapsed().as_secs_f64();
        if unmet.is_empty() {
            ctx.log_event(&format!("{summary} ({waited:.1} s)"), true);
            let mut result = if ctx.in_batch {
                Map::new()
            } else {
                observe(ctx, false, true)
            };
            result.insert("waited".into(), json!((waited * 10.0).round() / 10.0));
            return Ok(result);
        }
        if waited >= timeout {
            ctx.log_event(&format!("{summary} timed out"), false);
            let place = if seen_title.is_empty() {
                String::new()
            } else {
                format!("; the window is titled {}", quoted(&seen_title))
            };
            fail!(
                "waited {} s but {}{place}",
                pictures::number_text(timeout),
                unmet.join(" and ")
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Several steps in one request, with one observation at the end.
///
/// Nothing observes while the steps run, so their refs are the ones of the
/// observation the caller last saw, whatever the steps change. A step aimed
/// at a name looks for it when its turn comes.
fn cmd_do(ctx: &mut Ctx, req: &Value) -> Reply {
    let steps = req["steps"].as_array().cloned().unwrap_or_default();
    ctx.in_batch = true;
    let mut outcome = Ok(());
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        let cmd = str_of(step, "cmd");
        let run: fn(&Ctx, &Value) -> Reply = match cmd {
            "click" => cmd_click,
            "type" => cmd_type,
            "key" => cmd_key,
            "scroll" => cmd_scroll,
            "drag" => cmd_drag,
            "wait" => cmd_wait,
            _ => {
                let shown = if step.get("cmd").is_none_or(Value::is_null) {
                    "None".into()
                } else {
                    plain(&step["cmd"])
                };
                outcome = Err(format!("step {number}: {shown} cannot be used in `do`"));
                break;
            }
        };
        if let Err(error) = run(ctx, step) {
            outcome = Err(format!(
                "step {number} of {} ({cmd}) failed: {error}. {} step(s) before it were done; run `observe` to see where things stand",
                steps.len(),
                number - 1
            ));
            break;
        }
    }
    ctx.in_batch = false;
    outcome?;
    let mut result = observe(ctx, false, true);
    result.insert("steps".into(), json!(steps.len()));
    Ok(result)
}

/// A throwaway profile, so the session never touches the user's real Firefox.
fn firefox_profile(session: &Session) -> PathBuf {
    let profile = session.ff_profile();
    if !profile.is_dir() {
        let _ = std::fs::create_dir_all(&profile);
        let prefs = [
            ("browser.shell.checkDefaultBrowser", json!(false)),
            ("browser.aboutwelcome.enabled", json!(false)),
            ("startup.homepage_welcome_url", json!("")),
            ("startup.homepage_welcome_url.additional", json!("")),
            ("startup.homepage_override_url", json!("")),
            ("browser.startup.homepage_override.mstone", json!("ignore")),
            ("datareporting.policy.dataSubmissionEnabled", json!(false)),
            ("datareporting.policy.firstRunURL", json!("")),
            ("browser.tabs.warnOnClose", json!(false)),
            ("browser.sessionstore.resume_from_crash", json!(false)),
            ("browser.sessionstore.max_resumed_crashes", json!(0)),
            ("browser.newtab.preload", json!(false)),
            ("browser.startup.page", json!(0)),
            ("termsofuse.bypassNotification", json!(true)),
            ("accessibility.force_disabled", json!(-1)),
        ];
        let text: String = prefs
            .iter()
            .map(|(key, value)| format!("user_pref({}, {value});\n", quoted(key)))
            .collect();
        let _ = std::fs::write(profile.join("user.js"), text);
    }
    profile
}

fn cmd_launch(ctx: &Ctx, req: &Value) -> Reply {
    let mut argv = strings_of(&req["argv"]);
    if argv.is_empty() {
        fail!("nothing to launch");
    }
    let given = argv.join(" ");
    let mut env = ctx.env.clone();
    let program = Path::new(&argv[0])
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if req.get("a11y") == Some(&json!(false)) {
        // Keep the app off the accessibility bus: nothing can read its
        // elements, and it is spared the code paths that reading triggers.
        env.remove("ACCESSIBILITY_ENABLED");
        env.remove("QT_ACCESSIBILITY");
        env.insert("NO_AT_BRIDGE".into(), "1".into());
        env.insert("GTK_A11Y".into(), "none".into());
        env.insert("QT_LINUX_ACCESSIBILITY_ALWAYS_ON".into(), "0".into());
    } else if program == "firefox" {
        env.insert("GNOME_ACCESSIBILITY".into(), "1".into());
    }
    if program == "firefox" {
        let profile = firefox_profile(&ctx.session).to_string_lossy().into_owned();
        argv.splice(
            1..1,
            ["--no-remote".to_string(), "--profile".into(), profile],
        );
    }
    run::launch(&argv, &env, &ctx.session.run.join("launch.log"))
        .map_err(|error| format!("cannot launch {}: {error}", argv[0]))?;
    std::thread::sleep(Duration::from_secs_f64(
        float(req.get("wait")).unwrap_or(4.0).max(0.0),
    ));
    ctx.log_event(&format!("launch {}", head(&given, 80)), true);
    Ok(observe(ctx, false, false))
}

fn cmd_look(ctx: &Ctx, req: &Value) -> Reply {
    let png = ctx.screenshot_png()?;
    let region = req.get("region").filter(|r| truthy(Some(r))).map(|r| {
        let n = |i: usize| number(r.get(i)).unwrap_or(0);
        [n(0), n(1), n(2), n(3)]
    });
    let scale = float(req.get("scale")).unwrap_or(1.0);
    let quality = number(req.get("quality")).unwrap_or(60) as f32;
    let look = pictures::look(&png, region, scale, quality)?;
    let mut reply = Map::new();
    reply.insert("image_webp".into(), json!(look.webp_base64));
    reply.insert("screen".into(), json!(look.screen));
    reply.insert("origin".into(), json!(look.origin));
    reply.insert("scale".into(), json!(scale));
    reply.insert("size".into(), json!(look.size));
    // Asked for by a pane that shows the screen to a person.
    if let Some(limit) = number(req.get("events")).filter(|n| *n > 0) {
        reply.insert(
            "events".into(),
            json!(read_events(&ctx.session, limit as usize)),
        );
    }
    Ok(reply)
}

/// One preview frame: recent events, plus the screen scaled to the pane's cell grid.
fn cmd_frame(ctx: &Ctx, req: &Value) -> Reply {
    let png = ctx.screenshot_png()?;
    let cols = number(req.get("cols")).unwrap_or(80).max(1) as u32;
    let rows = number(req.get("rows")).unwrap_or(24).max(1) as u32;
    // Sub-pixels per terminal cell: 1x2 for half blocks, 2x3 for sextants.
    let sx = number(req.get("sx")).unwrap_or(1).max(1) as u32;
    let sy = number(req.get("sy")).unwrap_or(2).max(1) as u32;
    // In image mode the client gives the pixel size of the area it can draw on.
    let pixel_box = match (number(req.get("px_w")), number(req.get("px_h"))) {
        (Some(w), Some(h)) if w > 0 => Some((w.max(1) as u32, h.max(1) as u32)),
        _ => None,
    };
    // Optional cap on the longest edge of the picture that is sent.
    let max_px = number(req.get("max_px")).unwrap_or(0).max(0) as u32;
    let digest = format!(
        "{:016x}:{cols}x{rows}:{sx}x{sy}:{pixel_box:?}:{max_px}",
        pictures::fingerprint(&png)
    );
    let (window, windows) = ctx.current_window();
    let mut result = Map::new();
    result.insert("hash".into(), json!(digest));
    let limit = number(req.get("events")).unwrap_or(6).max(0) as usize;
    result.insert("events".into(), json!(read_events(&ctx.session, limit)));
    result.insert(
        "title".into(),
        window
            .as_ref()
            .map(|w| w["title"].clone())
            .unwrap_or(Value::Null),
    );
    result.insert("windows".into(), json!(windows.len()));
    if req.get("have").and_then(Value::as_str) != Some(digest.as_str()) {
        let frame = match pixel_box {
            Some(pixel_box) => pictures::frame_image(&png, pixel_box, max_px)?,
            None => pictures::frame_blocks(&png, cols, rows, sx, sy)?,
        };
        result.extend(frame);
    }
    Ok(result)
}

fn handle(req: &Value) -> Reply {
    let base = base();
    let cmd = str_of(req, "cmd");
    let mut result = Map::new();
    if cmd == "version" {
        result.insert("version".into(), json!(VERSION));
        return Ok(result);
    }
    if cmd == "sessions" {
        result.insert("sessions".into(), list_sessions(&base)?);
        return Ok(result);
    }
    let name = req
        .get("session")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_SESSION);
    let session = Session::new(&base, name)?;
    if cmd == "stop" {
        session.script(&base, &["stop"]);
        let forget = truthy(req.get("forget"));
        if forget {
            // Drop what the session saved: its browser profile and tool state.
            let doomed = if session.name == DEFAULT_SESSION {
                vec![session.ff_profile(), session.dir.join("state")]
            } else {
                vec![session.dir.clone()]
            };
            for path in doomed {
                let _ = std::fs::remove_dir_all(path);
            }
        }
        result.insert("stopped".into(), json!(true));
        result.insert("forgotten".into(), json!(forget));
        return Ok(result);
    }
    if cmd == "status" {
        if !session.running() {
            result.insert("running".into(), json!(false));
            return Ok(result);
        }
        let env = session.env()?;
        let ctx = Ctx {
            base,
            env,
            session,
            in_batch: false,
        };
        let (_, windows) = ctx.current_window();
        let events = std::fs::read(ctx.session.events())
            .map(|data| data.iter().filter(|&&b| b == b'\n').count())
            .unwrap_or(0);
        result.insert("running".into(), json!(true));
        result.insert("display".into(), json!(ctx.env.get("DISPLAY")));
        result.insert(
            "vnc_socket".into(),
            json!(ctx.session.run.join("vnc.sock").to_string_lossy()),
        );
        result.insert("windows".into(), windows_list(&windows, false));
        result.insert("events".into(), json!(events));
        return Ok(result);
    }
    if cmd == "frame" && !session.running() {
        result.insert("running".into(), json!(false));
        return Ok(result);
    }
    let geometry = req
        .get("geometry")
        .and_then(Value::as_str)
        .unwrap_or("1280x800");
    let started = session.ensure(&base, geometry)?;
    let env = session.env()?;
    let mut ctx = Ctx {
        base,
        env,
        session,
        in_batch: false,
    };
    let _ = &ctx.base;
    match cmd {
        "start" => {
            if started {
                ctx.log_event("session started", true);
            }
            result.insert("started".into(), json!(started));
            result.insert("display".into(), json!(ctx.env.get("DISPLAY")));
            Ok(result)
        }
        "observe" => Ok(observe(&ctx, truthy(req.get("all")), false)),
        "windows" => {
            result.insert("windows".into(), windows_list(&ctx.list_windows(), true));
            Ok(result)
        }
        "click" => cmd_click(&ctx, req),
        "type" => cmd_type(&ctx, req),
        "key" => cmd_key(&ctx, req),
        "scroll" => cmd_scroll(&ctx, req),
        "launch" => cmd_launch(&ctx, req),
        "drag" => cmd_drag(&ctx, req),
        "look" => cmd_look(&ctx, req),
        "frame" => cmd_frame(&ctx, req),
        "input" => cmd_input(&ctx, req),
        "wait" => cmd_wait(&ctx, req),
        "do" => cmd_do(&mut ctx, req),
        "text" => cmd_text(&ctx, req),
        other => fail!("unknown command: {other}"),
    }
}

fn main() {
    let mut input = String::new();
    let response = match std::io::stdin().read_to_string(&mut input) {
        Err(error) => json!({"ok": false, "error": format!("cannot read the request: {error}")}),
        Ok(_) => match serde_json::from_str::<Value>(&input) {
            Err(error) => {
                json!({"ok": false, "error": format!("the request is not JSON: {error}")})
            }
            // Report a crash as an error: never leave the client without a reply.
            Ok(request) => match std::panic::catch_unwind(|| handle(&request)) {
                Ok(Ok(reply)) => {
                    let mut response = Map::new();
                    response.insert("ok".into(), json!(true));
                    response.extend(reply);
                    Value::Object(response)
                }
                Ok(Err(error)) => json!({"ok": false, "error": error}),
                Err(panic) => {
                    let what = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown".into());
                    json!({"ok": false, "error": format!("the helper failed: {what}")})
                }
            },
        },
    };
    let mut out = std::io::stdout();
    let _ = write!(out, "{response}");
    let _ = out.flush();
}
