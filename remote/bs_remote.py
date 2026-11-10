#!/usr/bin/env python3
"""bash-screen remote helper.

Runs on the remote host, once per request, over SSH. It reads one JSON request
from stdin and writes one JSON response to stdout:

    request:  {"cmd": "<name>", ...arguments}
    response: {"ok": true, ...} or {"ok": false, "error": "<message>"}

It drives one GUI session (see session.sh) through agent-desktop, and adds the
normalisation that phase 0 showed to be necessary:

  * clicks go through "read the element bounds, click the centre", because
    agent-desktop's headed ref click is refused on web content and its semantic
    click can hang GTK3 apps that open a modal dialog;
  * refs are short ("e12") and always belong to the latest observation;
  * every action is followed by a short settle wait and a fresh observation.

This is the minimal stand-in for the planned Rust session supervisor.
"""
import base64
import hashlib
import io
import json
import os
import subprocess
import sys
import time

VERSION = "0.1.0"
BASE = os.path.dirname(os.path.abspath(__file__))
RUN = os.path.join(BASE, "run")
ENVFILE = os.path.join(RUN, "env.sh")
SESSION_SH = os.path.join(BASE, "session.sh")
EVENTS = os.path.join(RUN, "events.jsonl")
LAST_SNAPSHOT = os.path.join(RUN, "last_snapshot")
FF_PROFILE = os.path.join(BASE, "ffprofile")
SETTLE_SECONDS = 0.4

# Roles that carry no information for an agent unless they have a name.
NOISE_ROLES = {"group", "unknown", "separator", "scrollarea", "menu", "toolbar", "tablist"}


class Fail(Exception):
    pass


# --- session -----------------------------------------------------------------

def session_running():
    if not os.path.isfile(ENVFILE):
        return False
    try:
        with open(os.path.join(RUN, "xvnc.pid")) as f:
            os.kill(int(f.read().strip()), 0)
        return True
    except (OSError, ValueError):
        return False


def session_env():
    """Environment of the GUI session, parsed from the env file session.sh wrote."""
    out = subprocess.run(
        ["bash", "-c", f". '{ENVFILE}' && env -0"], capture_output=True, check=True
    ).stdout
    env = {}
    for item in out.split(b"\0"):
        if b"=" in item:
            key, _, value = item.partition(b"=")
            env[key.decode()] = value.decode(errors="replace")
    return env


def ensure_session(geometry):
    if session_running():
        return False
    if os.path.isdir(RUN):
        subprocess.run([SESSION_SH, "stop"], capture_output=True)
    env = dict(os.environ, BSCREEN_BASE=BASE, BSCREEN_MAXIMIZE="1")
    done = subprocess.run([SESSION_SH, "start", geometry], capture_output=True, text=True, env=env)
    if done.returncode != 0 or not session_running():
        raise Fail("could not start the session: " + (done.stderr or done.stdout).strip()[-400:])
    return True


# --- agent-desktop -----------------------------------------------------------

def ad(env, *args, timeout=30):
    """Run one agent-desktop command and return its parsed JSON envelope."""
    try:
        done = subprocess.run(
            ["agent-desktop", *args], capture_output=True, text=True, env=env, timeout=timeout
        )
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": {"code": "TIMEOUT", "message": "agent-desktop did not return"}}
    try:
        return json.loads(done.stdout)
    except ValueError:
        return {"ok": False, "error": {"code": "INTERNAL", "message": (done.stderr or done.stdout)[-300:]}}


def ad_error(envelope):
    err = envelope.get("error", {})
    return f'{err.get("code", "ERROR")}: {err.get("message", "")}'


def list_windows(env):
    envelope = ad(env, "list-windows")
    return envelope.get("data", []) if envelope.get("ok") else []


def current_window(env):
    windows = list_windows(env)
    for window in windows:
        if window.get("is_focused"):
            return window, windows
    return (windows[0] if windows else None), windows


# --- observation -------------------------------------------------------------

def visible_document(tree, title):
    """Name of the web document that belongs to the window title, if any.

    Browsers keep hidden documents in the tree (Firefox preloads a new-tab
    page). The visible one is the document the window is titled after.
    """
    names = []

    def walk(node):
        if node.get("role") == "webarea" and node.get("name"):
            names.append(node["name"])
        for child in node.get("children", []):
            walk(child)

    walk(tree)
    matches = [n for n in names if title and title.startswith(n)]
    return max(matches, key=len) if matches else None


