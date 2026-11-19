"""Loopback-only HTTP viewer using the parent's private JSON request pipe."""

import hashlib
import hmac
import json
import math
import re
import secrets
import sys
import threading
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


def validate_actions(actions):
    if not isinstance(actions, list) or not 1 <= len(actions) <= 100:
        raise ValueError("Expected between 1 and 100 input actions")
    clean = []
    for action in actions:
        if not isinstance(action, dict):
            raise ValueError("Invalid input action")
        kind = action.get("kind")
        item = {"kind": kind}
        if kind in ("click", "wheel"):
            for key in ("x", "y"):
                value = action.get(key)
                if type(value) is not int or not 0 <= value <= 65535:
                    raise ValueError("Invalid screen coordinates")
                item[key] = value
            if kind == "click":
                button = action.get("button", "left")
                if button not in ("left", "right", "middle"):
                    raise ValueError("Invalid mouse button")
                item["button"] = button
            else:
                dy = action.get("dy")
                if type(dy) is not int or not -30 <= dy <= 30:
                    raise ValueError("Invalid scroll distance")
                item["dy"] = dy
        elif kind == "text":
            value = action.get("text")
            if not isinstance(value, str) or not 1 <= len(value) <= 16000 or "\x00" in value:
                raise ValueError("Text must contain 1 to 16000 characters")
            item["text"] = value
        elif kind == "key":
            value = action.get("key")
            if not isinstance(value, str) or not re.fullmatch(r"(?:ctrl\+|alt\+|shift\+|super\+)*[a-z0-9_]+", value):
                raise ValueError("Invalid key combination")
            item["key"] = value
        else:
            raise ValueError("Unsupported input action")
        clean.append(item)
    return clean


class ViewerServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, config, bridge):
        super().__init__(("127.0.0.1", config["port"]), Handler)
        self.config = config
        self.bridge = bridge
        self.token = secrets.token_urlsafe(32)
        self.authority = f"127.0.0.1:{self.server_port}"
        self.origin = f"http://{self.authority}"
        self.bridge_lock = threading.Lock()

    def call(self, request):
        with self.bridge_lock:
            return self.bridge(request)


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(15)

    def log_message(self, *_args):
        pass  # URLs and input may contain private information.

    def respond(self, status, body, content_type="application/json"):
        if not isinstance(body, bytes):
            body = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; img-src 'self' data: blob:; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'self'; base-uri 'none'; form-action 'none'")
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def allowed(self, authenticated=False):
        if self.headers.get("Host") != self.server.authority:
            self.respond(403, {"error": "Invalid host"})
            return False
        origin = self.headers.get("Origin")
        if origin and origin != self.server.origin:
            self.respond(403, {"error": "Invalid origin"})
            return False
        if authenticated and not hmac.compare_digest(
            self.headers.get("Authorization", "").encode(), ("Bearer " + self.server.token).encode()
        ):
            self.respond(401, {"error": "Open the complete viewer URL printed in the terminal"})
            return False
        return True

    def do_GET(self):
        url = urlsplit(self.path)
        if not self.allowed(url.path.startswith("/api/")):
            return
        assets = {"/": ("html", "text/html; charset=utf-8"),
                  "/viewer.js": ("js", "text/javascript; charset=utf-8"),
                  "/style.css": ("css", "text/css; charset=utf-8")}
        if url.path in assets:
            key, mime = assets[url.path]
            self.respond(200, self.server.config[key].encode(), mime)
        elif url.path == "/api/meta":
            self.respond(200, {"label": self.server.config["label"], "interval": self.server.config["interval"]})
        elif url.path == "/api/frame":
            try:
                reply = self.server.call({"cmd": "look", "scale": 1.0, "quality": 90, "events": 12})
                picture = reply.pop("image_webp")
                digest = hashlib.sha256(picture.encode()).hexdigest()
                reply["hash"] = digest
                if parse_qs(url.query).get("have", [""])[0] != digest:
                    reply["image_webp"] = picture
                self.respond(200, reply)
            except Exception as error:
                self.respond(502, {"error": str(error)})
        else:
            self.respond(404, {"error": "Not found"})

    def do_POST(self):
        if not self.allowed(True):
            return
        if self.path != "/api/input":
            self.respond(404, {"error": "Not found"})
            return
        try:
            if self.headers.get("Content-Type", "").split(";")[0] != "application/json":
                raise ValueError("Expected application/json")
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 65536 or self.headers.get("Transfer-Encoding"):
                raise ValueError("Invalid input size")
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict):
                raise ValueError("Expected an input object")
            actions = validate_actions(payload.get("actions"))
        except (ValueError, OSError) as error:
            self.respond(400, {"error": str(error)})
            return
        try:
            self.respond(200, self.server.call({"cmd": "input", "actions": actions}))
        except Exception as error:
            # Never retry automatically: some actions may already have happened.
            self.respond(502, {"error": str(error)})


def pipe_bridge(request):
    print(json.dumps({"request": request}), flush=True)
    line = sys.stdin.readline()
    if not line:
        raise RuntimeError("Viewer connection closed")
    response = json.loads(line)
    if "error" in response:
        raise RuntimeError(response["error"])
    return response["reply"]


def main():
    config = json.loads(sys.stdin.readline())
    interval = config["interval"]
    if not math.isfinite(interval) or not 0.2 <= interval <= 60:
        raise ValueError("Invalid refresh interval")
    with ViewerServer(config, pipe_bridge) as server:
        url = server.origin + "/#" + server.token
        print(json.dumps({"event": "ready", "url": url}), flush=True)
        if config.get("open"):
            threading.Thread(target=webbrowser.open, args=(url,), daemon=True).start()
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
