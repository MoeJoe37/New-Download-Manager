#!/usr/bin/env python3
"""Run the repository's local validation, including optional browser and media integration."""
import argparse
import os
import platform
import shutil
import subprocess
import sys

from test_inventory import ROOT, build_test_binary


def run(command, **kwargs):
    print("Running " + " ".join(map(str, command)), flush=True)
    return subprocess.run(list(map(str, command)), cwd=ROOT, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="validate the optimized build")
    parser.add_argument("--browsers", action="store_true", help="include installed browsers and FFmpeg media integration")
    parser.add_argument("--firefox", default=shutil.which("firefox"), help="Firefox executable for the installed add-on test")
    args = parser.parse_args()
    required = ["cargo", "rustfmt"] + (["node", "ffmpeg", "ffprobe"] if args.browsers else [])
    missing = [name for name in required if not shutil.which(name)]
    if missing:
        parser.error("Install the contributor prerequisites for " + ", ".join(missing))
    if args.browsers and not args.firefox:
        parser.error("Install Firefox or pass --firefox /path/to/firefox")

    run(["cargo", "fmt", "--all", "--", "--check"])
    run([sys.executable, "tools/check-boundaries.py"])
    run([sys.executable, "tools/ndm_lng.py", "--check"])
    profile = ["--release"] if args.release else []
    run(["cargo", "clippy", "--locked", "--all-targets", *profile, "--", "-D", "warnings"])
    binary = build_test_binary(args.release)
    run([binary])
    if sys.platform == "linux":
        run([sys.executable, "tools/test_inventory.py", "--check", "--binary", binary])
        if platform.machine() == "x86_64":
            env = dict(os.environ, TZ="UTC", LC_ALL="C")
            for case in ["dialog_snapshots", "desktop_interactions"]:
                run([binary, case, "--ignored", "--test-threads=1"], env=env)
    if args.browsers:
        for script in ["test-extension.mjs", "test-extension-browser.mjs", "test-panel.mjs"]:
            run(["node", f"tools/{script}"])
        run([sys.executable, "tools/test-firefox.py", "--firefox", args.firefox])
        run([binary, "actual_media_", "--ignored", "--test-threads=1"])
    run(["git", "diff", "--check"])
    run(["git", "diff", "--cached", "--check"])
    print("Repository checks passed")


if __name__ == "__main__":
    main()