def compact_lines(node, lines, show_all, document=None):
    """Flatten the accessibility tree into one short line per useful element."""
    role = node.get("role") or "?"
    name = node.get("name")
    if role == "webarea" and document is not None and name != document:
        return  # a hidden document
    ref = node.get("ref_id")
    actions = [a for a in node.get("available_actions", []) if a not in ("SetFocus",)]
    interactive = bool(ref) and any(a != "Click" for a in actions) or role in (
        "button", "link", "menuitem", "checkbox", "radiobutton", "textfield", "combobox", "tab", "menubutton", "switch",
    )
    keep = (ref and interactive and not (role in NOISE_ROLES and not name)) or (show_all and name)
    if keep:
        parts = []
        parts.append(f"[{ref.split(':')[1]}]" if ref else "     ")
        parts.append(role)
        if name:
            parts.append(json.dumps(name[:120], ensure_ascii=False))
        value = node.get("value")
        if value not in (None, ""):
            parts.append("= " + json.dumps(str(value)[:120], ensure_ascii=False))
        states = [s for s in node.get("states", []) if s in ("checked", "selected", "disabled", "expanded", "focused")]
        if states:
            parts.append("(" + ",".join(states) + ")")
        lines.append(" ".join(parts))
    for child in node.get("children", []):
        compact_lines(child, lines, show_all, document)


def observe(env, show_all=False):
    window, windows = current_window(env)
    result = {
        "windows": [
            {"title": w.get("title"), "app": w.get("app_name"), "focused": bool(w.get("is_focused"))}
            for w in windows
        ]
    }
    if window is None:
        result.update(a11y=False, text="(no window is visible to the accessibility channel; use `look`)")
        return result
    args = ["snapshot", "--window-id", window["id"]]
    if not show_all:
        args += ["-i", "--compact"]
    envelope = ad(env, *args, timeout=40)
    if not envelope.get("ok"):
        result.update(a11y=False, text=f"(accessibility channel failed: {ad_error(envelope)}; use `look`)")
        return result
    data = envelope["data"]
    with open(LAST_SNAPSHOT, "w") as f:
        f.write(data["snapshot_id"])
    lines = []
    compact_lines(data["tree"], lines, show_all, visible_document(data["tree"], window.get("title") or ""))
    header = f'window: {window.get("title")} ({window.get("app_name")})'
    if not data.get("complete", True):
        header += "  [partial]"
    result.update(a11y=True, text="\n".join([header] + lines), elements=len(lines))
    return result


def full_ref(short):
    """Expand "e12" to the qualified ref of the latest observation."""
    if short.startswith("@"):
        return short
    try:
        with open(LAST_SNAPSHOT) as f:
            return f"@{f.read().strip()}:{short}"
    except OSError:
        raise Fail("no observation yet: run `observe` before using a ref")


def bounds_centre(env, ref):
    envelope = ad(env, "get", ref, "--property", "bounds")
    if not envelope.get("ok"):
        raise Fail(f"ref {ref}: {ad_error(envelope)} (refs expire after every action; run `observe` again)")
    bounds = envelope["data"].get("value")
    if not bounds:
        return None
    return int(bounds["x"] + bounds["width"] / 2), int(bounds["y"] + bounds["height"] / 2)


# --- pixels ------------------------------------------------------------------

def screenshot_png(env):
    path = os.path.join(RUN, "shot.png")
    if os.path.exists(path):
        os.remove(path)
    envelope = ad(env, "screenshot", path)
    if not envelope.get("ok"):
        raise Fail("screenshot failed: " + ad_error(envelope))
    with open(path, "rb") as f:
        return f.read()


def load_image(png_bytes):
    try:
        from PIL import Image
    except ImportError:
        raise Fail("the remote host needs the Python Pillow package for image processing")
    return Image, Image.open(io.BytesIO(png_bytes)).convert("RGB")


# --- events ------------------------------------------------------------------

def log_event(summary, ok=True):
    seq = 0
    try:
        with open(EVENTS, "rb") as f:
            seq = sum(1 for _ in f)
    except OSError:
        pass
    with open(EVENTS, "a", encoding="utf-8") as f:
        f.write(json.dumps({"seq": seq + 1, "t": time.time(), "ok": ok, "what": summary}, ensure_ascii=False) + "\n")


def read_events(limit):
    try:
        with open(EVENTS, encoding="utf-8") as f:
            return [json.loads(line) for line in f.readlines()[-limit:]]
    except OSError:
        return []


# --- commands ----------------------------------------------------------------

def after_action(env, summary):
    """Settle, log, and return the fresh observation that every action reports."""
    time.sleep(SETTLE_SECONDS)
    log_event(summary)
    return observe(env)


