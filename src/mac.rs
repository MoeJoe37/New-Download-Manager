//! macOS stand-ins for `win.rs`: the same functions, done the Mac way (`open`, a LaunchAgent, the browsers'
//! NativeMessagingHosts folders in ~/Library/Application Support, `scutil` for the proxy and VPNs, `afplay`,
//! `netstat` for the networks, AppleScript for the font and program choosers and for messages). The tray
//! icon is the menu bar item (main.rs, `tray`); what Linux and macOS share is in `posix.rs`.
// The title bars, the dialogs' owner and file type icons are AppKit's: nothing to do for them here.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub type Rgba = (usize, usize, Vec<u8>);
pub use crate::posix::{clip_seq, decimal_sep, is_host_launch, pac_proxy, running};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

/// ~/Library/Application Support, where the browsers keep their settings.
fn support() -> PathBuf {
    home().join("Library/Application Support")
}

/// A program's output, None when it can't run or fails.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// An AppleScript's answer (osascript), None on error or Cancel.
fn applescript(lines: &[&str]) -> Option<String> {
    let mut c = Command::new("osascript");
    for l in lines {
        c.args(["-e", l]);
    }
    let o = c.stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A string for AppleScript, in quotes.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ---------- programs NDM starts ----------

/// Nothing to take out of a child's environment on macOS.
pub fn clean_env(_c: &mut Command) {}

/// There is no X11 on macOS.
pub fn prefer_x11() -> bool {
    false
}

/// The main window goes to the menu bar on its second frame (main.rs): nothing to hold back here.
pub fn keep_hidden(_window: isize, _on: bool) {}

// ---------- file types and icons ----------

/// File type pictures come from AppKit, which NDM doesn't use: the list shows none.
pub fn shell_icon(_ext: &str, _folder: bool) -> Option<Rgba> {
    None
}

pub fn exe_icon(_path: &Path) -> Option<Rgba> {
    None
}

/// "ZIP file" for an extension.
pub fn type_name(ext: &str) -> String {
    if ext.is_empty() {
        return String::new();
    }
    crate::i18n::trf("%s file", &[&ext.to_uppercase()])
}

// ---------- browser cache ----------

/// Firefox's cached copy of `url` (~/Library/Caches/Firefox/Profiles).
pub fn url_cache(url: &str) -> Option<(PathBuf, String)> {
    crate::posix::firefox_url_cache(&[home().join("Library/Caches/Firefox/Profiles")], url)
}

// ---------- windows ----------

// The main window is shown and hidden through egui's viewport commands; there is no HWND (0).
pub static PUMP: AtomicBool = AtomicBool::new(false);
pub fn pump(_hwnd: isize) {}
pub fn wake(_hwnd: isize) {}
pub fn hide(_hwnd: isize) {}
pub fn visible(_hwnd: isize) -> bool {
    true
}
pub fn own_dialogs(_main: isize, _dark: bool, _retitle: bool, _light: &[String]) {}
pub fn dark_title(_hwnd: isize, _dark: bool) {}

/// A sound file through afplay.
pub fn play_sound(path: &str) {
    if Path::new(path).is_file() {
        let _ = Command::new("afplay").arg(path).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
}

/// Dark mode (System Settings -> Appearance), asked again at most every 3 s.
pub fn system_dark() -> bool {
    static LAST: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some((at, dark)) = *last {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return dark;
        }
    }
    let dark = output("defaults", &["read", "-g", "AppleInterfaceStyle"]).is_some_and(|s| s.trim().eq_ignore_ascii_case("dark"));
    *last = Some((std::time::Instant::now(), dark));
    dark
}

// ---------- proxy ----------

/// The system's proxy (System Settings -> Network -> Proxies, as `scutil --proxy` shows it) in Windows'
/// Internet Settings shape: (on, "http=host:port;https=host:port", bypass list).
pub fn system_proxy() -> (bool, String, String) {
    scutil_proxy(&output("scutil", &["--proxy"]).unwrap_or_default())
}

fn scutil_proxy(text: &str) -> (bool, String, String) {
    let (mut keys, mut bypass, mut in_list) = (std::collections::HashMap::new(), vec![], false);
    for l in text.lines().map(str::trim) {
        if l.starts_with("ExceptionsList") {
            in_list = true;
            continue;
        }
        if in_list {
            match l.split_once(" : ") {
                Some((_, v)) => bypass.push(v.trim().to_string()),
                None => in_list = false,
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(" : ") {
            keys.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let get = |k: &str| keys.get(k).cloned().unwrap_or_default();
    let server: Vec<String> = [("http", "HTTP"), ("https", "HTTPS"), ("ftp", "FTP")]
        .iter()
        .filter(|(_, k)| get(&format!("{k}Enable")) == "1" && !get(&format!("{k}Proxy")).is_empty())
        .map(|(n, k)| format!("{n}={}:{}", get(&format!("{k}Proxy")), get(&format!("{k}Port"))))
        .collect();
    (!server.is_empty(), server.join(";"), bypass.join(";"))
}

#[test]
fn scutil_proxy_output() {
    let t = "<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n    1 : 169.254/16\n  }\n  FTPPassive : 1\n  HTTPEnable : 1\n  HTTPPort : 3128\n  HTTPProxy : 10.0.0.1\n  HTTPSEnable : 0\n  HTTPSPort : 3129\n  HTTPSProxy : 10.0.0.2\n}\n";
    assert_eq!(scutil_proxy(t), (true, "http=10.0.0.1:3128".into(), "*.local;169.254/16".into()));
    assert!(!scutil_proxy("<dictionary> {\n  HTTPEnable : 0\n}\n").0);
}

/// Changing the system's proxy needs an administrator on macOS: "Use advanced browser integration" leaves it
/// alone (the browser extension takes the downloads).
pub fn set_system_proxy(_on: bool, _server: &str, _bypass: &str) {}

// ---------- Dial Up / VPN: the system's VPN services through scutil --nc ----------

/// (name, connected) for each VPN service `scutil --nc list` shows: `* (Connected)  ID  IPSec  "Work"  [IPSec]`.
fn nc_list() -> Vec<(String, bool)> {
    output("scutil", &["--nc", "list"]).map(|t| nc_parse(&t)).unwrap_or_default()
}

fn nc_parse(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let name = l.split('"').nth(1)?;
            Some((name.to_string(), l.contains("(Connected)")))
        })
        .collect()
}

#[test]
fn nc_lines() {
    let t = "Available network connection services in the current set (*=enabled):\n* (Disconnected)   A1B2  PPP --> L2TP  \"Office VPN\"   [PPP/L2TP]\n* (Connected)      C3D4  IPSec         \"Home\"         [IPSec]\n";
    assert_eq!(nc_parse(t), [("Office VPN".to_string(), false), ("Home".to_string(), true)]);
}

pub fn ras_entries() -> Vec<String> {
    nc_list().into_iter().map(|c| c.0).collect()
}

/// Is a VPN up? Without VPN services there is nothing to dial: yes.
pub fn ras_connected() -> bool {
    let l = nc_list();
    l.is_empty() || l.iter().any(|c| c.1)
}

/// macOS keeps a VPN's login in its own settings (System Settings -> VPN): nothing to save here.
pub fn ras_save(_entry: &str, _user: &str, _pass: &str, _save: bool) -> Result<(), String> {
    Ok(())
}

/// Starts `entry` and waits up to 90 s for it to connect.
pub fn ras_dial(entry: &str) -> Result<(), String> {
    output("scutil", &["--nc", "start", entry]).ok_or("the VPN didn't start")?;
    for _ in 0..180 {
        if output("scutil", &["--nc", "status", entry]).is_some_and(|s| s.lines().next().is_some_and(|l| l.trim() == "Connected")) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err("the VPN didn't connect".into())
}

pub fn ras_hangup_all() {
    for (name, up) in nc_list() {
        if up {
            let _ = output("scutil", &["--nc", "stop", &name]);
        }
    }
}

// ---------- programs and the desktop ----------

/// The browsers in /Applications or ~/Applications: (name, executable inside the app).
pub fn installed_browsers() -> Vec<(String, String)> {
    let known = [
        ("Google Chrome", "Google Chrome.app", "Google Chrome"),
        ("Chromium", "Chromium.app", "Chromium"),
        ("Microsoft Edge", "Microsoft Edge.app", "Microsoft Edge"),
        ("Brave", "Brave Browser.app", "Brave Browser"),
        ("Vivaldi", "Vivaldi.app", "Vivaldi"),
        ("Opera", "Opera.app", "Opera"),
        ("Mozilla Firefox", "Firefox.app", "firefox"),
    ];
    let dirs = [PathBuf::from("/Applications"), home().join("Applications")];
    known
        .iter()
        .filter_map(|(name, app, bin)| dirs.iter().map(|d| d.join(app).join("Contents/MacOS").join(bin)).find(|p| p.is_file()).map(|p| (name.to_string(), p.display().to_string())))
        .collect()
}

/// Start with the user's session: a LaunchAgent (~/Library/LaunchAgents/com.ndm.autostart.plist).
pub fn set_autostart(on: bool) {
    let file = home().join("Library/LaunchAgents/com.ndm.autostart.plist");
    if !on {
        let _ = std::fs::remove_file(file);
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>com.ndm.autostart</string>\n  <key>ProgramArguments</key><array><string>{}</string></array>\n  <key>RunAtLoad</key><true/>\n</dict>\n</plist>\n",
        xml(&exe.display().to_string())
    );
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    let _ = std::fs::write(file, plist);
}

/// Nothing to add to a menu: NDM.app is in Applications (or wherever the user put it).
pub fn install_desktop_entry() {}

const HOST: &str = "com.ndm.host";
const EXT_ID: &str = "fjkomnlijmdmohmlbfgkepgdieleinoa";

/// Writes the extension's native messaging host manifest where each installed browser looks for it.
pub fn register_host(_dir: &Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    let manifest = |allow: serde_json::Value| {
        let mut m = serde_json::json!({ "name": HOST, "description": "New Download Manager", "path": exe, "type": "stdio" });
        m.as_object_mut().unwrap().extend(allow.as_object().unwrap().clone());
        m.to_string()
    };
    let chrome = manifest(serde_json::json!({ "allowed_origins": [format!("chrome-extension://{EXT_ID}/")] }));
    let firefox = manifest(serde_json::json!({ "allowed_extensions": ["ndm@newdownloadmanager"] }));
    for (d, is_firefox) in host_dirs(&support()) {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(format!("{HOST}.json")), if is_firefox { &firefox } else { &chrome });
    }
}

/// The installed browsers' native messaging host folders (their settings folder exists), and whether each
/// is Firefox's.
fn host_dirs(support: &Path) -> Vec<(PathBuf, bool)> {
    let chromes = ["Google/Chrome", "Google/Chrome Beta", "Google/Chrome Canary", "Chromium", "Microsoft Edge", "Microsoft Edge Beta", "Microsoft Edge Dev",
        "BraveSoftware/Brave-Browser", "Vivaldi", "com.operasoftware.Opera", "Yandex/YandexBrowser"];
    let mut d: Vec<(PathBuf, bool)> = chromes.iter().map(|b| support.join(b)).filter(|b| b.is_dir()).map(|b| (b.join("NativeMessagingHosts"), false)).collect();
    if support.join("Firefox").is_dir() || support.join("Mozilla").is_dir() {
        d.push((support.join("Mozilla/NativeMessagingHosts"), true));
    }
    d
}

#[test]
fn host_dirs_of_installed_browsers() {
    let root = std::env::temp_dir().join(format!("ndm-test-machosts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for p in ["Google/Chrome", "Firefox"] {
        std::fs::create_dir_all(root.join(p)).unwrap();
    }
    let d = host_dirs(&root);
    assert_eq!(d, [(root.join("Google/Chrome/NativeMessagingHosts"), false), (root.join("Mozilla/NativeMessagingHosts"), true)]);
    let _ = std::fs::remove_dir_all(&root);
}

/// The native messaging host (see `posix::native_host`).
pub fn native_host(title: &str) {
    crate::posix::native_host(title, std::env::current_exe().ok(), clean_env);
}

/// When NDM can't start at all: an alert.
pub fn fatal(title: &str, text: &str) {
    eprintln!("{text}");
    let _ = applescript(&[&format!("display alert {} message {} as critical", quote(title), quote(text))]);
}

/// "Exit NDM" / "Saving settings...": macOS shows nothing while the app saves (it takes a moment).
pub fn exit_splash(_title: &str, _text: &str) {}

/// The font families installed (from the font files' names).
fn font_families() -> Vec<String> {
    let mut names: Vec<String> = font_dirs()
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            ["ttf", "otf", "ttc"].contains(&ext.as_str()).then(|| p.file_stem()?.to_str().map(|s| s.split('-').next().unwrap_or(s).to_string()))?
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

fn font_dirs() -> Vec<PathBuf> {
    vec!["/System/Library/Fonts".into(), "/System/Library/Fonts/Supplemental".into(), "/Library/Fonts".into(), home().join("Library/Fonts")]
}

/// A font and its size, picked in two AppleScript choosers. Returns (face, size in logical px) like
/// Windows' Font dialog; blocks like it.
pub fn choose_font(_owner: isize, face: &str, px: f32) -> Option<(String, f32)> {
    let list = font_families().iter().map(|f| quote(f)).collect::<Vec<_>>().join(", ");
    let face = applescript(&[&format!("choose from list {{{list}}} with prompt \"Font\" default items {{{}}}", quote(face))]).filter(|f| f != "false" && !f.is_empty())?;
    let pt = (px * 72.0 / 96.0).round();
    let size = applescript(&[&format!("text returned of (display dialog \"Size\" default answer \"{pt}\")")])?;
    let pt: f32 = size.trim().parse().ok().filter(|p: &f32| *p > 0.0)?;
    Some((face, pt * 96.0 / 72.0))
}

/// The file of an installed font: a face by name ("Helvetica", "Tahoma"), or the generic names NDM asks
/// for: "sans-serif", "sans-serif:lang=ar" (a font with Arabic letters) and "DejaVu Sans" (symbols).
pub fn font_file(face: &str) -> Option<PathBuf> {
    let candidates: &[&str] = match face {
        "sans-serif" => &["/System/Library/Fonts/Helvetica.ttc", "/System/Library/Fonts/SFNS.ttf"],
        "sans-serif:lang=ar" => &["/System/Library/Fonts/GeezaPro.ttc", "/System/Library/Fonts/Supplemental/GeezaPro.ttc", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"],
        "DejaVu Sans" => &["/System/Library/Fonts/Supplemental/Arial Unicode.ttf", "/Library/Fonts/Arial Unicode.ttf", "/System/Library/Fonts/Apple Symbols.ttf"],
        _ => &[],
    };
    if let Some(f) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Some(f);
    }
    let want = face.to_lowercase().replace(' ', "");
    font_dirs().iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().map(|e| e.path()).find(|p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        ["ttf", "otf", "ttc"].contains(&ext.as_str()) && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.to_lowercase().replace(' ', "") == want)
    })
}

/// "Open with...": the system's program chooser, then the program opens the file.
pub fn open_with(path: &Path) {
    let p = path.to_path_buf();
    std::thread::spawn(move || {
        if let Some(app) = applescript(&["POSIX path of (choose application as alias)"]) {
            let _ = Command::new("open").arg("-a").arg(app.trim()).arg(&p).spawn();
        }
    });
}

/// Finder with the file selected.
pub fn show_in_folder(path: &Path) {
    let _ = Command::new("open").arg("-R").arg(path).spawn();
}

/// Opens a file, folder or address with its default program.
pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let _ = Command::new("open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// "Turn off computer when done": shut down (asks the open programs to quit first), or sleep.
pub fn power_off(kind: u8, _force: bool) {
    match kind {
        0 => {
            let _ = applescript(&["tell application \"System Events\" to shut down"]);
        }
        _ => {
            let _ = Command::new("pmset").arg("sleepnow").spawn();
        }
    }
}

// ---------- drops ----------

/// Links for the UI to take (`take_drops`): macOS has no drop target window, and the main window takes
/// dropped files only (winit), so this stays empty.
static DROPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Repaints the UI (set by `accept_drops`).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// The UI should bring the main window back (a second start).
static SHOW_MAIN: AtomicBool = AtomicBool::new(false);

pub fn take_drops() -> Vec<String> {
    std::mem::take(&mut DROPS.lock().unwrap())
}

pub fn take_show() -> bool {
    SHOW_MAIN.swap(false, Relaxed)
}

/// A second start asked for the window: `take_show` hands it to the UI.
pub fn show_main() {
    SHOW_MAIN.store(true, Relaxed);
    if let Some(w) = WAKE.get() {
        w();
    }
}

pub fn accept_drops(_window: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
}

/// No floating drop target on macOS (the menu hides its item).
pub fn drop_target(_show: bool, _picture: &Rgba) {}

pub fn drop_target_visible() -> bool {
    false
}

// ---------- networks ----------

/// The networks a download can be spread over: up, not loopback, with an address and a default route
/// (`netstat -rn`, the scoped default routes included: one per connected network).
pub fn nics() -> Vec<crate::net::Nic> {
    let routes = [output("netstat", &["-rn", "-f", "inet"]), output("netstat", &["-rn", "-f", "inet6"])].into_iter().flatten().collect::<Vec<_>>().join("\n");
    crate::posix::ifaddrs(&default_route_devices(&routes))
}

/// The interfaces of `netstat -rn`'s "default" lines (Destination Gateway Flags Netif [Expire]).
fn default_route_devices(netstat: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in netstat.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.first() == Some(&"default") && f.len() >= 4 && !f[3].starts_with("lo") && !out.iter().any(|o| o == f[3]) {
            out.push(f[3].to_string());
        }
    }
    out
}

#[test]
fn netstat_defaults() {
    let t = "Routing tables\n\nInternet:\nDestination        Gateway            Flags               Netif Expire\ndefault            192.168.1.1        UGScg                 en0       \ndefault            172.20.10.1        UGScIg                en8       \n127                127.0.0.1          UCS                   lo0       \n";
    assert_eq!(default_route_devices(t), ["en0", "en8"]);
}

// ---------- packages ----------

/// NDM on macOS is an app bundle, never a package.
pub fn package_owner(_exe: &Path) -> Option<&'static str> {
    None
}

pub fn install_package(_file: &Path) -> Result<(), String> {
    Err("not on macOS".into())
}
