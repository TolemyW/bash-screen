// The remote screen of a bash-screen session, as a pane in the Herdr app.
// Everything goes through the bash-screen CLI: `look` for the picture and the
// recent actions, and `input` for what the person does on it.
import { View, div, image } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { exec, theme, appHome, props, checking, nextCommand, notify } from "herdr";

// Pictures are shown from this folder, relative to the app's home. It has to be
// under mods/, because the app reloads a picture saved there and shows any other
// path as first loaded. It must not be this mod's own folder, because a file
// saved there restarts the mod.
const SHOWN = "mods/bash-screen-frame";
// Fresh screenshots land here first; a file here is not watched by the app.
const WORK = "data/bash-screen";

const HEADER = 26;
const INTERVAL_MS = 1500;
// How long a newly saved picture stays behind the old one before the two swap,
// so that the app has reloaded it by the time it is seen.
const SWAP_MS = 500; // measured: the app shows a saved picture 0.2 to 0.45 s later
// How long the mark of a press stays after its outcome is known.
const MARK_MS = 900;
const WHEEL_STEP = 40; // pixels of wheel travel for one notch on the remote side
// The recent actions, the agent's and the person's, are listed in whatever room
// the picture leaves: under it, or beside it when the pane is wider than the picture.
const LOG_LINE = 18;
const LOG_PAD = 4; // above the first line when under the picture
const LOG_MOST = 12; // the CLI sends this many
const LOG_SIDE = 200; // narrowest column worth having beside the picture

// Takes a screenshot and publishes it only when it differs from the last one.
// Arguments: work file, last-published copy, file to publish to, shrink factor, "first" when nothing is shown yet
// (the copy kept from before a reload must not count as already shown), the key of the pane to follow or "-",
// then bash-screen's own flags. `--viewer` tells the CLI that this pane is open, so that it does not open
// another one, and asks for the recent actions.
const LOOK = [
  'next=$1; last=$2; target=$3; scale=$4; first=$5; follow=$6; shift 6',
  '[ "$follow" = - ] || export BASH_SCREEN_FOLLOW="$follow"',
  '[ "$first" = first ] && rm -f "$last"',
  'mkdir -p "$(dirname "$next")" "$(dirname "$target")"',
  'bash-screen "$@" --json look --viewer --scale "$scale" -o "$next" || exit 1',
  'if cmp -s "$next" "$last"; then echo same; else cp "$next" "$last" && mv "$next" "$target" && echo changed; fi',
].join("; ");

// Starts the tunnel in the background and prints its process id once it has stayed up for a second.
const TUNNEL = 'nohup "$@" >/dev/null 2>&1 & pid=$!; sleep 1; kill -0 "$pid" 2>/dev/null && echo "$pid"';
const OPEN = 'if command -v open >/dev/null 2>&1; then open "$1"; else xdg-open "$1"; fi';

const NAMED = {
  enter: "return", backspace: "backspace", escape: "escape", tab: "tab", delete: "delete",
  up: "up", down: "down", left: "left", right: "right",
  home: "home", end: "end", pageup: "pageup", pagedown: "pagedown",
};

export default class Screen extends View {
  init(_props, cx) {
    this.id = localStorage.getItem("id") ?? Math.random().toString(36).slice(2, 10);
    localStorage.setItem("id", this.id);
    // Where the last press landed, in the picture's own pixels: { x, y, state }.
    // "waiting" until the screen answers, then "changed" or "unchanged" for a moment.
    this.mark = null;
    this.pressed = null; // { at, what, sent }: timing of the action the person is waiting on
    this.size = null; // of this pane, logical pixels
    this.screen = null; // of the remote display, [width, height]
    this.showing = null; // { host, session } the CLI says the picture is of
    this.events = []; // recent actions on that screen, oldest first: { seq, ok, what }
    this.front = null; // "a" or "b": the picture on top; the other one receives the next change
    this.busy = false;
    this.error = null;
    this.note = ""; // what the last action did
    this.queue = []; // actions waiting to be sent, in order
    this.sending = false;
    this.wheel = 0;
    this.burst = 0; // extra looks still owed after an action
    this.starting = false;
    this.tunnel = parse(localStorage.getItem("tunnel") ?? ""); // { pid, port } while taken over
    this.scale = localStorage.getItem("scale") === "1" ? "1" : "0.5";
    this.handle = cx.focus_handle();
    if (checking()) return;
    cx.timer.every(INTERVAL_MS, (cx) => this.refresh(cx));
    cx.spawn(async (cx) => {
      for (;;) {
        if ((await nextCommand()) === "focus") this.handle.focus();
      }
    });
    this.refresh(cx);
    if (this.tunnel) {
      // A tunnel from before a reload: keep it if it is still running.
      const tunnel = this.tunnel;
      cx.spawn(async (cx) => {
        const alive = (await exec("kill", ["-0", String(tunnel.pid)])).code === 0;
        if (!alive && this.tunnel === tunnel) {
          this.tunnel = null;
          localStorage.removeItem("tunnel");
          cx.notify();
        }
      });
    }
  }

