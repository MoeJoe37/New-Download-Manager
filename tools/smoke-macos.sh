#!/usr/bin/env bash
# Start-up test of the macOS disk image:  bash tools/smoke-macos.sh installer/out/NDM-<version>-macOS.dmg
# Installs NDM.app from it into ~/Applications, checks its signature, and starts it natively and, where
# Rosetta is installed, as the Intel build: each passes when NDM's browser endpoint answers within 60 s
# without a crash.log. Used by CI.
set -uo pipefail
dmg="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
mnt="$(mktemp -d)"
hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" >/dev/null || { echo "FAIL: the disk image doesn't open"; exit 1; }
mkdir -p ~/Applications
rm -rf ~/Applications/NDM.app
cp -R "$mnt/NDM.app" ~/Applications/
hdiutil detach "$mnt" -quiet
app=~/Applications/NDM.app
codesign --verify --deep --strict "$app" || { echo "FAIL: signature"; exit 1; }
lipo -info "$app/Contents/MacOS/ndm"
data=~/Library/"Application Support"/NewDownloadManager

start() { # label, command...
    local label="$1"
    shift
    rm -f "$data/crash.log"
    "$@" >/tmp/ndm-smoke.log 2>&1 &
    local pid=$! ok=1
    for _ in $(seq 60); do
        if curl -sf -X POST -H 'X-NDM: 1' -o /dev/null http://127.0.0.1:9614/config; then ok=0; break; fi
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    # a few seconds of frames: the window's link drops and, when on, the drop target are set up
    sleep 5
    kill -0 "$pid" 2>/dev/null || { echo "-- NDM ended by itself"; ok=1; }
    if grep -q "link drops on the main window" /tmp/ndm-smoke.log; then ok=1; fi
    pkill -x ndm 2>/dev/null
    sleep 2
    if [ -e "$data/crash.log" ]; then echo "-- crash.log:"; cat "$data/crash.log"; ok=1; fi
    if [ $ok = 0 ]; then echo "NDM started: $label"; else echo "FAIL: NDM didn't start: $label"; cat /tmp/ndm-smoke.log; fi
    return $ok
}

failed=0
start "Apple Silicon" "$app/Contents/MacOS/ndm" || failed=1
# again with the floating drop target shown (Tasks -> Show drop target)
if [ -f "$data/state.json" ]; then
    python3 - "$data/state.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
def on(o):
    if isinstance(o, dict):
        if "drop_target" in o:
            o["drop_target"] = True
        for v in o.values():
            on(v)
on(d)
json.dump(d, open(p, "w"))
PY
    start "Apple Silicon, drop target shown" "$app/Contents/MacOS/ndm" || failed=1
else
    echo "FAIL: no state.json after the first start"; failed=1
fi
if arch -x86_64 /usr/bin/true 2>/dev/null; then
    start "Intel (Rosetta)" arch -x86_64 "$app/Contents/MacOS/ndm" || failed=1
else
    echo "-- no Rosetta: the Intel build isn't started"
fi
exit $failed
