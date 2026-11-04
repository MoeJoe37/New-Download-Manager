//! macOS stand-ins for `win.rs`: the same functions, done the Mac way (`open`, a LaunchAgent, the browsers'
//! NativeMessagingHosts folders in ~/Library/Application Support, `scutil` for the proxy and VPNs, `afplay`,
//! `netstat` for the networks, AppleScript for the font and program choosers and for messages). The tray
//! icon is the menu bar item (main.rs, `tray`); what Linux and macOS share is in `posix.rs`.
// The title bars, the dialogs' owner and file type icons are AppKit's: nothing to do for them here.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
// NSFilenamesPboardType: what winit's main window takes (files), so NDM hands those on
#[allow(deprecated)]
use objc2_app_kit::{
    NSBitmapImageRep, NSColor, NSCompositingOperation, NSDeviceRGBColorSpace, NSDragOperation, NSEvent, NSFilenamesPboardType, NSFloatingWindowLevel, NSGraphicsContext, NSImage, NSPanel, NSPasteboard,
    NSPasteboardTypeString, NSPasteboardTypeURL, NSResponder, NSScreen, NSView, NSWindowCollectionBehavior, NSWindowStyleMask, NSBackingStoreType, NSWorkspace,
};
use objc2_foundation::{NSArray, NSObject, NSPoint, NSRect, NSSize, NSString, NSUInteger};

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

/// The Finder's picture for a file type ("/" = a folder), 32 pixels.
pub fn shell_icon(ext: &str, folder: bool) -> Option<Rgba> {
    let kind = if folder { "public.folder" } else if ext.is_empty() { "public.data" } else { ext };
    #[allow(deprecated)] // iconForContentType needs UniformTypeIdentifiers; this takes extensions and UTIs
    let img = NSWorkspace::sharedWorkspace().iconForFileType(&NSString::from_str(kind));
    image_rgba(&img, 32)
}

/// A program's icon (an .app, or the app bundle a program sits in).
pub fn exe_icon(path: &Path) -> Option<Rgba> {
    let app = path.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).unwrap_or(path);
    if !app.exists() {
        return None;
    }
    let img = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&app.to_string_lossy()));
    image_rgba(&img, 32)
}

/// An NSImage drawn at `px` x `px` pixels, as straight (not premultiplied) RGBA rows from the top.
fn image_rgba(img: &NSImage, px: usize) -> Option<Rgba> {
    let rep = unsafe { NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(NSBitmapImageRep::alloc(), std::ptr::null_mut(), px as isize, px as isize, 8, 4, true, false, NSDeviceRGBColorSpace, (px * 4) as isize, 32) }?;
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    let all = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
    img.drawInRect_fromRect_operation_fraction(all, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)), NSCompositingOperation::Copy, 1.0);
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let row = rep.bytesPerRow() as usize;
    let bytes = unsafe { std::slice::from_raw_parts(data, row * px) };
    let mut out = Vec::with_capacity(px * px * 4);
    for y in 0..px {
        for p in bytes[y * row..y * row + px * 4].chunks_exact(4) {
            let a = p[3] as u32;
            let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
            out.extend([un(p[0]), un(p[1]), un(p[2]), p[3]]);
        }
    }
    (out.iter().skip(3).step_by(4).any(|a| *a != 0)).then_some((px, px, out))
}

/// An NSImage of straight RGBA pixels, shown at `size` points.
fn image_of((w, h, px): &Rgba, size: f64) -> Option<Retained<NSImage>> {
    let rep = unsafe { NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(NSBitmapImageRep::alloc(), std::ptr::null_mut(), *w as isize, *h as isize, 8, 4, true, false, NSDeviceRGBColorSpace, (*w * 4) as isize, 32) }?;
    let data = rep.bitmapData();
    if data.is_null() || px.len() < w * h * 4 {
        return None;
    }
    let row = rep.bytesPerRow() as usize;
    for y in 0..*h {
        let dst = unsafe { std::slice::from_raw_parts_mut(data.add(y * row), w * 4) };
        for (d, s) in dst.chunks_exact_mut(4).zip(px[y * w * 4..(y + 1) * w * 4].chunks_exact(4)) {
            let a = s[3] as u32;
            let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            d.copy_from_slice(&[pre(s[0]), pre(s[1]), pre(s[2]), s[3]]);
        }
    }
    rep.setSize(NSSize::new(size, size));
    let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(size, size));
    img.addRepresentation(&rep);
    Some(img)
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
    let (on, mut server, bypass) = scutil_proxy(&output("scutil", &["--proxy"]).unwrap_or_default());
    if on && crate::mac_capture::has_journal() {
        let local = format!("http=127.0.0.1:{}", crate::core::PROXY_PORT);
        server = server.split(';').filter(|entry| *entry != local).collect::<Vec<_>>().join(";");
    }
    (!server.is_empty(), server, bypass)
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

