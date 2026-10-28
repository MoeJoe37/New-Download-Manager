#!/usr/bin/env bash
# Builds NDM-x86_64.AppImage on Linux (or in WSL):  bash tools/appimage.sh
# Needs: Rust, build-essential, pkg-config, libgtk-3-dev, libxdo-dev, libayatana-appindicator3-dev,
# librsvg2-common, patchelf, curl, file.
# Releases are built on Ubuntu 20.04 (glibc 2.31, see .github/workflows/release.yml), so the AppImage
# starts on that and every newer distribution; a build on a newer system only runs on newer ones.
# The source is copied to ~/.cache/ndm-build first: building on /mnt/c is slow and would share target/.
set -euo pipefail
src="$(cd "$(dirname "$0")/.." && pwd)"
work="$HOME/.cache/ndm-build"
mkdir -p "$work"
rsync -a --delete --exclude target --exclude installer/out --exclude .git "$src/" "$work/src/" 2>/dev/null ||
    { rm -rf "$work/src"; mkdir -p "$work/src"; (cd "$src" && tar --exclude=./target --exclude=./installer/out --exclude=./.git -cf - .) | tar -xf - -C "$work/src"; }
cd "$work/src"
cargo build --release

# AppDir: the program with help, languages and the browser extension next to it (NDM looks beside its exe)
app="$work/AppDir"
rm -rf "$app"
mkdir -p "$app/usr/bin" "$app/usr/share/applications" "$app/usr/share/icons/hicolor/256x256/apps"
cp target/release/ndm "$app/usr/bin/"
cp -r help Languages extension LICENSE "$app/usr/bin/"
cp assets/ndm.png "$app/usr/share/icons/hicolor/256x256/apps/ndm.png"
cat > "$app/usr/share/applications/ndm.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=New Download Manager
GenericName=Download Manager
Comment=Segmented downloads, browser capture, video streams and site grabbing
Exec=ndm
Icon=ndm
StartupWMClass=ndm
Categories=Network;FileTransfer;
Terminal=false
EOF

fetch() { # url file
    [ -s "$work/$2" ] || { curl -sSfL -o "$work/$2" "$1"; chmod +x "$work/$2"; }
}
fetch https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage linuxdeploy-x86_64.AppImage
fetch https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh linuxdeploy-plugin-gtk.sh
fetch https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage appimagetool-x86_64.AppImage
export APPIMAGE_EXTRACT_AND_RUN=1 # no FUSE needed (WSL, containers)
export PATH="$work:$PATH"         # linuxdeploy finds its gtk plugin on PATH

# The tray library is opened at run time (dlopen), so linuxdeploy can't see it: name it, and its
# dbusmenu / indicator libraries come along. Distributions without it then still get a tray icon.
tray="$(ldconfig -p | awk '/libayatana-appindicator3\.so\.1 /{print $NF; exit}')"
[ -n "$tray" ] || { echo "libayatana-appindicator3.so.1 not found"; exit 1; }

# linuxdeploy copies the libraries ndm links into the AppDir; the gtk plugin adds GTK's pixbuf loaders,
# modules and schemas and sets them up at start. appimagetool packs it with its static runtime, which
# needs no libfuse2 (gone from Ubuntu 22.04 and later).
cd "$work"
# libxkbcommon stays the system's: winit opens the system's libxkbcommon-x11 at run time, which crashes
# against an older bundled libxkbcommon. Every desktop has it.
DEPLOY_GTK_VERSION=3 linuxdeploy-x86_64.AppImage --appdir "$app" --executable "$app/usr/bin/ndm" --library "$tray" \
    --exclude-library 'libxkbcommon.so*' --desktop-file "$app/usr/share/applications/ndm.desktop" --icon-file "$app/usr/share/icons/hicolor/256x256/apps/ndm.png" --plugin gtk
rm -f "$app"/usr/lib/libxkbcommon.so* # in case a plugin copied it anyway
ARCH=x86_64 appimagetool-x86_64.AppImage --no-appstream "$app" NDM-x86_64.AppImage
mkdir -p "$src/installer/out"
cp NDM-x86_64.AppImage "$src/installer/out/"
echo "built $src/installer/out/NDM-x86_64.AppImage"
