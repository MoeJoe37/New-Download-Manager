#!/usr/bin/env python3
"""Exercise startup measurement with real launcher and child processes."""
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import time
import unittest

from benchmark import startup


@unittest.skipUnless(sys.platform == "linux", "startup measurement uses Linux process groups")
class StartupTests(unittest.TestCase):
    def fixture(self, root, mode):
        app = root / "fixture-app"
        app.write_text("#!/usr/bin/env python3\n" + f"MODE = {mode!r}\nLOG = {str(root / 'starts.jsonl')!r}\n" + '''
import http.server
import json
import os
import subprocess
import sys
import time

if MODE == "failure":
    sys.exit("intentional fixture failure")
if MODE == "handoff" and len(sys.argv) == 1:
    subprocess.Popen([sys.executable, __file__, "renderer"])
    sys.exit(0)
time.sleep(0.15)
with open(LOG, "a") as log:
    log.write(json.dumps({"appdata": os.environ["APPDATA"], "config": os.environ["XDG_CONFIG_HOME"]}) + "\\n")

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.send_response(200)
        self.send_header("X-NDM", "1")
        self.send_header("Content-Length", "0")
        self.end_headers()
    def log_message(self, *args):
        pass

server = http.server.HTTPServer(("127.0.0.1", 9614), Handler)
server.serve_forever()
''')
        app.chmod(0o755)
        return app

    def assert_bridge_released(self):
        deadline = time.monotonic() + 2
        while True:
            with socket.socket() as probe:
                if probe.connect_ex(("127.0.0.1", 9614)) != 0:
                    return
            self.assertLess(time.monotonic(), deadline, "startup measurement left its app running")
            time.sleep(0.01)

    def exercise_startup(self, mode):
        with tempfile.TemporaryDirectory(prefix="ndm-startup-test-") as directory:
            root = Path(directory)
            result = startup(self.fixture(root, mode))
            self.assertEqual(len(result["trials_seconds"]), 3)
            self.assert_bridge_released()
            starts = [json.loads(line) for line in (root / "starts.jsonl").read_text().splitlines()]
            profiles = {entry["appdata"] for entry in starts}
            self.assertEqual(len(profiles), 3)
            for entry in starts:
                self.assertEqual(entry["appdata"], entry["config"])
                self.assertFalse(os.path.exists(entry["appdata"]))

    def test_direct_startup(self):
        self.exercise_startup("direct")

    def test_renderer_handoff_and_child_cleanup(self):
        self.exercise_startup("handoff")

    def test_failed_startup_reports_its_output(self):
        with tempfile.TemporaryDirectory(prefix="ndm-startup-test-") as directory:
            with self.assertRaisesRegex(RuntimeError, "intentional fixture failure"):
                startup(self.fixture(Path(directory), "failure"))
            self.assert_bridge_released()


if __name__ == "__main__":
    unittest.main()