/// Links dropped on the main window or the drop target, for the UI to take (`take_drops`).
static DROPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Repaints the UI (set by `accept_drops`).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// The UI should bring the main window back (a second start, a drop, a double click on the drop target).
static SHOW_MAIN: AtomicBool = AtomicBool::new(false);
static DROP_SHOWN: AtomicBool = AtomicBool::new(false);
thread_local!(static DROP_WIN: std::cell::RefCell<Option<Retained<NSPanel>>> = const { std::cell::RefCell::new(None) });

pub fn take_drops() -> Vec<String> {
    std::mem::take(&mut DROPS.lock().unwrap())
}

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

/// A second start asked for the window: `take_show` hands it to the UI.
pub fn show_main() {
    wake_ui(true);
}

/// Every http, https and ftp address in dropped text, once each.
fn dropped_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split_whitespace().map(|w| w.trim_end_matches([',', '.', ';', ')', ']', '>', '"', '\''])) {
        if ["http://", "https://", "ftp://"].iter().any(|s| w.to_ascii_lowercase().starts_with(s)) && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// The links a drag carries: a link's address (public.url) or the addresses in dragged text.
fn pasteboard_links(pb: &NSPasteboard) -> Vec<String> {
    let mut text = String::new();
    for t in unsafe { [NSPasteboardTypeURL, NSPasteboardTypeString] } {
        if let Some(s) = pb.stringForType(t) {
            text.push_str(&s.to_string());
            text.push('\n');
        }
    }
    dropped_links(&text)
}

/// Finder files: winit takes those (dropped files).
#[allow(deprecated)]
fn has_files(pb: &NSPasteboard) -> bool {
    pb.propertyListForType(unsafe { NSFilenamesPboardType }).is_some()
}

fn drag_pasteboard(sender: *mut AnyObject) -> Option<Retained<NSPasteboard>> {
    if sender.is_null() {
        return None;
    }
    unsafe { msg_send![&*sender, draggingPasteboard] }
}

#[allow(deprecated)]
fn drag_types() -> Retained<NSArray<NSString>> {
    unsafe { NSArray::from_slice(&[NSFilenamesPboardType, NSPasteboardTypeURL, NSPasteboardTypeString]) }
}

/// winit's own drag handlers on the main window's delegate (files), which links go around.
static WINIT_ENTERED: OnceLock<Imp> = OnceLock::new();
static WINIT_PERFORM: OnceLock<Imp> = OnceLock::new();
type WinitDrag = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;

unsafe extern "C-unwind" fn main_entered(this: *mut AnyObject, sel: Sel, sender: *mut AnyObject) -> NSUInteger {
    if let Some(pb) = drag_pasteboard(sender).filter(|pb| !has_files(pb)) {
        return if pasteboard_links(&pb).is_empty() { 0 } else { NSDragOperation::Copy.0 };
    }
    match WINIT_ENTERED.get() {
        Some(f) => {
            let f: WinitDrag = std::mem::transmute(*f);
            if f(this, sel, sender).as_bool() { NSDragOperation::Copy.0 } else { 0 }
        }
        None => 0,
    }
}

unsafe extern "C-unwind" fn main_perform(this: *mut AnyObject, sel: Sel, sender: *mut AnyObject) -> Bool {
    if let Some(pb) = drag_pasteboard(sender).filter(|pb| !has_files(pb)) {
        let links = pasteboard_links(&pb);
        if links.is_empty() {
            return Bool::NO;
        }
        DROPS.lock().unwrap().extend(links);
        wake_ui(false);
        return Bool::YES;
    }
    match WINIT_PERFORM.get() {
        Some(f) => {
            let f: WinitDrag = std::mem::transmute(*f);
            f(this, sel, sender)
        }
        None => Bool::NO,
    }
}

/// `window` is the main window's NSView (0: only the repaint, set before the window exists). winit's
/// window takes dropped files only; NDM registers it for links and text too and puts its own handlers in
/// front of winit's on the window's delegate, handing files on to winit.
pub fn accept_drops(window: isize, wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(wake));
    if window == 0 || WINIT_ENTERED.get().is_some() {
        return;
    }
    let view = unsafe { &*(window as *const NSView) };
    let Some(delegate) = view.window().and_then(|w| {
        w.registerForDraggedTypes(&drag_types());
        w.delegate()
    }) else {
        eprintln!("link drops on the main window: no window delegate");
        return;
    };
    let obj: &AnyObject = (*delegate).as_ref();
    let cls = obj.class() as *const AnyClass as *mut AnyClass;
    unsafe {
        let entered: Imp = std::mem::transmute(main_entered as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSUInteger);
        let perform: Imp = std::mem::transmute(main_perform as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool);
        if let Some(old) = objc2::ffi::class_replaceMethod(cls, sel!(draggingEntered:), entered, c"Q@:@".as_ptr()) {
            let _ = WINIT_ENTERED.set(old);
        }
        if let Some(old) = objc2::ffi::class_replaceMethod(cls, sel!(performDragOperation:), perform, c"B@:@".as_ptr()) {
            let _ = WINIT_PERFORM.set(old);
        }
    }
    if WINIT_ENTERED.get().is_none() || WINIT_PERFORM.get().is_none() {
        eprintln!("link drops on the main window: the window's delegate ({}) has no drag handlers", obj.class().name().to_string_lossy());
    }
}