def cmd_click(env, req):
    if "ref" in req:
        ref = full_ref(req["ref"])
        centre = bounds_centre(env, ref)
        if centre is None:
            # No position known (for example GTK4 under some window managers):
            # fall back to the semantic click, without the actionability wait.
            envelope = ad(env, "click", ref, "--timeout-ms", "0")
            if not envelope.get("ok"):
                raise Fail(ad_error(envelope))
            return after_action(env, f'click {req["ref"]} (semantic)')
        x, y = centre
    else:
        x, y = int(req["x"]), int(req["y"])
    args = ["--headed", "mouse-click", "--xy", f"{x},{y}"]
    if req.get("button"):
        args += ["--button", req["button"]]
    if req.get("count"):
        args += ["--count", str(req["count"])]
    envelope = ad(env, *args)
    if not envelope.get("ok"):
        raise Fail(ad_error(envelope))
    target = req.get("ref") or ""
    return after_action(env, f"click {target} at {x},{y}".replace("  ", " "))


def cmd_type(env, req):
    text = req["text"]
    if "ref" in req:
        envelope = ad(env, "--headed", "type", full_ref(req["ref"]), text)
        if not envelope.get("ok"):
            raise Fail(ad_error(envelope))
    else:
        # No target element: paste into whatever has keyboard focus.
        for args in (["clipboard-set", text], ["press", "ctrl+v"]):
            envelope = ad(env, *args)
            if not envelope.get("ok"):
                raise Fail(ad_error(envelope))
    shown = text if len(text) <= 40 else text[:37] + "..."
    return after_action(env, f'type {req.get("ref", "")} {json.dumps(shown, ensure_ascii=False)}'.replace("  ", " "))


def cmd_key(env, req):
    for combo in req["keys"]:
        envelope = ad(env, "press", combo)
        if not envelope.get("ok"):
            raise Fail(f"{combo}: {ad_error(envelope)}")
    return after_action(env, "key " + " ".join(req["keys"]))


def cmd_scroll(env, req):
    if "ref" in req:
        envelope = ad(env, "scroll-to", full_ref(req["ref"]))
        summary = f'scroll to {req["ref"]}'
    else:
        args = ["--headed", "mouse-wheel", "--x", str(req.get("x", 640)), "--y", str(req.get("y", 400)),
                f'--dy={req.get("dy", -5)}']
        envelope = ad(env, *args)
        summary = f'scroll dy={req.get("dy", -5)}'
    if not envelope.get("ok"):
        raise Fail(ad_error(envelope))
    return after_action(env, summary)


def firefox_profile():
    """A throwaway profile, so the session never touches the user's real Firefox."""
    if not os.path.isdir(FF_PROFILE):
        os.makedirs(FF_PROFILE)
        prefs = {
            "browser.shell.checkDefaultBrowser": False,
            "browser.aboutwelcome.enabled": False,
            "startup.homepage_welcome_url": "",
            "startup.homepage_welcome_url.additional": "",
            "startup.homepage_override_url": "",
            "browser.startup.homepage_override.mstone": "ignore",
            "datareporting.policy.dataSubmissionEnabled": False,
            "datareporting.policy.firstRunURL": "",
            "browser.tabs.warnOnClose": False,
            "browser.sessionstore.resume_from_crash": False,
            "browser.sessionstore.max_resumed_crashes": 0,
            "browser.newtab.preload": False,
            "browser.startup.page": 0,
            "termsofuse.bypassNotification": True,
            "accessibility.force_disabled": -1,
        }
        with open(os.path.join(FF_PROFILE, "user.js"), "w") as f:
            for key, value in prefs.items():
                f.write(f"user_pref({json.dumps(key)}, {json.dumps(value)});\n")
    return FF_PROFILE


def cmd_launch(env, req):
    argv = list(req["argv"])
    if os.path.basename(argv[0]) == "firefox":
        argv = [argv[0], "--no-remote", "--profile", firefox_profile(), *argv[1:]]
        env = dict(env, GNOME_ACCESSIBILITY="1")
    log = open(os.path.join(RUN, "launch.log"), "ab")
    try:
        subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    except OSError as error:
        raise Fail(f"cannot launch {argv[0]}: {error}")
    time.sleep(float(req.get("wait", 4)))
    log_event("launch " + " ".join(req["argv"])[:80])
    return observe(env)


def cmd_look(env, req):
    Image, image = load_image(screenshot_png(env))
    width, height = image.size
    if req.get("region"):
        x, y, w, h = req["region"]
        image = image.crop((x, y, x + w, y + h))
        origin = [x, y]
    else:
        origin = [0, 0]
    scale = float(req.get("scale", 1.0))
    if scale != 1.0:
        image = image.resize((max(1, int(image.width * scale)), max(1, int(image.height * scale))), Image.LANCZOS)
    out = io.BytesIO()
    image.save(out, "WEBP", quality=int(req.get("quality", 60)))
    return {
        "image_webp": base64.b64encode(out.getvalue()).decode(),
        "screen": [width, height], "origin": origin, "scale": scale, "size": [image.width, image.height],
    }


