#!/usr/bin/env bash
# Builds NDM-x86_64.AppImage on Linux (or in WSL):  bash tools/appimage.sh
# Needs: Rust, build-essential, pkg-config, libgtk-3-dev, libxdo-dev, libayatana-appindicator3-dev, curl, file.
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
Comment=Fast segmented downloads, browser capture, video streams and site grabbing
Exec=ndm
Icon=ndm
Categories=Network;FileTransfer;
Terminal=false
EOF

# linuxdeploy copies the libraries ndm links (GTK...) into the AppDir and packs it
tool="$work/linuxdeploy-x86_64.AppImage"
[ -x "$tool" ] || { curl -sSL -o "$tool" https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage; chmod +x "$tool"; }
cd "$work"
export APPIMAGE_EXTRACT_AND_RUN=1 # no FUSE needed (WSL, containers)
OUTPUT="NDM-x86_64.AppImage" "$tool" --appdir "$app" --executable "$app/usr/bin/ndm" \
    --desktop-file "$app/usr/share/applications/ndm.desktop" --icon-file "$app/usr/share/icons/hicolor/256x256/apps/ndm.png" --output appimage
mkdir -p "$src/installer/out"
cp NDM-x86_64.AppImage "$src/installer/out/"
echo "built $src/installer/out/NDM-x86_64.AppImage"
