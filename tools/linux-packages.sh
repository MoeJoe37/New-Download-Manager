#!/usr/bin/env bash
# Builds NDM's Linux packages beside the AppImage:  bash tools/linux-packages.sh [path/to/ndm]
#   installer/out/ndm_<version>_amd64.deb        Debian, Ubuntu, Mint
#   installer/out/ndm-<version>-1.x86_64.rpm     Fedora, RHEL/Alma/Rocky, openSUSE
#   installer/out/ndm-<version>-x86_64.tar.gz    any distribution: unpack and run ./ndm
# They use the system's GTK; libxdo (not in RHEL's own repositories) comes with them in lib/.
# Needs: dpkg-deb and dpkg-shlibdeps (dpkg-dev), rpmbuild (rpm), patchelf. Without a binary given,
# runs cargo build --release. Releases build on Ubuntu 20.04, so the packages run on glibc 2.31 and later.
set -euo pipefail
src="$(cd "$(dirname "$0")/.." && pwd)"
cd "$src"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
bin="${1:-}"
if [ -z "$bin" ]; then
    cargo build --release
    bin="$src/target/release/ndm"
fi
out="$src/installer/out"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

xdo="$(ldd "$bin" | awk '/libxdo\.so/{print $3; exit}')"
[ -f "$xdo" ] || { echo "libxdo not found (install libxdo-dev)"; exit 1; }

# NDM's folder: the program, its help, languages and browser extension beside it, libxdo in lib/
app() { # dir
    mkdir -p "$1/lib"
    install -m 755 "$bin" "$1/ndm"
    cp -r help Languages extension LICENSE "$1/"
    install -m 644 "$xdo" "$1/lib/"
    patchelf --set-rpath '$ORIGIN/lib' "$1/ndm"
}
desktop() { # Exec
    cat <<EOF
[Desktop Entry]
Type=Application
Name=New Download Manager
GenericName=Download Manager
Comment=Segmented downloads, browser capture, video streams and site grabbing
Exec=$1
Icon=ndm
StartupWMClass=ndm
Categories=Network;FileTransfer;
Terminal=false
EOF
}
# the installed tree shared by the .deb and the .rpm
root="$work/root"
app "$root/usr/lib/ndm"
mkdir -p "$root/usr/bin" "$root/usr/share/applications" "$root/usr/share/icons/hicolor/256x256/apps" "$root/usr/share/doc/ndm"
ln -s ../lib/ndm/ndm "$root/usr/bin/ndm"
desktop ndm > "$root/usr/share/applications/ndm.desktop"
install -m 644 assets/ndm.png "$root/usr/share/icons/hicolor/256x256/apps/ndm.png"
install -m 644 LICENSE "$root/usr/share/doc/ndm/copyright"

# .deb: dependencies from the libraries ndm links (libxdo is bundled, the tray library is opened at run time)
mkdir -p "$work/shlibs/debian"
printf 'Source: ndm\n\nPackage: ndm\nArchitecture: amd64\n' > "$work/shlibs/debian/control"
depends="$(cd "$work/shlibs" && dpkg-shlibdeps -O -l"$root/usr/lib/ndm/lib" -xlibxdo3 --ignore-missing-info "$root/usr/lib/ndm/ndm" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')"
[ -n "$depends" ] || { echo "dpkg-shlibdeps found no dependencies"; exit 1; }
# newer Debian and Ubuntu renamed libgdk-pixbuf2.0-0
depends="$(echo "$depends" | sed 's/libgdk-pixbuf2\.0-0\( ([^)]*)\)\?/libgdk-pixbuf-2.0-0 | &/')"
deb="$work/deb"
cp -a "$root" "$deb"
mkdir -p "$deb/DEBIAN"
cat > "$deb/DEBIAN/control" <<EOF
Package: ndm
Version: $version
Section: web
Priority: optional
Architecture: amd64
Maintainer: New Download Manager <https://github.com/MoeJoe37/New-Download-Manager>
Installed-Size: $(du -sk "$deb/usr" | cut -f1)
Depends: $depends
Recommends: libayatana-appindicator3-1 | libappindicator3-1
Homepage: https://github.com/MoeJoe37/New-Download-Manager
Description: download manager with segmented downloads and browser capture
 New Download Manager downloads each file over several connections, resumes
 broken downloads, takes over downloads from the browser, saves video streams
 and downloads whole web sites. The tray icon needs libayatana-appindicator3.
EOF
dpkg-deb --root-owner-group --build "$deb" "$out/ndm_${version}_amd64.deb" >/dev/null

# .rpm: rpm works out the libraries ndm needs by their names, which every distribution provides
rpmtop="$work/rpm"
mkdir -p "$rpmtop"/{BUILD,RPMS,SPECS}
cat > "$rpmtop/SPECS/ndm.spec" <<EOF
Name: ndm
Version: $version
Release: 1
Summary: Download manager with segmented downloads and browser capture
License: see /usr/share/doc/ndm/copyright
URL: https://github.com/MoeJoe37/New-Download-Manager
# libxdo comes with it; it isn't in RHEL's own repositories
%global __requires_exclude ^libxdo\\\\.so.*$
%global __provides_exclude_from ^/usr/lib/ndm/.*$
%global debug_package %{nil}
%global __os_install_post %{nil}
%define _build_id_links none

%description
New Download Manager downloads each file over several connections, resumes broken
downloads, takes over downloads from the browser, saves video streams and downloads
whole web sites. The tray icon needs libayatana-appindicator3.

%install
cp -a $root/. %{buildroot}/

%files
/usr/lib/ndm
/usr/bin/ndm
/usr/share/applications/ndm.desktop
/usr/share/icons/hicolor/256x256/apps/ndm.png
%doc /usr/share/doc/ndm/copyright
EOF
rpmbuild --quiet --define "_topdir $rpmtop" -bb "$rpmtop/SPECS/ndm.spec"
cp "$rpmtop"/RPMS/x86_64/ndm-"$version"-1.x86_64.rpm "$out/"

# .tar.gz: the same folder to run in place, with its menu entry and icon to copy if wanted
tdir="$work/ndm-$version"
app "$tdir"
desktop ndm > "$tdir/ndm.desktop"
install -m 644 assets/ndm.png "$tdir/ndm.png"
tar -C "$work" -czf "$out/ndm-$version-x86_64.tar.gz" "ndm-$version"

ls -l "$out"/ndm_"$version"_amd64.deb "$out"/ndm-"$version"-1.x86_64.rpm "$out"/ndm-"$version"-x86_64.tar.gz
