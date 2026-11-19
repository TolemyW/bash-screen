"""Exercise the actual HTTP handler without an SSH host or desktop."""

import copy
import importlib.util
import json
from pathlib import Path
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import Request, urlopen

spec = importlib.util.spec_from_file_location("viewer", Path(__file__).parents[1] / "web/server.py")
viewer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(viewer)


class ViewerTests(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.frame = {"image_webp": "test-picture", "screen": [1280, 800], "events": [{"what": "test"}]}

        def bridge(request):
            self.requests.append(request)
            if request["cmd"] == "input":
                return {"done": len(request["actions"])}
            return copy.deepcopy(self.frame)

        config = {"port": 0, "interval": 1, "label": "test-host/test-session", "html": "<h1>Viewer</h1>", "js": "", "css": ""}
        self.server = viewer.ViewerServer(config, bridge)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def request(self, path, data=None, authenticated=True, **headers):
        if authenticated:
            headers["Authorization"] = "Bearer " + self.server.token
        if data is not None:
            data = json.dumps(data).encode()
            headers["Content-Type"] = "application/json"
        req = Request(self.server.origin + path, data=data, headers=headers)
        try:
            response = urlopen(req, timeout=2)
        except HTTPError as error:
            response = error
        with response:
            return response.status, response.read(), response.headers

    def test_only_authenticated_same_origin_requests_reach_the_remote(self):
        self.assertEqual(self.request("/api/frame", authenticated=False)[0], 401)
        self.assertEqual(self.request("/api/frame", Origin="https://example.org")[0], 403)
        self.assertEqual(self.request("/api/frame", Host="example.org")[0], 403)
        self.assertEqual(self.requests, [])
        status, body, _ = self.request("/api/meta")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["label"], "test-host/test-session")

    def test_static_assets_and_unchanged_frame(self):
        status, _, headers = self.request("/", authenticated=False)
        self.assertEqual(status, 200)
        self.assertIn("script-src 'self'", headers["Content-Security-Policy"])
        status, body, _ = self.request("/api/frame")
        frame = json.loads(body)
        self.assertEqual(status, 200)
        self.assertEqual(frame["image_webp"], "test-picture")
        status, body, _ = self.request("/api/frame?have=" + frame["hash"])
        self.assertEqual(status, 200)
        self.assertNotIn("image_webp", json.loads(body))
        self.assertEqual(self.requests[0], {"cmd": "look", "scale": 1.0, "quality": 90, "events": 12})

    def test_input_order_unicode_and_rejected_actions(self):
        actions = [{"kind": "click", "x": 20, "y": 30}, {"kind": "text", "text": "Hello 世界"}, {"kind": "key", "key": "ctrl+l"}]
        status, body, _ = self.request("/api/input", {"actions": actions})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["done"], 3)
        self.assertEqual([a["kind"] for a in self.requests[0]["actions"]], ["click", "text", "key"])
        for bad in [{"kind": "launch", "argv": ["sh"]}, {"kind": "click", "x": -1, "y": 0}, {"kind": "wheel", "x": 0, "y": 0, "dy": 999}, {"kind": "key", "key": "--help"}]:
            self.assertEqual(self.request("/api/input", {"actions": [bad]})[0], 400)
        self.assertEqual(len(self.requests), 1)

    def test_bridge_failure_is_reported_without_retry(self):
        calls = []
        def fail(request):
            calls.append(request)
            raise RuntimeError("Connection lost")
        self.server.bridge = fail
        status, body, _ = self.request("/api/input", {"actions": [{"kind": "key", "key": "return"}]})
        self.assertEqual(status, 502)
        self.assertEqual(json.loads(body)["error"], "Connection lost")
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
