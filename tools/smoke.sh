#!/usr/bin/env bash
# Start-up test of NDM's Linux builds on this distribution:  bash tools/smoke.sh FILE...
# FILE is NDM-x86_64.AppImage, a .deb, an .rpm or the .tar.gz. As root: installs a virtual X server,
# Mesa and the libraries every desktop has that AppImages leave out on purpose (X, fribidi,
# harfbuzz, libgpg-error...) with the distribution's package manager; a .deb or .rpm is installed
# with it too, which also checks the package's dependencies, and the .tar.gz gets GTK 3. Each one
# passes when NDM's browser endpoint (127.0.0.1:9614) answers within 30 s without a crash.log.
# Give the AppImage first: it is meant to start before GTK is installed. Used by CI per distribution.
set -uo pipefail
. /etc/os-release
echo "== $PRETTY_NAME"
case " $ID ${ID_LIKE:-} " in
    *" debian "* | *" ubuntu "*) pm=apt ;;
    *" fedora "* | *" rhel "* | *" centos "*) pm=dnf ;;
    *" suse "* | *" opensuse "*) pm=zypper ;;
    *" arch "*) pm=pacman ;;
    *) pm=none ;;
esac
install() { # packages or package files, by the distribution's own names
    case $pm in
        apt) DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" >/dev/null ;;
        dnf) dnf install -y -q "$@" >/dev/null ;;
        zypper) zypper -q -n --no-gpg-checks install --allow-unsigned-rpm "$@" >/dev/null ;;
        pacman) pacman -S --noconfirm --needed -q "$@" >/dev/null ;;
        none) echo "unknown distribution $ID: not installing $*" ;;
    esac
}
case $pm in
    apt) apt-get update -qq
        install xvfb libgl1 libegl1 libgl1-mesa-dri libx11-6 libxcursor1 libxrandr2 libxi6 libxkbcommon0 libxkbcommon-x11-0 \
            libfontconfig1 libfribidi0 libharfbuzz0b libgpg-error0 fonts-dejavu-core libdbus-1-3 ;;
    dnf) install xorg-x11-server-Xvfb mesa-libGL mesa-libEGL mesa-dri-drivers libX11 libXcursor libXrandr libXi libxkbcommon \
            libxkbcommon-x11 fontconfig fribidi harfbuzz dejavu-sans-fonts dbus-libs ;;
    zypper) install xorg-x11-server-Xvfb Mesa-libGL1 Mesa-libEGL1 Mesa-dri libX11-6 libXcursor1 libXrandr2 libXi6 libxkbcommon0 \
            libxkbcommon-x11-0 fontconfig libfribidi0 libharfbuzz0 dejavu-fonts libdbus-1-3 tar gzip ;;
    pacman) pacman -Syu --noconfirm --needed -q xorg-server-xvfb mesa libx11 libxcursor libxrandr libxi libxkbcommon \
            libxkbcommon-x11 fontconfig fribidi harfbuzz ttf-dejavu dbus >/dev/null ;;
esac
export DISPLAY=:77 LIBGL_ALWAYS_SOFTWARE=1
Xvfb :77 -screen 0 1280x800x24 >/dev/null 2>&1 &
sleep 2

# start: the program to run; passes when NDM answers
start() { # what, program
    local dir="$1" exe="$2" pid
    export HOME="$dir/home"
    mkdir -p "$HOME"
    local crash="$HOME/.config/NewDownloadManager/crash.log"
    "$exe" >"$dir/ndm.log" 2>&1 &
    pid=$!
    for _ in $(seq 30); do
        if (echo >/dev/tcp/127.0.0.1/9614) 2>/dev/null && [ ! -e "$crash" ]; then
            kill "$pid" 2>/dev/null
            wait "$pid" 2>/dev/null
            sleep 1
            return 0
        fi
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    kill "$pid" 2>/dev/null
    echo "-- output:"; cat "$dir/ndm.log"
    [ -e "$crash" ] && { echo "-- crash.log:"; cat "$crash"; }
    return 1
}

failed=0
for file in "$@"; do
    file="$(realpath "$file")"
    dir="$(mktemp -d)"
    name="$(basename "$file")"
    ok=1
    case "$name" in
        *.AppImage)
            (cd "$dir" && "$file" --appimage-extract >/dev/null) && start "$dir" "$dir/squashfs-root/AppRun" || ok=0
            [ $ok = 1 ] || { echo "-- missing libraries:"; ldd "$dir/squashfs-root/usr/bin/ndm" | grep "not found"; } ;;
        *.deb | *.rpm)
            if [[ "$name" == *.deb && $pm != apt ]] || [[ "$name" == *.rpm && $pm != dnf && $pm != zypper ]]; then
                echo "-- $name: not for $ID, skipped"; continue
            fi
            install "$file" || { echo "FAIL: $name doesn't install"; failed=1; continue; }
            start "$dir" /usr/bin/ndm || ok=0 ;;
        *.tar.gz)
            case $pm in
                apt) install libgtk-3-0 ;; dnf) install gtk3 ;; zypper) install libgtk-3-0 ;; pacman) install gtk3 ;;
            esac
            tar -C "$dir" -xzf "$file" && start "$dir" "$(ls -d "$dir"/ndm-*/)ndm" || ok=0
            [ $ok = 1 ] || { echo "-- missing libraries:"; ldd "$dir"/ndm-*/ndm | grep "not found"; } ;;
        *) echo "-- $name: unknown kind, skipped"; continue ;;
    esac
    if [ $ok = 1 ]; then echo "NDM started: $name"; else echo "FAIL: NDM didn't start: $name"; failed=1; fi
done
exit $failed