  async close(_cx) {
    const home = appHome();
    if (this.tunnel) await exec("kill", [String(this.tunnel.pid)]);
    await exec("rm", ["-f", ...["a", "b"].map((s) => `${home}/${this.path(s)}`),
      `${home}/${WORK}/${this.id}-next.png`, `${home}/${WORK}/${this.id}-last.png`]);
  }

  path(side) {
    return `${SHOWN}/${this.id}-${side}.png`;
  }

  // --host and --session come from the block's props; without them the CLI's defaults apply.
  target() {
    const p = props() ?? {};
    const flags = [];
    if (p.host) flags.push("--host", String(p.host));
    if (p.session) flags.push("--session", String(p.session));
    return flags;
  }

  // A pane opened by the CLI beside a terminal is given that terminal's key as
  // `follow`, and shows whichever session the terminal used last. The CLI reads
  // the key from its environment and then ignores --host and --session.
  follow() {
    const key = (props() ?? {}).follow;
    return key ? String(key) : null;
  }

  // Runs the CLI for this pane's session.
  cli(args, options) {
    const key = this.follow();
    const words = [...this.target(), ...args];
    if (!key) return exec("bash-screen", words, options);
    return exec("env", [`BASH_SCREEN_FOLLOW=${key}`, "bash-screen", ...words], options);
  }

  label() {
    const p = props() ?? {};
    const s = this.showing ?? {};
    return [s.host ?? p.host ?? "default host", s.session ?? p.session ?? "main"].join(" · ");
  }

  // Redraws only when something the person can see has changed.
  refresh(cx) {
    if (this.busy) return;
    this.busy = true;
    cx.spawn(async (cx) => {
      const home = appHome();
      const back = this.front === "a" ? "b" : "a";
      let error = null, changed = false, screen = this.screen, events = this.events, showing = this.showing;
      try {
        const r = await exec("sh", ["-c", LOOK, "bash-screen-look",
          `${home}/${WORK}/${this.id}-next.png`, `${home}/${WORK}/${this.id}-last.png`,
          `${home}/${this.path(back)}`, this.scale, this.front ? "later" : "first", this.follow() ?? "-",
          ...this.target()], { timeout: 20000 });
        const lines = r.stdout.trim().split("\n");
        const reply = parse(lines[0]);
        if (r.code === 0 && reply?.ok) {
          screen = reply.screen;
          events = reply.events ?? [];
          if (reply.host) showing = { host: reply.host, session: reply.session };
          changed = lines[lines.length - 1] === "changed";
        } else {
          error = (reply?.error || r.stderr || r.stdout || "bash-screen failed").trim();
        }
      } catch (e) {
        error = String(e?.message ?? e);
      }
      const redraw = error !== this.error || String(screen) !== String(this.screen)
        || stamp(events) !== stamp(this.events) || JSON.stringify(showing) !== JSON.stringify(this.showing);
      this.error = error;
      this.screen = screen;
      this.events = events;
      this.showing = showing;
      if (redraw) cx.notify();
      if (!changed) {
        this.busy = false;
        this.again(cx);
        return;
      }
      // The new picture is behind the old one now. Swap once the app has loaded it.
      cx.timer.after(this.front ? SWAP_MS : 0, (cx) => {
        this.front = back;
        this.busy = false;
        if (this.pressed && this.pressed.sent !== null) this.answered(true, cx);
        cx.notify();
        this.again(cx);
      });
    });
  }

