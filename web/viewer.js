"use strict";
const $ = (id) => document.getElementById(id);
const token = location.hash.slice(1) || sessionStorage.getItem("viewerToken") || "";
if (token) sessionStorage.setItem("viewerToken", token);
history.replaceState(null, "", location.pathname);
const screen = $("screen");
let interval = 1000, paused = false, control = false, hash = "", size = null;
let refreshing = false, timer, inputTimer, sending = false, queue = [], composing = false;

function status(message, state = "") {
  $("status").textContent = message;
  $("dot").className = state;
}
async function api(path, data) {
  const response = await fetch(path, {
    method: data ? "POST" : "GET", cache: "no-store",
    headers: {Authorization: `Bearer ${token}`, ...(data ? {"Content-Type": "application/json"} : {})},
    ...(data ? {body: JSON.stringify(data)} : {}),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || `Request failed (${response.status})`);
  return result;
}
function zoom() {
  const value = $("zoom").value;
  $("viewport").classList.toggle("fit", value === "fit");
  screen.style.width = value === "fit" || !size ? "" : `${size[0] * Number(value)}px`;
  screen.style.height = value === "fit" || !size ? "" : `${size[1] * Number(value)}px`;
}
async function refresh() {
  clearTimeout(timer);
  if (paused || refreshing) return;
  refreshing = true;
  const start = performance.now();
  try {
    const frame = await api(`/api/frame?have=${encodeURIComponent(hash)}`);
    if (frame.image_webp) {
      const source = `data:image/webp;base64,${frame.image_webp}`;
      const next = new Image();
      next.src = source;
      await next.decode();
      screen.src = source;
      hash = frame.hash;
      size = frame.screen;
      screen.hidden = false;
      $("empty").hidden = true;
      $("control").disabled = paused;
      zoom();
    }
    $("log").replaceChildren(...(frame.events || []).map((event) => {
      const li = document.createElement("li");
      li.textContent = event.what || JSON.stringify(event);
      return li;
    }));
    if (size) $("metrics").textContent = `${size[0]} × ${size[1]} · ${Math.round(performance.now() - start)} ms`;
    if (!paused) status(control ? "Live · Control enabled" : "Live · View only", "live");
  } catch (error) {
    status(error.message, "error");
  } finally {
    refreshing = false;
    if (!paused) timer = setTimeout(refresh, interval);
  }
}
function setControl(value) {
  control = value;
  $("control").setAttribute("aria-pressed", String(value));
  $("control").textContent = value ? "Release control" : "Take control";
  $("textform").hidden = !value;
  screen.classList.toggle("controlling", value);
  $("hint").textContent = value
    ? "Click the screen to focus · Ctrl+] releases control · Use Send text for input methods"
    : "View only · Take control to click, type, and scroll";
  if (value) screen.focus();
  else { queue = []; clearTimeout(inputTimer); }
  if (!paused) status(value ? "Live · Control enabled" : "Live · View only", "live");
}
function enqueue(action) {
  if (!control || paused || !size) return;
  // The remote clipboard tool parses leading dashes as flags.
  if (action.kind === "text" && action.text.startsWith("-")) {
    const rest = action.text.replace(/^-+/, "");
    for (let i = 0; i < action.text.length - rest.length; i++) enqueue({kind:"key", key:"minus"});
    if (rest) enqueue({kind:"text", text:rest});
    return;
  }
  const last = queue.at(-1);
  if (last?.kind === "text" && action.kind === "text" && last.text.length + action.text.length <= 16000) last.text += action.text;
  else if (last?.kind === "wheel" && action.kind === "wheel") last.dy = Math.max(-30, Math.min(30, last.dy + action.dy));
  else queue.push(action);
  clearTimeout(inputTimer);
  inputTimer = setTimeout(flush, 35);
}
async function flush() {
  if (sending || !queue.length) return;
  sending = true;
  try {
    while (queue.length && control) {
      const actions = queue.splice(0, 100);
      await api("/api/input", {actions});
    }
    if (!paused) refresh();
  } catch (error) {
    setControl(false);
    status(`Input failed; control released. ${error.message}`, "error");
  } finally { sending = false; }
}
function position(event) {
  if (!size) return null;
  const rect = screen.getBoundingClientRect();
  // object-fit can leave padding inside the image element.
  const scale = Math.min(rect.width / size[0], rect.height / size[1]);
  const width = size[0] * scale, height = size[1] * scale;
  const x = event.clientX - rect.left - (rect.width - width) / 2;
  const y = event.clientY - rect.top - (rect.height - height) / 2;
  if (x < 0 || y < 0 || x >= width || y >= height) return null;
  return {x: Math.min(size[0] - 1, Math.floor(x / scale)), y: Math.min(size[1] - 1, Math.floor(y / scale))};
}
screen.addEventListener("mousedown", (event) => {
  if (!control) return;
  event.preventDefault(); screen.focus();
  const point = position(event);
  if (point && event.button <= 2) enqueue({kind: "click", ...point, button: ["left", "middle", "right"][event.button]});
});
screen.addEventListener("contextmenu", (event) => { if (control) event.preventDefault(); });
screen.addEventListener("wheel", (event) => {
  if (!control) return;
  event.preventDefault();
  const point = position(event);
  if (point && event.deltaY) enqueue({kind: "wheel", ...point, dy: -Math.sign(event.deltaY) * Math.min(10, Math.max(1, Math.ceil(Math.abs(event.deltaY) / 40)))});
}, {passive: false});
const names = {Enter:"return", Backspace:"backspace", Tab:"tab", Escape:"escape", Delete:"delete", ArrowUp:"up", ArrowDown:"down", ArrowLeft:"left", ArrowRight:"right", Home:"home", End:"end", PageUp:"pageup", PageDown:"pagedown", " ":"space", "-":"minus", "=":"equal", "+":"plus", "[":"bracketleft", "]":"bracketright"};
screen.addEventListener("keydown", (event) => {
  if (!control) return;
  if (event.ctrlKey && event.key === "]") { event.preventDefault(); setControl(false); return; }
  if (event.isComposing || composing || ["Shift", "Control", "Alt", "Meta"].includes(event.key)) return;
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "v") return; // paste event carries text
  event.preventDefault();
  if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
    enqueue({kind:"text", text:event.key}); return;
  }
  const key = names[event.key] || (/^(F\d{1,2}|[a-z0-9])$/i.test(event.key) ? event.key.toLowerCase() : null);
  if (key) enqueue({kind:"key", key: `${event.ctrlKey || event.metaKey ? "ctrl+" : ""}${event.altKey ? "alt+" : ""}${event.shiftKey ? "shift+" : ""}${key}`});
});
screen.addEventListener("paste", (event) => {
  if (!control) return;
  event.preventDefault();
  const text = event.clipboardData.getData("text/plain");
  if (text.length > 16000) { status("Paste is limited to 16000 characters", "error"); return; }
  if (text) enqueue({kind:"text", text});
});
screen.addEventListener("compositionstart", () => { composing = true; });
screen.addEventListener("compositionend", (event) => { composing = false; if (event.data) enqueue({kind:"text", text:event.data}); });
$("textform").addEventListener("submit", (event) => {
  event.preventDefault();
  if ($("text").value) { enqueue({kind:"text", text:$("text").value}); $("text").value = ""; screen.focus(); }
});
$("enter").onclick = () => { enqueue({kind:"key", key:"return"}); screen.focus(); };
$("zoom").onchange = zoom;
$("control").onclick = () => setControl(!control);
$("pause").onclick = () => {
  paused = !paused;
  $("pause").textContent = paused ? "Resume" : "Pause";
  $("control").disabled = paused || !size;
  if (paused) { clearTimeout(timer); setControl(false); status("Paused"); }
  else refresh();
};
$("fullscreen").onclick = async () => {
  try {
    if (document.fullscreenElement) await document.exitFullscreen();
    else await document.documentElement.requestFullscreen();
  } catch (error) { status(error.message, "error"); }
};
(async () => {
  try {
    const meta = await api("/api/meta");
    $("host").textContent = meta.label;
    document.title = `${meta.label} · bash-screen`;
    interval = meta.interval * 1000;
    refresh();
  } catch (error) { status(error.message, "error"); }
})();
