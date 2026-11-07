#!/usr/bin/env python3
"""Install the shipped add-on in Firefox and exercise real content/background/download events."""
import argparse
import contextlib
import http.server
import io
import json
import os
from pathlib import Path
import queue
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import zipfile

ROOT = Path(__file__).resolve().parents[1]


class Marionette:
    def __init__(self, connection):
        self.sock = connection
        self.reader = connection.makefile("rb")
        self.sequence = 0
        self.read()

    def read(self):
        length = bytearray()
        while (char := self.reader.read(1)) != b":":
            if not char:
                raise EOFError("Firefox closed the automation connection")
            length.extend(char)
        return json.loads(self.reader.read(int(length)))

    def call(self, method, arguments=None):
        self.sequence += 1
        data = json.dumps([0, self.sequence, method, arguments or {}]).encode()
        self.sock.sendall(str(len(data)).encode() + b":" + data)
        kind, sequence, error, result = self.read()
        if kind != 1 or sequence != self.sequence or error:
            raise RuntimeError(f"{method} failed: {error or result}")
        return result["value"] if isinstance(result, dict) and set(result) == {"value"} else result


def stop(process):
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def launch(cleanup, command, port, log):
    # This isolated fixture needs privileged read access to Firefox's add-on state.
    process = subprocess.Popen(command, stdout=log, stderr=log,
                               env={**os.environ, "MOZ_REMOTE_ALLOW_SYSTEM_ACCESS": "1"})
    cleanup.callback(stop, process)
    deadline = time.monotonic() + 30
    while True:
        if process.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError("Firefox did not start its automation connection")
        try:
            connection = socket.create_connection(("127.0.0.1", port), timeout=15)
            break
        except ConnectionRefusedError:
            time.sleep(0.1)
    browser = Marionette(cleanup.enter_context(connection))
    browser.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
    return browser, process


def assert_signed_and_permanent(browser):
    browser.call("Marionette:SetContext", {"value": "chrome"})
    state = browser.call("WebDriver:ExecuteAsyncScript", {"script": '''
        const done = arguments[arguments.length - 1];
        const { AddonManager } = ChromeUtils.importESModule("resource://gre/modules/AddonManager.sys.mjs");
        AddonManager.getAddonByID("ndm@newdownloadmanager").then(addon => done(addon && {
            active: addon.isActive,
            temporary: addon.temporarilyInstalled,
            signed: addon.signedState,
            disabled: addon.appDisabled || addon.userDisabled,
        }));
    ''', "args": []})
    browser.call("Marionette:SetContext", {"value": "content"})
    assert state and state["active"] and state["temporary"] is False and state["signed"] > 0 and not state["disabled"], state


