#!/usr/bin/env python3
"""Build browser fixtures with NDM's real exporter for the installed-browser tests."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def export(destination):
    destination.mkdir(parents=True, exist_ok=True)
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    version = next(package["version"] for package in lock["package"] if package["name"] == "serde_json")
    with tempfile.TemporaryDirectory(prefix="ndm-export-fixtures-") as temp:
        project = Path(temp)
        (project / "Cargo.toml").write_text(
            '[package]\nname="ndm-export-fixtures"\nversion="0.0.0"\nedition="2021"\n'
            f'[dependencies]\nserde_json="={version}"\n', encoding="utf-8")
        (project / "src").mkdir()
        (project / "src/main.rs").write_text(
            '#[allow(dead_code)]\n'
            f'#[path={json.dumps(str(ROOT / "src/file_ops.rs"))}] mod file_ops;\n'
            f'#[path={json.dumps(str(ROOT / "src/app/browser_setup/export.rs"))}] mod export;\n'
            'fn main() {\n'
            'let args: Vec<_> = std::env::args_os().collect();\n'
            'let source = std::path::Path::new(&args[1]);\n'
            'let parent = std::path::Path::new(&args[2]);\n'
            'println!("{}", export::write_chromium_export(source, parent, "Browser test fixture").unwrap().display());\n'
            'println!("{}", export::write_firefox_export(parent, "Browser test fixture").unwrap().display());\n'
            '}\n', encoding="utf-8")
        subprocess.run(["cargo", "run", "--quiet", "--manifest-path", str(project / "Cargo.toml"),
                        "--", str(ROOT / "extension"), str(destination)], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=lambda path: Path(path).resolve())
    export(parser.parse_args().destination)