  // The action the person was waiting on has shown its result (or never will):
  // colour the mark by the outcome, put the time it took in the header, and keep it in the log.
  answered(changed, cx) {
    const p = this.pressed;
    if (!p) return;
    this.pressed = null;
    const mark = this.mark;
    if (mark) {
      mark.state = changed ? "changed" : "unchanged";
      cx.timer.after(MARK_MS, (cx) => {
        if (this.mark !== mark) return; // a later press has its own mark
        this.mark = null;
        cx.notify();
      });
    }
    const now = Date.now();
    const total = ((now - p.at) / 1000).toFixed(1);
    if (changed) this.note = `${p.what} · ${total} s`;
    const sent = p.sent === null ? "-" : String(p.sent - p.at);
    const line = `${new Date(p.at).toISOString()} ${p.what} sent_ms=${sent} shown_ms=${changed ? now - p.at : "-"}`;
    cx.spawn(async (_cx) => {
      await exec("sh", ["-c", 'echo "$1" >> "$2"', "bash-screen-log", line, `${appHome()}/${WORK}/timing.log`]);
    });
  }

  again(cx) {
    if (this.burst <= 0) {
      // Nothing changed after the action: stop waiting for an answer.
      if (this.pressed && this.pressed.sent !== null) { this.answered(false, cx); cx.notify(); }
      return;
    }
    this.burst -= 1;
    cx.timer.after(300, (cx) => this.refresh(cx));
  }

  // What the person does goes out in order, through the CLI's `input`, which injects
  // it and returns without reading the window back. Whatever piles up while one
  // call is on its way goes out together in the next.
  send(action, cx) {
    const last = this.queue[this.queue.length - 1];
    if (last && last.kind === "text" && action.kind === "text") last.text += action.text;
    else if (last && last.kind === "wheel" && action.kind === "wheel") last.dy += action.dy;
    else this.queue.push(action);
    if (this.sending) return;
    this.sending = true;
    cx.spawn(async (cx) => {
      while (this.queue.length) {
        const batch = this.queue.splice(0).flatMap(dashesAsKeys);
        const what = batch.map(describe).join(", ");
        let ok = false;
        try {
          const r = await this.cli(["--json", "input", JSON.stringify(batch)], { timeout: 30000 });
          ok = r.code === 0;
        } catch (_e) {
          ok = false;
        }
        this.note = ok ? what : `${what}: failed`;
        if (this.pressed && this.pressed.sent === null) this.pressed.sent = Date.now();
        if (!ok) this.answered(false, cx);
        cx.notify();
        // The screen answers over the next moments: look now and a few more times, without waiting for the timer.
        this.burst = 3;
        this.refresh(cx);
      }
      this.sending = false;
    });
  }

  press(point, shown, cx) {
    this.handle.focus();
    if (!this.screen || !point) return;
    const x = Math.round((point.x / shown.width) * this.screen[0]);
    const y = Math.round((point.y / shown.height) * this.screen[1]);
    if (x < 0 || y < 0 || x >= this.screen[0] || y >= this.screen[1]) return;
    this.mark = { x: point.x, y: point.y, state: "waiting" };
    this.pressed = { at: Date.now(), what: `click ${x},${y}`, sent: null };
    cx.notify();
    this.send({ kind: "click", x, y }, cx);
  }

  key(event, cx) {
    const m = event.modifiers;
    if (m.platform) return; // the app's own shortcuts
    const name = NAMED[event.key];
    if (!name && !m.control && !m.alt && event.key_char) {
      this.send({ kind: "text", text: event.key_char }, cx);
      return;
    }
    const base = name ?? (event.key === "space" ? "space" : event.key.length === 1 ? event.key : null);
    if (!base) return;
    const combo = [m.control && "ctrl", m.alt && "alt", m.shift && "shift", base].filter(Boolean).join("+");
    this.send({ kind: "key", key: combo }, cx);
  }

  turn(event, shown, cx) {
    const at = event.local_position;
    if (!this.screen || !at) return;
    this.wheel += event.delta.y;
    const notches = Math.trunc(this.wheel / WHEEL_STEP);
    if (!notches) return;
    this.wheel -= notches * WHEEL_STEP;
    const x = Math.round((at.x / shown.width) * this.screen[0]);
    const y = Math.round((at.y / shown.height) * this.screen[1]);
    this.send({ kind: "wheel", x, y, dy: notches }, cx);
  }

  toggleScale(cx) {
    this.scale = this.scale === "1" ? "0.5" : "1";
    localStorage.setItem("scale", this.scale);
    // The picture changes even if the screen has not: forget the last one.
    cx.spawn(async (cx) => {
      await exec("rm", ["-f", `${appHome()}/${WORK}/${this.id}-last.png`]);
      this.note = this.scale === "1" ? "full resolution" : "half resolution";
      cx.notify();
      this.refresh(cx);
    });
  }

