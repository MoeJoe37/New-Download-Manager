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
    pkill -x ndm 2>/dev/null
    sleep 2
    if [ -e "$data/crash.log" ]; then echo "-- crash.log:"; cat "$data/crash.log"; ok=1; fi
    if [ $ok = 0 ]; then echo "NDM started: $label"; else echo "FAIL: NDM didn't start: $label"; cat /tmp/ndm-smoke.log; fi
    return $ok
}

failed=0
start "Apple Silicon" "$app/Contents/MacOS/ndm" || failed=1
if arch -x86_64 /usr/bin/true 2>/dev/null; then
    start "Intel (Rosetta)" arch -x86_64 "$app/Contents/MacOS/ndm" || failed=1
else
    echo "-- no Rosetta: the Intel build isn't started"
fi
exit $failed
