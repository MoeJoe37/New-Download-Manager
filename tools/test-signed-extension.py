#!/usr/bin/env python3
"""Reject stale or changed signed packages before shipping the Firefox export."""
import hashlib
import json
from pathlib import Path
import zipfile

ROOT = Path(__file__).resolve().parents[1]
metadata = json.loads((ROOT / "assets/firefox/signed.json").read_text())
package = ROOT / "assets/firefox/ndm.xpi"
assert hashlib.sha256(package.read_bytes()).hexdigest() == metadata["sha256"], "Signed XPI changed"
with zipfile.ZipFile(package) as archive:
    assert archive.testzip() is None
    for name in ["cose.manifest", "cose.sig", "manifest.mf", "mozilla.sf", "mozilla.rsa"]:
        assert archive.getinfo("META-INF/" + name).file_size > 0
    manifest = json.loads(archive.read("manifest.json"))
    assert manifest["version"] == metadata["version"]
    assert manifest["browser_specific_settings"]["gecko"]["id"] == metadata["id"]
    expected = json.loads((ROOT / "extension/manifest.json").read_text())
    expected.pop("key", None)
    expected["background"].pop("service_worker", None)
    expected["permissions"].remove("downloads.ui")
    # App release tooling also bumps the source manifest; unchanged extension code
    # can keep its approved extension version independently of the app version.
    expected["version"] = metadata["version"]
    assert manifest == expected, "Firefox manifest changed: submit a new version to Mozilla"
    for source in sorted((ROOT / "extension").rglob("*")):
        if source.is_file() and source.name != "manifest.json":
            name = source.relative_to(ROOT / "extension").as_posix()
            assert archive.read(name) == source.read_bytes(), f"{name} changed: submit a new version to Mozilla"
    assert archive.read("LICENSE") == (ROOT / "LICENSE").read_bytes()
print(f"Mozilla-signed Firefox {metadata['version']}: checksum, signatures and source match")