  // Taking over: an SSH tunnel to the session's VNC socket, kept running until Stop.
  takeOver(cx) {
    if (this.tunnel || this.starting) return;
    this.starting = true;
    this.note = "opening the tunnel …";
    cx.notify();
    cx.spawn(async (cx) => {
      let problem = "take over: failed";
      try {
        const r = await this.cli(["vnc"], { timeout: 20000 });
        const words = r.stdout.split("\n")[0].trim().split(" ");
        const port = (r.stdout.match(/-L (\d+):/) ?? [])[1];
        if (r.code === 0 && words[0] === "ssh" && port) {
          words.splice(1, 0, "-o", "ExitOnForwardFailure=yes", "-o", "BatchMode=yes");
          const started = await exec("sh", ["-c", TUNNEL, "bash-screen-tunnel", ...words], { timeout: 20000 });
          const pid = started.stdout.trim();
          if (started.code === 0 && pid) {
            this.tunnel = { pid, port };
            localStorage.setItem("tunnel", JSON.stringify(this.tunnel));
            cx.write_to_clipboard(`localhost:${port}`);
            this.note = "";
            notify({
              title: `Taking over ${this.label()}`,
              body: `The screen is on localhost:${port} for a VNC client (no password). The address is on the clipboard. Press Stop in the pane when you are done.`,
              timeout: 12000,
            });
            problem = null;
          } else {
            problem = `take over: the tunnel did not start (is port ${port} in use?)`;
          }
        } else if (r.stderr.trim()) {
          problem = `take over: ${r.stderr.trim()}`;
        }
      } catch (_e) {
        problem = "take over: failed";
      }
      if (problem) {
        this.note = problem;
        notify({ title: "Take over failed", body: problem, level: "error" });
      }
      this.starting = false;
      cx.notify();
    });
  }

  stopTakeOver(cx) {
    const tunnel = this.tunnel;
    if (!tunnel) return;
    this.tunnel = null;
    localStorage.removeItem("tunnel");
    this.note = "take-over ended";
    cx.notify();
    cx.spawn(async (_cx) => {
      await exec("kill", [String(tunnel.pid)]);
      notify({ title: "Take-over ended", body: `The tunnel on localhost:${tunnel.port} is closed.`, timeout: 5000 });
    });
  }

  openViewer(cx) {
    const tunnel = this.tunnel;
    cx.spawn(async (cx) => {
      let ok = false;
      try {
        ok = (await exec("sh", ["-c", OPEN, "bash-screen-open", `vnc://localhost:${tunnel.port}`])).code === 0;
      } catch (_e) {
        ok = false;
      }
      if (!ok) {
        this.note = "no VNC viewer opened; connect one by hand";
        cx.notify();
      }
    });
  }

  // The largest size with the screen's proportions that fits under the header.
  // The picture comes first; the log gets the room that is left over, `log`:
  // "below" the picture, at its "side", or null when neither has room for it.
  shown() {
    if (!this.size || !this.screen) return null;
    const room = { width: this.size.width, height: this.size.height - HEADER };
    if (room.width <= 0 || room.height <= 0) return null;
    const k = Math.min(room.width / this.screen[0], room.height / this.screen[1]);
    const width = Math.floor(this.screen[0] * k);
    const height = Math.floor(this.screen[1] * k);
    const below = Math.floor((room.height - height - LOG_PAD) / LOG_LINE);
    let log = null;
    if (below >= 1) log = { where: "below", width, lines: Math.min(LOG_MOST, below) };
    else if (room.width - width >= LOG_SIDE) {
      log = { where: "side", width: room.width - width, lines: Math.min(LOG_MOST, Math.floor(room.height / LOG_LINE)) };
    }
    return { width, height, log: log && log.lines > 0 ? log : null };
  }

  // The last actions on the screen, oldest first.
  log(log, t) {
    const recent = this.events.slice(-log.lines);
    return v_flex().w(log.width).flex_none().px(8).text_size(12)
      .when(log.where === "below", (el) => el.pt(LOG_PAD))
      .children(recent.map((e) => h_flex().h(LOG_LINE).flex_none().gap(8).items_center()
        .child(div().w(36).flex_none().text_color(t.dim).child(String(e.seq)))
        .child(div().flex_1().truncate().text_color(e.ok === false ? t.blocked : t.muted).child(e.what))));
  }

  button(id, text, t, action) {
    return div().id(id).px(6).rounded(4).text_color(t.muted)
      .hover((s) => s.bg(t.raised).text_color(t.foreground))
      .on_click((_event, cx) => action(cx))
      .child(text);
  }

