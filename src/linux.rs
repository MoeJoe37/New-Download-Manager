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

// ---------- GTK thread ----------

/// GTK lives on one thread: started on first use, it runs GTK's main loop for good. False = no display.
fn gtk_ready() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let ok = gtk::init().is_ok();
            let _ = tx.send(ok);
            if ok {
                gtk::main();
            }
        });
        rx.recv().unwrap_or(false)
    })
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

/// Firefox's cached copy of `url`, copied out to a temporary file: (file, its Last-Modified). Windows reads
/// the WinINet cache instead.
// ponytail: reads every cache entry per lookup; keep an index if big caches make it slow
pub fn url_cache(url: &str) -> Option<(PathBuf, String)> {
    let home = home();
    let roots = [home.join(".cache/mozilla/firefox"), home.join("snap/firefox/common/.cache/mozilla/firefox")];
    let profiles: Vec<PathBuf> = roots.iter().filter_map(|r| std::fs::read_dir(r).ok()).flatten().flatten().map(|p| p.path()).collect();
    profiles.iter().find_map(|p| firefox_cache(&p.join("cache2/entries"), url))
}

fn firefox_cache(entries: &Path, url: &str) -> Option<(PathBuf, String)> {
    for e in std::fs::read_dir(entries).ok()?.flatten() {
        let Ok(data) = std::fs::read(e.path()) else { continue };
        let Some((body, key, head)) = cache2_entry(&data) else { continue };
        // the key is "[origin attributes,]a,:<url>"; with a Content-Encoding the stored body isn't the file
        let key_url = key.find(":http").or_else(|| key.find(":ftp")).map(|i| &key[i + 1..]);
        if key_url != Some(url) || header(&head, "content-encoding").is_some_and(|v| !v.eq_ignore_ascii_case("identity")) {
            continue;
        }
        let modified = header(&head, "last-modified")?;
        let out = std::env::temp_dir().join(format!("ndm-cache-{}", e.file_name().to_string_lossy()));
        std::fs::write(&out, body).ok()?;
        return Some((out, modified));
    }
    None
}

/// One cache2 file: (body, key, response head). The last 4 bytes say where the metadata starts: a 4-byte
/// hash and 2 bytes per 256 KiB chunk, eight 32-bit fields, the key and a 0, then "name\0value\0" pairs.
fn cache2_entry(d: &[u8]) -> Option<(&[u8], String, String)> {
    let be = |at: usize| d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let meta = be(d.len().checked_sub(4)?)?;
    let hdr = meta + 4 + 2 * meta.div_ceil(262144);
    let key_len = be(hdr + 24)?;
    let key = String::from_utf8_lossy(d.get(hdr + 32..hdr + 32 + key_len)?).into_owned();
    let elems = d.get(hdr + 33 + key_len..d.len() - 4)?;
    let mut it = elems.split(|b| *b == 0);
    let mut head = String::new();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k == b"response-head" {
            head = String::from_utf8_lossy(v).into_owned();
        }
    }
    Some((&d[..meta], key, head))
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
}

