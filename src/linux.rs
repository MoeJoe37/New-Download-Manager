//! Linux stand-ins for `win.rs`: the same functions, done the freedesktop way (xdg-open,
//! ~/.config/autostart, the browsers' NativeMessagingHosts folders, fontconfig, shared-mime-info and the
//! icon theme, NetworkManager for dial-up / VPN, GNOME's proxy settings, Firefox's cache), with GTK on its
//! own thread for the tray, the drop target and the font / program choosers.
// The title bars are the window manager's: dark_title / own_dialogs have nothing to do here.
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{mpsc, Mutex, OnceLock};

pub type Rgba = (usize, usize, Vec<u8>);
pub use crate::posix::{clip_seq, decimal_sep, is_host_launch, pac_proxy, running};

// ---------- programs NDM starts ----------

/// What the AppImage's GTK hook sets for NDM's own GTK (bundled schemas, modules, pixbuf loaders, X11);
/// other programs need their own system's.
const APPIMAGE_ENV: [&str; 13] = ["GTK_DATA_PREFIX", "GTK_THEME", "GDK_BACKEND", "GSETTINGS_SCHEMA_DIR", "GI_TYPELIB_PATH", "GTK_EXE_PREFIX", "GTK_PATH",
    "GTK_IM_MODULE_FILE", "GDK_PIXBUF_MODULE_FILE", "APPDIR", "APPIMAGE", "ARGV0", "OWD"];

/// A program to run with the environment it would have outside the AppImage.
fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    clean_env(&mut c);
    c
}

/// Takes the AppImage's own settings out of a child's environment (nothing to do outside an AppImage).
pub fn clean_env(c: &mut Command) {
    let Some(dir) = std::env::var_os("APPDIR") else { return };
    for k in APPIMAGE_ENV {
        c.env_remove(k);
    }
    if let Some(dirs) = std::env::var_os("XDG_DATA_DIRS") {
        let kept: Vec<PathBuf> = std::env::split_paths(&dirs).filter(|d| !d.starts_with(&dir)).collect();
        if let Ok(v) = std::env::join_paths(kept) {
            c.env("XDG_DATA_DIRS", v);
        }
    }
}

// ---------- GTK thread ----------

/// GTK lives on one thread: started on first use, it runs GTK's main loop for good. False = no display.
fn gtk_ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // like the main window (see `prefer_x11`): on Wayland GTK can't keep the drop target on top or place it
            if prefer_x11() {
                gdk::set_allowed_backends("x11");
            }
            let ok = gtk::init().is_ok();
            let _ = tx.send(ok);
            if ok {
                gtk::main();
            }
        });
        rx.recv().unwrap_or(false)
    })
}

/// X11 (XWayland on a Wayland desktop) whenever DISPLAY is set: on Wayland a window can't hide itself
/// or stay on top. Without DISPLAY, both NDM and GTK stay on Wayland.
pub fn prefer_x11() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// Run `f` on the GTK thread without waiting. False when there is no GTK.
pub fn gtk_spawn(f: impl FnOnce() + Send + 'static) -> bool {
    if !gtk_ready() {
        return false;
    }
    glib::MainContext::default().invoke(f);
    true
}

/// Run `f` on the GTK thread and wait for its result (never from the GTK thread itself).
fn gtk_run<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    let (tx, rx) = mpsc::channel();
    let sent = gtk_spawn(move || {
        let _ = tx.send(f());
    });
    if sent { rx.recv().ok() } else { None }
}

// ---------- file types and icons ----------

/// The icon theme's picture for `icon` at 32 px (Windows' large shell icon), as RGBA. GTK thread only.
fn themed(icon: &gio::Icon) -> Option<Rgba> {
    let info = gtk::IconTheme::default()?.lookup_by_gicon(icon, 32, gtk::IconLookupFlags::FORCE_SIZE)?;
    let pb = info.load_icon().ok()?;
    let pb = if pb.has_alpha() { pb } else { pb.add_alpha(false, 0, 0, 0).ok()? };
    let (w, h, stride) = (pb.width() as usize, pb.height() as usize, pb.rowstride() as usize);
    let bytes = pb.read_pixel_bytes();
    let px = (0..h).flat_map(|y| bytes[y * stride..y * stride + w * 4].iter().copied()).collect();
    Some((w, h, px))
}

/// shared-mime-info's type for a file name with this extension ("zip" -> "application/zip").
fn content_type(ext: &str) -> glib::GString {
    gio::content_type_guess(Some(format!("file.{ext}").as_str()), &[]).0
}

/// The icon theme's icon for files with this extension (or for a folder).
pub fn shell_icon(ext: &str, folder: bool) -> Option<Rgba> {
    let ext = ext.to_string();
    gtk_run(move || {
        let icon: gio::Icon = if folder { gio::ThemedIcon::new("folder").upcast() } else { gio::content_type_get_icon(&content_type(&ext)) };
        themed(&icon)
    })
    .flatten()
}

/// shared-mime-info's description of a file extension, e.g. "Zip archive".
pub fn type_name(ext: &str) -> String {
    if ext.is_empty() {
        return String::new();
    }
    gio::content_type_get_description(&content_type(ext)).to_string()
}

/// The icon of the program's desktop entry (the browsers on the Options pages).
pub fn exe_icon(path: &Path) -> Option<Rgba> {
    let name = path.file_name()?.to_os_string();
    gtk_run(move || {
        let app = gio::AppInfo::all().into_iter().find(|a| a.executable().file_name() == Some(&name))?;
        themed(&app.icon()?)
    })
    .flatten()
}

// ---------- browser cache ----------