define_class!(
    /// The drop target's picture: links dropped on it are downloaded; drag it to move it, double click
    /// for the main window, right click to close it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NDMDropTargetView"]
    #[ivars = Retained<NSImage>]
    struct DropView;

    impl DropView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let all = self.bounds();
            self.ivars().drawInRect_fromRect_operation_fraction(all, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)), NSCompositingOperation::SourceOver, 1.0);
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.clickCount() >= 2 {
                wake_ui(true);
            } else if let Some(w) = self.window() {
                w.performWindowDragWithEvent(event);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            // close it; "Show drop target" brings it back
            if let Some(w) = self.window() {
                w.orderOut(None);
            }
            DROP_SHOWN.store(false, Relaxed);
            wake_ui(false);
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: *mut AnyObject) -> NSUInteger {
            match drag_pasteboard(sender) {
                Some(pb) if !pasteboard_links(&pb).is_empty() => NSDragOperation::Copy.0,
                _ => 0,
            }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: *mut AnyObject) -> Bool {
            let links = drag_pasteboard(sender).map(|pb| pasteboard_links(&pb)).unwrap_or_default();
            if links.is_empty() {
                return Bool::NO;
            }
            DROPS.lock().unwrap().extend(links);
            wake_ui(true);
            Bool::YES
        }
    }
);

/// Show or hide the drop target: a floating picture over every window and Space, bottom right of the
/// screen, that links can be dragged onto.
pub fn drop_target(show: bool, picture: &Rgba) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    DROP_WIN.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            if !show {
                return;
            }
            *cell = make_drop_target(mtm, picture);
        }
        let Some(panel) = cell.as_ref() else { return };
        if show {
            panel.orderFrontRegardless();
        } else {
            panel.orderOut(None);
        }
        DROP_SHOWN.store(show, Relaxed);
    });
}

fn make_drop_target(mtm: MainThreadMarker, picture: &Rgba) -> Option<Retained<NSPanel>> {
    const SIZE: f64 = 64.0;
    let image = image_of(picture, SIZE)?;
    let area = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame())?;
    let at = NSPoint::new(area.origin.x + area.size.width - SIZE - 40.0, area.origin.y + 120.0);
    let frame = NSRect::new(at, NSSize::new(SIZE, SIZE));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(NSPanel::alloc(mtm), frame, style, NSBackingStoreType::Buffered, false);
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Stationary | NSWindowCollectionBehavior::IgnoresCycle);
    let view = DropView::alloc(mtm).set_ivars(image);
    let view: Retained<DropView> = unsafe { msg_send![super(view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), frame.size)] };
    view.registerForDraggedTypes(&drag_types());
    panel.setContentView(Some(&view));
    Some(panel)
}

pub fn drop_target_visible() -> bool {
    DROP_SHOWN.load(Relaxed)
}

#[test]
fn file_type_icons() {
    for (ext, folder) in [("pdf", false), ("zip", false), ("", false), ("/", true)] {
        let (w, h, px) = shell_icon(ext, folder).unwrap_or_else(|| panic!("no icon for {ext:?}"));
        assert_eq!((w, h, px.len()), (32, 32, 32 * 32 * 4));
        assert!(px.chunks_exact(4).filter(|p| p[3] > 200).count() > 100, "{ext:?}: mostly see-through");
    }
    assert!(exe_icon(Path::new("/System/Applications/TextEdit.app")).or_else(|| exe_icon(Path::new("/Applications/Safari.app"))).is_some());
    // NDM's own picture survives the trip into an NSImage (the drop target) and back
    let red = (2, 2, [255u8, 0, 0, 255].repeat(4));
    let (_, _, px) = image_rgba(&image_of(&red, 2.0).unwrap(), 4).unwrap();
    assert!(px.chunks_exact(4).all(|p| p[0] > 240 && p[1] < 16 && p[3] == 255), "{px:?}");
}

#[test]
fn links_from_a_drag() {
    let pb = NSPasteboard::pasteboardWithUniqueName();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str("https://example.com/a.zip"), unsafe { NSPasteboardTypeURL });
    pb.setString_forType(&NSString::from_str("https://example.com/a.zip"), unsafe { NSPasteboardTypeString });
    assert_eq!(pasteboard_links(&pb), ["https://example.com/a.zip"]);
    assert!(!has_files(&pb));
    pb.clearContents();
    pb.setString_forType(&NSString::from_str("see http://a.example/x and ftp://b.example/y, or not:this"), unsafe { NSPasteboardTypeString });
    assert_eq!(pasteboard_links(&pb), ["http://a.example/x", "ftp://b.example/y"]);
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