#[test]
fn firefox_cache_entry() {
    let dir = std::env::temp_dir().join(format!("ndm-test-ff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (body, key) = (b"cached copy".to_vec(), b"O^partitionKey=x,a,:http://h/f.txt".to_vec());
    let mut f = body.clone();
    f.extend([0u8; 6]); // hash + one chunk's hash
    for v in [3u32, 1, 0, 0, 0, 0, key.len() as u32, 0] {
        f.extend(v.to_be_bytes());
    }
    f.extend(&key);
    f.push(0);
    f.extend(b"response-head\0HTTP/1.1 200 OK\r\nLast-Modified: Tue, 01 Mar 2022 10:00:00 GMT\r\n\0");
    f.extend((body.len() as u32).to_be_bytes());
    std::fs::write(dir.join("ABC"), f).unwrap();
    let (file, since) = firefox_cache(&dir, "http://h/f.txt").expect("found");
    assert_eq!((std::fs::read(file).unwrap(), since.as_str()), (body, "Tue, 01 Mar 2022 10:00:00 GMT"));
    assert!(firefox_cache(&dir, "http://h/other").is_none());
    let _ = std::fs::remove_dir_all(&dir);
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

/// X11 / Wayland keep no clipboard change counter: a hash of the clipboard text stands in for it.
pub fn clip_seq() -> u32 {
    use std::hash::{Hash, Hasher};
    thread_local!(static CLIP: std::cell::RefCell<Option<arboard::Clipboard>> = std::cell::RefCell::new(arboard::Clipboard::new().ok()));
    let text = CLIP.with(|c| c.borrow_mut().as_mut().and_then(|c| c.get_text().ok())).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish() as u32
}

/// A tray on the desktop: a StatusNotifierItem host on the session bus (KDE, Xfce, Cinnamon, MATE,
/// GNOME with the AppIndicator extension). Without one a tray icon would be invisible.
pub fn tray_host() -> bool {
    Command::new("gdbus")
        .args(["call", "--session", "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.NameHasOwner", "org.kde.StatusNotifierWatcher"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("true"))
}

/// A sound file through PulseAudio / PipeWire, else ALSA.
pub fn play_sound(path: &str) {
    if Path::new(path).is_file() && Command::new("paplay").arg(path).spawn().is_err() {
        let _ = Command::new("aplay").arg("-q").arg(path).spawn();
    }
}

/// The locale's decimal separator (LC_NUMERIC), "." if unknown.
pub fn decimal_sep() -> &'static str {
    static SEP: OnceLock<String> = OnceLock::new();
    SEP.get_or_init(|| {
        let o = Command::new("locale").arg("decimal_point").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if o.is_empty() { ".".into() } else { o }
    })
}

/// GNOME / GTK's dark preference.
pub fn system_dark() -> bool {
    static DARK: OnceLock<bool> = OnceLock::new();
    *DARK.get_or_init(|| {
        let get = |key: &str| Command::new("gsettings").args(["get", "org.gnome.desktop.interface", key]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
        get("color-scheme").contains("dark") || get("gtk-theme").contains("dark")
    })
}

// ---------- proxy ----------

fn gsettings(args: &[&str]) -> Option<String> {
    let o = Command::new("gsettings").args(args).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string())
}

/// The desktop's proxy as Windows' Internet Settings would give it: (on, "http=host:port;https=host:port",
/// bypass list). GNOME's settings (also Cinnamon, MATE, Budgie), else the environment (http_proxy,
/// https_proxy, no_proxy).
pub fn system_proxy() -> (bool, String, String) {
    match gsettings(&["get", "org.gnome.system.proxy", "mode"]).as_deref() {
        Some("manual") => {
            let hp = |k: &str| {
                let schema = format!("org.gnome.system.proxy.{k}");
                let host = gsettings(&["get", &schema, "host"]).unwrap_or_default();
                let port = gsettings(&["get", &schema, "port"]).unwrap_or_default();
                (!host.is_empty() && port != "0").then(|| format!("{k}={host}:{port}"))
            };
            let server = [hp("http"), hp("https")].into_iter().flatten().collect::<Vec<_>>().join(";");
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
    let (http, https) = (host(var("http_proxy")), host(var("https_proxy")));
    let server = [("http", http), ("https", https)].into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";");
    (!server.is_empty(), server, var("no_proxy").replace(',', ";"))
}

/// Sets the desktop's proxy (GNOME's settings, as `system_proxy` reads them) for the browsers.
/// `server` is "host:port" or "http=host:port;https=host:port".
// ponytail: GNOME's settings only; KDE keeps its own in kioslaverc
pub fn set_system_proxy(on: bool, server: &str, bypass: &str) {
    let set = |schema: &str, key: &str, v: &str| {
        let _ = Command::new("gsettings").args(["set", schema, key, v]).stderr(Stdio::null()).status();
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

/// The PAC helper functions browsers provide (Netscape's list); dnsResolve and myIpAddress come from Rust.
const PAC_UTILS: &str = r#"
function dnsDomainIs(h, d) { return h.length >= d.length && h.substring(h.length - d.length) == d; }
function dnsDomainLevels(h) { return h.split('.').length - 1; }
function isPlainHostName(h) { return h.indexOf('.') < 0; }
function isResolvable(h) { return dnsResolve(h) != null; }
function localHostOrDomainIs(h, hd) { return h == hd || hd.lastIndexOf(h + '.', 0) == 0; }
function _ip(a) { var b = a.split('.'); return ((b[0] << 24) | (b[1] << 16) | (b[2] << 8) | (b[3] | 0)) >>> 0; }
function isInNet(h, pat, mask) {
  if (!/^\d+\.\d+\.\d+\.\d+$/.test(h)) { h = dnsResolve(h); if (h == null) return false; }
  var m = _ip(mask); return ((_ip(h) & m) >>> 0) == ((_ip(pat) & m) >>> 0);
}
function shExpMatch(s, p) {
  return new RegExp('^' + p.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.') + '$').test(s);
}
function _gmt(a) { return a.length > 0 && a[a.length - 1] == 'GMT'; }
function weekdayRange() {
  var days = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'], a = arguments, g = _gmt(a), n = g ? a.length - 1 : a.length, d = new Date();
  var t = g ? d.getUTCDay() : d.getDay(), d1 = days.indexOf(a[0]), d2 = n > 1 ? days.indexOf(a[1]) : d1;
  return d1 <= d2 ? t >= d1 && t <= d2 : t >= d1 || t <= d2;
}
function timeRange() {
  var a = arguments, g = _gmt(a), n = g ? a.length - 1 : a.length, d = new Date();
  var s = (g ? d.getUTCHours() : d.getHours()) * 3600 + (g ? d.getUTCMinutes() : d.getMinutes()) * 60 + (g ? d.getUTCSeconds() : d.getSeconds());
  var v = [].slice.call(a, 0, n).map(Number), lo, hi;
  if (n == 1) { lo = v[0] * 3600; hi = lo + 3599; }
  else if (n == 2) { lo = v[0] * 3600; hi = v[1] == v[0] ? lo + 3599 : v[1] * 3600 - 1; }
  else if (n == 4) { lo = v[0] * 3600 + v[1] * 60; hi = v[2] * 3600 + v[3] * 60; }
  else if (n == 6) { lo = v[0] * 3600 + v[1] * 60 + v[2]; hi = v[3] * 3600 + v[4] * 60 + v[5]; }
  else return false;
  return lo <= hi ? s >= lo && s <= hi : s >= lo || s <= hi;
}
function dateRange() {
  // each argument is a day (1-31), a month name or a year (> 31); compared on the fields given
  var mon = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
  var a = arguments, g = _gmt(a), n = g ? a.length - 1 : a.length, d = new Date();
  var now = [g ? d.getUTCFullYear() : d.getFullYear(), g ? d.getUTCMonth() : d.getMonth(), g ? d.getUTCDate() : d.getDate()];
  var ps = [].slice.call(a, 0, n).map(function (x) { var m = mon.indexOf(x); return m >= 0 ? [1, m] : x > 31 ? [0, +x] : [2, +x]; });
  var half = n > 1 ? n / 2 : 1, from = ps.slice(0, half), to = n > 1 ? ps.slice(half) : from;
  var used = [false, false, false]; from.forEach(function (p) { used[p[0]] = true; });
  function key(list) { var k = now.slice(); (list || []).forEach(function (p) { k[p[0]] = p[1]; });
    var r = 0; for (var i = 0; i < 3; i++) if (used[i]) r = r * 10000 + k[i]; return r; }
  var lo = key(from), hi = key(to), c = key(null);
  return lo <= hi ? c >= lo && c <= hi : c >= lo || c <= hi;
}
"#;

/// The proxy ("host:port") a PAC script picks for `url`, run by QuickJS; None = DIRECT or failure.
pub fn pac_proxy(pac: &str, url: &str) -> Option<String> {
    // each script is fetched once (WinHTTP keeps its copy too)
    static SCRIPTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let cached = SCRIPTS.lock().unwrap().iter().find(|s| s.0 == pac).map(|s| s.1.clone());
    let script = match cached {
        Some(s) => s,
        None => {
            let s = match pac.strip_prefix("file://").or_else(|| pac.starts_with('/').then_some(pac)) {
                Some(p) => std::fs::read_to_string(p).ok()?,
                None => ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(15)).build().get(pac).call().ok()?.into_string().ok()?,
            };
            SCRIPTS.lock().unwrap().push((pac.to_string(), s.clone()));
            s
        }
    };
    let host = url::Url::parse(url).ok()?.host_str()?.to_string();
    pac_pick(&find_proxy(&script, url, &host)?)
}

fn find_proxy(script: &str, url: &str, host: &str) -> Option<String> {
    use rquickjs::{Context, Function, Runtime};
    let rt = Runtime::new().ok()?;
    let ctx = Context::full(&rt).ok()?;
    ctx.with(|ctx| {
        let g = ctx.globals();
        let resolve = |h: String| -> Option<String> {
            use std::net::ToSocketAddrs;
            (h.as_str(), 0).to_socket_addrs().ok()?.find(|a| a.is_ipv4()).map(|a| a.ip().to_string())
        };
        // the address outgoing traffic leaves from (connecting a UDP socket sends nothing)
        let my_ip = || -> String {
            let s = std::net::UdpSocket::bind("0.0.0.0:0").ok();
            s.and_then(|s| s.connect("8.8.8.8:53").ok().and_then(|_| s.local_addr().ok())).map_or("127.0.0.1".into(), |a| a.ip().to_string())
        };
        g.set("dnsResolve", Function::new(ctx.clone(), resolve).ok()?).ok()?;
        g.set("myIpAddress", Function::new(ctx.clone(), my_ip).ok()?).ok()?;
        ctx.eval::<(), _>(PAC_UTILS).ok()?;
        ctx.eval::<(), _>(script).ok()?;
        let f: Function = g.get("FindProxyForURL").ok()?;
        f.call::<_, String>((url, host)).ok()
    })
}

/// The first entry of "PROXY a:1; DIRECT": a proxy's "host:port", None for DIRECT (or SOCKS).
fn pac_pick(answer: &str) -> Option<String> {
    let first = answer.split(';').map(str::trim).find(|e| !e.is_empty())?;
    let (kind, hp) = first.split_once(char::is_whitespace).unwrap_or((first, ""));
    ["PROXY", "HTTP", "HTTPS"].iter().any(|k| kind.eq_ignore_ascii_case(k)).then(|| hp.trim().to_string())
}

#[test]
fn pac_scripts() {
    let s = r#"function FindProxyForURL(url, host) {
        if (isPlainHostName(host) || shExpMatch(host, "*.local") || isInNet(host, "10.0.0.0", "255.0.0.0")) return "DIRECT";
        if (dnsDomainIs(host, ".example.com") && weekdayRange("SUN", "SAT") && timeRange(0, 24) && dateRange(1, 31)) return "PROXY px.example.com:3128; DIRECT";
        return "SOCKS s:1080";
    }"#;
    let pick = |host: &str| pac_pick(&find_proxy(s, &format!("http://{host}/f"), host).expect("script runs"));
    assert_eq!(pick("a.example.com").as_deref(), Some("px.example.com:3128"));
    for host in ["intranet", "box.local", "10.1.2.3", "other.org"] {
        assert_eq!(pick(host), None, "{host}");
    }
}

// ---------- Dial Up / VPN: NetworkManager's connections through nmcli ----------

/// Connection types that dial: VPNs, WireGuard, PPPoE and mobile broadband.
const DIAL_TYPES: [&str; 5] = ["vpn", "wireguard", "pppoe", "gsm", "cdma"];

/// Dial connections as nmcli lists them ("name:type", a ':' in a name as "\:"): (name, type).
/// None = no NetworkManager.
fn nm_conns(active: bool) -> Option<Vec<(String, String)>> {
    let mut c = Command::new("nmcli");
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
    let o = Command::new("nmcli").args(args).output().map_err(|e| e.to_string())?;
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
    let _ = std::fs::write(file, format!("[Desktop Entry]\nType=Application\nName=New Download Manager\nExec=\"{}\"\nIcon=ndm\nX-GNOME-Autostart-enabled=true\n", exe.display()));
}

const HOST: &str = "com.ndm.host";
const EXT_ID: &str = "fjkomnlijmdmohmlbfgkepgdieleinoa";

/// Writes the extension's native messaging host manifest where each browser looks for it.
pub fn register_host(_dir: &Path) {
    let Some(exe) = me() else { return };
    let manifest = |allow: serde_json::Value| {
        let mut m = serde_json::json!({ "name": HOST, "description": "New Download Manager", "path": exe, "type": "stdio" });
        m.as_object_mut().unwrap().extend(allow.as_object().unwrap().clone());
        m.to_string()
    };
    let chrome = manifest(serde_json::json!({ "allowed_origins": [format!("chrome-extension://{EXT_ID}/")] }));
    let firefox = manifest(serde_json::json!({ "allowed_extensions": ["ndm@newdownloadmanager"] }));
    let cfg = config_dir();
    let dirs = [
        (cfg.join("google-chrome"), &chrome),
        (cfg.join("chromium"), &chrome),
        (cfg.join("microsoft-edge"), &chrome),
        (cfg.join("BraveSoftware/Brave-Browser"), &chrome),
        (cfg.join("vivaldi"), &chrome),
    ];
    for (browser, text) in dirs {
        // only browsers that are installed (their profile folder exists)
        if browser.is_dir() {
            let d = browser.join("NativeMessagingHosts");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join(format!("{HOST}.json")), text);
        }
    }
    if home().join(".mozilla").is_dir() {
        let d = home().join(".mozilla/native-messaging-hosts");
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(format!("{HOST}.json")), &firefox);
    }
}

pub fn is_host_launch(args: &[String]) -> bool {
    args.first().is_some_and(|a| a.starts_with("chrome-extension://") || a.to_ascii_lowercase().ends_with(".json"))
}

/// The native messaging host: reads the extension's message, starts the app unless it's running, answers.
pub fn native_host(title: &str) {
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    let mut len = [0u8; 4];
    if std::io::stdin().read_exact(&mut len).is_ok() {
        let _ = std::io::stdin().take(u32::from_le_bytes(len) as u64).read_to_end(&mut Vec::new());
    }
    if !running(title, false) {
        if let Some(exe) = me() {
            // its own process group, so closing the browser doesn't close the app
            let _ = Command::new(exe).arg("/tray").process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
    let reply = br#"{"ok":true}"#;
    let mut out = std::io::stdout();
    let _ = out.write_all(&(reply.len() as u32).to_le_bytes()).and_then(|_| out.write_all(reply)).and_then(|_| out.flush());
}

/// Is NDM running? Its browser endpoint answers on 127.0.0.1.
pub fn running(_title: &str, _activate: bool) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], crate::core::PORT));
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
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
    let o = Command::new("fc-match").args(["-f", "%{file}", face]).output().ok()?;
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
    let shown = Command::new("gdbus")
        .args(["call", "--session", "--dest", "org.freedesktop.FileManager1", "--object-path", "/org/freedesktop/FileManager1", "--method", "org.freedesktop.FileManager1.ShowItems", &format!("['{}']", uri.replace('\'', "%27")), ""])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
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

/// The main window is winit's and takes dropped files only, so links go to the drop target.
// ponytail: no link drops on the main window (winit's X11 drop handler accepts file:// only)
pub fn accept_drops(_hwnd: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
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
    (win.set_decorated(false), win.set_keep_above(true), win.set_skip_taskbar_hint(true), win.set_skip_pager_hint(true));
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
            text = data.text().map(|t| t.to_string()).unwrap_or_else(|| String::from_utf16_lossy(&raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()));
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
    let _ = Command::new("xdg-open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// "Turn off computer when done": shut down in a minute (`shutdown -c` cancels it), suspend or hibernate.
pub fn power_off(kind: u8, _force: bool) {
    let _ = match kind {
        1 => Command::new("systemctl").arg("suspend").spawn(),
        2 => Command::new("systemctl").arg("hibernate").spawn(),
        _ => Command::new("shutdown").args(["-h", "+1", "Downloads complete. The computer will shut down in a minute."]).spawn(),
    };
}