/// Firefox's cached copy of `url` (Windows reads the WinINet cache instead): the profiles in ~/.cache, also
/// Snap's.
pub fn url_cache(url: &str) -> Option<(PathBuf, String)> {
    let home = home();
    crate::posix::firefox_url_cache(&[home.join(".cache/mozilla/firefox"), home.join("snap/firefox/common/.cache/mozilla/firefox")], url)
}

// ---------- windows ----------

// The main window is shown and hidden through egui's viewport commands on Linux; there is no HWND (0).
pub static PUMP: AtomicBool = AtomicBool::new(false);
pub fn pump(_hwnd: isize) {}
pub fn wake(_hwnd: isize) {}
pub fn hide(_hwnd: isize) {}
pub fn visible(_hwnd: isize) -> bool {
    true
}
pub fn own_dialogs(_main: isize, _dark: bool, _retitle: bool, _light: &[String]) {}
pub fn dark_title(_hwnd: isize, _dark: bool) {}

/// A tray on the desktop: a StatusNotifierItem host on the session bus (KDE, Xfce, Cinnamon, MATE,
/// GNOME with the AppIndicator extension). Without one a tray icon would be invisible.
pub fn tray_host() -> bool {
    dbus_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", ("org.kde.StatusNotifierWatcher",).to_variant()).and_then(|v| v.child_value(0).get::<bool>()).unwrap_or(false)
}

/// A method call on the session bus (gio, so no `gdbus` program is needed); None without a bus or on error.
fn dbus_call(dest: &str, path: &str, iface: &str, method: &str, args: glib::Variant) -> Option<glib::Variant> {
    static BUS: OnceLock<Option<gio::DBusConnection>> = OnceLock::new();
    let bus = BUS.get_or_init(|| gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok()).as_ref()?;
    bus.call_sync(Some(dest), path, iface, method, Some(&args), None, gio::DBusCallFlags::NONE, 3000, None::<&gio::Cancellable>).ok()
}

/// Does the tray library load? tray-icon opens libayatana-appindicator3 (or the older libappindicator3)
/// at run time and panics, taking NDM down, when neither is installed.
pub fn indicator_lib() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        ["libayatana-appindicator3.so.1", "libappindicator3.so.1", "libayatana-appindicator3.so", "libappindicator3.so"].iter().any(|name| {
            let c = std::ffi::CString::new(*name).unwrap();
            // SAFETY: dlopen with a valid C string; the handle is kept open, as tray-icon opens the same library
            !unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY) }.is_null()
        })
    })
}

/// A sound file through PulseAudio, PipeWire or ALSA, whichever player is installed.
pub fn play_sound(path: &str) {
    if !Path::new(path).is_file() {
        return;
    }
    for (player, quiet) in [("paplay", None), ("pw-play", None), ("aplay", Some("-q"))] {
        if cmd(player).args(quiet).arg(path).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok() {
            return;
        }
    }
}

/// GNOME / GTK's dark preference.
/// The desktop's dark style, asked again on every call (the UI checks about every 2 s): the desktop
/// portal's color-scheme (1 = dark; GNOME, KDE, Xfce...), else GNOME's settings.
pub fn system_dark() -> bool {
    let portal = dbus_call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings", "Read", ("org.freedesktop.appearance", "color-scheme").to_variant()).and_then(|r| {
        // (v), the v holding a u, or on older portals another v around it
        let mut v = r.child_value(0);
        while let Some(inner) = v.as_variant() {
            v = inner;
        }
        v.get::<u32>()
    });
    match portal {
        Some(1) => true,
        Some(2) => false,
        _ => {
            let get = |key: &str| gsettings(&["get", "org.gnome.desktop.interface", key]).unwrap_or_default().to_lowercase();
            get("color-scheme").contains("dark") || get("gtk-theme").contains("dark")
        }
    }
}

// ---------- proxy ----------

fn gsettings(args: &[&str]) -> Option<String> {
    let o = cmd("gsettings").args(args).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string())
}

/// The desktop's proxy as Windows' Internet Settings would give it: (on, "http=host:port;https=host:port",
/// bypass list). GNOME's settings (also Cinnamon, MATE, Budgie), else the environment (http_proxy,
/// https_proxy, ftp_proxy, no_proxy).
pub fn system_proxy() -> (bool, String, String) {
    if is_kde() {
        return kde_proxy(&std::fs::read_to_string(config_dir().join("kioslaverc")).unwrap_or_default());
    }
    match gsettings(&["get", "org.gnome.system.proxy", "mode"]).as_deref() {
        Some("manual") => {
            let hp = |k: &str| {
                let schema = format!("org.gnome.system.proxy.{k}");
                let host = gsettings(&["get", &schema, "host"]).unwrap_or_default();
                let port = gsettings(&["get", &schema, "port"]).unwrap_or_default();
                (!host.is_empty() && port != "0").then(|| format!("{k}={host}:{port}"))
            };
            let server = [hp("http"), hp("https"), hp("ftp")].into_iter().flatten().collect::<Vec<_>>().join(";");
            // ['localhost', '127.0.0.0/8']
            let ignore = gsettings(&["get", "org.gnome.system.proxy", "ignore-hosts"]).unwrap_or_default();
            let bypass = ignore.split(',').map(|h| h.trim().trim_matches(['[', ']', '\'', ' '])).filter(|h| !h.is_empty()).collect::<Vec<_>>().join(";");
            return (!server.is_empty(), server, bypass);
        }
        Some("none") => return (false, String::new(), String::new()),
        _ => {}
    }
    let var = |k: &str| std::env::var(k).or_else(|_| std::env::var(k.to_uppercase())).unwrap_or_default();
    let host = |v: String| v.split("://").last().unwrap_or("").trim_end_matches('/').to_string();
    let (http, https, ftp) = (host(var("http_proxy")), host(var("https_proxy")), host(var("ftp_proxy")));
    let server = [("http", http), ("https", https), ("ftp", ftp)].into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";");
    (!server.is_empty(), server, var("no_proxy").replace(',', ";"))
}

fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_uppercase().contains("KDE"))
}

/// KDE's proxy (~/.config/kioslaverc) in `system_proxy`'s shape. Only ProxyType=1 (manual) counts as on;
/// a host is written "http://host port" or "host:port".
fn kde_proxy(text: &str) -> (bool, String, String) {
    let mut in_proxy = false;
    let (mut on, mut server, mut bypass) = (false, vec![], String::new());
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            in_proxy = l == "[Proxy Settings]";
            continue;
        }
        let Some((k, v)) = l.split_once('=').filter(|_| in_proxy) else { continue };
        let hp = |v: &str| {
            let v = v.trim().split("://").last().unwrap_or("").trim_end_matches('/');
            v.rsplit_once(' ').map_or(v.to_string(), |(h, p)| format!("{}:{p}", h.trim()))
        };
        match k.trim() {
            "ProxyType" => on = v.trim() == "1",
            "httpProxy" | "httpsProxy" | "ftpProxy" if !v.trim().is_empty() => server.push(format!("{}={}", k.trim().trim_end_matches("Proxy"), hp(v))),
            "NoProxyFor" => bypass = v.split(',').map(str::trim).filter(|h| !h.is_empty()).collect::<Vec<_>>().join(";"),
            _ => {}
        }
    }
    (on && !server.is_empty(), server.join(";"), bypass)
}

#[test]
fn kde_proxy_reads_kioslaverc() {
    let t = "[General]\nhttpProxy=x\n[Proxy Settings]\nProxyType=1\nhttpProxy=http://10.0.0.1 3128\nhttpsProxy=10.0.0.2:3129\nNoProxyFor=localhost,.corp\n";
    assert_eq!(kde_proxy(t), (true, "http=10.0.0.1:3128;https=10.0.0.2:3129".into(), "localhost;.corp".into()));
    assert!(!kde_proxy(&t.replace("ProxyType=1", "ProxyType=2")).0);
}

/// Writes KDE's proxy (kioslaverc, through kwriteconfig6 or 5) and tells running KDE programs.
fn set_kde_proxy(on: bool, server: &str, bypass: &str) {
    let write = |key: &str, v: &str| {
        let args = ["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key, v];
        let ok = |p: &str| cmd(p).args(args).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        let _ = ok("kwriteconfig6") || ok("kwriteconfig5");
    };
    if on {
        for kind in ["http", "https", "ftp"] {
            let hp = server.split(';').find_map(|p| p.strip_prefix(&format!("{kind}="))).or_else(|| (!server.contains('=')).then_some(server)).unwrap_or("");
            write(&format!("{kind}Proxy"), &if hp.is_empty() { String::new() } else { format!("http://{hp}") });
        }
        write("NoProxyFor", &bypass.split(';').map(str::trim).filter(|h| !h.is_empty() && *h != "<local>").collect::<Vec<_>>().join(","));
    }
    write("ProxyType", if on { "1" } else { "0" });
    let _ = cmd("dbus-send").args(["--type=signal", "/KIO/Scheduler", "org.kde.KIO.Scheduler.reparseSlaveConfiguration", "string:"]).stderr(Stdio::null()).status();
}

/// Sets the desktop's proxy (KDE's or GNOME's settings, as `system_proxy` reads them) for the browsers.
/// `server` is "host:port" or "http=host:port;https=host:port".
pub fn set_system_proxy(on: bool, server: &str, bypass: &str) {
    if is_kde() {
        return set_kde_proxy(on, server, bypass);
    }
    let set = |schema: &str, key: &str, v: &str| {
        let _ = cmd("gsettings").args(["set", schema, key, v]).stderr(Stdio::null()).status();
    };
    if !on {
        set("org.gnome.system.proxy", "mode", "none");
        return;
    }
    for kind in ["http", "https"] {
        let hp = server.split(';').find_map(|p| p.strip_prefix(&format!("{kind}="))).or_else(|| (!server.contains('=')).then_some(server)).unwrap_or("");
        let (host, port) = hp.rsplit_once(':').unwrap_or((hp, "0"));
        let schema = format!("org.gnome.system.proxy.{kind}");
        set(&schema, "host", host);
        set(&schema, "port", port);
    }
    let hosts: Vec<String> = bypass.split(';').map(str::trim).filter(|h| !h.is_empty() && *h != "<local>").map(|h| format!("'{h}'")).collect();
    set("org.gnome.system.proxy", "ignore-hosts", &format!("[{}]", hosts.join(", ")));
    set("org.gnome.system.proxy", "mode", "manual");
}

// ---------- Dial Up / VPN: NetworkManager's connections through nmcli ----------

/// Connection types that dial: VPNs, WireGuard, PPPoE and mobile broadband.
const DIAL_TYPES: [&str; 5] = ["vpn", "wireguard", "pppoe", "gsm", "cdma"];

/// Dial connections as nmcli lists them ("name:type", a ':' in a name as "\:"): (name, type).
/// None = no NetworkManager.
fn nm_conns(active: bool) -> Option<Vec<(String, String)>> {
    let mut c = cmd("nmcli");
    c.args(["-t", "-f", "NAME,TYPE", "connection", "show"]);
    if active {
        c.arg("--active");
    }
    let o = c.stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(nm_parse(&String::from_utf8_lossy(&o.stdout)))
}

fn nm_parse(text: &str) -> Vec<(String, String)> {
    // types: "vpn", "wireguard", "pppoe", "gsm", "802-11-wireless", ...
    text.lines().filter_map(|l| l.rsplit_once(':')).filter(|(_, t)| DIAL_TYPES.contains(t)).map(|(n, t)| (n.replace("\\:", ":"), t.to_string())).collect()
}

#[test]
fn nmcli_lines() {
    let text = "Home Wi-Fi:802-11-wireless\nOffice\\: VPN:vpn\nwg0:wireguard\nlo:loopback\n";
    assert_eq!(nm_parse(text), [("Office: VPN".to_string(), "vpn".to_string()), ("wg0".into(), "wireguard".into())]);
}

/// NetworkManager's dial-up / VPN connections.
pub fn ras_entries() -> Vec<String> {
    nm_conns(false).unwrap_or_default().into_iter().map(|c| c.0).collect()
}

/// Is a dial-up / VPN connection up? Without NetworkManager there is nothing to dial: yes.
pub fn ras_connected() -> bool {
    nm_conns(true).is_none_or(|c| !c.is_empty())
}

/// Passwords for this session only ("Save password" unticked), handed to nmcli when dialing.
static PASSWORDS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn nm_kind(entry: &str) -> Option<String> {
    nm_conns(false)?.into_iter().find(|c| c.0 == entry).map(|c| c.1)
}

fn nmcli(args: &[&str]) -> Result<(), String> {
    let o = cmd("nmcli").args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().trim_start_matches("Error: ").to_string()) }
}

