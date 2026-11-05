#!/usr/bin/env python3
"""Measure repeatable NDM workloads and optionally startup on a disposable Linux desktop."""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

from test_inventory import ROOT, build_test_binary


def measured(command):
    with tempfile.TemporaryFile(mode="w+") as output:
        started = time.monotonic()
        process = subprocess.Popen(command, cwd=ROOT, text=True, stdout=output, stderr=output)
        deadline = started + 120
        usage = None
        try:
            if hasattr(os, "wait4"):
                while True:
                    pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                    if pid:
                        process.returncode = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -os.WTERMSIG(status)
                        break
                    if time.monotonic() > deadline:
                        raise TimeoutError(f"Workload timed out: {command}")
                    time.sleep(0.02)
            else:
                process.wait(timeout=120)
        except BaseException:
            process.kill()
            process.wait()
            raise
        elapsed = time.monotonic() - started
        output.seek(0)
        log = output.read()
    if process.returncode:
        raise RuntimeError(log[-12000:])
    stats = {"wall_seconds": elapsed}
    if usage is not None:
        stats.update(user_cpu_seconds=usage.ru_utime, system_cpu_seconds=usage.ru_stime,
                     io_read_blocks=usage.ru_inblock, io_write_blocks=usage.ru_oublock)
        if sys.platform in {"linux", "darwin"}:
            stats["peak_rss_bytes"] = usage.ru_maxrss * (1024 if sys.platform == "linux" else 1)
    samples = []
    for line in log.splitlines():
        _, marker, value = line.partition("NDM_BENCH ")
        if marker:
            samples.append(json.loads(value))
    if not samples:
        raise RuntimeError(f"Workload emitted no measurements: {log[-2000:]}")
    return {"samples": samples, "process": stats}


def startup(app):
    if sys.platform != "linux":
        raise RuntimeError("The startup workload uses a disposable Linux desktop; omit --app on other systems")
    ports_script = (ROOT / "extension/background/desktop.js").read_text()
    ports = json.loads("[" + re.search(r"const PORTS = \[([^]]+)\]", ports_script)[1] + "]")
    for port in ports:
        with socket.socket() as probe:
            probe.settimeout(0.1)
            if probe.connect_ex(("127.0.0.1", port)) == 0:
                raise RuntimeError("A browser bridge port is occupied; use an isolated desktop for startup measurement")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    samples = []
    for _ in range(3):
        with tempfile.TemporaryDirectory(prefix="ndm-startup-") as profile, tempfile.TemporaryFile(mode="w+") as output:
            env = dict(os.environ, APPDATA=profile, XDG_CONFIG_HOME=profile)
            started = time.monotonic()
            process = subprocess.Popen([str(app)], env=env, cwd=ROOT, stdout=output, stderr=output, start_new_session=True)
            try:
                deadline = started + 60
                ready = False
                while time.monotonic() < deadline and process.poll() is None:
                    for port in ports:
                        request = urllib.request.Request(f"http://127.0.0.1:{port}/config", data=b"", headers={"X-NDM": "1"})
                        try:
                            with opener.open(request, timeout=0.2) as response:
                                ready = response.headers.get("X-NDM") == "1"
                        except OSError:
                            continue
                        if ready:
                            break
                    if ready:
                        break
                    time.sleep(0.02)
                if not ready:
                    output.seek(0)
                    raise RuntimeError("App startup did not reach its bridge endpoint\n" + output.read()[-4000:])
                samples.append(time.monotonic() - started)
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
    return {"case": "startup_to_bridge", "trials_seconds": samples, "median_seconds": sorted(samples)[1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--debug", action="store_true", help="measure an existing debug build")
    parser.add_argument("--app", type=Path, help="also measure startup on an isolated Linux desktop, such as CI under Xvfb")
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/performance.json")
    parser.add_argument("--baseline", type=Path, help="include elapsed-time ratios against an earlier report from the same environment")
    args = parser.parse_args()
    binary = build_test_binary(not args.debug).resolve()
    report = {
        "schema": 1,
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "platform": platform.platform(), "machine": platform.machine(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "profile": "debug" if args.debug else "release",
        "workloads": {},
    }
    for case in ["performance_download", "http_protocol_benchmark"]:
        report["workloads"][case] = measured([str(binary), case, "--ignored", "--nocapture", "--test-threads=1"])
    if args.app:
        report["startup"] = startup(args.app.resolve())
    if args.baseline:
        baseline = json.loads(args.baseline.read_text())
        if (baseline["platform"], baseline["machine"], baseline["profile"]) != (report["platform"], report["machine"], report["profile"]):
            raise RuntimeError("Compare reports from the same platform, architecture and build profile")
        if baseline.get("schema") != report["schema"]:
            raise RuntimeError("The baseline report uses a different schema")
        ratios = {}
        for case, result in report["workloads"].items():
            previous = {(sample["case"], sample.get("workload", "")): sample
                        for sample in baseline["workloads"][case]["samples"]}
            current = {(sample["case"], sample.get("workload", "")): sample for sample in result["samples"]}
            if set(current) != set(previous):
                raise RuntimeError("The baseline contains different workload cases")
            for key, sample in current.items():
                ratios["/".join(key)] = sample["median_seconds"] / previous[key]["median_seconds"]
        report["elapsed_ratios_to_baseline"] = ratios
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Performance report saved to {args.output}")


if __name__ == "__main__":
    main()