  // The mark of a press: pulsing in the accent colour while the screen has not
  // answered, then steady in `done` when the picture changed and in `blocked` when it did not.
  ring(mark, t) {
    const ring = div().absolute().left(mark.x - 7).top(mark.y - 7).w(14).h(14).rounded(7).border(2);
    if (mark.state === "waiting") {
      return ring.border_color(t.accent).loop("opacity", { from: 0.35, to: 1, period: 600 });
    }
    return ring.border_color(mark.state === "changed" ? t.done : t.blocked);
  }

  render(_cx) {
    const t = theme();
    const shown = this.shown();
    const status = this.error ? "not connected" : this.screen ? `${this.screen[0]}×${this.screen[1]}` : "connecting …";

    // Taking over changes the header in place, in colour and content, so that
    // the picture under it stays where it is.
    const over = this.tunnel;
    const header = h_flex().h(HEADER).flex_none().px(8).gap(6).items_center().text_size(12)
      .bg(over ? t.selected : t.chrome).text_color(over ? t.foreground : t.muted)
      .child(div().text_color(t.foreground).when(Boolean(over), (el) => el.font_semibold())
        .child(over ? "Taken over" : "bash-screen"))
      .child(div().truncate().child(over ? `VNC on localhost:${over.port}, no password` : this.label()))
      .child(div().flex_1().truncate().text_color(over ? t.muted : t.dim).child(this.note))
      .when(!over, (el) => el
        .child(div().text_color(this.error ? t.blocked : t.dim).child(status))
        .child(this.button("scale", this.scale === "1" ? "1:1" : "1:2", t, (cx) => this.toggleScale(cx)))
        .child(this.button("takeover", "Take over", t, (cx) => this.takeOver(cx))))
      .when(Boolean(over), (el) => el
        .child(this.button("viewer", "Open viewer", t, (cx) => this.openViewer(cx)).text_color(t.foreground))
        .child(this.button("stop", "Stop", t, (cx) => this.stopTakeOver(cx)).text_color(t.foreground)));

    let body;
    if (this.front && shown) {
      const back = this.front === "a" ? "b" : "a";
      const layer = (side) => image(this.path(side)).absolute().top(0).left(0).w(shown.width).h(shown.height);
      // Both pictures are always drawn, the current one last, so on top. A change is
      // saved to the one behind, where its reload cannot be seen.
      body = div().id("shot").relative().w(shown.width).h(shown.height).flex_none().bg(t.background)
        .on_mouse_down("left", (event, cx) => this.press(event.local_position, shown, cx))
        .on_scroll_wheel((event, cx) => this.turn(event, shown, cx))
        .child(layer(back))
        .child(layer(this.front))
        .when(Boolean(this.mark), (el) => el.child(this.ring(this.mark, t)));
    } else {
      body = div().p(12).text_size(12).text_color(this.error ? t.blocked : t.dim)
        .child(this.error ?? "Waiting for the first picture …");
    }

    const log = this.front && shown && this.events.length ? shown.log : null;
    return v_flex().id("root").size_full().bg(t.background)
      .track_focus(this.handle)
      .on_key_down((event, cx) => this.key(event, cx))
      .on_size_change((size, cx) => { this.size = size; cx.notify(); })
      .child(header)
      // With the log at its side the picture moves to the left edge; otherwise it is centred.
      .child(h_flex().flex_none().items_start().when(log?.where !== "side", (el) => el.justify_center())
        .child(body)
        .when(log?.where === "side", (el) => el.child(this.log(log, t))))
      .when(log?.where === "below", (el) => el
        .child(h_flex().flex_none().justify_center().child(this.log(log, t))));
  }
}

// Text is pasted remotely by a tool that reads a leading dash as a flag, so
// leading dashes go as key presses and the rest as text.
function dashesAsKeys(action) {
  if (action.kind !== "text" || !action.text.startsWith("-")) return [action];
  const rest = action.text.replace(/^-+/, "");
  const keys = Array(action.text.length - rest.length).fill({ kind: "key", key: "minus" });
  return rest ? [...keys, { kind: "text", text: rest }] : keys;
}

function describe(action) {
  if (action.kind === "click") return `click ${action.x},${action.y}`;
  if (action.kind === "text") return `type ${action.text.length} chars`;
  if (action.kind === "key") return `key ${action.key}`;
  return `scroll ${action.dy}`;
}

// Changes exactly when the list of actions does: sequence numbers only grow.
function stamp(events) {
  const last = events[events.length - 1];
  return last ? `${events.length}:${last.seq}` : "";
}

function parse(text) {
  try {
    return JSON.parse(text);
  } catch (_e) {
    return null;
  }
}