/// User name (and, with `save`, the password) NetworkManager keeps for `entry` (Dial Up page's Apply).
pub fn ras_save(entry: &str, user: &str, pass: &str, save: bool) -> Result<(), String> {
    let kind = nm_kind(entry).ok_or("NetworkManager has no such connection")?;
    PASSWORDS.lock().unwrap().retain(|p| p.0 != entry);
    if !save && !pass.is_empty() {
        PASSWORDS.lock().unwrap().push((entry.to_string(), pass.to_string()));
    }
    let pass = if save { pass } else { "" };
    match kind.as_str() {
        "wireguard" => Ok(()), // keys, no login
        "pppoe" | "gsm" | "cdma" => nmcli(&["connection", "modify", "id", entry, &format!("{kind}.username"), user, &format!("{kind}.password"), pass]),
        _ => {
            nmcli(&["connection", "modify", "id", entry, "vpn.user-name", user])?;
            if save { nmcli(&["connection", "modify", "id", entry, "+vpn.data", "password-flags=0", "+vpn.secrets", &format!("password={pass}")]) } else { Ok(()) }
        }
    }
}

/// Brings `entry` up (synchronously), with this session's password when one was given.
pub fn ras_dial(entry: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let pass = PASSWORDS.lock().unwrap().iter().find(|p| p.0 == entry).map(|p| p.1.clone());
    let file = std::env::temp_dir().join(format!("ndm-dial-{}", std::process::id()));
    let mut args = vec!["--wait", "90", "connection", "up", "id", entry];
    if let Some(p) = &pass {
        let setting = match nm_kind(entry).as_deref() {
            Some("pppoe") => "pppoe.password",
            Some("gsm") => "gsm.password",
            Some("cdma") => "cdma.password",
            _ => "vpn.secrets.password",
        };
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&file).map_err(|e| e.to_string())?;
        writeln!(f, "{setting}:{p}").map_err(|e| e.to_string())?;
        args.extend(["passwd-file", file.to_str().unwrap_or("")]);
    }
    let r = nmcli(&args);
    let _ = std::fs::remove_file(&file);
    r
}

/// "Hang up modem when done": every dial-up / VPN connection that is up.
pub fn ras_hangup_all() {
    for (name, _) in nm_conns(true).unwrap_or_default() {
        let _ = nmcli(&["connection", "down", "id", &name]);
    }
}

// ---------- programs and the desktop ----------

/// Browsers found on PATH: (name, executable).
pub fn installed_browsers() -> Vec<(String, String)> {
    let known = [
        ("Google Chrome", "google-chrome"),
        ("Chromium", "chromium"),
        ("Chromium", "chromium-browser"),
        ("Microsoft Edge", "microsoft-edge"),
        ("Brave", "brave-browser"),
        ("Vivaldi", "vivaldi"),
        ("Opera", "opera"),
        ("Mozilla Firefox", "firefox"),
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut out: Vec<(String, String)> = vec![];
    for (name, bin) in known {
        if let Some(exe) = std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file()) {
            if !out.iter().any(|b| b.0 == name) {
                out.push((name.into(), exe.display().to_string()));
            }
        }
    }
    out
}

/// This program's path as the desktop sees it: an AppImage runs from a temporary mount, so its own file.
fn me() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok())
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

/// Start with the desktop session (~/.config/autostart).
pub fn set_autostart(on: bool) {
    let file = config_dir().join("autostart").join("ndm.desktop");
    if !on {
        let _ = std::fs::remove_file(file);
        return;
    }
    let Some(exe) = me() else { return };
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    let _ = std::fs::write(file, desktop_entry(&exe) + "X-GNOME-Autostart-enabled=true\n");
}

/// NDM's .desktop entry for `exe`. Exec quotes the path and escapes \ " ` $ inside it.
pub fn desktop_entry(exe: &Path) -> String {
    let mut path = String::new();
    for c in exe.display().to_string().chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            path.push('\\');
        }
        path.push(c);
    }
    format!("[Desktop Entry]\nType=Application\nName=New Download Manager\nGenericName=Download Manager\nExec=\"{path}\"\nIcon=ndm\nStartupWMClass=ndm\nCategories=Network;FileTransfer;\nTerminal=false\n")
}

