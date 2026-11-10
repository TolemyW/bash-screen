#!/usr/bin/env python3
"""Fixture server for phase 0: serves the test page and logs every request.

The request log is the independent oracle: a test passes only if the action the
agent performed shows up here, regardless of what the agent's own tools report.

usage: server.py PORT LOGFILE
"""
import sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote

PAGE2 = b"<!doctype html><title>Page Two</title><h1>Page Two</h1><a href='/'>Back</a>"


class Handler(SimpleHTTPRequestHandler):
    logfile: Path

    def do_GET(self):
        with self.logfile.open("a", encoding="utf-8") as f:
            f.write(unquote(self.path) + "\n")
        if self.path.startswith(("/event", "/submit")):
            body = b"<!doctype html><title>Submitted</title><h1>Submitted</h1><a href='/'>Back</a>"
            if self.path.startswith("/event"):
                body = b"ok"
            self._send(body)
        elif self.path.startswith("/page2.html"):
            self._send(PAGE2)
        else:
            super().do_GET()

    def _send(self, body: bytes):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def main():
    port, logfile = int(sys.argv[1]), Path(sys.argv[2])
    Handler.logfile = logfile
    here = Path(__file__).resolve().parent
    handler = partial(Handler, directory=str(here))
    ThreadingHTTPServer(("127.0.0.1", port), handler).serve_forever()


if __name__ == "__main__":
    main()
