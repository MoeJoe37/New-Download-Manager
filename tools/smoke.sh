#!/usr/bin/env bash
# Start-up test of the AppImage on this distribution:  bash tools/smoke.sh NDM-x86_64.AppImage
# Installs a virtual X server, Mesa and the X libraries every desktop has (as root, with the
# distribution's package manager), starts NDM and passes when its browser endpoint (127.0.0.1:9614)
# answers within 30 s without a crash.log. Used by CI in containers of each distribution.
set -uo pipefail
img="$(realpath "${1:?usage: smoke.sh NDM-x86_64.AppImage}")"
. /etc/os-release
echo "== $PRETTY_NAME"
case " $ID ${ID_LIKE:-} " in
    *" debian "* | *" ubuntu "*)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq && apt-get install -y -qq xvfb libgl1 libegl1 libgl1-mesa-dri libx11-6 libxcursor1 libxrandr2 libxi6 \
            libxkbcommon0 libxkbcommon-x11-0 libfontconfig1 fonts-dejavu-core libdbus-1-3 >/dev/null ;;
    *" fedora "* | *" rhel "* | *" centos "*)
        dnf install -y -q xorg-x11-server-Xvfb mesa-libGL mesa-libEGL mesa-dri-drivers libX11 libXcursor libXrandr libXi \
            libxkbcommon libxkbcommon-x11 fontconfig dejavu-sans-fonts dbus-libs >/dev/null ;;
    *" suse "* | *" opensuse "*)
        zypper -q -n install xorg-x11-server-Xvfb Mesa-libGL1 Mesa-libEGL1 Mesa-dri libX11-6 libXcursor1 libXrandr2 libXi6 \
            libxkbcommon0 libxkbcommon-x11-0 fontconfig dejavu-fonts libdbus-1-3 >/dev/null ;;
    *" arch "*)
        pacman -Syu --noconfirm --needed -q xorg-server-xvfb mesa libx11 libxcursor libxrandr libxi libxkbcommon libxkbcommon-x11 \
            fontconfig ttf-dejavu dbus >/dev/null ;;
    *) echo "unknown distribution $ID: installing nothing" ;;
esac

dir="$(mktemp -d)"
cd "$dir" && "$img" --appimage-extract >/dev/null || { echo "FAIL: can't extract the AppImage"; exit 1; }
export HOME="$dir/home" DISPLAY=:77 LIBGL_ALWAYS_SOFTWARE=1
mkdir -p "$HOME"
Xvfb :77 -screen 0 1280x800x24 >/dev/null 2>&1 &
sleep 2
squashfs-root/AppRun >"$dir/ndm.log" 2>&1 &
pid=$!
crash="$HOME/.config/NewDownloadManager/crash.log"
for _ in $(seq 30); do
    if (echo >/dev/tcp/127.0.0.1/9614) 2>/dev/null && [ ! -e "$crash" ]; then
        echo "NDM started"
        kill "$pid" 2>/dev/null
        exit 0
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
done
echo "FAIL: NDM didn't start"
echo "-- missing libraries:"; ldd squashfs-root/usr/bin/ndm | grep "not found"
echo "-- output:"; cat "$dir/ndm.log"
[ -e "$crash" ] && { echo "-- crash.log:"; cat "$crash"; }
exit 1