/// Run from an AppImage: NDM's entry in the applications menu and its icon, rewritten only when they
/// changed (the AppImage moved, or a new icon).
pub fn install_desktop_entry() {
    let Some(img) = std::env::var_os("APPIMAGE").map(PathBuf::from) else { return };
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|d| d.is_absolute()).unwrap_or_else(|| home().join(".local/share"));
    let write = |file: PathBuf, bytes: &[u8]| {
        if std::fs::read(&file).ok().as_deref() != Some(bytes) {
            let _ = std::fs::create_dir_all(file.parent().unwrap());
            let _ = std::fs::write(&file, bytes);
        }
    };
    write(data.join("icons/hicolor/256x256/apps/ndm.png"), include_bytes!("../assets/ndm.png"));
    write(data.join("applications/ndm.desktop"), desktop_entry(&img).as_bytes());
}

#[test]
fn desktop_entry_quotes() {
    let e = desktop_entry(Path::new("/home/u/My Apps/NDM \"1\".AppImage"));
    assert!(e.contains("Exec=\"/home/u/My Apps/NDM \\\"1\\\".AppImage\"\n") && e.contains("StartupWMClass=ndm\n"), "{e}");
}

const HOST: &str = "com.ndm.host";

/// Writes the extension's native messaging host manifest where each browser looks for it.
pub fn register_host(_dir: &Path) {
    let Some(exe) = me() else { return };
    let manifest = |allow: serde_json::Value| {
        let mut m = serde_json::json!({ "name": HOST, "description": "New Download Manager", "path": exe, "type": "stdio" });
        m.as_object_mut().unwrap().extend(allow.as_object().unwrap().clone());
        m.to_string()
    };
    let chrome = manifest(crate::core::chrome_origins());
    let firefox = manifest(serde_json::json!({ "allowed_extensions": ["ndm@newdownloadmanager"] }));
    for (d, is_firefox) in host_dirs(&home(), &config_dir()) {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(format!("{HOST}.json")), if is_firefox { &firefox } else { &chrome });
    }
}

/// The native messaging host folders of the installed browsers (their profile folder exists), and
/// whether each is Firefox's. Snap's Firefox reads ~/.mozilla through the desktop portal.
fn host_dirs(home: &Path, cfg: &Path) -> Vec<(PathBuf, bool)> {
    let chromes = ["google-chrome", "google-chrome-beta", "google-chrome-unstable", "chromium", "microsoft-edge", "microsoft-edge-beta", "microsoft-edge-dev",
        "BraveSoftware/Brave-Browser", "vivaldi", "vivaldi-snapshot", "opera", "yandex-browser"];
    let mut d: Vec<(PathBuf, bool)> = chromes.iter().map(|b| cfg.join(b)).filter(|b| b.is_dir()).map(|b| (b.join("NativeMessagingHosts"), false)).collect();
    if home.join(".mozilla").is_dir() || home.join("snap/firefox").is_dir() {
        d.push((home.join(".mozilla/native-messaging-hosts"), true));
    }
    d
}

