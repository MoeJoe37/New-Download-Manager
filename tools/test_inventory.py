#!/usr/bin/env python3
"""Track test names and ignored status across Rust module moves."""

import argparse
import difflib
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
SNAPSHOT = ROOT / "tools" / "snapshots" / "tests.txt"


def build_test_binary(release=True):
    command = ["cargo", "test", "--locked", "--no-run", "--message-format=json"]
    if release:
        command.append("--release")
    build = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
    binaries = []
    for line in build.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("profile", {}).get("test") and item.get("executable"):
            binaries.append(Path(item["executable"]))
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one application test executable, found {binaries}")
    return binaries[0]


def list_tests(command, *flags):
    result = subprocess.check_output([*command, "--list", *flags], cwd=ROOT, text=True)
    return [line[:-len(": test")] for line in result.splitlines() if line.endswith(": test")]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    parser.add_argument("--binary", type=lambda path: Path(path).resolve(), help="use an existing test executable")
    args = parser.parse_args()
    command = [str(args.binary)] if args.binary else ["cargo", "test", "--release", "--locked", "--", "--color", "never"]
    tests = list_tests(command)
    ignored = set(list_tests(command, "--ignored"))
    if not tests or not ignored.issubset(tests):
        sys.exit("Could not read the Rust test inventory")

    entries = sorted((name.rsplit("::", 1)[-1], name in ignored) for name in tests)
    actual = [f"{name}{' (ignored)' if is_ignored else ''}\n" for name, is_ignored in entries]
    if args.write:
        SNAPSHOT.write_text("".join(actual), encoding="utf-8")
        return
    expected = SNAPSHOT.read_text(encoding="utf-8").splitlines(keepends=True)
    if actual != expected:
        sys.stderr.writelines(difflib.unified_diff(expected, actual, str(SNAPSHOT.relative_to(ROOT)), "current tests"))
        sys.exit(1)
    print(f"Test inventory matches ({len(tests)} tests, {len(ignored)} ignored)")


if __name__ == "__main__":
    main()