def cmd_frame(env, req):
    """One preview frame: recent events, plus the screen scaled to the pane's cell grid."""
    png = screenshot_png(env)
    cols, rows = int(req["cols"]), int(req["rows"])
    # Sub-pixels per terminal cell: 1x2 for half blocks, 2x3 for sextants.
    sx, sy = int(req.get("sx", 1)), int(req.get("sy", 2))
    # In image mode the client gives the pixel size of the area it can draw on.
    box = (int(req["px_w"]), int(req["px_h"])) if req.get("px_w") else None
    # Optional cap on the longest edge of the picture that is sent.
    max_px = int(req.get("max_px") or 0)
    digest = hashlib.sha1(png).hexdigest()[:16] + f":{cols}x{rows}:{sx}x{sy}:{box}:{max_px}"
    window, windows = current_window(env)
    result = {
        "hash": digest,
        "events": read_events(int(req.get("events", 6))),
        "title": window.get("title") if window else None,
        "windows": len(windows),
    }
    if digest != req.get("have") and box:
        Image, image = load_image(png)
        # Fit the screen into the box. The client scales the picture to the
        # display size, so never send more pixels than the screen has.
        screen = list(image.size)
        ratio = min(box[0] / image.width, box[1] / image.height)
        display = (max(1, int(image.width * ratio)), max(1, int(image.height * ratio)))
        send = min(1.0, ratio)
        if max_px:
            send = min(send, max_px / max(image.width, image.height))
        if send < 1:
            image = image.resize((max(1, int(image.width * send)), max(1, int(image.height * send))), Image.LANCZOS)
        out = io.BytesIO()
        image.save(out, "WEBP", quality=70)
        result.update(w=image.width, h=image.height, display_w=display[0], display_h=display[1], screen=screen,
                      webp=base64.b64encode(out.getvalue()).decode())
    elif digest != req.get("have"):
        Image, image = load_image(png)
        # A cell is about twice as tall as it is wide, so the picture keeps its
        # shape when it covers cells_w x cells_h cells with cells_w : 2*cells_h
        # equal to the screen's width : height.
        ratio = min(cols / image.width, (rows * 2) / image.height)
        cells_w = max(1, int(image.width * ratio))
        cells_h = max(1, int(image.height * ratio / 2))
        size = (cells_w * sx, cells_h * sy)
        small = image.resize(size, Image.LANCZOS)
        out = io.BytesIO()
        small.save(out, "PNG", optimize=True)
        result.update(w=size[0], h=size[1], png=base64.b64encode(out.getvalue()).decode(), screen=list(image.size))
    return result


def handle(req):
    cmd = req.get("cmd")
    if cmd == "version":
        return {"version": VERSION}
    if cmd == "stop":
        subprocess.run([SESSION_SH, "stop"], capture_output=True, env=dict(os.environ, BSCREEN_BASE=BASE))
        return {"stopped": True}
    if cmd == "status":
        if not session_running():
            return {"running": False}
        env = session_env()
        _, windows = current_window(env)
        return {"running": True, "display": env.get("DISPLAY"), "vnc_socket": os.path.join(RUN, "vnc.sock"),
                "windows": [{"title": w.get("title"), "app": w.get("app_name")} for w in windows],
                "events": len(read_events(10 ** 6))}
    if cmd == "frame" and not session_running():
        return {"running": False}
    started = ensure_session(req.get("geometry", "1280x800"))
    env = session_env()
    if cmd == "start":
        if started:
            log_event("session started")
        return {"started": started, "display": env.get("DISPLAY")}
    if cmd == "observe":
        return observe(env, show_all=bool(req.get("all")))
    if cmd == "windows":
        return {"windows": [{"title": w.get("title"), "app": w.get("app_name"), "focused": bool(w.get("is_focused"))}
                            for w in list_windows(env)]}
    table = {"click": cmd_click, "type": cmd_type, "key": cmd_key, "scroll": cmd_scroll,
             "launch": cmd_launch, "look": cmd_look, "frame": cmd_frame}
    if cmd not in table:
        raise Fail(f"unknown command: {cmd}")
    return table[cmd](env, req)


def main():
    try:
        request = json.load(sys.stdin)
        response = {"ok": True, **handle(request)}
    except Fail as error:
        response = {"ok": False, "error": str(error)}
    except Exception as error:  # report, never hang the client on a traceback
        response = {"ok": False, "error": f"{type(error).__name__}: {error}"}
    json.dump(response, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
