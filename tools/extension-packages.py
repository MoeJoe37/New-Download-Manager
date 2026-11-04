"""The browser extension as the stores take it: installer/out/NDM-<version>-chrome-extension.zip (Chrome Web
Store, Edge Add-ons, Opera) and NDM-<version>-firefox-extension.zip (addons.mozilla.org).

    python tools/extension-packages.py            # into installer/out
    python tools/extension-packages.py OUT_DIR

The folder in extension/ is what NDM installs unpacked: its manifest has a "key" (a fixed id, which NDM's
native messaging host and localhost server accept) and both background forms. The stores give their copy
an id of their own and refuse the key, so each package drops it, and keeps only the background its browser
runs (Chromium: the service worker; Firefox: the scripts). Add a store's id to the repository variable
NDM_EXTENSION_IDS (release.yml) so NDM answers that copy too (core::extension_ids).
"""
import json, os, sys, zipfile

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
SRC = os.path.join(ROOT, "extension")


def manifest_for(store):
    m = json.load(open(os.path.join(SRC, "manifest.json"), encoding="utf-8"))
    m.pop("key", None)
    bg = m.get("background", {})
    if store == "chrome":
        m.pop("browser_specific_settings", None)
        bg.pop("scripts", None)
    else:
        bg.pop("service_worker", None)
        # downloads.ui is Chromium's (hiding its download bar); Firefox warns about it
        m["permissions"] = [p for p in m.get("permissions", []) if p != "downloads.ui"]
    return m


def package(store, out_dir, version):
    path = os.path.join(out_dir, f"NDM-{version}-{store}-extension.zip")
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("manifest.json", json.dumps(manifest_for(store), indent=2, ensure_ascii=False) + "\n")
        for base, dirs, files in os.walk(SRC):
            dirs.sort()
            for f in sorted(files):
                full = os.path.join(base, f)
                rel = os.path.relpath(full, SRC).replace(os.sep, "/")
                if rel != "manifest.json" and not f.startswith("."):
                    z.write(full, rel)
    return path


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "installer", "out")
    os.makedirs(out, exist_ok=True)
    version = json.load(open(os.path.join(SRC, "manifest.json"), encoding="utf-8"))["version"]
    for store in ["chrome", "firefox"]:
        p = package(store, out, version)
        with zipfile.ZipFile(p) as z:
            m = json.loads(z.read("manifest.json"))
            assert "key" not in m and m["version"] == version, p
            assert ("service_worker" in m["background"]) == (store == "chrome"), p
            names = z.namelist()
            assert all(n in names for n in ["background.js", "content.js", "page.js", "options.html", "icons/128.png"]), names
        print(p)