def run(firefox, extension, permanent=False):
    handoffs = queue.Queue()
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        archive.writestr("fixture.txt", "NDM Firefox fixture")
    payload = output.getvalue()
    if extension.is_file():
        with zipfile.ZipFile(extension) as archive:
            script = archive.read("background/desktop.js").decode()
    else:
        script = (extension / "background/desktop.js").read_text()
    ports = json.loads("[" + re.search(r"const PORTS = \[([^]]+)\]", script)[1] + "]")
    marked_port = ports[-1]

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def respond(self, body, headers):
            self.send_response(200)
            for name, value in headers.items():
                self.send_header(name, value)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path.endswith(".zip"):
                self.respond(payload, {"Content-Type": "application/zip", "Content-Disposition": 'attachment; filename="fixture.zip"'})
            else:
                self.respond(b'<main><a id="download" href="/file.zip">First</a> <a href="/other.zip">Second</a></main>',
                             {"Content-Type": "text/html", "Set-Cookie": "session=ndm-fixture; Path=/"})

        def do_POST(self):
            data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            headers = {"Content-Type": "application/json"}
            if self.server.server_port == marked_port:
                headers["X-NDM"] = "1"
                if self.path == "/add":
                    handoffs.put(json.loads(data))
            body = b'{"menu_dl":true,"menu_all":true}' if self.path == "/config" else b"{}"
            self.respond(body, headers)

    with contextlib.ExitStack() as cleanup:
        # Reserve every candidate so this fixture cannot contact an already running NDM instance.
        for port in [*ports, 0]:
            server = cleanup.enter_context(http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler))
            threading.Thread(target=server.serve_forever, daemon=True).start()
            cleanup.callback(server.shutdown)
        site = f"http://127.0.0.1:{server.server_port}"
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            automation_port = probe.getsockname()[1]
        profile = Path(cleanup.enter_context(tempfile.TemporaryDirectory(prefix="ndm-firefox-")))
        (profile / "user.js").write_text(f'user_pref("marionette.port", {automation_port});\n')
        log = cleanup.enter_context(tempfile.TemporaryFile(mode="w+"))
        command = [firefox, "--headless", "--no-remote", "--profile", str(profile),
                   "--remote-allow-system-access", "--marionette"]
        try:
            browser, process = launch(cleanup, command, automation_port, log)
            installed = browser.call("Addon:Install", {"path": str(extension), "temporary": not permanent})
            addon = installed.get("value") if isinstance(installed, dict) else installed
            assert addon == "ndm@newdownloadmanager", installed
            browser.call("WebDriver:Navigate", {"url": site})
            deadline = time.monotonic() + 10
            while True:
                browser.call("WebDriver:PerformActions", {"actions": [{
                    "type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
                    "actions": [{"type": "pointerMove", "duration": 0, "x": 600, "y": 400},
                                {"type": "pointerDown", "button": 0}],
                }]})
                browser.call("WebDriver:ExecuteScript", {"script": '''
                    const range = document.createRange();
                    range.selectNodeContents(document.querySelector('main'));
                    getSelection().removeAllRanges();
                    getSelection().addRange(range);
                ''', "args": []})
                browser.call("WebDriver:PerformActions", {"actions": [{
                    "type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
                    "actions": [{"type": "pointerUp", "button": 0}],
                }]})
                panels = browser.call("WebDriver:FindElements", {
                    "using": "css selector", "value": 'html > div[style*="2147483647"]',
                })
                if panels:
                    break
                assert time.monotonic() < deadline, "Selected-links content script did not show its panel"
                time.sleep(0.1)
            browser.call("WebDriver:ElementClick", {"id": next(iter(panels[0].values()))})
            selected = handoffs.get(timeout=15)
            assert {link["url"] for link in selected["links"]} == {site + "/file.zip", site + "/other.zip"}, selected
            element = browser.call("WebDriver:FindElement", {"using": "css selector", "value": "#download"})
            browser.call("WebDriver:ElementClick", {"id": next(iter(element.values()))})
            download = handoffs.get(timeout=15)
            assert download["url"] == site + "/file.zip" and download["check"] is True, download
            assert "session=ndm-fixture" in download["cookies"], download
            print("Firefox: shipped manifest, selected links, cookie handoff, download interception and alternate-port discovery")
            if permanent:
                assert_signed_and_permanent(browser)
                browser.call("WebDriver:DeleteSession")
                stop(process)
                browser, _ = launch(cleanup, command, automation_port, log)
                assert_signed_and_permanent(browser)
                print("Firefox: Mozilla signature accepted; permanent add-on remains active after browser restart")
        except Exception:
            log.seek(0)
            print(log.read()[-8000:])
            raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--firefox", default=shutil.which("firefox"))
    parser.add_argument("--extension", type=lambda path: Path(path).resolve(), default=ROOT / "extension")
    parser.add_argument("--permanent", action="store_true", help="require a signed, permanent install that survives restart")
    args = parser.parse_args()
    if not args.firefox:
        parser.error("install Firefox or pass --firefox /path/to/firefox")
    run(args.firefox, args.extension, args.permanent)