#[test]
fn host_dirs_cover_snap_firefox() {
    let home = std::env::temp_dir().join(format!("ndm-test-hosts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    for p in ["snap/firefox", ".config/google-chrome-beta"] {
        std::fs::create_dir_all(home.join(p)).unwrap();
    }
    let d = host_dirs(&home, &home.join(".config"));
    assert!(d.contains(&(home.join(".mozilla/native-messaging-hosts"), true)));
    assert!(d.contains(&(home.join(".config/google-chrome-beta/NativeMessagingHosts"), false)));
    assert_eq!(d.len(), 2);
    let _ = std::fs::remove_dir_all(&home);
}

/// The native messaging host (see `posix::native_host`).
pub fn native_host(title: &str) {
    crate::posix::native_host(title, me(), clean_env);
}

/// When NDM can't start at all: on the terminal, and in a desktop dialog when zenity or kdialog is there.
pub fn fatal(title: &str, text: &str) {
    eprintln!("{text}");
    let _ = cmd("zenity").args(["--error", "--title", title, "--text", text]).status().or_else(|_| cmd("kdialog").args(["--title", title, "--error", text]).status());
}

/// A second start asked for the window: `take_show` hands it to the UI.
pub fn show_main() {
    SHOW_MAIN.store(true, Relaxed);
}

/// "Exit NDM" / "Saving settings..." while the app stops its downloads and saves; it goes away with the
/// process.
pub fn exit_splash(title: &str, text: &str) {
    let (title, text) = (title.to_string(), text.to_string());
    gtk_run(move || {
        let w = gtk::Window::new(gtk::WindowType::Toplevel);
        w.set_title(&title);
        w.set_position(gtk::WindowPosition::Center);
        w.set_resizable(false);
        w.set_keep_above(true);
        let l = gtk::Label::new(Some(&text));
        l.set_margin(24);
        w.add(&l);
        w.show_all();
        while gtk::events_pending() {
            gtk::main_iteration();
        }
        std::mem::forget(w);
    });
}

/// GTK's font chooser. Returns (face, size in logical px) like Windows' Font dialog; blocks like it.
pub fn choose_font(_owner: isize, face: &str, px: f32) -> Option<(String, f32)> {
    let start = format!("{face} {}", (px * 72.0 / 96.0).round());
    gtk_run(move || {
        let d = gtk::FontChooserDialog::new(Some("Font"), None::<&gtk::Window>);
        d.set_font(&start);
        let ok = d.run() == gtk::ResponseType::Ok;
        let pick = d.font_desc().filter(|_| ok).map(|f| (f.family().map(|s| s.to_string()).unwrap_or_default(), f.size() as f32 / gtk::pango::SCALE as f32 * 96.0 / 72.0));
        // SAFETY: the dialog is ours and nothing uses it after this
        unsafe { d.destroy() };
        pick.filter(|p| !p.0.is_empty() && p.1 > 0.0)
    })
    .flatten()
}

/// The file of an installed font face, from fontconfig.
pub fn font_file(face: &str) -> Option<PathBuf> {
    let o = cmd("fc-match").args(["-f", "%{file}", face]).output().ok()?;
    let f = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    f.is_file().then_some(f)
}

/// "Open with...": GTK's program chooser for the file's type; the default program without GTK.
pub fn open_with(path: &Path) {
    let p = path.to_path_buf();
    let shown = gtk_spawn({
        let p = p.clone();
        move || {
            let file = gio::File::for_path(&p);
            let d = gtk::AppChooserDialog::new(None::<&gtk::Window>, gtk::DialogFlags::MODAL, &file);
            if d.run() == gtk::ResponseType::Ok {
                if let Some(app) = d.app_info() {
                    let _ = app.launch(&[file], None::<&gio::AppLaunchContext>);
                }
            }
            // SAFETY: the dialog is ours and nothing uses it after this
            unsafe { d.destroy() };
        }
    });
    if !shown {
        open(&p);
    }
}

/// The file manager with the file selected (FileManager1 on the session bus: Nautilus, Dolphin, Nemo,
/// Caja, Thunar), else its folder.
pub fn show_in_folder(path: &Path) {
    let uri = gio::File::for_path(path).uri().to_string();
    let shown = dbus_call("org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1", "ShowItems", (vec![uri], "").to_variant()).is_some();
    if !shown {
        open(path.parent().unwrap_or(path));
    }
}

// ---------- drop target ----------

/// Links dropped on the drop target, for the UI to take (`take_drops`).
static DROPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Repaints the UI after a drop (set by `accept_drops`).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// The UI should bring the main window back (a drop, or a double click on the drop target).
static SHOW_MAIN: AtomicBool = AtomicBool::new(false);
static DROP_SHOWN: AtomicBool = AtomicBool::new(false);
thread_local!(static DROP_WIN: std::cell::RefCell<Option<gtk::Window>> = const { std::cell::RefCell::new(None) });

pub fn take_drops() -> Vec<String> {
    std::mem::take(&mut DROPS.lock().unwrap())
}

/// Should the main window come back (see SHOW_MAIN)? Clears the request.
pub fn take_show() -> bool {
    SHOW_MAIN.swap(false, Relaxed)
}

fn wake_ui(show: bool) {
    if show {
        SHOW_MAIN.store(true, Relaxed);
    }
    if let Some(w) = WAKE.get() {
        w();
    }
}

/// Links from a drop: a link's address (text/uri-list) or every address in dropped text.
fn dropped_links(text: &str) -> Vec<String> {
    text.split_whitespace().filter(|w| ["http://", "https://", "ftp://"].iter().any(|s| w.to_ascii_lowercase().starts_with(s))).map(str::to_string).collect()
}

/// Links dropped on the main window. winit's X11 drop handler takes file:// only, so NDM gives the window an
/// XdndProxy: drag sources talk to a window of NDM's own (on its own X connection) that takes links and text.
/// `xid` 0 only sets the repaint (the drop target); on Wayland without XWayland there is no X window.
pub fn accept_drops(xid: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
    if xid != 0 {
        std::thread::spawn(move || {
            if let Err(e) = xdnd_proxy(xid as u32) {
                eprintln!("link drops on the main window: {e}");
            }
        });
    }
}

/// Started for the browser (`/tray`): eframe maps the main window after its first frame whatever NDM asks,
/// so it is asked to map minimized (WM_HINTS initial state Iconic) until NDM hides it to the tray; `false`
/// puts the normal state back for the next time it is shown. X11 only (`xid` 0 on Wayland does nothing).
pub fn keep_hidden(xid: isize, on: bool) {
    if xid != 0 {
        let _ = initial_state(xid as u32, if on { 3 } else { 1 });
    }
}

fn initial_state(xid: u32, state: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;
    let (conn, _) = x11rb::connect(None)?;
    let r = conn.get_property(false, xid, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9)?.reply()?;
    let mut hints: Vec<u32> = r.value32().map(|v| v.collect()).unwrap_or_default();
    hints.resize(9, 0);
    // flags: StateHint (2); then input, initial_state
    hints[0] |= 2;
    hints[2] = state;
    conn.change_property32(PropMode::REPLACE, xid, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, &hints)?;
    conn.flush()?;
    Ok(())
}

/// The drop formats NDM takes, most useful first.
const DROP_TYPES: [&str; 4] = ["text/uri-list", "text/x-moz-url", "UTF8_STRING", "text/plain"];

/// Text from a drop in `kind` (text/x-moz-url is UTF-16: "url\ntitle").
fn drop_text(kind: &str, data: &[u8]) -> String {
    if kind == "text/x-moz-url" {
        return String::from_utf16_lossy(&data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>());
    }
    String::from_utf8_lossy(data).into_owned()
}

/// XDND (version 5) target for the main window, run on its own thread.
fn xdnd_proxy(main: u32) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass};
    use x11rb::protocol::Event;
    use x11rb::wrapper::ConnectionExt as _;
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atom = |n: &str| -> Result<u32, Box<dyn std::error::Error>> { Ok(conn.intern_atom(false, n.as_bytes())?.reply()?.atom) };
    let [aware, proxy, enter, position, status, leave, drop, finished, selection, copy, type_list, prop] =
        ["XdndAware", "XdndProxy", "XdndEnter", "XdndPosition", "XdndStatus", "XdndLeave", "XdndDrop", "XdndFinished", "XdndSelection", "XdndActionCopy", "XdndTypeList", "NDM_DROP"].map(atom);
    let (aware, proxy, enter, position, status, leave, drop, finished, selection, copy, type_list, prop) = (aware?, proxy?, enter?, position?, status?, leave?, drop?, finished?, selection?, copy?, type_list?, prop?);
    let kinds: Vec<(u32, &str)> = DROP_TYPES.iter().map(|t| atom(t).map(|a| (a, *t))).collect::<Result<_, _>>()?;
    let me = conn.generate_id()?;
    conn.create_window(0, me, root, -10, -10, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new())?;
    conn.change_property32(PropMode::REPLACE, me, aware, AtomEnum::ATOM, &[5])?;
    conn.change_property32(PropMode::REPLACE, me, proxy, AtomEnum::WINDOW, &[me])?;
    conn.change_property32(PropMode::REPLACE, main, proxy, AtomEnum::WINDOW, &[me])?;
    conn.flush()?;
    let send = |to: u32, kind: u32, data: [u32; 5]| -> Result<(), Box<dyn std::error::Error>> {
        conn.send_event(false, to, EventMask::NO_EVENT, ClientMessageEvent::new(32, to, kind, data))?;
        conn.flush()?;
        Ok(())
    };
    // the drag in progress: (its source, the format NDM asks for)
    let mut drag: Option<(u32, Option<(u32, &str)>)> = None;
    loop {
        match conn.wait_for_event()? {
            Event::ClientMessage(e) if e.format == 32 => {
                let d = e.data.as_data32();
                let source = d[0];
                if e.type_ == enter {
                    // up to three formats in the message, or all of them in XdndTypeList
                    let offered: Vec<u32> = if d[1] & 1 == 1 {
                        conn.get_property(false, source, type_list, AtomEnum::ATOM, 0, 1024)?.reply()?.value32().map(|v| v.collect()).unwrap_or_default()
                    } else {
                        d[2..5].iter().copied().filter(|a| *a != 0).collect()
                    };
                    drag = Some((source, kinds.iter().find(|(a, _)| offered.contains(a)).copied()));
                } else if e.type_ == position {
                    let ok = drag.as_ref().is_some_and(|(s, k)| *s == source && k.is_some());
                    // accepted (bit 0), keep sending positions (bit 1); the target is the window the source sees
                    send(source, status, [main, if ok { 3 } else { 2 }, 0, 0, if ok { copy } else { 0 }])?;
                } else if e.type_ == leave {
                    drag = None;
                } else if e.type_ == drop {
                    match drag.as_ref().and_then(|(s, k)| k.filter(|_| *s == source)) {
                        Some((kind, _)) => {
                            conn.convert_selection(me, selection, kind, prop, d[2])?;
                            conn.flush()?;
                        }
                        None => {
                            send(source, finished, [main, 0, 0, 0, 0])?;
                            drag = None;
                        }
                    }
                }
            }
            Event::SelectionNotify(e) if e.requestor == me => {
                let Some((source, Some((_, kind)))) = drag.take() else { continue };
                let mut got = false;
                if e.property != u32::from(AtomEnum::NONE) {
                    let r = conn.get_property(true, me, prop, AtomEnum::ANY, 0, 1 << 20)?.reply()?;
                    let links = dropped_links(&drop_text(kind, &r.value));
                    if !links.is_empty() {
                        DROPS.lock().unwrap().extend(links);
                        got = true;
                        wake_ui(false);
                    }
                }
                send(source, finished, [main, got as u32, if got { copy } else { 0 }, 0, 0])?;
            }
            _ => {}
        }
    }
}

