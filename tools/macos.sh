#!/usr/bin/env bash
# Builds NDM.app and installer/out/NDM-<version>-macOS.dmg on a Mac:  bash tools/macos.sh
# One app for Apple Silicon and Intel Macs (both builds joined with lipo), for macOS 11 and later.
# Signed ad hoc (Apple Silicon runs signed code only), so the first start needs right click -> Open
# (README, Installing); with MACOS_SIGN_IDENTITY (a "Developer ID Application" certificate in the
# keychain) signed with it for the hardened runtime, and with APPLE_ID, APPLE_TEAM_ID and
# APPLE_APP_PASSWORD as well, notarized by Apple and stapled, so it opens like any app (release.yml).
# Needs: Rust (rustup) and Xcode's command line tools.
set -euo pipefail
src="$(cd "$(dirname "$0")/.." && pwd)"
cd "$src"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
export MACOSX_DEPLOYMENT_TARGET=11.0
targets=(aarch64-apple-darwin x86_64-apple-darwin)
rustup target add "${targets[@]}" >/dev/null
for t in "${targets[@]}"; do
    cargo build --release --target "$t"
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
app="$work/dmg/NDM.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/ndm" target/aarch64-apple-darwin/release/ndm target/x86_64-apple-darwin/release/ndm
# NDM looks for its help, languages and browser extension in Contents/Resources (i18n::beside_exe)
cp -R help Languages extension LICENSE "$app/Contents/Resources/"
cp assets/ndm.icns "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>NDM</string>
  <key>CFBundleDisplayName</key><string>New Download Manager</string>
  <key>CFBundleIdentifier</key><string>com.ndm.newdownloadmanager</string>
  <key>CFBundleExecutable</key><string>ndm</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>New Download Manager, AGPL-3.0</string>
</dict>
</plist>
EOF
identity="${MACOS_SIGN_IDENTITY:--}"
if [ "$identity" = "-" ]; then
    codesign --force --deep --sign - "$app"
else
    codesign --force --deep --options runtime --timestamp --sign "$identity" "$app"
fi
codesign --verify --deep --strict "$app"

# the disk image: the app and a link to Applications to drag it onto
ln -s /Applications "$work/dmg/Applications"
mkdir -p installer/out
dmg="installer/out/NDM-$version-macOS.dmg"
rm -f "$dmg"
hdiutil create -volname "New Download Manager" -srcfolder "$work/dmg" -ov -format UDZO "$dmg" >/dev/null
if [ "$identity" != "-" ]; then
    codesign --force --timestamp --sign "$identity" "$dmg"
    if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
        # Apple checks the disk image and the app in it; the ticket is then stapled to the image so it
        # opens without a network check
        xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait
        xcrun stapler staple "$dmg"
        xcrun stapler validate "$dmg"
    fi
fi
lipo -info "$app/Contents/MacOS/ndm"
ls -l "$dmg"
