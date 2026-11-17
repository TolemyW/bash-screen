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
  * refs are short ("e12") and stay with their element from one observation
    to the next, for as long as the element is in the window;
  * every action is followed by a short settle wait and a fresh observation,
    reported as what changed when little did.

This is the minimal stand-in for the planned Rust session supervisor.
"""
import base64
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import time

VERSION = "0.4.0"
BASE = os.path.dirname(os.path.abspath(__file__))
SESSION_SH = os.path.join(BASE, "session.sh")
SESSIONS = os.path.join(BASE, "sessions")
DEFAULT_SESSION = "main"
SETTLE_SECONDS = 0.4

# Set by use_session() for the session a request names.
SESSION = SDIR = RUN = ENVFILE = EVENTS = REFS = FF_PROFILE = None

# Set while a batch of steps runs (see cmd_do): single steps do not observe.
IN_BATCH = False

# Roles that carry no information for an agent unless they have a name.
NOISE_ROLES = {"group", "unknown", "separator", "scrollarea", "menu", "toolbar", "tablist"}


class Fail(Exception):
    pass


# --- session -----------------------------------------------------------------

def session_dir(name):
    """Where a session keeps its state. The default session lives directly in
    BASE, which is where the only session lived before sessions had names."""
    return BASE if name == DEFAULT_SESSION else os.path.join(SESSIONS, name)


def use_session(name):
    """Point this request at one session. Sessions on a host share the
    installed tools and nothing else: each has its own display, session bus,
    browser profile and event log."""
    global SESSION, SDIR, RUN, ENVFILE, EVENTS, REFS, FF_PROFILE
    ok = 0 < len(name) <= 32 and name[0].isalnum() and all(c.isalnum() or c in "-_" for c in name)
    if not ok or not name.isascii() or name != name.lower():
        raise Fail("a session name is 1-32 characters: lower-case letters, digits, - and _")
    SESSION = name
    SDIR = session_dir(name)
    RUN = os.path.join(SDIR, "run")
    ENVFILE = os.path.join(RUN, "env.sh")
    EVENTS = os.path.join(RUN, "events.jsonl")
    REFS = os.path.join(RUN, "refs.json")
    FF_PROFILE = os.path.join(SDIR, "ffprofile")


def session_sh(*args):
    env = dict(os.environ, BSCREEN_BASE=BASE, BSCREEN_SESSION_DIR=SDIR, BSCREEN_MAXIMIZE="1")
    return subprocess.run([SESSION_SH, *args], capture_output=True, text=True, env=env)


def list_sessions():
    names = [DEFAULT_SESSION]
    if os.path.isdir(SESSIONS):
        names += sorted(n for n in os.listdir(SESSIONS) if os.path.isdir(os.path.join(SESSIONS, n)))
    found = []
    for name in names:
        use_session(name)
        if session_running():
            found.append({"name": name, "running": True, "display": session_env().get("DISPLAY")})
        elif name != DEFAULT_SESSION:
            found.append({"name": name, "running": False})
    return found


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
    for tool in ("root/usr/bin/Xtigervnc", "root/usr/bin/openbox", "ad/node_modules/.bin/agent-desktop"):
        if not os.path.exists(os.path.join(BASE, tool)):
            raise Fail("the remote tools are not installed here; run `bash-screen setup` first")
    if os.path.isdir(RUN):
        session_sh("stop")
    done = session_sh("start", geometry)
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


def listed_elements(node, found, show_all, document=None):
    """Flatten the accessibility tree into one entry per useful element."""
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
        parts = [role]
        if name:
            parts.append(json.dumps(name[:120], ensure_ascii=False))
        value = node.get("value")
        if value not in (None, ""):
            parts.append("= " + json.dumps(str(value)[:120], ensure_ascii=False))
        states = [s for s in node.get("states", []) if s in ("checked", "selected", "disabled", "expanded", "focused")]
        if states:
            parts.append("(" + ",".join(states) + ")")
        found.append({"ad": ref, "role": role, "name": name or "", "line": " ".join(parts),
                      "bounds": node.get("bounds")})
    for child in node.get("children", []):
        listed_elements(child, found, show_all, document)


def read_window(env, show_all=False, bounds=False):
    """The current window and its elements, read now. Returns (window,
    windows, elements, partial); elements is None, with a reason in its
    place, when the window cannot be read."""
    window, windows = current_window(env)
    if window is None:
        return None, windows, "(no window is visible to the accessibility channel; use `look`)", False
    args = ["snapshot", "--window-id", window["id"]]
    if not show_all:
        args += ["-i", "--compact"]
    if bounds:
        args.append("--include-bounds")
    envelope = ad(env, *args, timeout=40)
    if not envelope.get("ok"):
        return window, windows, f"(accessibility channel failed: {ad_error(envelope)}; use `look`)", False
    data = envelope["data"]
    found = []
    listed_elements(data["tree"], found, show_all, visible_document(data["tree"], window.get("title") or ""))
    return window, windows, found, not data.get("complete", True)


def last_observation():
    try:
        with open(REFS, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def give_refs(elements, before):
    """Give each element that can be acted on a ref of our own.

    An element keeps its ref from the observation before when it is still
    there, which is judged by its role, its name and its place among the
    elements sharing both. A ref is never given to a second element, so one
    whose element has gone fails instead of landing somewhere else.
    """
    known = {tuple(e["key"]): e["id"] for e in (before or {}).get("elements", [])}
    number = (before or {}).get("next", 1)
    seen = {}
    for element in elements:
        if not element["ad"]:
            continue
        nth = seen.get((element["role"], element["name"]), 0)
        seen[(element["role"], element["name"])] = nth + 1
        key = (element["role"], element["name"], nth)
        if key in known:
            element["id"] = known[key]
        else:
            element["id"] = f"e{number}"
            number += 1
        element["key"] = key
    return number


def changes_since(before, window, elements, show_all):
    """What differs from the observation before, as lines, or None when the
    two cannot be compared or differ so much that the whole list reads better."""
    if not before or before.get("window") != window["id"] or before.get("all") != show_all:
        return None
    old = {e["id"]: e["line"] for e in before["elements"]}
    new = {e["id"]: e["line"] for e in elements if e.get("id")}
    lines = [f"+ [{i}] {line}" for i, line in new.items() if i not in old]
    lines += [f"~ [{i}] {line}" for i, line in new.items() if i in old and old[i] != line]
    lines += [f"- [{i}] {line}" for i, line in old.items() if i not in new]
    if len(lines) > max(12, len(new) // 2):
        return None
    return lines


def observe(env, show_all=False, brief=False):
    """Read the current window and remember it as the latest observation.
    With `brief`, also say what changed since the one before."""
    window, windows, elements, partial = read_window(env, show_all)
    result = {
        "windows": [
            {"title": w.get("title"), "app": w.get("app_name"), "focused": bool(w.get("is_focused"))}
            for w in windows
        ]
    }
    if not isinstance(elements, list):
        result.update(a11y=False, text=elements)
        return result
    before = last_observation()
    number = give_refs(elements, before)
    title = window.get("title") or ""
    with open(REFS, "w", encoding="utf-8") as f:
        json.dump({"next": number, "window": window["id"], "all": show_all, "title": title,
                   "elements": [{k: e[k] for k in ("id", "key", "ad", "line")} for e in elements if e.get("id")]},
                  f, ensure_ascii=False)
    header = f'window: {title} ({window.get("app_name")})'
    if partial:
        header += "  [partial]"
    lines = [(f'[{e["id"]}] ' if e.get("id") else "     ") + e["line"] for e in elements]
    result.update(a11y=True, text="\n".join([header] + lines), elements=len(lines))
    changes = changes_since(before, window, elements, show_all) if brief else None
    if changes is not None:
        if before.get("title") != title:
            header += f'  [was: {before.get("title")}]'
        if changes:
            same = len(lines) - sum(1 for line in changes if line[0] in "+~")
            summary = f"{len(changes)} changed (+ new, ~ different, - gone); the other {same} elements and their refs are as before"
        else:
            summary = f"nothing changed: the same {len(lines)} elements, and their refs, as before"
        result["brief"] = "\n".join([header, summary] + changes)
    return result


def centre(bounds):
    if not bounds:
        return None
    return int(bounds["x"] + bounds["width"] / 2), int(bounds["y"] + bounds["height"] / 2)


def target_of(env, req):
    """The element a request is aimed at, by ref or by name, as a dict:
    `ad` (its agent-desktop ref), `shown` (how the log names it) and, where
    already known, `centre`. None for a request aimed at no element."""
    if "ref" in req:
        short = req["ref"]
        if short.startswith("@"):
            return {"ad": short, "shown": short}
        seen = last_observation()
        if seen is None:
            raise Fail("no observation yet: run `observe` before using a ref")
        for element in seen["elements"]:
            if element["id"] == short:
                return {"ad": element["ad"], "shown": short, "id": short, "all": seen.get("all", False)}
        raise Fail(f"{short} is not in the window any more; run `observe` for the current refs")
    if req.get("name"):
        return find_by_name(env, req["name"])
    return None


def find_by_name(env, wanted):
    """The one element with this name, read now. The name must match whole,
    or failing that be contained, without regard to case."""
    _, _, elements, _ = read_window(env, bounds=True)
    quoted = json.dumps(wanted, ensure_ascii=False)
    if not isinstance(elements, list):
        raise Fail(f"cannot look for {quoted}: {elements}")
    elements = [e for e in elements if e["ad"]]
    low = wanted.lower()
    found = [e for e in elements if e["name"].lower() == low] or [e for e in elements if low in e["name"].lower()]
    if not found:
        raise Fail(f"no element is named {quoted}; run `observe` to see the names")
    if len(found) > 1:
        some = "; ".join(e["line"] for e in found[:6]) + ("; ..." if len(found) > 6 else "")
        raise Fail(f"{len(found)} elements are named {quoted} ({some}); run `observe` and use a ref")
    return {"ad": found[0]["ad"], "shown": quoted, "centre": centre(found[0]["bounds"])}


def centre_of(env, target):
    """Where to click for this element: (x, y), or None when it has no
    known position."""
    if "centre" in target:
        return target["centre"]
    envelope = ad(env, "get", target["ad"], "--property", "bounds")
    if envelope.get("ok"):
        return centre(envelope["data"].get("value"))
    if envelope.get("error", {}).get("code") == "AMBIGUOUS_TARGET" and target.get("id"):
        # agent-desktop will not find an element again that looks like its
        # neighbours (three "Delete" buttons). Our own refs tell them apart
        # by their order, so read the window again and take that one.
        _, _, elements, _ = read_window(env, show_all=target["all"], bounds=True)
        if isinstance(elements, list):
            give_refs(elements, last_observation())
            for element in elements:
                if element.get("id") == target["id"]:
                    return centre(element["bounds"])
    raise Fail(f"{target['shown']}: {ad_error(envelope)} (the element may have gone; run `observe`)")


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
    return {} if IN_BATCH else observe(env, brief=True)


def cmd_click(env, req):
    target = target_of(env, req)
    shown = target["shown"] if target else ""
    if target:
        at = centre_of(env, target)
        if at is None:
            # No position known (for example GTK4 under some window managers):
            # fall back to the semantic click, without the actionability wait.
            envelope = ad(env, "click", target["ad"], "--timeout-ms", "0")
            if not envelope.get("ok"):
                raise Fail(ad_error(envelope))
            return after_action(env, f"click {shown} (semantic)")
        x, y = at
    else:
        x, y = int(req["x"]), int(req["y"])
    def press(x, y):
        args = ["--headed", "mouse-click", "--xy", f"{x},{y}"]
        if req.get("button"):
            args += ["--button", req["button"]]
        if req.get("count"):
            args += ["--count", str(req["count"])]
        return ad(env, *args)

    envelope = press(x, y)
    if not envelope.get("ok") and target and ad(env, "scroll-to", target["ad"]).get("ok"):
        # Most likely scrolled out of view: bring the element in and look
        # again for where it is now.
        time.sleep(SETTLE_SECONDS)
        if req.get("name"):
            target = find_by_name(env, req["name"])
        at = centre_of(env, target)
        if at is not None:
            x, y = at
            envelope = press(x, y)
    if not envelope.get("ok"):
        raise Fail(ad_error(envelope))
    return after_action(env, f"click {shown} at {x},{y}".replace("  ", " "))


def cmd_type(env, req):
    text = req["text"]
    target = target_of(env, req)
    shown_target = target["shown"] if target else ""
    envelope = ad(env, "--headed", "type", target["ad"], text) if target else None
    if envelope is not None and not envelope.get("ok"):
        if envelope.get("error", {}).get("code") != "AMBIGUOUS_TARGET":
            raise Fail(ad_error(envelope))
        # An element agent-desktop cannot tell from its neighbours: put the
        # keyboard focus on it with a click, then paste as below.
        at = centre_of(env, target)
        click = ad(env, "--headed", "mouse-click", "--xy", f"{at[0]},{at[1]}") if at else envelope
        if not click.get("ok"):
            raise Fail(ad_error(click))
        envelope = None
    if envelope is None:
        # No target element: paste into whatever has keyboard focus.
        for args in (["clipboard-set", text], ["press", "ctrl+v"]):
            envelope = ad(env, *args)
            if not envelope.get("ok"):
                raise Fail(ad_error(envelope))
    shown = text if len(text) <= 40 else text[:37] + "..."
    return after_action(env, f"type {shown_target} {json.dumps(shown, ensure_ascii=False)}".replace("  ", " "))


def cmd_key(env, req):
    for combo in req["keys"]:
        envelope = ad(env, "press", combo)
        if not envelope.get("ok"):
            raise Fail(f"{combo}: {ad_error(envelope)}")
    return after_action(env, "key " + " ".join(req["keys"]))


def cmd_scroll(env, req):
    target = target_of(env, req)
    if target:
        envelope = ad(env, "scroll-to", target["ad"])
        summary = f"scroll to {target['shown']}"
    else:
        args = ["--headed", "mouse-wheel", "--x", str(req.get("x", 640)), "--y", str(req.get("y", 400)),
                f'--dy={req.get("dy", -5)}']
        envelope = ad(env, *args)
        summary = f'scroll dy={req.get("dy", -5)}'
    if not envelope.get("ok"):
        raise Fail(ad_error(envelope))
    return after_action(env, summary)


def cmd_input(env, req):
    """Input from a person using the preview: inject it and return at once.

    Unlike the agent's actions this reads nothing back; the preview fetches
    the next frame itself. Each action is logged, marked as a person's.
    """
    done = []
    for action in req.get("actions", []):
        kind = action.get("kind")
        if kind == "click":
            x, y = int(action["x"]), int(action["y"])
            args = ["--headed", "mouse-click", "--xy", f"{x},{y}"]
            if action.get("button") and action["button"] != "left":
                args += ["--button", action["button"]]
            button = action.get("button") or "left"
            steps, summary = [args], ("click" if button == "left" else button + "-click") + f" at {x},{y}"
        elif kind == "wheel":
            x, y, dy = int(action["x"]), int(action["y"]), int(action["dy"])
            steps = [["--headed", "mouse-wheel", "--x", str(x), "--y", str(y), f"--dy={dy}"]]
            summary = f"scroll dy={dy}"
        elif kind == "text":
            text = action["text"]
            steps = [["clipboard-set", text], ["press", "ctrl+v"]]
            shown = text if len(text) <= 40 else text[:37] + "..."
            summary = "type " + json.dumps(shown, ensure_ascii=False)
        elif kind == "key":
            steps, summary = [["press", action["key"]]], "key " + action["key"]
        else:
            raise Fail(f"unknown input: {kind}")
        for args in steps:
            envelope = ad(env, *args)
            if not envelope.get("ok"):
                log_event(f"you: {summary} failed", ok=False)
                raise Fail(f"{summary}: {ad_error(envelope)}")
        done.append(summary)
    # Runs of the same kind read better as one line.
    merged = []
    for summary in done:
        if merged and summary.startswith("key ") and merged[-1].startswith("key ") and len(merged[-1]) < 60:
            merged[-1] += summary[3:]
        else:
            merged.append(summary)
    for summary in merged:
        log_event("you: " + summary)
    return {"done": len(done)}


def window_strings(env):
    """Title of the current window and every name and value in it, for `wait`."""
    window, _ = current_window(env)
    if window is None:
        return "", []
    title = window.get("title") or ""
    envelope = ad(env, "snapshot", "--window-id", window["id"], timeout=40)
    if not envelope.get("ok"):
        return title, []
    tree = envelope["data"]["tree"]
    document = visible_document(tree, title)
    found = []

    def walk(node):
        if node.get("role") == "webarea" and document is not None and node.get("name") != document:
            return  # a hidden document
        for key in ("name", "value"):
            if node.get(key) not in (None, ""):
                found.append(str(node[key]))
        for child in node.get("children", []):
            walk(child)

    walk(tree)
    # Running text, which the tree above does not carry.
    found += read_text(env, title) or []
    return title, found


# Elements whose content reads as one line of text.
ONE_LINE_ROLES = {"paragraph", "heading", "link", "button", "listitem", "cell", "label", "caption", "menuitem", "tab"}


def text_lines(node, document=None):
    """What a person would read in this part of the tree, top to bottom.

    A name is left out when the text under its element already says it, as
    with a link and the words inside it.
    """
    role = node.get("role") or ""
    if role == "webarea" and document is not None and node.get("name") != document:
        return []  # a hidden document
    below = []
    for child in node.get("children", []):
        below += text_lines(child, document)
    if role in ONE_LINE_ROLES and below:
        below = [" ".join(below)]
    squeeze = lambda text: "".join(str(text).split()).lower()
    name, value = node.get("name"), node.get("value")
    said = squeeze(" ".join(below))
    own = []
    if name and squeeze(name) not in said:
        own.append(" ".join(str(name).split()))
    if value not in (None, "") and squeeze(value) not in said and squeeze(value) != squeeze(name or ""):
        own.append(" ".join(str(value).split()))
    if own and role in ONE_LINE_ROLES and below:
        return [": ".join(own) + " " + below[0]]
    return ([": ".join(own)] if own else []) + below


EMBEDDED = chr(0xFFFC)  # stands for a child element inside a text
NO_TEXT_PACKAGES = "python3-gi gir1.2-atspi-2.0"


def read_text(env, title):
    """The text of the window with this title as a person reads it, read
    from the accessibility bus itself: agent-desktop reports elements, not
    the running text of paragraphs. Returns lines, or None where the host
    lacks the Python bindings for it."""
    os.environ.update(env)
    try:
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
    except (ImportError, ValueError):
        return None

    def showing(node):
        return node.get_state_set().contains(Atspi.StateType.SHOWING)

    def children(node):
        found = (node.get_child_at_index(i) for i in range(node.get_child_count()))
        return [child for child in found if child is not None]

    def own_text(node):
        try:
            iface = node.get_text_iface()
            if iface is None:
                return None
            return Atspi.Text.get_text(iface, 0, Atspi.Text.get_character_count(iface))
        except Exception:
            return None

    def lines_of(node):
        # Visible, not showing: what is scrolled out of view belongs to the text.
        if not node.get_state_set().contains(Atspi.StateType.VISIBLE):
            return []
        role = node.get_role_name()
        text = own_text(node)
        kids = children(node)
        if role in ("entry", "password text", "spin button"):
            return ["[" + ("*" * len(text or "") if role == "password text" else (text or "")) + "]"]
        if text and text.replace(EMBEDDED, "").strip():
            # Running text with its children in place, as one line.
            pieces = text.split(EMBEDDED)
            parts = [pieces[0]]
            for index, piece in enumerate(pieces[1:]):
                if index < len(kids):
                    parts.append(" ".join(lines_of(kids[index])))
                parts.append(piece)
            return [" ".join("".join(parts).split())]
        below = []
        for child in kids:
            below += lines_of(child)
        if role in ("table row", "list item") and below:
            return [" | ".join(below) if role == "table row" else " ".join(below)]
        if not below and node.get_name():
            return [" ".join(node.get_name().split())]
        return below

    def find(node, wanted, depth):
        if wanted(node):
            return node
        if depth == 0:
            return None
        for child in children(node):
            hit = find(child, wanted, depth - 1)
            if hit is not None:
                return hit
        return None

    try:
        desktop = Atspi.get_desktop(0)
        window = find(desktop, lambda n: n.get_role_name() in ("frame", "dialog", "window")
                      and n.get_name() == title and showing(n), 2)
        if window is None:
            return None
        # In a browser, the page is what is wanted, not the toolbars around it.
        page = find(window, lambda n: n.get_role_name() == "document web" and showing(n), 12)
        return [line for line in lines_of(page or window) if line]
    except Exception:
        return None


def cmd_text(env, req):
    """The text of the current window, without refs: for reading a page."""
    window, _ = current_window(env)
    if window is None:
        raise Fail("no window is visible to the accessibility channel; use `look`")
    title = window.get("title") or ""
    result = {"title": title, "app": window.get("app_name")}
    lines = read_text(env, title)
    if lines is None:
        envelope = ad(env, "snapshot", "--window-id", window["id"], timeout=40)
        if not envelope.get("ok"):
            raise Fail(f"accessibility channel failed: {ad_error(envelope)}; use `look`")
        tree = envelope["data"]["tree"]
        lines = [line for line in text_lines(tree, visible_document(tree, title)) if line]
        result["note"] = ("only the names of elements: running text such as paragraphs is missing, "
                          f"because the host lacks the packages {NO_TEXT_PACKAGES}")
    result.update(text="\n".join(lines), lines=len(lines))
    return result


def cmd_wait(env, req):
    """Wait until every given condition holds, then observe. Without a
    condition, wait until the picture on the screen stops changing."""
    text, gone, title = req.get("text"), req.get("gone"), req.get("title")
    stable = bool(req.get("stable")) or not (text or gone or title)
    timeout = min(float(req.get("timeout", 15)), 300.0)
    wanted = [f"{label} {json.dumps(value, ensure_ascii=False)}"
              for label, value in (("text", text), ("gone", gone), ("title", title)) if value]
    summary = "wait " + ", ".join(wanted + (["stable"] if stable else []))
    started = time.time()
    last_picture, seen_title = None, ""
    while True:
        unmet = []
        if text or gone or title:
            seen_title, strings = window_strings(env)
            has = lambda needle: any(needle.lower() in s.lower() for s in strings)
            if text and not has(text):
                unmet.append(f"no element contains {json.dumps(text, ensure_ascii=False)}")
            if gone and has(gone):
                unmet.append(f"an element still contains {json.dumps(gone, ensure_ascii=False)}")
            if title and title.lower() not in seen_title.lower():
                unmet.append(f"the window title does not contain {json.dumps(title, ensure_ascii=False)}")
        if stable:
            picture = hashlib.sha1(screenshot_png(env)).hexdigest()
            if picture != last_picture:
                unmet.append("the screen was still changing")
            last_picture = picture
        waited = time.time() - started
        if not unmet:
            log_event(f"{summary} ({waited:.1f} s)")
            result = {} if IN_BATCH else observe(env, brief=True)
            result["waited"] = round(waited, 1)
            return result
        if waited >= timeout:
            log_event(f"{summary} timed out", ok=False)
            where = f"; the window is titled {json.dumps(seen_title, ensure_ascii=False)}" if seen_title else ""
            raise Fail(f"waited {timeout:g} s but " + " and ".join(unmet) + where)
        time.sleep(0.5)


def cmd_do(env, req):
    """Several steps in one request, with one observation at the end.

    Nothing observes while the steps run, so their refs are the ones of the
    observation the caller last saw, whatever the steps change. A step aimed
    at a name looks for it when its turn comes.
    """
    global IN_BATCH
    steps = req.get("steps") or []
    IN_BATCH = True
    table = {"click": cmd_click, "type": cmd_type, "key": cmd_key, "scroll": cmd_scroll,
             "drag": cmd_drag, "wait": cmd_wait}
    try:
        for number, step in enumerate(steps, 1):
            if step.get("cmd") not in table:
                raise Fail(f'step {number}: {step.get("cmd")} cannot be used in `do`')
            try:
                table[step["cmd"]](env, step)
            except Fail as error:
                raise Fail(f'step {number} of {len(steps)} ({step.get("cmd")}) failed: {error}. '
                           f"{number - 1} step(s) before it were done; run `observe` to see where things stand")
    finally:
        IN_BATCH = False
    result = observe(env, brief=True)
    result["steps"] = len(steps)
    return result


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


def cmd_drag(env, req):
    (x1, y1), (x2, y2) = [int(v) for v in req["from"]], [int(v) for v in req["to"]]
    envelope = ad(env, "--headed", "drag", f"--from-xy={x1},{y1}", f"--to-xy={x2},{y2}")
    if not envelope.get("ok"):
        raise Fail(ad_error(envelope))
    return after_action(env, f"drag {x1},{y1} to {x2},{y2}")


def cmd_launch(env, req):
    argv = list(req["argv"])
    if req.get("a11y") is False:
        # Keep the app off the accessibility bus: nothing can read its
        # elements, and it is spared the code paths that reading triggers.
        env = {k: v for k, v in env.items() if k not in ("ACCESSIBILITY_ENABLED", "QT_ACCESSIBILITY")}
        env.update(NO_AT_BRIDGE="1", GTK_A11Y="none", QT_LINUX_ACCESSIBILITY_ALWAYS_ON="0")
    elif os.path.basename(argv[0]) == "firefox":
        env = dict(env, GNOME_ACCESSIBILITY="1")
    if os.path.basename(argv[0]) == "firefox":
        argv = [argv[0], "--no-remote", "--profile", firefox_profile(), *argv[1:]]
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
    if cmd == "sessions":
        return {"sessions": list_sessions()}
    use_session(req.get("session") or DEFAULT_SESSION)
    if cmd == "stop":
        session_sh("stop")
        if req.get("forget"):
            # Drop what the session saved: its browser profile and tool state.
            doomed = [FF_PROFILE, os.path.join(SDIR, "state")] if SESSION == DEFAULT_SESSION else [SDIR]
            for path in doomed:
                shutil.rmtree(path, ignore_errors=True)
        return {"stopped": True, "forgotten": bool(req.get("forget"))}
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
             "launch": cmd_launch, "drag": cmd_drag, "look": cmd_look, "frame": cmd_frame, "input": cmd_input,
             "wait": cmd_wait, "do": cmd_do, "text": cmd_text}
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