/// Show or hide the drop target: a floating, always-on-top picture links can be dragged onto, bottom
/// right of the screen. Drag it to move it, double click for the main window, right click to close it.
pub fn drop_target(show: bool, (iw, ih, px): &Rgba) {
    let (w, h, px) = (*iw as i32, *ih as i32, px.clone());
    gtk_spawn(move || {
        DROP_WIN.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.is_none() {
                if !show {
                    return;
                }
                *cell = Some(make_drop_target(w, h, px));
            }
            let win = cell.as_ref().unwrap();
            if show { win.show_all() } else { win.hide() }
            DROP_SHOWN.store(show, Relaxed);
        })
    });
}

fn make_drop_target(w: i32, h: i32, px: Vec<u8>) -> gtk::Window {
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("Drop target");
    win.set_decorated(false); win.set_keep_above(true); win.set_skip_taskbar_hint(true); win.set_skip_pager_hint(true);
    win.set_type_hint(gdk::WindowTypeHint::Utility);
    win.set_accept_focus(false);
    win.set_default_size(w, h);
    win.set_resizable(false);
    // see-through where the picture is, if the screen composites
    if let Some(v) = WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
        win.set_visual(Some(&v));
    }
    win.set_app_paintable(true);
    let pb = gtk::gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(px), gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, w, h, w * 4);
    win.connect_draw(move |_, cr| {
        cr.set_operator(gtk::cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        cr.set_operator(gtk::cairo::Operator::Over);
        cr.set_source_pixbuf(&pb, 0.0, 0.0);
        let _ = cr.paint();
        glib::Propagation::Stop
    });
    if let Some(area) = gdk::Display::default().and_then(|d| d.primary_monitor().or_else(|| d.monitor(0))).map(|m| m.workarea()) {
        win.move_(area.x() + area.width() - w - 40, area.y() + area.height() - h - 120);
    }
    win.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
    win.connect_button_press_event(|win, e| {
        match (e.button(), e.event_type()) {
            (1, gdk::EventType::DoubleButtonPress) => wake_ui(true),
            (1, gdk::EventType::ButtonPress) => {
                let (x, y) = e.root();
                win.begin_move_drag(1, x as i32, y as i32, e.time());
            }
            (3, gdk::EventType::ButtonPress) => {
                // close it; "Show drop target" brings it back
                win.hide();
                DROP_SHOWN.store(false, Relaxed);
                wake_ui(false);
            }
            _ => {}
        }
        glib::Propagation::Stop
    });
    let targets = ["text/uri-list", "text/x-moz-url", "UTF8_STRING", "text/plain"].map(|t| gtk::TargetEntry::new(t, gtk::TargetFlags::OTHER_APP, 0));
    win.drag_dest_set(gtk::DestDefaults::ALL, &targets, gdk::DragAction::COPY | gdk::DragAction::LINK | gdk::DragAction::MOVE);
    win.connect_drag_data_received(|_, _, _, _, data, _, _| {
        let mut text = data.uris().join("\n");
        if text.is_empty() {
            // text/x-moz-url is UTF-16: "url\ntitle"
            let raw = data.data();
            text = data.text().map(|t| t.to_string()).unwrap_or_else(|| String::from_utf16_lossy(&raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>()));
        }
        let links = dropped_links(&text);
        if !links.is_empty() {
            DROPS.lock().unwrap().extend(links);
            wake_ui(true);
        }
    });
    win
}

pub fn drop_target_visible() -> bool {
    DROP_SHOWN.load(Relaxed)
}

/// Opens a file, folder or address with the desktop's default program.
pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let _ = cmd("xdg-open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// "Turn off computer when done": shut down in a minute (`shutdown -c` cancels it), suspend or hibernate.
pub fn power_off(kind: u8, _force: bool) {
    let _ = match kind {
        1 => cmd("systemctl").arg("suspend").spawn(),
        2 => cmd("systemctl").arg("hibernate").spawn(),
        _ => cmd("shutdown").args(["-h", "+1", "Downloads complete. The computer will shut down in a minute."]).spawn(),
    };
}

/// The networks a download can be spread over: up, not loopback, with an address and a default route.
pub fn nics() -> Vec<crate::net::Nic> {
    let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let route6 = std::fs::read_to_string("/proc/net/ipv6_route").unwrap_or_default();
    crate::posix::ifaddrs(&default_route_devices(&route, &route6))
}

/// The devices with a default route in /proc/net/route (Destination and Mask 00000000) and
/// /proc/net/ipv6_route (all-zero destination with prefix length 00).
fn default_route_devices(route: &str, route6: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut add = |d: &str| {
        if d != "lo" && !out.iter().any(|o| o == d) {
            out.push(d.to_string());
        }
    };
    for l in route.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() > 7 && f[1] == "00000000" && f[7] == "00000000" {
            add(f[0]);
        }
    }
    for l in route6.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() == 10 && f[0].bytes().all(|b| b == b'0') && f[1] == "00" {
            add(f[9]);
        }
    }
    out
}

#[test]
fn default_routes() {
    let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        wlp2s0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
        wlp2s0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
        docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";
    let route6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 enx0a1b2c3d4e5f\n\
        00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n\
        fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 wlp2s0\n";
    assert_eq!(default_route_devices(route, route6), ["wlp2s0", "enx0a1b2c3d4e5f"]);
}

// ---------- packages ----------

/// The package format that installed `exe` ("deb" or "rpm"), None when no package owns it (the
/// AppImage, the .tar.gz).
pub fn package_owner(exe: &Path) -> Option<&'static str> {
    let exe = exe.canonicalize().ok()?;
    let owns = |p: &str, args: &[&str]| on_path(p) && cmd(p).args(args).arg(&exe).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if owns("dpkg-query", &["-S"]) {
        Some("deb")
    } else if owns("rpm", &["-qf"]) {
        Some("rpm")
    } else {
        None
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// Installs a downloaded .deb or .rpm with the system's package manager, as root: pkexec has the
/// desktop ask for the password. apt, dnf and zypper also bring in what it depends on.
pub fn install_package(file: &Path) -> Result<(), String> {
    let f = file.display().to_string();
    let args: Vec<&str> = if f.ends_with(".deb") {
        if on_path("apt-get") { vec!["apt-get", "install", "-y", "--allow-downgrades", &f] } else { vec!["dpkg", "-i", &f] }
    } else if on_path("dnf") {
        vec!["dnf", "install", "-y", &f]
    } else if on_path("zypper") {
        vec!["zypper", "-n", "--no-gpg-checks", "install", "--allow-unsigned-rpm", &f]
    } else {
        vec!["rpm", "-U", &f]
    };
    if !on_path("pkexec") {
        return Err(format!("pkexec is missing: install the update by hand with {}", args.join(" ")));
    }
    let o = cmd("pkexec").args(&args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    match o.status.code() {
        Some(0) => Ok(()),
        // pkexec: the password dialog was closed, or the user may not install software
        Some(126 | 127) => Err("The update wasn't installed: the password wasn't given.".into()),
        _ => Err(String::from_utf8_lossy(&o.stderr).lines().rfind(|l| !l.trim().is_empty()).unwrap_or("the package manager failed").to_string()),
    }
}
