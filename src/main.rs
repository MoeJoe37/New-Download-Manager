#![windows_subsystem = "windows"]
mod core;
mod dlu;
mod engine;
mod grabber;
mod i18n;
mod net;
mod remux;
mod stream;
use i18n::tr;
#[cfg_attr(not(windows), path = "linux.rs")]
mod win;

use crate::core::{Core, Incoming, Settings, GENERAL};
use crate::engine::{Info, Probe, Status};
use eframe::egui::{self, Align, Align2, Color32, FontId, Key, Layout, Rect, RichText, Sense, Vec2, ViewportBuilder, ViewportCommand, ViewportId};
use egui_extras::{Column, TableBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
#[cfg(windows)]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TITLE: &str = "New Download Manager 1.0";
/// Where new versions are published: a GitHub "owner/repo" whose latest release carries the new program
/// (Help -> Check for updates, About -> Check for Update, and the automatic check every 2 weeks).
/// Empty = no update source; the check says so.
const UPDATE_REPO: &str = "MoeJoe37/New-Download-Manager";
/// The Linux tray icon is up (its GTK thread owns it): closing the window hides it instead of quitting.
static LINUX_TRAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(not(windows))]
thread_local!(static LIMIT_ITEM: std::cell::RefCell<Option<CheckMenuItem>> = const { std::cell::RefCell::new(None) });
/// First argument of the copy started by "Update now".
const UPDATED: &str = "/updated";
/// The message when the release information can't be read.
const UPDATE_INVALID: &str = "Check for updates failed, because update information is invalid.\r\nIf you use wi-fi that requires logging in in the browser, please log in before checking for updates";

/// A newer release: its version, notes, this platform's program to download ("" = none attached) and
/// the release's web page.
#[derive(Clone, Debug, PartialEq)]
struct Release {
    version: String,
    notes: String,
    exe: String,
    page: String,
}

/// Installed by the setup (its uninstaller sits next to the exe): then an update runs the new setup, which also
/// brings the new language, help and extension files.
fn installed() -> bool {
    cfg!(windows) && std::env::current_exe().is_ok_and(|e| e.with_file_name("unins000.exe").exists())
}

/// How NDM was installed on Linux, which decides the release file "Update now" fetches and how it
/// installs it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum LinuxInstall {
    AppImage,
    Deb,
    Rpm,
    /// The .tar.gz, unpacked where its user can write.
    Folder,
}

fn linux_install() -> LinuxInstall {
    static KIND: std::sync::OnceLock<LinuxInstall> = std::sync::OnceLock::new();
    *KIND.get_or_init(|| {
        if std::env::var_os("APPIMAGE").is_some() {
            return LinuxInstall::AppImage;
        }
        let exe = std::env::current_exe().unwrap_or_default();
        match win::package_owner(&exe) {
            Some("deb") => LinuxInstall::Deb,
            Some(_) => LinuxInstall::Rpm,
            None => LinuxInstall::Folder,
        }
    })
}

/// The release file that replaces this program: the setup (or a loose ndm.exe) on Windows; on Linux the
/// AppImage, the .deb, the .rpm or the .tar.gz, whichever this copy came as.
fn update_asset(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if !cfg!(windows) {
        match linux_install() {
            LinuxInstall::AppImage => n.ends_with(".appimage") && n.contains("x86_64"),
            LinuxInstall::Deb => n.ends_with("_amd64.deb"),
            LinuxInstall::Rpm => n.ends_with(".x86_64.rpm"),
            LinuxInstall::Folder => n.starts_with("ndm-") && n.ends_with("-x86_64.tar.gz"),
        }
    } else if installed() {
        n.starts_with("ndm-setup") && n.ends_with(".exe")
    } else {
        n == "ndm.exe"
    }
}

/// The latest release on GitHub, if newer than this build (None = this is the latest).
fn check_update(agent: &ureq::Agent) -> Result<Option<Release>, String> {
    if UPDATE_REPO.is_empty() {
        return Err(i18n::tr(UPDATE_INVALID)); // no update source in this build
    }
    let url = format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest");
    let v: serde_json::Value = agent.get(&url).set("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))?;
    let tag = v["tag_name"].as_str().unwrap_or("").trim_start_matches(['v', 'V']).to_string();
    let exe = v["assets"].as_array().into_iter().flatten().filter(|a| a["name"].as_str().is_some_and(update_asset)).find_map(|a| a["browser_download_url"].as_str()).unwrap_or("").to_string();
    if tag.is_empty() {
        return Err(i18n::tr(UPDATE_INVALID));
    }
    let page = v["html_url"].as_str().map_or_else(|| format!("https://github.com/{UPDATE_REPO}/releases/latest"), str::to_string);
    Ok(newer(&tag, env!("CARGO_PKG_VERSION")).then(|| Release { version: tag, notes: v["body"].as_str().unwrap_or("").to_string(), exe, page }))
}

/// "1.10.0" > "1.9.2": dotted numbers compared field by field.
fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| s.split(['.', '-']).map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

#[test]
fn versions() {
    assert!(newer("1.10.0", "1.9.2") && newer("2", "1.9") && !newer("1.0.0", "1.0.0") && !newer("0.9", "1.0.0"));
}

/// Update now: download the release's program, swap it for the running one (Windows lets a running exe be
/// renamed, Linux replaces the AppImage's file) and start it. `got` = (bytes, total) for the progress bar.
fn install_update(agent: &ureq::Agent, url: &str, got: &std::sync::atomic::AtomicU64, total: &std::sync::atomic::AtomicU64) -> Result<(), String> {
    use std::io::Read;
    // an AppImage runs from a temporary mount: replace the AppImage file itself
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).map_or_else(std::env::current_exe, Ok).map_err(|e| e.to_string())?;
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    total.store(resp.header("Content-Length").and_then(|l| l.parse().ok()).unwrap_or(0), Relaxed);
    let (mut r, mut data, mut buf) = (resp.into_reader(), Vec::new(), [0u8; 65536]);
    loop {
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        got.store(data.len() as u64, Relaxed);
    }
    let kind = if cfg!(windows) { None } else { Some(linux_install()) };
    let magic: &[u8] = match kind {
        None => b"MZ",
        Some(LinuxInstall::AppImage) => b"\x7fELF",
        Some(LinuxInstall::Deb) => b"!<arch>",
        Some(LinuxInstall::Rpm) => b"\xed\xab\xee\xdb",
        Some(LinuxInstall::Folder) => b"\x1f\x8b",
    };
    if data.len() < 1024 || !data.starts_with(magic) {
        return Err(String::new()); // not the right file: just "cannot download the update file"
    }
    match kind {
        Some(LinuxInstall::Deb | LinuxInstall::Rpm) => {
            // the system's package manager installs it, after the desktop asks for the password
            let file = std::env::temp_dir().join(if kind == Some(LinuxInstall::Deb) { "ndm-update.deb" } else { "ndm-update.rpm" });
            std::fs::write(&file, &data).map_err(|e| e.to_string())?;
            win::install_package(&file)?;
            std::process::Command::new(&exe).arg(UPDATED).spawn().map_err(|e| e.to_string())?;
            return Ok(());
        }
        Some(LinuxInstall::Folder) => {
            update_folder(&data, exe.parent().ok_or("no folder")?)?;
            std::process::Command::new(&exe).arg(UPDATED).spawn().map_err(|e| e.to_string())?;
            return Ok(());
        }
        _ => {}
    }
    if installed() {
        // the setup waits for this copy to exit, installs over it and starts the new version
        let setup = std::env::temp_dir().join("NDM-Setup.exe");
        std::fs::write(&setup, &data).map_err(|e| e.to_string())?;
        let log = format!("/LOG={}", setup.with_extension("log").display());
        std::process::Command::new(&setup).args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", &log, &format!("/NDMPID={}", std::process::id())]).spawn().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let new = exe.with_extension("new.exe");
    std::fs::write(&new, &data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let old = exe.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe); // put the old one back
        return Err(e.to_string());
    }
    std::process::Command::new(&exe).arg(UPDATED).spawn().map_err(|e| e.to_string())?;
    Ok(())
}
/// The .tar.gz's update: unpacked beside the old folder, then each file put in place (the running program
/// by renaming over it, which Linux allows).
fn update_folder(tgz: &[u8], dir: &Path) -> Result<(), String> {
    let tmp = dir.with_file_name(format!(".ndm-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let file = tmp.join("ndm.tar.gz");
    std::fs::write(&file, tgz).map_err(|e| e.to_string())?;
    let ok = std::process::Command::new("tar").arg("-xzf").arg(&file).arg("-C").arg(&tmp).status().is_ok_and(|s| s.success());
    let new = std::fs::read_dir(&tmp).ok().into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.join("ndm").is_file());
    let result = match new.filter(|_| ok) {
        None => Err("the update file is damaged".to_string()),
        Some(new) => (|| -> std::io::Result<()> {
            for e in std::fs::read_dir(&new)? {
                let e = e?;
                let target = dir.join(e.file_name());
                if e.file_type()?.is_dir() {
                    let _ = std::fs::remove_dir_all(&target);
                    copy_dir(&e.path(), &target)?;
                } else {
                    std::fs::rename(e.path(), &target)?;
                }
            }
            Ok(())
        })()
        .map_err(|e| e.to_string()),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

const DARK_DLG: Color32 = Color32::from_rgb(0x39, 0x39, 0x39);
const DARK_TREE: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);
const DARK_MENU_HOT: Color32 = Color32::from_rgb(0x50, 0x50, 0x50);
const BAR: Color32 = Color32::from_rgb(38, 120, 220);

/// Started by the browser extension (`/tray`, `win::native_host`): only the tray icon.
fn tray_start() -> bool {
    std::env::args().any(|a| a.eq_ignore_ascii_case("/tray"))
}

fn main() -> eframe::Result {
    // no console: a panic would vanish without a trace, so it goes to crash.log next to state.json
    std::panic::set_hook(Box::new(|p| {
        let bt = std::backtrace::Backtrace::force_capture();
        let _ = std::fs::write(core::store_path().with_file_name("crash.log"), format!("{p}\n{bt}"));
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if win::is_host_launch(&args) {
        win::native_host(TITLE);
        return Ok(());
    }
    if args.first().is_some_and(|a| a == UPDATED) {
        // started by Update now: wait for the old copy to finish saving and exit (15 s at most)
        for _ in 0..75 {
            if !win::running(TITLE, false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old.exe")); // left behind by an update
    }
    // the AppImage's GTK hook asks for X11; on a Wayland desktop without XWayland GTK would then fail
    #[cfg(not(windows))]
    if !win::prefer_x11() && std::env::var_os("GDK_BACKEND").is_some_and(|b| b == "x11") {
        std::env::remove_var("GDK_BACKEND"); // no other thread runs yet
    }
    let cli = core::cli_args(&args);
    // a second start for the browser (/tray) mustn't bring the window out of the tray
    if win::running(TITLE, !tray_start()) {
        // hand the command line to the running copy through its local endpoint
        if let Some(i) = cli {
            let body = serde_json::json!({ "url": i.url, "filename": i.filename, "force": true, "cli": true, "folder": i.folder,
                "silent": i.silent, "later": i.later, "quit": i.quit, "start_queue": i.start_queue });
            let _ = ureq::post(&format!("http://127.0.0.1:{}/add", core::PORT)).set("X-NDM", "1").send_string(&body.to_string());
        }
        return Ok(());
    }
    win::register_host(core::store_path().parent().unwrap());
    #[cfg(not(windows))]
    win::install_desktop_entry();
    let (w, h, rgba) = app_icon();
    let viewport = ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size([1000.0, 560.0])
        .with_min_inner_size([640.0, 320.0])
        .with_icon(Arc::new(egui::IconData { rgba, width: w as u32, height: h as u32 }));
    // the X11 class / Wayland app id docks match to ndm.desktop (StartupWMClass=ndm)
    #[cfg(not(windows))]
    let viewport = viewport.with_app_id("ndm");
    #[allow(unused_mut)]
    let mut opts = eframe::NativeOptions { viewport, ..Default::default() };
    #[cfg(not(windows))]
    if win::prefer_x11() {
        opts.event_loop_builder = Some(Box::new(|b| {
            use winit::platform::x11::EventLoopBuilderExtX11;
            b.with_x11();
        }));
    }
    eframe::run_native(TITLE, opts, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

// ---------- formatting ----------

fn human(n: u64) -> String {
    let n = n as f64;
    match n {
        _ if n < 1024.0 => format!("{n} {}", tr("Bytes")),
        _ if n < 1024.0 * 1024.0 => format!("{:.2} {}", n / 1024.0, tr("KB")),
        _ if n < 1024.0 * 1024.0 * 1024.0 => format!("{:.2} {}", n / 1048576.0, tr("MB")),
        _ => format!("{:.2} {}", n / 1073741824.0, tr("GB")),
    }
}

fn rate(bps: f64) -> String {
    if bps <= 0.0 { String::new() } else { format!("{}/{}", human(bps as u64), tr("sec")) }
}

/// NDM's time left (0x43f730): seconds, minutes and seconds, or hours and minutes ("%ld hour(s) %d min" in
/// its built-in English; the language files say "%d hours %d min").
fn time_left(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, m) {
        _ if h > 10000 => String::new(),
        (0, 0) => i18n::trf("%d sec", &[&s]),
        (0, _) => i18n::trf("%d min %d sec", &[&m, &s]),
        _ if i18n::current().is_empty() => format!("{h} hour(s) {m} min"),
        _ => i18n::trf("%d hours %d min", &[&h, &m]),
    }
}

/// NDM's download-progress sizes (0x4a34c0): three truncated decimals with the locale's separator and two
/// spaces before the unit ("796.000  KB"); below 1 KB "N  Bytes"; 1000-1023 of a unit go to the next one.
fn size3(n: u64) -> String {
    let (b, kb, mb, gb) = (n % 1024, n / 1024 % 1024, n / 1_048_576 % 1024, n / 1_073_741_824);
    let (whole, frac, unit) = if gb == 0 && mb < 1000 {
        if mb == 0 && kb < 1000 {
            if kb == 0 {
                return format!("{b}  {}", tr("Bytes"));
            }
            (kb, b * 1000 / 1024, "KB")
        } else {
            (mb, kb * 1000 / 1024, "MB")
        }
    } else {
        (gb, mb * 1000 / 1024, "GB")
    };
    format!("{whole}{}{frac:03}  {}", win::decimal_sep(), tr(unit))
}

/// A percentage as NDM prints it: whole part, separator, two truncated decimals.
fn pct2(p: f64) -> String {
    let whole = p.trunc();
    format!("{}{}{:02}", whole as u64, win::decimal_sep(), ((p - whole) * 100.0).trunc() as u64)
}

fn date(ts: i64) -> String {
    use chrono::TimeZone;
    if ts == 0 {
        return String::new();
    }
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.format("%b %d %H:%M:%S %Y").to_string()).unwrap_or_default()
}

/// "http://u:p@host/x" -> ("http://host/x", "u", "p"); the File Properties Login/Password fields.
/// `url` with login:password put in (Add URL / Batch "Use authorization").
fn with_login(url: &str, login: &str, pass: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username(login);
            let _ = u.set_password(Some(pass));
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

fn split_login(url: &str) -> (String, String, String) {
    match url::Url::parse(url) {
        Ok(mut u) if !u.username().is_empty() => {
            let dec = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
            let (l, p) = (dec(u.username()), dec(u.password().unwrap_or("")));
            let _ = u.set_username("");
            let _ = u.set_password(None);
            (u.to_string(), l, p)
        }
        _ => (url.to_string(), String::new(), String::new()),
    }
}

fn join_login(url: &str, login: &str, pass: &str) -> String {
    match url::Url::parse(url.trim()) {
        Ok(mut u) if !login.is_empty() => {
            let _ = u.set_username(login);
            let _ = u.set_password(Some(pass).filter(|p| !p.is_empty()));
            u.to_string()
        }
        _ => url.trim().to_string(),
    }
}

fn clipboard() -> String {
    arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
}

/// Virus scanner arguments: `[File]` replaced by the downloaded file, or the file appended (template 494's note).
fn virus_args(args: &str, file: &Path) -> Vec<String> {
    let f = file.display().to_string();
    let mut v: Vec<String> = args.split_whitespace().map(|a| a.replace("[File]", &f)).collect();
    if !args.contains("[File]") {
        v.push(f);
    }
    v
}

#[test]
fn virus_arg_list() {
    let p = Path::new(r"C:\d\a b.zip");
    assert_eq!(virus_args("", p), [r"C:\d\a b.zip"]);
    assert_eq!(virus_args("/scan=[File] /q", p), [r"/scan=C:\d\a b.zip", "/q"]);
}

/// One entry of an NDM export file.
#[derive(Debug, PartialEq)]
struct Ef2 {
    url: String,
    referrer: String,
    cookies: String,
}

/// NDM export file (.ef2, CUrlExporter): "<", address, "referer: ", "cookie: ", "User-Agent: " lines, ">".
fn ef2_write(items: &[Info], ua: &str) -> String {
    let mut s = String::new();
    for i in items {
        s += &format!("<\r\n{}\r\n", i.url);
        for (k, v) in [("referer", &i.referrer), ("cookie", &i.cookies), ("User-Agent", &ua.to_string())] {
            if !v.is_empty() {
                s += &format!("{k}: {v}\r\n");
            }
        }
        s += ">\r\n";
    }
    s
}

fn ef2_read(text: &str) -> Vec<Ef2> {
    let mut out = Vec::new();
    for block in text.split('<').skip(1) {
        let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty() && *l != ">");
        let Some(url) = lines.next().filter(|u| u.contains("://")) else { continue };
        let mut e = Ef2 { url: url.to_string(), referrer: String::new(), cookies: String::new() };
        for l in lines {
            if let Some((k, v)) = l.split_once(": ") {
                match k.to_ascii_lowercase().as_str() {
                    "referer" => e.referrer = v.to_string(),
                    "cookie" => e.cookies = v.to_string(),
                    _ => {}
                }
            }
        }
        out.push(e);
    }
    out
}

#[test]
fn ef2_round_trip() {
    let i = Info { url: "https://a.com/f.zip".into(), referrer: "https://a.com/".into(), cookies: "k=v".into(), ..Default::default() };
    let text = ef2_write(&[i, Info { url: "http://b.org/x".into(), ..Default::default() }], "UA");
    let back = ef2_read(&text);
    assert_eq!(back.len(), 2);
    assert_eq!(back[0], Ef2 { url: "https://a.com/f.zip".into(), referrer: "https://a.com/".into(), cookies: "k=v".into() });
    assert_eq!(back[1].url, "http://b.org/x");
}

fn links(text: &str) -> Vec<String> {
    text.split_whitespace().filter(|w| ["http://", "https://", "ftp://"].iter().any(|p| w.starts_with(p)) || stream::is_media_url(w)).map(str::to_string).collect()
}

/// The browser's own extensions page, for the browsers the extension (Chromium MV3) runs in.
fn extension_page(b: &core::Browser) -> Option<String> {
    let k = core::browser_kind(&format!("{} {}", b.name, b.exe));
    if k == "firefox" {
        return Some("about:debugging#/runtime/this-firefox".into());
    }
    matches!(k, "chrome" | "edge" | "opera" | "vivaldi" | "brave" | "yandex").then(|| format!("{}://extensions", if k == "chrome" { "chrome" } else if k == "yandex" { "browser" } else { k }))
}

const FIREFOX_GUIDE: &str = "To capture downloads from Firefox, open its extensions page (about:debugging), click \"This Firefox\", press \"Load Temporary Add-on...\" and select manifest.json in this folder. Firefox removes the add-on when it closes, so load it again after a restart:";

fn integration(b: &core::Browser) -> &'static str {
    if extension_page(b).is_some() { "Extension" } else { "Not supported" }
}

/// Stop a download that was started early from "Download File Info" and delete its data.
fn discard(j: &engine::Job) {
    j.stop();
    while j.running() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(j.info().part());
    let _ = std::fs::remove_dir_all(j.info().part()); // a stream's pieces
}

/// Horizontal DLU scale of the pages NDM creates inside its Options sheet and Scheduler (2.0 px per DLU at
/// 125%, measured; their frames use dlu::DX).
const PAGE_DX: f32 = 1.6;

/// Download progress dialog 103 across: NDM lays it out 723 px wide for 353 DLU at 125% (measured).
const PROG_DX: f32 = 723.0 / 353.0 / 1.25;

/// List row height: NDM's 19 px at 125%.
const ROW_H: f32 = 15.2;

/// Segoe UI, the font of NDM's main window.
fn segoe(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("segoe".into()))
}

/// A Scheduler time as typed: "11:00:00 PM", "11:00 PM", "23:00:00" or "23:00".
fn sched_time(s: &str) -> Option<chrono::NaiveTime> {
    let s = s.trim().to_uppercase();
    ["%I:%M:%S %p", "%I:%M %p", "%H:%M:%S", "%H:%M"].iter().find_map(|f| chrono::NaiveTime::parse_from_str(&s, f).ok())
}

/// "Download limits" node in the Scheduler's queue tree.
const LIMITS: u32 = u32::MAX;

fn explore(arg: impl AsRef<std::ffi::OsStr>) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(arg).spawn();
    #[cfg(not(windows))]
    win::open(arg);
}

fn open_folder_of(path: &Path) {
    if path.exists() {
        #[cfg(windows)]
        explore(format!("/select,{}", path.display()));
        #[cfg(not(windows))]
        win::show_in_folder(path);
    } else if let Some(p) = path.parent() {
        explore(p);
    }
}

/// The unpacked browser extension: next to the exe or in one of its parent folders (cargo builds).
fn extension_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundled = exe.ancestors().map(|a| a.join("extension")).find(|p| p.join("manifest.json").exists())?;
    if std::env::var_os("APPIMAGE").is_none() {
        return Some(bundled);
    }
    // inside the AppImage the bundled copy is under /tmp/.mount_*, a new folder every run: browsers get a
    // copy beside state.json that stays put, refreshed when NDM's extension changes
    static STABLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    STABLE.get_or_init(|| {
        let d = core::store_path().with_file_name("extension");
        refresh_copy(&bundled, &d).ok().map(|_| d)
    })
    .clone()
    .or(Some(bundled))
}

/// Makes `to` a copy of the folder `from` unless their manifest.json files are already the same.
fn refresh_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    let manifest = |d: &Path| std::fs::read(d.join("manifest.json")).ok();
    if manifest(to).is_some() && manifest(to) == manifest(from) {
        return Ok(());
    }
    copy_dir(from, to)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

#[test]
fn extension_copy_refreshes() {
    let base = std::env::temp_dir().join(format!("ndm-test-ext-{}", std::process::id()));
    let (from, to) = (base.join("bundled"), base.join("stable"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(from.join("icons")).unwrap();
    std::fs::write(from.join("manifest.json"), "v1").unwrap();
    std::fs::write(from.join("icons/a.png"), "png").unwrap();
    refresh_copy(&from, &to).unwrap();
    assert_eq!(std::fs::read_to_string(to.join("icons/a.png")).unwrap(), "png");
    // same manifest: left alone
    std::fs::write(from.join("background.js"), "new").unwrap();
    refresh_copy(&from, &to).unwrap();
    assert!(!to.join("background.js").exists());
    // a new version: copied again
    std::fs::write(from.join("manifest.json"), "v2").unwrap();
    refresh_copy(&from, &to).unwrap();
    assert_eq!(std::fs::read_to_string(to.join("manifest.json")).unwrap(), "v2");
    assert!(to.join("background.js").exists());
    let _ = std::fs::remove_dir_all(&base);
}

/// NDM's own help: `help\*.htm` next to the exe or in a parent folder.
fn help_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|a| a.join("help")).find(|d| d.join("index.htm").exists())
}

/// The help page the UI should show next ("page.htm" or "page.htm#anchor"), set by `help`.
static HELP_REQ: Mutex<Option<String>> = Mutex::new(None);

/// Opens NDM's help at `page` in its own window (`Dialog::Help`). Returns whether the help exists, so Help
/// buttons grey out without it.
fn help(page: &str, open: bool) -> bool {
    let ok = help_dir().is_some();
    if ok && open {
        *HELP_REQ.lock().unwrap() = Some(page.to_string());
    }
    ok
}

/// What a help block is: the page heading (h1), its lead-in (p class="lead"), a section heading (h2),
/// body text, a list item, a code line (pre) or a tip / note callout (p class="tip" / "note").
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum HelpKind {
    #[default]
    Text,
    Title,
    Lead,
    Heading,
    Item,
    Code,
    Tip,
    Note,
    Image,
}

/// A run of help text: bold, italic, key / code, link target.
#[derive(Clone, Default, PartialEq, Debug)]
struct HelpRun {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    link: Option<String>,
}

/// A help block: its kind, its id (the anchor links point at) and its runs. An <ol> item has its
/// number; an image has its file (relative to help/) and its alt text, also its caption, as the runs.
#[derive(Default)]
struct HelpPara {
    kind: HelpKind,
    id: Option<String>,
    runs: Vec<HelpRun>,
    num: Option<u32>,
    src: Option<String>,
}

impl HelpPara {
    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect::<String>().trim().to_string()
    }
}

/// The small HTML the help pages are written in (h1, h2, p, p class=lead/tip/note, ul/ol/li, pre, img,
/// b, i, kbd/code, a href) as blocks; returns the page's <title> too.
fn help_parse(html: &str) -> (String, Vec<HelpPara>) {
    let lower = html.to_ascii_lowercase();
    let title = lower.find("<title>").and_then(|a| lower[a..].find("</title>").map(|b| html[a + 7..a + b].trim().to_string())).unwrap_or_default();
    let body = lower.find("<body").map_or(html, |i| &html[i..]);
    let attr = |tag: &str, name: &str| {
        let l = tag.to_ascii_lowercase();
        let i = l.find(&format!("{name}="))? + name.len() + 1;
        let v = tag[i..].trim_start_matches('"');
        let end = if tag[i..].starts_with('"') { v.find('"') } else { v.find([' ', '>']) };
        Some(v[..end.unwrap_or(v.len())].to_string())
    };
    // the item number of the list being read (None in a <ul>)
    let mut ol: Option<u32> = None;
    let (mut paras, mut cur) = (vec![], HelpPara::default());
    let (mut bold, mut italic, mut code, mut link) = (0, 0, 0, None::<String>);
    let flush = |cur: &mut HelpPara, paras: &mut Vec<HelpPara>| {
        let kind = cur.kind;
        if !cur.text().is_empty() {
            paras.push(std::mem::take(cur));
        }
        (cur.kind, cur.num) = (kind, None);
    };
    for chunk in body.split('<').skip(1) {
        let (tag, text) = chunk.split_once('>').unwrap_or((chunk, ""));
        let name = tag.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let block = match name.as_str() {
            "h1" => Some(HelpKind::Title),
            "h2" | "h3" => Some(HelpKind::Heading),
            "li" => Some(HelpKind::Item),
            "pre" => Some(HelpKind::Code),
            "p" | "div" => Some(match attr(tag, "class").as_deref() {
                Some("lead") => HelpKind::Lead,
                Some("tip") => HelpKind::Tip,
                Some("note") => HelpKind::Note,
                _ => HelpKind::Text,
            }),
            _ => None,
        };
        match name.as_str() {
            _ if block.is_some() => {
                flush(&mut cur, &mut paras);
                (cur.kind, cur.id) = (block.unwrap(), attr(tag, "id"));
                if name == "li" {
                    ol = ol.map(|n| n + 1);
                    cur.num = ol;
                }
            }
            "/h1" | "/h2" | "/h3" | "/li" | "/pre" | "/p" | "/div" | "ul" | "/ul" | "ol" | "/ol" => {
                flush(&mut cur, &mut paras);
                cur.kind = HelpKind::Text;
                ol = match name.as_str() {
                    "ol" => Some(0),
                    "ul" | "/ul" | "/ol" => None,
                    _ => ol,
                };
            }
            "img" => {
                flush(&mut cur, &mut paras);
                let alt = attr(tag, "alt").unwrap_or_default().replace("&quot;", "\"").replace("&amp;", "&");
                paras.push(HelpPara { kind: HelpKind::Image, id: attr(tag, "id"), runs: vec![HelpRun { text: alt, ..Default::default() }], num: None, src: attr(tag, "src") });
            }
            "br" => flush(&mut cur, &mut paras),
            "b" | "strong" => bold += 1,
            "/b" | "/strong" => bold -= 1,
            "i" | "em" => italic += 1,
            "/i" | "/em" => italic -= 1,
            "kbd" | "code" => code += 1,
            "/kbd" | "/code" => code -= 1,
            "a" => link = attr(tag, "href"),
            "/a" => link = None,
            _ => {}
        }
        let t = text.replace("&nbsp;", " ").replace("&gt;", ">").replace("&lt;", "<").replace("&quot;", "\"").replace("&amp;", "&");
        let t = if cur.kind == HelpKind::Code { t.trim().to_string() } else { t.split_whitespace().collect::<Vec<_>>().join(" ") };
        if t.is_empty() {
            continue;
        }
        // keep the space the source had between runs
        let space = if text.starts_with(char::is_whitespace) && !cur.runs.is_empty() { " " } else { "" };
        let end = if text.ends_with(char::is_whitespace) { " " } else { "" };
        cur.runs.push(HelpRun { text: format!("{space}{t}{end}"), bold: bold > 0, italic: italic > 0, code: code > 0, link: link.clone() });
    }
    flush(&mut cur, &mut paras);
    (title, paras)
}

#[test]
fn help_pages() {
    let (_, p) = help_parse(r#"<body><h1>T</h1><ol><li>One</li><li>Two</li></ol><ul><li>Dot</li></ul><p><img src="images/a.png" alt="The main window"></p></body>"#);
    assert_eq!(p.iter().map(|p| p.num).collect::<Vec<_>>(), [None, Some(1), Some(2), None, None]);
    assert_eq!((p[4].kind, p[4].src.as_deref(), p[4].text().as_str()), (HelpKind::Image, Some("images/a.png"), "The main window"));
    let (title, p) = help_parse(r#"<html><head><title>T</title></head><body><h1>Head</h1><p class="lead">Lead</p><h2 id="x">Sec</h2><p><b>Bold</b> and <a href="b.htm#y">a link</a> -&gt; <kbd>Del</kbd></p><ul><li>One</li></ul><pre>a  b</pre><p class="tip">Tip</p></body></html>"#);
    assert_eq!(title, "T");
    let kinds: Vec<_> = p.iter().map(|p| p.kind).collect();
    use HelpKind::*;
    assert_eq!(kinds, [Title, Lead, Heading, Text, Item, Code, Tip]);
    assert_eq!(p[2].id.as_deref(), Some("x"));
    let runs: Vec<_> = p[3].runs.iter().map(|r| (r.text.as_str(), r.bold, r.code, r.link.as_deref())).collect();
    assert_eq!(runs, [("Bold", true, false, None), (" and ", false, false, None), ("a link", false, false, Some("b.htm#y")), (" -> ", false, false, None), ("Del", false, true, None)]);
    assert_eq!(p[5].text(), "a  b");
    // every shipped page parses, has a heading, and its links point at pages and sections that exist
    let dir = help_dir().expect("help folder");
    for f in std::fs::read_dir(&dir).unwrap().flatten().filter(|f| f.path().extension().is_some_and(|e| e == "htm")) {
        let (t, paras) = help_parse(&std::fs::read_to_string(f.path()).unwrap());
        assert!(!t.is_empty() && paras.first().is_some_and(|p| p.kind == Title), "{:?}", f.path());
        // screenshots: there, readable, small enough, with a caption
        for img in paras.iter().filter(|p| p.kind == Image) {
            let src = img.src.as_deref().unwrap_or_default();
            let file = dir.join(src);
            let size = std::fs::metadata(&file).unwrap_or_else(|_| panic!("{src} in {:?}", f.path())).len();
            let pic = image::open(&file).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert!(pic.width() <= 1400 && size <= 300 * 1024, "{src}: {} px, {size} bytes", pic.width());
            assert!(!img.text().is_empty(), "{src} has no alt text");
        }
        for l in paras.iter().flat_map(|p| &p.runs).filter_map(|r| r.link.as_deref()).filter(|l| !l.contains("://")) {
            let (file, anchor) = l.split_once('#').map_or((l, None), |(f, a)| (f, Some(a)));
            let (_, target) = help_parse(&std::fs::read_to_string(dir.join(file)).unwrap_or_else(|_| panic!("{l} in {:?}", f.path())));
            assert!(anchor.is_none_or(|a| target.iter().any(|p| p.id.as_deref() == Some(a))), "{l} in {:?}", f.path());
        }
    }
}

/// The help window's three icons (Segoe Fluent Icons / MDL2 glyphs, drawn white and tinted when shown).
const HELP_SEARCH: usize = 0;
const HELP_BACK: usize = 1;
const HELP_FWD: usize = 2;
const HELP_CONTROLS: [u32; 3] = [0xE721, 0xE72B, 0xE72A];

/// A texture made once and kept in egui's memory.
fn cached_texture(ctx: &egui::Context, name: &str, make: impl FnOnce() -> Option<win::Rgba>) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("tex", name));
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return t;
    }
    let t = make().map(|px| texture(ctx, name, &px));
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

fn help_icons(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    cached_texture(ctx, "help_icons", || {
        let cells: Vec<(u32, [u8; 3])> = HELP_CONTROLS.iter().map(|g| (*g, [255; 3])).collect();
        glyph_strip(&cells, 64, 64, 46.0)
    })
}

/// A help page's screenshot (`src` relative to help/), loaded once.
fn help_image(ctx: &egui::Context, dir: &Path, src: &str) -> Option<egui::TextureHandle> {
    cached_texture(ctx, &format!("help:{src}"), || {
        let i = image::open(dir.join(src)).ok()?.into_rgba8();
        Some((i.width() as usize, i.height() as usize, i.into_raw()))
    })
}

fn help_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    cached_texture(ctx, "help_logo", || Some(app_icon()))
}

/// Paints icon `i` of the help strip in `rect`, tinted `tint`.
fn help_icon(ui: &egui::Ui, tex: &egui::TextureHandle, i: usize, rect: Rect, tint: Color32) {
    let n = HELP_CONTROLS.len() as f32;
    let uv = Rect::from_min_max(egui::pos2(i as f32 / n, 0.0), egui::pos2((i + 1) as f32 / n, 1.0));
    egui::Image::new((tex.id(), rect.size())).uv(uv).tint(tint).paint_at(ui, rect);
}

/// A help paragraph's runs, wrapped: links in `link` (a click sets `go`), keys and code on `chip`.
fn help_runs(ui: &mut egui::Ui, runs: &[HelpRun], size: f32, color: Color32, strong: Color32, (link, chip): (Color32, Color32), go: &mut Option<String>) -> egui::Response {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 5.0);
        for r in runs {
            let mut t = if r.code {
                RichText::new(format!("\u{2009}{}\u{2009}", r.text.trim())).monospace().size(size - 1.5).color(strong).background_color(chip)
            } else {
                RichText::new(r.text.as_str()).size(size).color(if r.bold { strong } else { color })
            };
            if r.bold {
                t = t.strong();
            }
            if r.italic {
                t = t.italics();
            }
            match &r.link {
                Some(l) => {
                    if ui.add(egui::Link::new(t.color(link))).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        *go = Some(l.clone());
                    }
                }
                None => {
                    ui.label(t);
                }
            }
        }
    })
    .response
}

/// Help search: every section whose text has all the words, under its page and heading.
fn help_search(
    ui: &mut egui::Ui,
    topics: &[(String, String, String)],
    read: &dyn Fn(&str) -> (String, Vec<HelpPara>),
    query: &str,
    shown: &str,
    (text, strong, weak, link, row): (Color32, Color32, Color32, Color32, Color32),
    go: &mut Option<String>,
) {
    ui.label(RichText::new(i18n::trf("Results for \"%s\"", &[&shown])).size(22.0).strong().color(strong));
    ui.add_space(12.0);
    let mut found = 0;
    for (f, name, _) in topics.iter().skip(1) {
        let (_, paras) = read(f);
        let mut seen = HashSet::new();
        let mut head: (Option<String>, String) = (None, String::new());
        for p in &paras {
            if p.kind == HelpKind::Heading {
                head = (p.id.clone(), p.text());
            }
            let t = p.text();
            let low = t.to_lowercase();
            if !query.split_whitespace().all(|w| low.contains(w)) || !seen.insert(head.0.clone()) {
                continue;
            }
            found += 1;
            let r = egui::Frame::new().corner_radius(3).inner_margin(egui::Margin::symmetric(8, 7)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let at = if head.1.is_empty() { tr(name) } else { format!("{}  \u{203A}  {}", tr(name), tr(&head.1)) };
                ui.label(RichText::new(at).size(14.0).color(link));
                let snip = if t.chars().count() > 180 { t.chars().take(180).collect::<String>() + "\u{2026}" } else { t.clone() };
                ui.label(RichText::new(snip).size(13.0).color(text));
            });
            let r = r.response.interact(Sense::click());
            if r.hovered() {
                ui.painter().rect_filled(r.rect, 3.0, row.gamma_multiply(0.5));
            }
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                *go = Some(head.0.as_ref().map_or(f.clone(), |id| format!("{f}#{id}")));
            }
        }
    }
    if found == 0 {
        ui.label(RichText::new(tr("Nothing matches. Try fewer or different words.")).size(14.0).color(weak));
    }
}

/// The app's own icon (assets/ndm.png, 256 px with alpha; the .exe gets assets/ndm.ico via build.rs).
fn app_icon() -> win::Rgba {
    let i = eframe::icon_data::from_png_bytes(include_bytes!("../assets/ndm.png")).expect("bundled icon");
    (i.width as usize, i.height as usize, i.rgba)
}

/// Tray picture for View -> "NDM tray icon": 1 = 3D style (the app icon), 2 = Classic style (assets/ndm2d.png).
fn tray_picture(style: u8) -> Option<tray_icon::Icon> {
    let (w, h, rgba) = match style {
        2 => eframe::icon_data::from_png_bytes(include_bytes!("../assets/ndm2d.png")).map(|i| (i.width as usize, i.height as usize, i.rgba)).ok()?,
        _ => app_icon(),
    };
    tray_icon::Icon::from_rgba(rgba, w as u32, h as u32).ok()
}

fn texture(ctx: &egui::Context, name: &str, (w, h, px): &win::Rgba) -> egui::TextureHandle {
    ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([*w, *h], px), egui::TextureOptions::LINEAR)
}

/// NDM's toolbar strips (normal, hot, and a generated disabled one), 12 icons each.
struct Toolbar {
    normal: egui::TextureHandle,
    hot: egui::TextureHandle,
    disabled: egui::TextureHandle,
    /// View -> Toolbar -> Small Buttons: 32x32 strips (normal, hot, disabled) from the same skin.
    small: [Option<egui::TextureHandle>; 3],
}

const TB_ICONS: usize = 12;

/// The grabber's menu bar and toolbar heights (points).
const GRAB_MENU: f32 = 20.0;
const GRAB_TB: f32 = 30.0;
/// Wizard step captions, in NDM's order.
const GRAB_STEPS: [&str; 4] = ["Set start page", "Save files to", "Set explorer filters", "Set file filters"];
/// File-type filters offered by the wizard's drop-down lists.
/// Open a web address in the default browser.
fn open_url(u: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", u]).spawn();
    #[cfg(not(windows))]
    win::open(u);
}

/// "Open with...": the system's program chooser.
fn open_with(p: impl AsRef<std::ffi::OsStr>) {
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").arg("shell32.dll,OpenAs_RunDLL").arg(p).spawn();
    #[cfg(not(windows))]
    win::open_with(Path::new(p.as_ref()));
}

const GRAB_FILTERS: [(&str, &str); 7] = [
    ("All Files", "*.*"),
    ("Images Files", "*.jpg;*.jpeg;*.png;*.gif;*.webp;*.bmp"),
    ("Audio Files", "*.mp3;*.flac;*.wav;*.ogg;*.m4a"),
    ("Video Files", "*.mp4;*.mkv;*.avi;*.webm;*.mov"),
    ("Compressed", "*.zip;*.rar;*.7z;*.gz;*.tar"),
    ("Documents", "*.pdf;*.doc;*.docx;*.xls;*.xlsx;*.ppt;*.pptx"),
    ("Web content", "*.htm;*.html;*.css;*.js;*.jpg;*.jpeg;*.png;*.gif;*.webp;*.svg;*.ico;*.woff;*.woff2"),
];

/// A grabber menu command (menu 256).
enum GrabCmd {
    New,
    Save,
    SaveTpl,
    Recent(String),
    Schedule,
    Close,
}

/// (strip index, fallback glyph, label, tooltip, enabled, drop-down)
type TbButton = (usize, &'static str, String, String, bool, bool);

/// RT_TOOLBAR 166 in NDM's order: (fallback glyph, label, tooltip).
const TB_BUTTONS: [(&str, &str, &str); TB_ICONS] = [
    ("➕", "Add URL", "Add new download address manually"),
    ("▶", "Resume", "Start/Resume downloading selected file(s)"),
    ("⏹", "Stop", "Stop downloading selected file(s)"),
    ("⏸", "Stop All", "Stop all downloads"),
    ("🗑", "Delete", "Remove selected file(s) from the list"),
    ("✔", "Delete Co...", "Remove all completed files from the list"),
    ("⚙", "Options", "Browsers/System integration, File types, Proxy, Passwords and others"),
    ("⏰", "Scheduler", "Scheduler and download queues"),
    ("⏩", "Start Queue", "Start main download queue, or choose a queue to start from drop-down list"),
    ("⏏", "Stop Queue", "Stop main download queue, or choose a queue to stop from drop-down list"),
    ("🌐", "Grabber", "Run site grabber"),
    ("✉", "Tell a Friend", "Write a letter to your friend about New Download Manager"),
];

/// NDM's own icons: glyphs of Windows' icon font (Segoe Fluent Icons; Segoe MDL2 Assets on Windows 10),
/// drawn in colour into textures when NDM starts. Nothing is bundled and nothing comes from another program.
fn icon_font() -> Option<&'static ab_glyph::FontVec> {
    static FONT: std::sync::OnceLock<Option<ab_glyph::FontVec>> = std::sync::OnceLock::new();
    #[cfg(not(windows))]
    return FONT.get_or_init(|| ab_glyph::FontVec::try_from_vec(include_bytes!("../assets/linux/MaterialIcons-Regular.ttf").to_vec()).ok()).as_ref();
    #[cfg(windows)]
    FONT.get_or_init(|| {
        let dir = Path::new(&std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
        ["SegoeIcons.ttf", "segmdl2.ttf"].iter().find_map(|f| std::fs::read(dir.join(f)).ok()).and_then(|b| ab_glyph::FontVec::try_from_vec(b).ok())
    })
    .as_ref()
}

/// The icon font's code point for a Segoe icon: the same on Windows; Google's Material Icons (Apache 2.0,
/// assets/linux) elsewhere, where Segoe's icon fonts don't exist.
fn icon_cp(cp: u32) -> u32 {
    if cfg!(windows) {
        return cp;
    }
    match cp {
        0xE710 => 0xE145, // Add -> add
        0xE713 => 0xE8B8, // Setting -> settings
        0xE714 => 0xE04B, // Video -> videocam
        0xE715 => 0xE158, // Mail -> mail
        0xE71A => 0xE047, // Stop -> stop
        0xE721 => 0xE8B6, // Search -> search
        0xE72A => 0xE5C8, // Forward -> arrow_forward
        0xE72B => 0xE5C4, // Back -> arrow_back
        0xE72C => 0xE5D5, // Refresh -> refresh
        0xE739 => 0xE835, // Checkbox -> check_box_outline_blank
        0xE73A => 0xE834, // CheckboxComposite -> check_box
        0xE74D => 0xE872, // Delete -> delete
        0xE756 => 0xE5C3, // CommandPrompt -> apps
        0xE768 => 0xE037, // Play -> play_arrow
        0xE769 => 0xE034, // Pause -> pause
        0xE774 => 0xE80B, // Globe -> public
        0xE787 => 0xE935, // Calendar -> calendar_today
        0xE790 => 0xE40A, // Color -> palette
        0xE7B8 => 0xE149, // Package -> archive
        0xE7C4 => 0xE871, // TaskView -> dashboard
        0xE7EF => 0xE7FD, // Admin -> person
        0xE80F => 0xE88A, // Home -> home
        0xE81D => 0xEF71, // -> stop_circle
        0xE823 => 0xE8B5, // Recent -> schedule
        0xE838 => 0xE2C8, // FolderOpen -> folder_open
        0xE895 => 0xE627, // Sync -> sync
        0xE896 => 0xF090, // Download -> download
        0xE8A5 => 0xE873, // Document -> description
        0xE8AF => 0xEB2F, // -> lan
        0xE8B7 => 0xE2C7, // Folder -> folder
        0xE8D6 => 0xE405, // Audio -> music_note
        0xE8D7 => 0xE897, // Permissions -> lock
        0xE8F4 => 0xE2CC, // NewFolder -> create_new_folder
        0xE8FD => 0xE896, // BulletedList -> list
        0xE930 => 0xE86C, // Completed -> check_circle
        0xE945 => 0xEA0B, // LightningBolt -> bolt
        0xE946 => 0xE88E, // Info -> info
        0xE9D9 => 0xE429, // Diagnostic -> tune
        0xEA80 => 0xE0F0, // Lightbulb -> lightbulb
        0xEC4A => 0xE9E4, // Speed -> speed
        0xF5B0 => 0xE1C4, // PlaySolid -> play_circle
        _ => cp,
    }
}

/// A strip of `cells` (glyph code point, colour) side by side, each cell `cw` x `ch` px with the glyph `px` high.
fn glyph_strip(cells: &[(u32, [u8; 3])], cw: usize, ch: usize, px: f32) -> Option<win::Rgba> {
    use ab_glyph::Font;
    let font = icon_font()?;
    let (w, mut out) = (cw * cells.len(), vec![0u8; cw * cells.len() * ch * 4]);
    for (k, (cp, rgb)) in cells.iter().enumerate() {
        let id = font.glyph_id(char::from_u32(icon_cp(*cp))?);
        let Some(g) = font.outline_glyph(id.with_scale(font.pt_to_px_scale(px * 0.75).unwrap_or(ab_glyph::PxScale::from(px)))) else { continue };
        let b = g.px_bounds();
        let (ox, oy) = ((k * cw) as f32 + (cw as f32 - b.width()) / 2.0, (ch as f32 - b.height()) / 2.0);
        g.draw(|x, y, c| {
            let (x, y) = ((ox + x as f32) as usize, (oy + y as f32) as usize);
            if x < w && y < ch {
                let i = (y * w + x) * 4;
                out[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], (c.min(1.0) * 255.0) as u8]);
            }
        });
    }
    Some((w, ch, out))
}

/// A lighter copy of a strip, for hovered buttons.
fn lighter((w, h, mut px): win::Rgba) -> win::Rgba {
    for p in px.chunks_mut(4) {
        for c in &mut p[..3] {
            *c = (*c as u16 + (255 - *c as u16) * 2 / 5) as u8;
        }
    }
    (w, h, px)
}

/// The toolbar's icons, in `TB_BUTTONS` order: (Segoe icon code point, colour).
const TB_GLYPHS: [(u32, [u8; 3]); TB_ICONS] = [
    (0xE710, [46, 158, 79]),   // Add URL
    (0xE768, [30, 136, 229]),  // Resume
    (0xE71A, [229, 57, 53]),   // Stop
    (0xE769, [251, 140, 0]),   // Stop All
    (0xE74D, [211, 47, 47]),   // Delete
    (0xE930, [0, 137, 123]),   // Delete Completed
    (0xE713, [126, 87, 194]),  // Options
    (0xE823, [245, 124, 0]),   // Scheduler
    (0xF5B0, [67, 160, 71]),   // Start Queue
    (0xE81D, [198, 40, 40]),   // Stop Queue
    (0xE774, [3, 155, 229]),   // Grabber
    (0xE715, [142, 36, 170]),  // Tell a Friend
];

impl Toolbar {
    fn load(ctx: &egui::Context, _skin: &str, _dark: bool) -> Option<Toolbar> {
        // cells as the toolbar draws them: 56 x 42 px large, 32 x 32 px small
        let normal = glyph_strip(&TB_GLYPHS, 56, 42, 34.0)?;
        let small = glyph_strip(&TB_GLYPHS, 32, 32, 24.0);
        let (hot, small_hot) = (Some(lighter(normal.clone())), small.clone().map(lighter));
        let hot = hot.unwrap_or_else(|| normal.clone());
        let small_hot = small_hot.or_else(|| small.clone());
        let gray = |mut g: win::Rgba| {
            for p in g.2.chunks_mut(4) {
                let l = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
                p.copy_from_slice(&[l, l, l, p[3] / 2]);
            }
            g
        };
        let small_gray = small.clone().map(gray);
        let tex = |n: &str, b: &Option<win::Rgba>| b.as_ref().map(|b| texture(ctx, n, b));
        Some(Toolbar {
            normal: texture(ctx, "tb", &normal),
            hot: texture(ctx, "tbh", &hot),
            disabled: texture(ctx, "tbd", &gray(normal.clone())),
            small: [tex("tbs", &small), tex("tbsh", &small_hot), tex("tbsd", &small_gray)],
        })
    }

    /// The image strip for a button state; falls back to the large strip when there is no small one.
    fn strip(&self, small: bool, enabled: bool, hot: bool) -> egui::TextureId {
        let k = if !enabled { 2 } else if hot { 1 } else { 0 };
        match &self.small[k] {
            Some(t) if small => t.id(),
            _ => [&self.normal, &self.hot, &self.disabled][k].id(),
        }
    }
}

// ---------- rows ----------

#[derive(Clone, Copy, PartialEq)]
enum Which {
    All,
    Unfinished,
    Finished,
}

/// The selected tree node: a status folder (optionally narrowed to a category), grabber projects or a queue.
#[derive(Clone, PartialEq)]
enum Cat {
    Files(Which, Option<String>),
    /// "Grabber projects" (None) or one saved project.
    Grabber(Option<String>),
    /// "Queues" (None) or one queue.
    Queue(Option<u32>),
}

/// Options -> Downloads "If a duplicate download link is added:" in NDM's combo order.
const DUPL: [&str; 4] = [
    "Show a dialog and ask what to do.",
    "Add the duplicate with a numbered file name",
    "Add the duplicate and overwrite the existing file",
    "If existing file is complete, show download complete dialog; otherwise resume it.",
];

/// List columns in NDM's own index order.
#[derive(Clone, Copy, PartialEq)]
enum Col {
    Name,
    Added,
    Q,
    Size,
    Status,
    Left,
    Rate,
    LastTry,
    Desc,
    SaveTo,
    Referer,
    Parent,
}

/// (column, header, default width in px).
const COLS: [(Col, &str, u16); 12] = [
    (Col::Name, "File Name", 150),
    (Col::Added, "Date Added", 65),
    (Col::Q, "Q", 20),
    (Col::Size, "Size", 75),
    (Col::Status, "Status", 65),
    (Col::Left, "Time left", 75),
    (Col::Rate, "Transfer rate", 95),
    (Col::LastTry, "Last Try Date", 65),
    (Col::Desc, "Description", 200),
    (Col::SaveTo, "Save To", 200),
    (Col::Referer, "Referer", 200),
    (Col::Parent, "Parent web page", 200),
];

#[derive(Default)]
struct Row {
    id: u64,
    name: String,
    url: String,
    path: PathBuf,
    queued: bool,
    queue: u32,
    size: Option<u64>,
    done: u64,
    status: Status,
    running: bool,
    speed: f64,
    last_try: i64,
    added: i64,
    desc: String,
    category: String,
    referrer: String,
    parent: String,
}

impl Row {
    fn left(&self) -> Option<u64> {
        let size = self.size?;
        (self.running && self.speed > 0.0).then(|| (size.saturating_sub(self.done) as f64 / self.speed) as u64)
    }
    fn pct(&self) -> Option<f64> {
        self.size.filter(|s| *s > 0).map(|s| self.done as f64 * 100.0 / s as f64)
    }
    fn status_text(&self) -> String {
        match &self.status {
            Status::Complete => tr("Complete"),
            Status::Error(_) => tr("Error"),
            Status::Connecting => tr("Connecting..."),
            _ if self.done == 0 && !self.running => String::new(),
            _ => self.pct().map_or_else(|| human(self.done), |p| format!("{p:.2}%")),
        }
    }
    fn cell(&self, c: Col) -> String {
        match c {
            Col::Name => if self.name.is_empty() { self.url.clone() } else { self.name.clone() },
            Col::Q => if self.queued { "Q".into() } else { String::new() },
            Col::Size => self.size.map(human).unwrap_or_default(),
            Col::Status => self.status_text(),
            Col::Left => self.left().map(time_left).unwrap_or_default(),
            Col::Rate => if self.running { rate(self.speed) } else { String::new() },
            Col::LastTry => date(self.last_try),
            Col::Desc => self.desc.clone(),
            Col::Added => date(self.added),
            Col::SaveTo => self.path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
            Col::Referer => self.referrer.clone(),
            Col::Parent => self.parent.clone(),
        }
    }
}

// ---------- dialogs ----------

type ProbeSlot = Arc<Mutex<Option<Result<Probe, String>>>>;

enum Dialog {
    /// `clip`: opened by the clipboard monitor.
    AddUrl { url: String, auth: bool, login: String, pass: String, clip: bool },
    /// `replace`: "overwrite" duplicate: save over this download's file and drop it from the list.
    /// `auto`: taken over from the browser without being asked (counts toward "cancelled twice").
    /// `early`: Options -> Downloads "Start downloading immediately while displaying" - the download already
    /// running while the dialog is open.
    /// `dir`: folder given on the command line (/p), used instead of the category's.
    FileInfo { info: Info, path: String, probe: ProbeSlot, applied: bool, error: String, hint: String, remember: bool, replace: Option<u64>, auto: bool, early: Option<Arc<engine::Job>>, dir: String },
    /// "Cancelling an automatically started download" (template 302, 0x5037a0).
    Cancelled { site: String, url: String, by_site: bool, by_addr: bool, dont_show: bool },
    /// "The list of address exceptions" (template 303) and its "Add" box (304).
    Exceptions { list: Vec<String>, sel: Option<usize>, ask: bool },
    ExcAdd { addr: String },
    /// "Browser details" (template 297) for every entry with this name.
    BrowserDetails { name: String },
    /// Setting the extension up in one browser ("Google Chrome Integration", template 381).
    Integrate { b: core::Browser },
    /// "Using special keys" (template 229).
    Keys { k: core::Keys },
    /// Browser context menu items (template 334).
    MenuItems { dl: bool, all: bool },
    /// "Customize download panels in browsers" (CEditBrowsersDwnlPanelDlg): web-players page (340) and
    /// selected-links page (348, not implemented).
    Panel { tab: u8, types: Vec<(String, bool)>, min: std::collections::BTreeMap<String, u64>, mini: bool, sel: Option<usize>,
        nocap: bool, drm: bool, smode: u8, sites: Vec<String>, smini: bool, ssel: Option<usize> },
    /// "Enter site name" (template 349) for the selected-links panel's site list.
    PanelSite { name: String },
    /// Add file type (template 341) to the panel's list.
    PanelType { name: String },
    /// Minimum size (template 488) for a panel file type.
    PanelMin { ext: String, kb: String },
    /// Download panel exceptions (template 370).
    PanelExc { text: String },
    /// `min`: start minimised (Start view "Show minimized"), cleared after the first frame.
    Progress { id: u64, tab: u8, details: bool, min: bool },
    /// "Customize download progress dialog" (template 367) from Options -> Downloads.
    ProgCustom { view: u8, sl: bool, c: bool, btns: bool },
    Complete { id: u64, dont_show: bool },
    Delete { ids: Vec<u64>, files: bool, dont_show: bool },
    Options { tab: u8, s: Settings },
    /// `queue` is the edited copy of the queue selected in the tree; Apply stores it.
    Scheduler { tab: u8, queue: core::Queue, qsel: Option<u64>, error: String },
    /// Duplicate download link (template 298): the new link and the download already in the list.
    Duplicate { inc: Incoming, old: u64, choice: u8, remember: bool },
    /// File Properties (template 135) for a finished download.
    FileProps { id: u64, url: String, desc: String, referer: String, login: String, pass: String },
    /// NDM categories (template 204). `orig` = name of the edited category, None when adding.
    Category { orig: Option<String>, cat: core::Category, folder: String, error: String },
    /// "Do you really want to delete ... category" confirmation.
    CatDelete { name: String },
    /// Advanced proxy settings (template 369).
    AdvProxy { adv: core::AdvProxy },
    /// A plain message box.
    Info { title: String, text: String },
    /// Connection page's "Networks...": the networks a download is spread over; `off` = unticked.
    Networks { nics: Vec<net::Nic>, off: Vec<String> },
    /// Download limits exceeded! (template 194).
    QuotaHit { text: String },
    /// "Enter queue name" (template 310). `files` are moved into the new queue.
    QueueName { name: String, files: Vec<u64> },
    /// `letters`: "Replace asterisk to Letters" with `lf`..=`lt`; `auth` puts login:password into every address.
    Batch { url: String, from: u32, to: u32, width: u32, letters: bool, lf: String, lt: String, auth: bool, login: String, pass: String },
    /// Queue selection panel (template 345) after "Download Later" or a batch: queue name, "Start queue
    /// processing", "Don't ask me again"; `batch` says which Options tick the last one clears.
    QueuePick { ids: Vec<u64>, q: String, start: bool, never: bool, batch: bool },
    /// "Saving the project of NDM Grabber" (template 269); `orig` = renaming that project (no `proj`).
    GrabSave { name: String, orig: Option<String>, quiet: bool, proj: Option<Box<core::GrabProject>>, sched: bool },
    /// "To use project scheduler you should save the project at first. Save the project now?"
    /// 0x4e8c80, Yes / No) for Schedule the project on an unsaved project.
    GrabSchedAsk { proj: Box<core::GrabProject> },
    /// "Schedule the grabber project" (template 284), edited copy.
    GrabSched { p: Box<core::GrabProject> },
    /// "Connecting to the site" (template 279): the start page wants a login.
    GrabLogin { realm: String, site: String, user: String, pass: String, save: bool },
    /// "Export download list" (template 191): 0 queue, 1 selected, 2 all; then a text or .ef2 file.
    Export { ef2: bool, mode: u8 },
    /// "Virus checking" (template 494) from Options -> Downloads.
    Virus { exe: String, args: String },
    /// "NDM is waiting for the new address to resume the file" + Stop waiting (template 344,
    /// CMsgWaitingNewLinkDlg 0x4fee60) while "Refresh download address" waits for the browser.
    WaitLink { name: String },
    /// NDM's help (`help\*.htm`): the page shown, an anchor to scroll to once, pages visited (Back / Forward)
    /// and the search box.
    Help { page: String, anchor: Option<String>, back: Vec<String>, fwd: Vec<String>, search: String },
    /// "Dial up" (template 184, CDialUpProgress): `Core::ensure_online`'s status, Redial now and Cancel.
    DialUp,
    /// The Site Grabber: wizard `step` (0 start page, 1 save files to, 2 explorer filters, 3 file filters, NDM's
    /// order); `adv` = the page's "Advanced >>" half shown; with `crawl` the results window, `node` = its tree
    /// selection (0 Thumbnails View, 1 All Files, 2 Link View, 3 Folder View, 4 / 5 main-domain / other
    /// sites, 10 + k the k-th page under Link View).
    Grabber { step: u8, adv: bool, node: usize, opts: grabber::Opts, save_mode: u8, save_cat: String, folder: String, crawl: Option<Arc<grabber::Crawl>> },
    /// "NDM Grabber Statistics" (template 271), from the grabber toolbar's last button.
    GrabStats { crawl: Arc<grabber::Crawl> },
    /// "Template Name" (template 268) for Project -> "Save current settings as a template"; `replace` =
    /// asking "The template with such name already exists..." (20603).
    GrabTpl { name: String, proj: Box<core::GrabProject>, replace: bool },
    /// "Load project" (template 270): the saved projects.
    GrabLoad { sel: Option<usize> },
    /// "Settings of NDM Grabber" (template 262), edited copy.
    GrabSettings { explore: String, dl: String, link: bool, iec: bool, adv: bool },
    /// "Manual login page": the page is open in the browser; OK explores with the browser's cookies.
    GrabManual { url: String },
    About,
    /// "Customize Toolbar": the edited button order (-1 = separator) and the selected rows of both lists.
    TbCustomize { layout: Vec<i8>, avail: Option<usize>, cur: Option<usize> },
    /// Tip of the Day (template 105, CTipDlg): the tip shown.
    Tip { text: String },
    /// Speed Limiter settings (template 337).
    Limiter { kb: String, startup: bool },
    /// Columns (template 209): in display order.
    Columns { list: Vec<(u8, bool, u16)>, sel: usize, width: String },
    /// "Max. connections number for a server" (template 226); `orig` = edited row of the exceptions.
    ServerConn { orig: Option<usize>, server: String, n: String },
    /// "Quick Update" (template 182): checking for a new version (`quiet`: the automatic check, which
    /// says nothing unless there is one), or `get`ting it after "Update now" (bytes, total, result).
    Update { slot: Arc<Mutex<Option<Result<Option<Release>, String>>>>, quiet: bool },
    /// "New version of New Download Manager is available" (template 144).
    /// `get` while "Update now" downloads: (bytes, total, result).
    NewVersion { rel: Release, get: Option<(Arc<std::sync::atomic::AtomicU64>, Arc<std::sync::atomic::AtomicU64>, Arc<Mutex<Option<Result<(), String>>>>)> },
    /// Find file (template 506, 0x4b0b10): search text and NDM's dwSearchFlags.
    Find { text: String, flags: u32 },
}

struct App {
    core: Arc<Core>,
    rx: Receiver<Incoming>,
    cat: Cat,
    sel: HashSet<u64>,
    anchor: Option<u64>,
    sort: (Col, bool),
    /// Titles of this frame's dialogs that NDM keeps light in dark mode (their title bars).
    light_titles: Vec<String>,
    /// Find Next starts at this row of the list, and the row to scroll to.
    find_pos: usize,
    scroll_to: Option<usize>,
    show_cats: bool,
    dialogs: Vec<(u64, Dialog)>,
    /// Where each open dialog first appeared: centred on the main window.
    dlg_pos: HashMap<u64, egui::Pos2>,
    serial: u64,
    quitting: bool,
    /// "Refresh download address": the download waiting for a fresh link from the browser.
    refreshing: Option<u64>,
    /// Login typed in "Connecting to the site" for the grabber to explore again with.
    grab_auth: Option<(String, String)>,
    /// Manual login page done: the browser's cookies for the grabber to explore with.
    grab_cookies: Option<String>,
    /// Project -> Load... (template 270): the project the grabber window opens next.
    grab_load: Option<String>,
    /// The name the open grabber project was just saved under (Save / save before scheduling).
    grab_saved_as: Option<String>,
    /// Grabber Thumbnails View: decoded pictures (None = not a picture).
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// The grabber's "Add Filter" popup text.
    grab_filter_buf: String,
    /// Explorer's type names by extension (grabber "File Type" column).
    type_names: HashMap<String, String>,
    /// Whether the drop target window was last shown (to notice it being closed from its own menu).
    drop_shown: bool,
    /// OK in "Advanced proxy settings": handed to the open Options window.
    adv_done: Option<core::AdvProxy>,
    /// Changes made by dialogs opened from Options, applied to its edited copy (or the settings if it's closed).
    opt_patch: Vec<Box<dyn FnOnce(&mut Settings)>>,
    /// OK in the 304 "Add" box: handed to the open 303 list.
    exc_add: Option<String>,
    /// Results of the 341 / 488 boxes, handed to the open download panel dialog.
    panel_add: Option<String>,
    /// Site typed in "Enter site name" (349) for the open download panel settings.
    panel_site: Option<String>,
    panel_min: Option<(String, u64)>,
    /// "Add version..." in Browser details: handed to the open Options window.
    browser_add: Vec<core::Browser>,
    /// Automatically taken over addresses the user cancelled, and how often (template 302 after the second).
    cancels: HashMap<String, u8>,
    clip_seq: Option<u32>,
    /// Expanded tree nodes ("all", "unf", "fin", "q").
    expanded: HashSet<String>,
    toolbar: Option<Toolbar>,
    /// (skin, dark) the toolbar strips were loaded for
    tb_key: (String, bool),
    icons: HashMap<String, Option<egui::TextureHandle>>,
    hwnd: Arc<AtomicIsize>,
    /// The tray icon, its Speed Limiter item and the other items with their English text (relabelled on a
    /// language change).
    tray: Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)>,
    /// Per-download "Options on completion": (exit app, turn off computer).
    on_done: HashMap<u64, (bool, bool)>,
    res: Res,
    dark: bool,
    theme_check: u32,
    /// Languages\ndm_*.lng: (file stem, name), read once.
    langs: Vec<(String, String)>,
}

/// Classic Windows look: Tahoma, square corners. Light = NDM's default system colours.
/// Dark = NDM's "Dark Mode support" (Windows dark app mode, DarkMode_Explorer / ItemsView themes).
/// Colours from dark palette object (ctor 0x437aa0): dialogs and lists 0x393939,
/// text white, disabled 0x808080; tree 0x202020 (0x437df0); menu 0x202020, hot item 0x505050 (0x4ff150).
/// Edit fields are drawn by Windows' DarkMode_CFD theme, not 25,25,25 is an estimate.
/// Set when the Language menu was first opened: `style` then loads the CJK fonts the language names need.
static ALL_CJK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The list font's face (View -> Font), read by `style`.
static LIST_FONT: Mutex<String> = Mutex::new(String::new());

fn style(ctx: &egui::Context, dark: bool) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [("tahoma", r"C:\Windows\Fonts\tahoma.ttf"), ("segoe", r"C:\Windows\Fonts\segoeui.ttf")] {
        if let Ok(b) = std::fs::read(file) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, name.into());
            break;
        }
    }
    // Linux: egui's built-in font stays first (the dialogs are laid out for its widths); the desktop's
    // sans-serif and DejaVu Sans fill in the symbols it lacks (arrows, ticks) and other scripts
    #[cfg(not(windows))]
    {
        let mut seen = vec![];
        for (name, face) in [("sys", "sans-serif"), ("sys_fb", "DejaVu Sans")] {
            let Some(f) = win::font_file(face).filter(|f| !seen.contains(f)) else { continue };
            if let Ok(b) = std::fs::read(&f) {
                fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
                fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push(name.into());
                seen.push(f);
            }
        }
    }
    // Fallbacks for scripts Tahoma lacks: Segoe UI (Georgian...), Nirmala (Hindi), and the CJK font of the
    // chosen language (loaded only then; the files are large).
    let cjk = match i18n::current().as_str() {
        "ndm_zh" => r"C:\Windows\Fonts\msyh.ttc",
        "ndm_zh_tw" => r"C:\Windows\Fonts\msjh.ttc",
        "ndm_ja" => r"C:\Windows\Fonts\YuGothM.ttc",
        "ndm_ko" => r"C:\Windows\Fonts\malgun.ttf",
        _ => "",
    };
    // once the Language menu was opened: Chinese (both scripts) and Korean for the language names
    let (zh, ko) = if ALL_CJK.load(Relaxed) { (r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\malgun.ttf") } else { ("", "") };
    for (name, file) in [("segoe_fb", r"C:\Windows\Fonts\segoeui.ttf"), ("nirmala", r"C:\Windows\Fonts\Nirmala.ttf"), ("cjk", cjk), ("zh", zh), ("ko", ko)] {
        if let Ok(b) = std::fs::read(file) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(b)));
            fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push(name.into());
        }
    }
    // NDM's main window (menu bar, toolbar labels, tree, list) uses the system font, Segoe UI 9pt
    let mut ui_fam: Vec<String> = fonts.font_data.contains_key("segoe_fb").then(|| "segoe_fb".to_string()).into_iter().collect();
    ui_fam.extend(fonts.families[&egui::FontFamily::Proportional].iter().filter(|f| *f != "segoe_fb").cloned());
    fonts.families.insert(egui::FontFamily::Name("segoe".into()), ui_fam);
    // the download list: the chosen face first, the usual fallbacks after it
    let mut list_fam = fonts.families[&egui::FontFamily::Proportional].clone();
    let face = LIST_FONT.lock().unwrap().clone();
    if let Some(b) = win::font_file(&face).filter(|_| !face.is_empty()).and_then(|f| std::fs::read(f).ok()) {
        fonts.font_data.insert("list".into(), Arc::new(egui::FontData::from_owned(b)));
        list_fam.insert(0, "list".into());
    }
    fonts.families.insert(egui::FontFamily::Name("list".into()), list_fam);
    ctx.set_fonts(fonts);
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
    // both themes are set up: NDM keeps most dialogs light while the main window is dark (`dialog`)
    for dark in [false, true] {
        let (text, chrome, field) = if dark {
            (Color32::WHITE, DARK_DLG, Color32::from_rgb(25, 25, 25))
        } else {
            (Color32::BLACK, Color32::from_rgb(240, 240, 240), Color32::WHITE)
        };
        let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        (v.panel_fill, v.window_fill, v.extreme_bg_color) = (chrome, chrome, field);
        v.faint_bg_color = if dark { Color32::from_gray(30) } else { Color32::from_gray(248) };
        v.override_text_color = Some(text);
        v.window_corner_radius = egui::CornerRadius::ZERO;
        v.menu_corner_radius = egui::CornerRadius::ZERO;
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow { offset: [2, 2], blur: 4, spread: 0, color: Color32::from_black_alpha(60) };
        v.selection.bg_fill = if dark { Color32::from_gray(77) } else { Color32::from_rgb(204, 232, 255) };
        v.selection.stroke = egui::Stroke::new(1.0_f32, text);
        v.hyperlink_color = if dark { Color32::from_rgb(96, 170, 255) } else { BAR };
        let w = &mut v.widgets;
        let states: [(&mut egui::style::WidgetVisuals, Color32, Color32); 5] = if dark {
            [
                (&mut w.noninteractive, chrome, Color32::from_gray(70)),
                (&mut w.inactive, Color32::from_gray(51), Color32::from_gray(155)),
                (&mut w.hovered, Color32::from_gray(69), Color32::from_gray(155)),
                (&mut w.active, Color32::from_gray(85), Color32::from_gray(180)),
                (&mut w.open, Color32::from_gray(69), Color32::from_gray(155)),
            ]
        } else {
            [
                (&mut w.noninteractive, chrome, Color32::from_gray(200)),
                (&mut w.inactive, Color32::from_rgb(225, 225, 225), Color32::from_rgb(173, 173, 173)),
                (&mut w.hovered, Color32::from_rgb(229, 241, 251), Color32::from_rgb(0, 120, 215)),
                (&mut w.active, Color32::from_rgb(204, 228, 247), Color32::from_rgb(0, 84, 153)),
                (&mut w.open, Color32::from_rgb(204, 228, 247), Color32::from_rgb(0, 84, 153)),
            ]
        };
        for (wv, fill, stroke) in states {
            wv.corner_radius = egui::CornerRadius::ZERO;
            (wv.bg_fill, wv.weak_bg_fill) = (fill, fill);
            wv.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
            wv.fg_stroke.color = text;
            wv.expansion = 0.0;
        }
        ctx.set_visuals_of(if dark { egui::Theme::Dark } else { egui::Theme::Light }, v);
    }
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = Vec2::new(10.0, 3.0);
        s.spacing.item_spacing = Vec2::new(6.0, 4.0);
        s.spacing.interact_size.y = 18.0;
        // Tahoma 8 as NDM's dialogs, tree and list draw it (fitted to text widths measured on NDM; the
        // menu bar is Segoe UI 12.2, the toolbar labels Segoe UI 11.6)
        for (t, size) in [(egui::TextStyle::Body, 10.0), (egui::TextStyle::Button, 10.0), (egui::TextStyle::Small, 10.0), (egui::TextStyle::Heading, 14.0)] {
            s.text_styles.insert(t, FontId::proportional(size));
        }
    });
}

/// "Start positions and download progress by connections" bar: one filled span per segment.
/// "Start positions and download progress by connections" (static 1179). With NDM's bar bitmaps (358 ends,
/// 359 background, 361 downloaded, 362 start marker: 15 px columns stretched across) it is drawn
/// draws it: the background, each connection's downloaded run in blue and a red mark where it started.
fn segment_bar(ui: &mut egui::Ui, info: &Info, h: f32, bars: Option<[egui::TextureId; 4]>) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    let c = dlu::pal(ui);
    let p = ui.painter();
    if let Some([ends, bg, blue, red]) = bars {
        let ppp = ui.ctx().pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let px = 1.0 / ppp;
        let top = snap(rect.center().y - 7.5 / ppp);
        let bar = egui::Rect::from_min_max(egui::pos2(snap(rect.left()), top), egui::pos2(snap(rect.right()), top + 15.0 / ppp));
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let col = |t: egui::TextureId, a: f32, b: f32| p.image(t, egui::Rect::from_x_y_ranges(snap(a)..=snap(b).max(snap(a) + px), bar.y_range()), uv, Color32::WHITE);
        col(bg, bar.left(), bar.right());
        col(ends, bar.left(), bar.left() + px);
        col(ends, bar.right() - px, bar.right());
        let Some(size) = info.size.filter(|s| *s > 0) else { return };
        let inner = bar.shrink2(Vec2::new(px, 0.0));
        let x = |b: u64| inner.left() + inner.width() * (b as f32 / size as f32);
        for s in &info.segs {
            let done = if info.status == Status::Complete { s.stop.unwrap_or(s.next) } else { s.next };
            if done > s.start {
                col(blue, x(s.start) + px, x(done));
            }
            col(red, x(s.start), x(s.start) + px);
        }
        return;
    }
    p.rect_filled(rect, 0.0, c.field);
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, c.edge), egui::StrokeKind::Inside);
    let Some(size) = info.size.filter(|s| *s > 0) else { return };
    let inner = rect.shrink(1.0);
    let x = |b: u64| inner.left() + inner.width() * (b as f32 / size as f32);
    for s in &info.segs {
        let done = if info.status == Status::Complete { s.stop.unwrap_or(s.next) } else { s.next };
        p.rect_filled(egui::Rect::from_x_y_ranges(x(s.start)..=x(done), inner.y_range()), 0.0, BAR);
    }
}

/// NDM's icons (see `icon_font`), keyed by the slot numbers the UI asks for.
#[derive(Default)]
struct Res {
    tree: HashMap<u16, egui::TextureHandle>,
    icons: HashMap<u16, egui::TextureHandle>,
    /// Download progress connection bar: ends, background, downloaded, start marker (358, 359, 361, 362).
    bars: Option<[egui::TextureHandle; 4]>,
    /// Grabber: the wizard's picture and toolbar
    /// 260's 24 px strips (bitmaps 265 normal, 266 hot, and a greyed copy).
    grab_pic: Option<egui::TextureHandle>,
    grab_tb: Option<[egui::TextureHandle; 3]>,
}

impl Res {
    fn load(ctx: &egui::Context) -> Res {
        let mut r = Res::default();
        if let Ok(img) = image::load_from_memory_with_format(include_bytes!("../assets/grabber.jpg"), image::ImageFormat::Jpeg) {
            let px = img.to_rgba8();
            r.grab_pic = Some(ctx.load_texture("grabpic", egui::ColorImage::from_rgba_unmultiplied([px.width() as usize, px.height() as usize], px.as_raw()), egui::TextureOptions::LINEAR));
        }
        let tex = |name: &str, i: Option<win::Rgba>| i.map(|i| texture(ctx, name, &i));
        // tree images (drawn 16 x 13 pt): folders, statuses, categories, grabber projects, queues, limits
        const FOLDER: [u8; 3] = [240, 170, 30];
        let tree: [(u16, u32, [u8; 3]); 20] = [
            (214, 0xE838, FOLDER), (215, 0xE8B7, FOLDER), (216, 0xE930, [46, 158, 79]), (217, 0xE930, [46, 158, 79]),
            (218, 0xE896, [30, 136, 229]), (219, 0xE896, [30, 136, 229]), (220, 0xE8D6, [216, 27, 96]), (221, 0xE714, [229, 57, 53]),
            (222, 0xE756, [84, 110, 122]), (223, 0xE8A5, [30, 136, 229]), (224, 0xE8B7, FOLDER), (225, 0xE7B8, [141, 110, 99]),
            (282, 0xE774, [3, 155, 229]), (311, 0xE8FD, [94, 53, 177]), (312, 0xE8FD, [67, 160, 71]), (313, 0xE8FD, [94, 53, 177]),
            (315, 0xE895, [0, 137, 123]), (316, 0xE895, [67, 160, 71]), (329, 0xEC4A, [251, 140, 0]), (104, 0xEA80, [251, 192, 45]),
        ];
        for (id, cp, rgb) in tree {
            let (w, h, px) = if id == 104 { (48, 48, 40.0) } else { (32, 26, 24.0) };
            if let Some(t) = tex(&format!("ndmt{id}"), glyph_strip(&[(cp, rgb)], w, h, px)) {
                r.tree.insert(id, t);
            }
        }
        // grabber toolbar (10 icons, drawn 24 pt): explore, stop, check all, uncheck, download, stop download,
        // add to the main list, refresh, schedule, statistics
        let gtb = [(0xE721, [30, 136, 229]), (0xE71A, [229, 57, 53]), (0xE73A, [46, 158, 79]), (0xE739, [117, 117, 117]), (0xE896, [30, 136, 229]),
            (0xE81D, [198, 40, 40]), (0xE8F4, [245, 124, 0]), (0xE72C, [0, 137, 123]), (0xE823, [245, 124, 0]), (0xE946, [94, 53, 177])];
        if let Some(n) = glyph_strip(&gtb, 48, 48, 34.0) {
            let mut g = n.clone();
            for p in g.2.chunks_mut(4) {
                let l = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
                p.copy_from_slice(&[l, l, l, p[3] / 2]);
            }
            r.grab_tb = Some([texture(ctx, "gtbn", &n), texture(ctx, "gtbh", &lighter(n.clone())), texture(ctx, "gtbg", &g)]);
        }
        // 32 pt page and dialog icons: Options pages, site logins, dial up, sounds, queues, grabber schedule
        let pages: [(u16, u32, [u8; 3]); 11] = [
            (277, 0xE774, [3, 155, 229]), (295, 0xE8A5, [30, 136, 229]), (276, 0xE8B7, FOLDER), (293, 0xE896, [46, 158, 79]),
            (273, 0xE9D9, [94, 53, 177]), (278, 0xE8AF, [0, 137, 123]), (272, 0xE8D7, [251, 140, 0]), (275, 0xE7EF, [84, 110, 122]),
            (274, 0xE8D6, [216, 27, 96]), (331, 0xE8FD, [94, 53, 177]), (285, 0xE823, [245, 124, 0]),
        ];
        for (id, cp, rgb) in pages {
            if let Some(t) = tex(&format!("ndmi{id}"), glyph_strip(&[(cp, rgb)], 64, 64, 52.0)) {
                r.icons.insert(id, t);
            }
        }
        r
    }
    fn tree(&self, id: u16) -> Option<egui::TextureId> {
        self.tree.get(&id).map(|t| t.id())
    }
    fn icon(&self, id: u16) -> Option<egui::TextureId> {
        self.icons.get(&id).map(|t| t.id())
    }
}

impl App {
    fn new(ctx: &egui::Context) -> App {
        let core = Core::load();
        let on = core.settings().adv_integration;
        core.apply_integration(on);
        let dark =core.settings().dark_support && win::system_dark();
        *LIST_FONT.lock().unwrap() = core.settings().list_font.clone();
        i18n::load(&core.settings().language.clone());
        style(ctx, dark);
        let hwnd = Arc::new(AtomicIsize::new(0));
        let (tx, rx) = channel();
        if let Some(inc) = core::cli_args(&std::env::args().skip(1).collect::<Vec<_>>()) {
            let _ = tx.send(inc);
        }
        let c = ctx.clone();
        core.spawn_background(move || c.request_repaint());
        let (c, h) = (ctx.clone(), hwnd.clone());
        core.spawn_server(tx, move || {
            // only the download's dialog shows; the main window stays in the tray (win::pump)
            win::wake(h.load(Relaxed));
            c.request_repaint();
        });
        let tray = App::tray(ctx, &core, &hwnd);
        let expanded = core.settings().expanded.iter().cloned().collect();
        let mut app = App {
            core,
            rx,
            cat: Cat::Files(Which::All, None),
            expanded,
            refreshing: None,
            grab_auth: None,
            grab_cookies: None,
            grab_load: None,
            grab_saved_as: None,
            thumbs: HashMap::new(),
            grab_filter_buf: String::new(),
            type_names: HashMap::new(),
            drop_shown: false,
            adv_done: None,
            opt_patch: Vec::new(),
            exc_add: None,
            browser_add: Vec::new(),
            panel_add: None,
            panel_site: None,
            panel_min: None,
            cancels: HashMap::new(),
            clip_seq: None,
            sel: HashSet::new(),
            anchor: None,
            sort: (Col::Added, true),
            light_titles: Vec::new(),
            find_pos: 0,
            scroll_to: None,
            show_cats: true,
            dialogs: vec![],
            dlg_pos: HashMap::new(),
            serial: 0,
            quitting: false,
            toolbar: None,
            tb_key: (String::new(), false),
            icons: HashMap::new(),
            hwnd,
            tray,
            on_done: HashMap::new(),
            res: Res::load(ctx),
            dark,
            theme_check: 0,
            langs: i18n::list(),
        };
        if app.core.settings().tips_startup && !tray_start() {
            let text = app.next_tip();
            app.open(Dialog::Tip { text });
        }
        // NDM checks for a new release every 2 weeks
        let checked = app.core.settings().update_checked;
        if !UPDATE_REPO.is_empty() && chrono::Local::now().timestamp() - checked > 14 * 86400 {
            app.check_updates(ctx, true);
        }
        // Windows hooks drops up once it has the window (update); the Linux drop target only needs a repaint
        #[cfg(not(windows))]
        {
            let c = ctx.clone();
            win::accept_drops(0, move || c.request_repaint());
        }
        app
    }

    /// Linux: an AppIndicator on the GTK thread (tray-icon needs GTK's loop and keeps its items there), only
    /// where the desktop shows one; else closing quits. Sets LINUX_TRAY; the items stay on that thread
    /// (`LIMIT_ITEM` for the Speed Limiter tick).
    #[cfg(not(windows))]
    fn tray(ctx: &egui::Context, core: &Arc<Core>, _: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let style = core.settings().tray_icon;
        // without the indicator library tray-icon panics on the GTK thread, which aborts NDM
        if style == 3 || !win::tray_host() || !win::indicator_lib() {
            return None;
        }
        let (core, ctx) = (core.clone(), ctx.clone());
        win::gtk_spawn(move || {
            let open = MenuItem::new(i18n::tr("Open"), true, None);
            let startq = MenuItem::new(i18n::tr("Start queue"), true, None);
            let stopq = MenuItem::new(i18n::tr("Stop queue"), true, None);
            let limit = CheckMenuItem::new(i18n::tr("Speed Limiter"), true, core.settings().limit_on, None);
            let exit = MenuItem::new(i18n::tr("Exit"), true, None);
            let menu = Menu::new();
            if menu.append_items(&[&open, &PredefinedMenuItem::separator(), &startq, &stopq, &limit, &PredefinedMenuItem::separator(), &exit]).is_err() {
                return;
            }
            let Some(tray) = tray_picture(style).and_then(|i| TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(TITLE).with_icon(i).build().ok()) else {
                return;
            };
            LINUX_TRAY.store(true, Relaxed);
            let ids = (open.id().clone(), startq.id().clone(), stopq.id().clone(), limit.id().clone(), exit.id().clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let (open, startq, stopq, limit, exit) = &ids;
                if e.id == *open {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                } else if e.id == *startq {
                    core.start_queue(core::MAIN_QUEUE);
                } else if e.id == *stopq {
                    core.stop_queue(core::MAIN_QUEUE);
                } else if e.id == *limit {
                    let on = !core.settings().limit_on;
                    core.settings().limit_on = on;
                    core.apply_settings();
                } else if e.id == *exit {
                    core.shutdown();
                    std::process::exit(0);
                }
                ctx.request_repaint();
            }));
            LIMIT_ITEM.with(|l| *l.borrow_mut() = Some(limit));
            std::mem::forget(tray); // lives as long as the app
        });
        None
    }

    /// Notification-area icon. Its handlers run on the UI thread's message loop even while
    /// the window is hidden, so they act on Core and the HWND directly.
    #[cfg(windows)]
    fn tray(ctx: &egui::Context, core: &Arc<Core>, hwnd: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let open = MenuItem::new(&i18n::tr_os("Open"), true, None);
        let startq = MenuItem::new(&i18n::tr_os("Start queue"), true, None);
        let stopq = MenuItem::new(&i18n::tr_os("Stop queue"), true, None);
        let limit = CheckMenuItem::new(&i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
        let exit = MenuItem::new(&i18n::tr_os("Exit"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &startq, &stopq, &limit, &PredefinedMenuItem::separator(), &exit]).ok()?;
        let style = core.settings().tray_icon;
        let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(TITLE).with_icon(tray_picture(style)?).build().map_err(|e| eprintln!("tray: {e}")).ok()?;
        let _ = tray.set_visible(style != 3);

        let ids = (open.id().clone(), startq.id().clone(), stopq.id().clone(), limit.id().clone(), exit.id().clone());
        let (core2, hwnd2, ctx2) = (core.clone(), hwnd.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let (open, startq, stopq, limit, exit) = &ids;
            if e.id == *open {
                win::show(hwnd2.load(Relaxed));
            } else if e.id == *startq {
                core2.start_queue(core::MAIN_QUEUE);
            } else if e.id == *stopq {
                core2.stop_queue(core::MAIN_QUEUE);
            } else if e.id == *limit {
                let on = !core2.settings().limit_on;
                core2.settings().limit_on = on;
                core2.apply_settings();
            } else if e.id == *exit {
                win::exit_splash(&i18n::tr_os("Exit NDM"), &i18n::tr_os("Saving settings..."));
                core2.shutdown();
                std::process::exit(0);
            }
            ctx2.request_repaint();
        }));
        let (hwnd3, ctx3) = (hwnd.clone(), ctx.clone());
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if matches!(e, TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } | TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) {
                win::show(hwnd3.load(Relaxed));
                ctx3.request_repaint();
            }
        }));
        let items = vec![(open, "Open"), (startq, "Start queue"), (stopq, "Stop queue"), (exit, "Exit")];
        Some((tray, limit, items))
    }

    /// Cached Explorer icon for a file extension ("/" = folder).
    fn icon(&mut self, ctx: &egui::Context, ext: &str) -> Option<egui::TextureId> {
        self.icons
            .entry(ext.to_string())
            .or_insert_with(|| win::shell_icon(ext, ext == "/").map(|i| texture(ctx, &format!("icon{ext}"), &i)))
            .as_ref()
            .map(|t| t.id())
    }

    fn exe_icon(&mut self, ctx: &egui::Context, exe: &str) -> Option<egui::TextureId> {
        let key = format!("exe:{exe}");
        self.icons
            .entry(key.clone())
            .or_insert_with(|| win::exe_icon(Path::new(exe)).map(|i| texture(ctx, &key, &i)))
            .as_ref()
            .map(|t| t.id())
    }

    fn add_url(&mut self) {
        let url = links(&clipboard()).into_iter().next().unwrap_or_default();
        self.open(Dialog::AddUrl { url, auth: false, login: String::new(), pass: String::new(), clip: false });
    }

    /// "Tell a friend": a ready-to-send letter in the default mail program.
    fn tell_friend() {
        let enc = |t: &str| percent_encoding::utf8_percent_encode(t, percent_encoding::NON_ALPHANUMERIC).to_string();
        let body = "Hi,\r\n\r\nI use New Download Manager to speed up my downloads. It splits files into several connections, \
resumes broken downloads, has a scheduler, a site grabber and a browser button for videos.\r\n\r\nGive it a try!";
        open_url(&format!("mailto:?subject={}&body={}", enc("A download manager you should try"), enc(body)));
    }

    fn grabber(&mut self) {
        let opts = self.new_grab_opts();
        let folder = self.core.settings().base_dir.clone();
        self.open(Dialog::Grabber { step: 0, adv: false, node: 1, opts, save_mode: 0, save_cat: GENERAL.into(), folder, crawl: None });
    }

    /// Add the grabber's checked files to the list (queued); `start` also starts the queue.
    fn grab(&mut self, crawl: &grabber::Crawl, opts: &grabber::Opts, save_mode: u8, save_cat: &str, folder: &str, start: bool) {
        self.core.grab_files(&mut crawl.state.lock().unwrap().files, opts, save_mode, save_cat, folder, start);
    }

    /// Explore with the wizard's settings in the background.
    fn crawl(&self, ctx: &egui::Context, opts: &grabber::Opts) -> Arc<grabber::Crawl> {
        let c = Arc::new(grabber::Crawl::default());
        let (o, a, c2, ctx2, t) = (opts.clone(), self.core.agent(&opts.start), c.clone(), ctx.clone(), self.core.grab_tuning());
        std::thread::spawn(move || {
            grabber::run(o, a, c2, t);
            ctx2.request_repaint();
        });
        c
    }

    /// Tree menu of a saved grabber project: 0 open, 1 schedule, 2 rename, 3 delete.
    fn project_cmd(&mut self, k: usize, name: &str) {
        let Some(p) = self.core.settings().grab_projects.iter().find(|p| p.opts.name == name).cloned() else { return };
        match k {
            0 => {
                // the results window with what the project found last time
                let c = Arc::new(grabber::Crawl::default());
                {
                    let mut st = c.state.lock().unwrap();
                    (st.files, st.pages, st.done) = (p.files.clone(), p.pages, true);
                    st.status = format!("Done. {} web pages processed, {} files found", p.pages, p.files.len()); // as grabber::run says it
                }
                c.auto_started.store(true, Relaxed);
                self.open(Dialog::Grabber { step: 0, adv: false, node: 1, opts: p.opts, save_mode: p.save_mode, save_cat: p.save_cat, folder: p.folder, crawl: Some(c) });
            }
            1 => self.open(Dialog::GrabSched { p: Box::new(p) }),
            2 => self.open(Dialog::GrabSave { name: name.to_string(), orig: Some(name.to_string()), quiet: false, proj: None, sched: false }),
            _ => {
                let mut s = self.core.settings();
                s.grab_projects.retain(|p| p.opts.name != name);
                s.grab_recent.retain(|n| n != name);
                drop(s);
                self.cat = Cat::Grabber(None);
                self.core.save();
            }
        }
        if k == 0 {
            Self::touch_recent(&mut self.core.settings(), name);
        }
    }

    /// Options -> Schedule the project / toolbar: the scheduler (284) for a saved project, else NDM's
    /// "save it first?" question.
    fn grab_schedule(&mut self, proj: core::GrabProject, saved: bool) {
        if saved {
            let name = proj.opts.name.trim().to_string();
            self.project_cmd(1, &name);
        } else {
            self.open(Dialog::GrabSchedAsk { proj: Box::new(proj) });
        }
    }

    /// `name` to the top of Recent Projects (4 kept).
    fn touch_recent(s: &mut Settings, name: &str) {
        s.grab_recent.retain(|n| n != name);
        s.grab_recent.insert(0, name.to_string());
        s.grab_recent.truncate(4);
    }

    /// Store a grabber project under `name` (replacing one of that name, or `orig` when renaming).
    fn save_project(&mut self, name: &str, orig: Option<&str>, proj: Option<core::GrabProject>) {
        let mut s = self.core.settings();
        match proj {
            Some(mut p) => {
                p.opts.name = name.to_string();
                match s.grab_projects.iter_mut().find(|x| x.opts.name == name) {
                    Some(x) => {
                        // keep its schedule
                        (p.sched, p.periodic, p.action) = (x.sched.clone(), x.periodic, x.action);
                        *x = p;
                    }
                    None => s.grab_projects.push(p),
                }
                Self::touch_recent(&mut s, name);
            }
            None => {
                if let Some(x) = s.grab_projects.iter_mut().find(|x| Some(x.opts.name.as_str()) == orig) {
                    x.opts.name = name.to_string();
                }
                for n in s.grab_recent.iter_mut().filter(|n| Some(n.as_str()) == orig) {
                    *n = name.to_string();
                }
                if matches!(&self.cat, Cat::Grabber(Some(c)) if Some(c.as_str()) == orig) {
                    self.cat = Cat::Grabber(Some(name.to_string()));
                }
            }
        }
        drop(s);
        self.core.save();
    }

    /// Check for a new version in the background (Quick Update, template 182).
    fn check_updates(&mut self, ctx: &egui::Context, quiet: bool) {
        if self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::Update { .. } | Dialog::NewVersion { .. })) {
            return;
        }
        let slot: Arc<Mutex<Option<Result<Option<Release>, String>>>> = Arc::default();
        let (s, agent, c) = (slot.clone(), self.core.agent("https://api.github.com/"), ctx.clone());
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(check_update(&agent));
            c.request_repaint();
        });
        self.open(Dialog::Update { slot, quiet });
    }

    fn open_find(&mut self) {
        let (text, flags) = {
            let s = self.core.settings();
            (s.search.clone(), s.search_flags)
        };
        self.open(Dialog::Find { text, flags });
    }

    /// Find Next: select the next matching download from `find_pos` on, wrapping
    /// to the top; "No such files found" when nothing in the list matches.
    fn find_next(&mut self) {
        let (needle, flags) = {
            let s = self.core.settings();
            (s.search.clone(), s.search_flags)
        };
        if needle.is_empty() {
            return self.open_find();
        }
        let rows = self.rows();
        let from = self.find_pos.min(rows.len());
        match (from..rows.len()).chain(0..from).find(|&k| find_hit(flags, &needle, &rows[k])) {
            Some(k) => {
                self.sel = HashSet::from([rows[k].id]);
                self.anchor = Some(rows[k].id);
                (self.find_pos, self.scroll_to) = (k + 1, Some(k));
            }
            None => self.open(Dialog::Info { title: "Find file".into(), text: "No such files found".into() }),
        }
    }

    fn open(&mut self, d: Dialog) {
        if let Dialog::Progress { id, .. } | Dialog::Complete { id, .. } = d {
            let same = |x: &Dialog| matches!(x, Dialog::Progress { id: i, .. } | Dialog::Complete { id: i, .. } if *i == id);
            self.dialogs.retain(|(_, x)| !same(x));
        }
        self.serial += 1;
        self.dialogs.push((self.serial, d));
    }

    /// A link that is already in the list is handled as NDM does (Options -> Downloads, dialog 298).
    fn incoming(&mut self, ctx: &egui::Context, inc: Incoming) {
        if !inc.links.is_empty() {
            return self.all_links(inc);
        }
        if inc.start_queue {
            self.core.start_queue(core::MAIN_QUEUE);
        }
        if inc.url.is_empty() {
            return;
        }
        if inc.silent {
            // /n: no questions; /a only adds it to the queue
            let mut i = self.core.new_info(&inc.url);
            if !inc.folder.is_empty() {
                i.folder = inc.folder.clone();
            }
            i.name = inc.filename.clone();
            i.queued = inc.later;
            let id = i.id;
            let job = self.core.add(i);
            if !inc.later {
                self.core.start(&job);
                if inc.quit {
                    self.on_done.insert(id, (true, false));
                }
                // NDM still shows the progress window for /n downloads
                if self.core.settings().show_progress {
                    self.auto_progress(id);
                }
            }
            return;
        }
        let url = self.core.settings().with_login(&inc.url);
        if let Some(j) = self.refreshing.take().and_then(|id| self.core.find(id)) {
            // the first link after "Refresh download address" becomes the download's new address;
            // the next start re-checks size/ranges and the 64-byte resume check catches a different file
            {
                let mut i = j.info();
                (i.url, i.cookies, i.probed) = (url, inc.cookies, false);
                if !inc.referrer.is_empty() {
                    i.referrer = inc.referrer;
                }
            }
            let id = j.info().id;
            j.stop();
            return self.resume(&[id]);
        }
        let Some(old) = self.core.jobs().iter().map(|j| j.info()).find(|i| i.url == url).map(|i| i.id) else {
            return self.file_info(ctx, inc);
        };
        let (ask, action) = {
            let s = self.core.settings();
            (s.dupl_ask, s.dupl_action)
        };
        if ask {
            self.open(Dialog::Duplicate { inc, old, choice: action, remember: false });
        } else {
            self.duplicate(ctx, inc, old, action);
        }
    }

    /// Browser "Download all links": the page's links in the grabber's file list, files of the
    /// Options -> File types list ticked.
    fn all_links(&mut self, inc: Incoming) {
        let types = self.core.settings().file_types.clone();
        let crawl = Arc::new(grabber::Crawl::default());
        {
            let mut st = crawl.state.lock().unwrap();
            for l in inc.links {
                let name = engine::sanitize(&engine::url_name(&l.url));
                let checked = types.split_whitespace().any(|t| t.trim_start_matches('.').eq_ignore_ascii_case(&core::ext(&name)));
                st.files.push(grabber::Found { url: l.url, name, text: l.text, checked, ..Default::default() });
            }
            (st.pages, st.done) = (1, true);
            st.status = format!("{} links", st.files.len());
        }
        let opts = grabber::Opts { name: "Download all links".into(), start: inc.url, ..Default::default() };
        let folder = self.core.settings().base_dir.clone();
        self.open(Dialog::Grabber { step: 3, adv: false, node: 1, opts, save_mode: 0, save_cat: GENERAL.into(), folder, crawl: Some(crawl) });
    }

    /// NDM's clipboard monitor: a copied address (first line, http/https) of a file type from
    /// Options -> File types opens "Enter new address" retitled (0x407900 reads it, 0x407c90 asks).
    /// The clipboard is only read while watching is on; the text already there when it starts isn't taken.
    fn watch_clipboard(&mut self) {
        if !self.core.settings().clip_watch {
            self.clip_seq = None;
            return;
        }
        let seq = win::clip_seq();
        if self.clip_seq.replace(seq).is_none_or(|old| old == seq) {
            return;
        }
        let text = clipboard();
        let url = text.lines().next().unwrap_or("").trim().to_string();
        // NDM also takes mms:// and rtmp://; the engine speaks http(s) and ftp only
        let web = url.len() > 6 && (["http://", "https://", "ftp://"].iter().any(|p| url.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p))) || stream::is_media_url(&url));
        let ext = core::ext(&engine::url_name(&url));
        let wanted = self.core.settings().file_types.split_whitespace().any(|t| t.trim_start_matches('.').eq_ignore_ascii_case(&ext));
        if web && wanted && !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::AddUrl { .. })) {
            self.open(Dialog::AddUrl { url, auth: false, login: String::new(), pass: String::new(), clip: true });
        }
    }

    fn duplicate(&mut self, ctx: &egui::Context, inc: Incoming, old: u64, action: u8) {
        match action {
            1 => self.file_info(ctx, inc), // unique_name numbers it
            2 => {
                self.file_info(ctx, inc);
                if let (Some(j), Some((_, Dialog::FileInfo { path, replace, .. }))) = (self.core.find(old), self.dialogs.last_mut()) {
                    *path = j.info().path().display().to_string();
                    *replace = Some(old);
                }
            }
            _ => match self.core.find(old) {
                Some(j) if j.info().status == Status::Complete => self.open(Dialog::Complete { id: old, dont_show: false }),
                Some(_) => self.resume(&[old]),
                None => self.file_info(ctx, inc),
            },
        }
    }

    fn file_info(&mut self, ctx: &egui::Context, inc: Incoming) {
        let mut info = self.core.new_info(&inc.url);
        if inc.quit {
            self.on_done.insert(info.id, (true, false));
        }
        info.parent = inc.referrer.clone(); // the extension sends the tab URL
        (info.referrer, info.cookies, info.browser_proxy) = (inc.referrer, inc.cookies, inc.proxy);
        let slot: ProbeSlot = Arc::default();
        let hint = inc.filename.rsplit(['/', '\\']).next().map(engine::sanitize).filter(|n| n != "download").unwrap_or_default();
        let name = if hint.is_empty() { engine::sanitize(&engine::url_name(&info.url)) } else { hint.clone() };
        if !inc.folder.is_empty() {
            info.folder = inc.folder.clone();
        }
        let path = Path::new(&info.folder).join(&name).display().to_string();
        let at_once = self.core.settings().start_at_once;
        let early = at_once.then(|| {
            // downloads into the category's folder under a free name until the dialog is answered
            let mut i = info.clone();
            i.category = self.core.settings().category_for(&name, &i.url);
            i.folder = self.core.settings().folder_for(&i.category);
            i.name = self.core.unique_name(&i.folder, &name);
            let job = engine::Job::new(i);
            self.core.start(&job);
            job
        });
        if early.is_none() {
            let (s, c, agent) = (slot.clone(), ctx.clone(), self.core.agent(&info.url));
            let (url, r, k) = (info.url.clone(), info.referrer.clone(), info.cookies.clone());
            std::thread::spawn(move || {
                *s.lock().unwrap() = Some(engine::probe(&agent, &url, &r, &k));
                c.request_repaint();
            });
        }
        self.open(Dialog::FileInfo { info, path, probe: slot, applied: false, error: String::new(), hint, remember: false, replace: None, auto: !inc.force, early, dir: inc.folder });
    }

    fn resume(&mut self, ids: &[u64]) {
        let show = self.core.settings().show_progress;
        for id in ids {
            let Some(j) = self.core.find(*id) else { continue };
            if !j.running() && j.info().status != Status::Complete {
                self.core.start(&j);
                if show {
                    self.auto_progress(*id);
                }
            }
        }
    }

    /// Progress window for a download just started, as "Start view" says (0x5bbd80 reads PrgrDlgVisiblity).
    fn auto_progress(&mut self, id: u64) {
        let view = self.core.settings().prog_view;
        if view != 2 {
            let details = self.core.settings().prog_details;
            self.open(Dialog::Progress { id, tab: 0, details, min: view == 1 });
        }
    }

    fn stop(&self, ids: &[u64]) {
        ids.iter().filter_map(|id| self.core.find(*id)).for_each(|j| j.stop());
    }

    fn redownload(&mut self, ids: &[u64]) {
        for id in ids {
            if let Some(j) = self.core.find(*id).filter(|j| !j.running()) {
                if j.info().status == Status::Complete {
                    let _ = std::fs::remove_file(j.info().path());
                }
                j.reset();
            }
        }
        self.resume(ids);
    }

    fn remove(&mut self, ids: &[u64], delete_files: bool) {
        let mut jobs = self.core.jobs.lock().unwrap();
        let (gone, keep): (Vec<_>, Vec<_>) = jobs.drain(..).partition(|j| ids.contains(&j.info().id));
        *jobs = keep;
        drop(jobs);
        for j in gone {
            j.stop();
            self.sel.remove(&j.info().id);
            std::thread::spawn(move || {
                while j.running() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                let i = j.info();
                let _ = std::fs::remove_file(i.part());
                let _ = std::fs::remove_dir_all(i.part());
                if delete_files && i.status == Status::Complete {
                    let _ = std::fs::remove_file(i.path());
                }
            });
        }
        self.core.save();
    }


    fn rows(&self) -> Vec<Row> {
        let grab_urls: HashSet<String> = match &self.cat {
            Cat::Grabber(Some(p)) => self.core.settings().grab_projects.iter().filter(|x| x.opts.name == *p).flat_map(|x| x.files.iter().map(|f| f.url.clone())).collect(),
            _ => HashSet::new(),
        };
        let mut rows: Vec<Row> = self
            .core
            .jobs()
            .iter()
            .map(|j| {
                let speed = j.speed();
                let i = j.info();
                Row {
                    id: i.id,
                    name: i.name.clone(),
                    url: i.url.clone(),
                    path: i.path(),
                    queued: i.queued,
                    queue: i.queue,
                    size: i.size,
                    done: i.done(),
                    status: i.status.clone(),
                    running: j.running(),
                    speed,
                    last_try: i.last_try,
                    added: i.added,
                    desc: i.description.clone(),
                    category: i.category.clone(),
                    referrer: i.referrer.clone(),
                    parent: i.parent.clone(),
                }
            })
            .filter(|r| match &self.cat {
                Cat::Files(w, c) => {
                    let st = match w {
                        Which::All => true,
                        Which::Unfinished => r.status != Status::Complete,
                        Which::Finished => r.status == Status::Complete,
                    };
                    st && c.as_ref().is_none_or(|c| r.category == *c)
                }
                Cat::Grabber(None) => false,
                Cat::Grabber(Some(_)) => grab_urls.contains(r.url.as_str()),
                Cat::Queue(q) => r.queued && q.is_none_or(|q| r.queue == q),
            })
            .collect();
        let (col, asc) = self.sort;
        rows.sort_by(|a, b| {
            let o = match col {
                Col::Added => (a.added, a.id).cmp(&(b.added, b.id)),
                Col::Name => a.cell(Col::Name).to_lowercase().cmp(&b.cell(Col::Name).to_lowercase()),
                Col::Q => a.queued.cmp(&b.queued),
                Col::Size => a.size.cmp(&b.size),
                Col::Status => a.pct().unwrap_or(0.0).total_cmp(&b.pct().unwrap_or(0.0)),
                Col::Left => a.left().cmp(&b.left()),
                Col::Rate => a.speed.total_cmp(&b.speed),
                Col::LastTry => a.last_try.cmp(&b.last_try),
                Col::Desc => a.desc.cmp(&b.desc),
                Col::SaveTo => a.path.cmp(&b.path),
                Col::Referer => a.referrer.cmp(&b.referrer),
                Col::Parent => a.parent.cmp(&b.parent),
            };
            if asc { o } else { o.reverse() }
        });
        rows
    }

    fn sort_by(&mut self, c: Col) {
        self.sort = if self.sort.0 == c { (c, !self.sort.1) } else { (c, true) };
    }

    fn add_queued(&self, urls: Vec<String>) -> Vec<u64> {
        let mut ids = Vec::new();
        for u in urls {
            let mut i = self.core.new_info(&u);
            i.queued = true;
            ids.push(i.id);
            self.core.add(i);
        }
        ids
    }

    /// Queue selection panel (template 345) if Options asks for it (Download Later / batch).
    fn pick_queue(&mut self, ids: Vec<u64>, batch: bool) {
        let s = self.core.settings();
        let ask = if batch { s.ask_queue_batch } else { s.ask_queue_later };
        let q = s.queues[0].name.clone();
        drop(s);
        if ask && !ids.is_empty() {
            self.open(Dialog::QueuePick { ids, q, start: false, never: false, batch });
        }
    }

    /// File -> Import: a text file of links, or an NDM export file (.ef2) with referer and cookies.
    fn import(&self, ef2: bool) {
        let (name, ext) = if ef2 { ("NDM export files", "ef2") } else { ("Text Files", "txt") };
        let Some(p) = rfd::FileDialog::new().add_filter(tr(name), &[ext]).pick_file() else { return };
        let text = std::fs::read_to_string(p).unwrap_or_default();
        if !ef2 {
            self.add_queued(links(&text));
            return;
        }
        for e in ef2_read(&text) {
            let mut i = self.core.new_info(&e.url);
            (i.referrer, i.cookies, i.queued) = (e.referrer, e.cookies, true);
            self.core.add(i);
        }
    }

    /// File -> Export after "Export download list" (template 191): the queued, selected or all downloads.
    fn export(&self, ef2: bool, mode: u8) {
        let ext = if ef2 { "ef2" } else { "txt" };
        let Some(p) = rfd::FileDialog::new().set_file_name(format!("downloads.{ext}")).add_filter(ext, &[ext]).save_file() else { return };
        let items: Vec<Info> = self.core.jobs().iter().map(|j| j.info().clone()).filter(|i| match mode {
            0 => i.queued,
            1 => self.sel.contains(&i.id),
            _ => true,
        }).collect();
        let ua = self.core.settings().user_agent.clone();
        let out = if ef2 { ef2_write(&items, &ua) } else { items.iter().map(|i| i.url.as_str()).collect::<Vec<_>>().join("\r\n") };
        let _ = std::fs::write(p, out);
    }

    // ---------- main window ----------

    /// Main menu, NDM's menu resource 129 item for item.
    fn menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, sel: &[u64]) {
        egui::menu::bar(ui, |ui| {
            // NDM's menu bar pads each item differently (measured with GetMenuItemRect at 125%, item width
            // minus the label's width in the menu font): Tasks 42, File 28, Downloads 52, View 33, Help 29 px
            let pad = |ui: &mut egui::Ui, px: f32| ui.spacing_mut().button_padding.x = px / 1.25 / 2.0;
            ui.style_mut().text_styles.insert(egui::TextStyle::Button, segoe(12.2)); // NDM's menu font
            ui.spacing_mut().item_spacing.x = 0.0;
            let (font, ink) = (egui::TextStyle::Button.resolve(ui.style()), ui.visuals().text_color());
            let mn = |t: &str| {
                // ponytail: underlines the first letter, as NDM's English and most translations; reading the
                // '&' position from the .lng files would cover the rest
                let t = tr(t);
                let k = t.chars().next().map_or(0, char::len_utf8);
                let mut job = egui::text::LayoutJob::default();
                let fmt = |u: bool| egui::TextFormat { font_id: font.clone(), color: ink, underline: if u { egui::Stroke::new(1.0_f32, ink) } else { egui::Stroke::NONE }, ..Default::default() };
                job.append(&t[..k], 0.0, fmt(true));
                job.append(&t[k..], 0.0, fmt(false));
                job
            };
            pad(ui, 42.0);
            ui.menu_button(mn("Tasks"), |ui| {
                if ui.button(tr("Add new download")).clicked() {
                    self.add_url();
                    ui.close_menu();
                }
                if ui.button(tr("Add batch download")).clicked() {
                    self.open(Dialog::Batch { url: "http://www.example.com/img*.jpg".into(), from: 1, to: 10, width: 1, letters: false, lf: "a".into(), lt: "z".into(), auth: false, login: String::new(), pass: String::new() });
                    ui.close_menu();
                }
                if ui.button(tr("Add batch download from clipboard")).clicked() {
                    self.add_queued(links(&clipboard()));
                    ui.close_menu();
                }
                if ui.button(tr("Run site grabber")).clicked() {
                    self.grabber();
                    ui.close_menu();
                }
                ui.separator();
                let mut dt = self.core.settings().drop_target;
                if ui.checkbox(&mut dt, tr("Show drop target")).clicked() {
                    self.core.settings().drop_target = dt;
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button(tr("Export"), |ui| {
                    for (t, ef2) in [("To NDM export file", true), ("To text file", false)] {
                        if ui.button(tr(t)).clicked() {
                            let mode = if sel.is_empty() { 2 } else { 1 };
                            self.open(Dialog::Export { ef2, mode });
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button(tr("Import"), |ui| {
                    for (t, ef2) in [("From NDM export file", true), ("From text file", false)] {
                        if ui.button(tr(t)).clicked() {
                            self.import(ef2);
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button(tr("Exit")).clicked() {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            });
            pad(ui, 28.0);
            ui.menu_button(mn("File"), |ui| {
                ui.add_enabled_ui(!sel.is_empty(), |ui| {
                    if ui.button(tr("Stop Download")).clicked() {
                        self.stop(sel);
                        ui.close_menu();
                    }
                    if ui.button(tr("Remove")).clicked() {
                        self.ask_delete(sel.to_vec());
                        ui.close_menu();
                    }
                    if ui.button(tr("Download Now")).clicked() {
                        self.resume(sel);
                        ui.close_menu();
                    }
                    if ui.button(tr("Redownload")).clicked() {
                        self.redownload(sel);
                        ui.close_menu();
                    }
                });
            });
            pad(ui, 52.0);
            ui.menu_button(mn("Downloads"), |ui| {
                if ui.button(tr("Pause All")).clicked() || ui.button(tr("Stop All")).clicked() {
                    self.core.stop_all();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Delete All Completed")).clicked() {
                    self.remove_completed();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Find (Ctrl-F)")).clicked() {
                    self.open_find();
                    ui.close_menu();
                }
                if ui.add_enabled(!self.core.settings().search.is_empty(), egui::Button::new(tr("Find Next (F3)"))).clicked() {
                    self.find_next();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("Scheduler")).clicked() {
                    self.open_scheduler(0);
                    ui.close_menu();
                }
                let queues: Vec<(u32, String)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name))).collect();
                for start in [true, false] {
                    ui.menu_button(tr(if start { "Start queue" } else { "Stop queue" }), |ui| {
                        for (id, name) in &queues {
                            if ui.add_enabled(self.core.queue_running(*id) != start, egui::Button::new(name)).clicked() {
                                if start { self.core.start_queue(*id) } else { self.core.stop_queue(*id) }
                                ui.close_menu();
                            }
                        }
                    });
                }
                ui.separator();
                ui.menu_button(tr("Speed Limiter"), |ui| {
                    let on = self.core.settings().limit_on;
                    for (label, v) in [("Turn on", true), ("Turn off", false)] {
                        if ui.add_enabled(on != v, egui::Button::new(label)).clicked() {
                            self.core.settings().limit_on = v;
                            self.core.apply_settings();
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button(tr("Settings...")).clicked() {
                        let s = self.core.settings();
                        let d = Dialog::Limiter { kb: s.limit_kb.to_string(), startup: s.limit_startup };
                        drop(s);
                        self.open(d);
                        ui.close_menu();
                    }
                });
                ui.separator();
                if ui.button(tr("Options")).clicked() {
                    self.options(0);
                    ui.close_menu();
                }
            });
            pad(ui, 33.0);
            ui.menu_button(mn("View"), |ui| {
                if ui.button(tr(if self.show_cats { "Hide categories" } else { "Show categories" })).clicked() {
                    self.show_cats = !self.show_cats;
                    ui.close_menu();
                }
                ui.menu_button(tr("Arrange files"), |ui| {
                    use Col::*;
                    let by = [(Added, "Order Of Addition"), (Name, "File Name"), (Size, "Size"), (Status, "Status"), (Left, "Time Left"), (Rate, "Transfer Rate"), (LastTry, "Last Try Date"), (Desc, "Description"), (SaveTo, "Save Path"), (Referer, "Referer"), (Parent, "Parent Web Page")];
                    for (c, n) in by {
                        if ui.radio(self.sort.0 == c, tr(&format!("By {n}"))).clicked() {
                            self.sort_by(c);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button(tr("Toolbar"), |ui| self.toolbar_menu(ui));
                ui.menu_button(tr("NDM tray icon"), |ui| {
                    // 0x45ada0: delete the icon, then add it back with the new picture unless "Don't show"
                    let cur = self.core.settings().tray_icon;
                    for (v, label) in [(1, "3D style"), (2, "Classic style"), (3, "Don't show")] {
                        if ui.radio(cur == v, tr(label)).clicked() {
                            self.core.settings().tray_icon = v;
                            self.core.save();
                            if let Some((tray, ..)) = &self.tray {
                                let _ = tray.set_icon(tray_picture(v));
                                let _ = tray.set_visible(v != 3);
                            }
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button(tr("Customize URL List...")).clicked() {
                    self.open_columns();
                    ui.close_menu();
                }
                let mut ds = self.core.settings().dark_support;
                if ui.checkbox(&mut ds, tr("Dark Mode support")).on_hover_text(tr("New Download Manager will use Dark Mode when it enabled in Windows settings\r\n(Right click on Desktop -> Personalize -> Colors -> Dark app mode)")).changed() {
                    self.core.settings().dark_support = ds;
                    self.theme_check = u32::MAX;
                }
                ui.menu_button(tr("Font"), |ui| {
                    let (face, px) = { let s = self.core.settings(); (s.list_font.clone(), s.list_font_px) };
                    let mut pick = None;
                    if ui.button(tr("Choose Font...")).clicked() {
                        ui.close_menu();
                        pick = win::choose_font(self.hwnd.load(Relaxed), if face.is_empty() { "Tahoma" } else { &face }, px);
                    }
                    if ui.add_enabled(!face.is_empty(), egui::Button::new(tr("Reset to default Font"))).clicked() {
                        ui.close_menu();
                        pick = Some((String::new(), 10.0));
                    }
                    if let Some((f, px)) = pick {
                        *LIST_FONT.lock().unwrap() = f.clone();
                        { let mut s = self.core.settings(); (s.list_font, s.list_font_px) = (f, px.clamp(6.0, 40.0)); }
                        self.core.save();
                        style(ui.ctx(), self.dark);
                    }
                });
                ui.separator();
                ui.menu_button(tr("Language"), |ui| {
                    // NDM's Language menu: English plus every Languages\ndm_*.lng
                    if !ALL_CJK.swap(true, Relaxed) {
                        style(ui.ctx(), self.dark); // the Chinese and Korean names need their fonts
                    }
                    let cur = self.core.settings().language.clone();
                    let mut pick = None;
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        if ui.radio(cur.is_empty(), "English").clicked() {
                            pick = Some(String::new());
                        }
                        for (stem, name) in &self.langs {
                            if ui.radio(cur == *stem, name.as_str()).clicked() {
                                pick = Some(stem.clone());
                            }
                        }
                    });
                    if let Some(stem) = pick {
                        i18n::load(&stem);
                        if let Some((_, limit, items)) = &self.tray {
                            for (m, en) in items {
                                m.set_text(i18n::tr_os(en));
                            }
                            limit.set_text(i18n::tr_os("Speed Limiter"));
                        }
                        self.core.settings().language = stem;
                        self.core.save();
                        style(ui.ctx(), self.dark);
                        ui.close_menu();
                    }
                });
            });
            pad(ui, 29.0);
            ui.menu_button(mn("Help"), |ui| {
                // NDM's four help entries, each opening a page of NDM's own help
                for (t, page) in [("Help contents", "index.htm"), ("Grabber Help", "grabber.htm"), ("Tutorials", "starting.htm"), ("Scheduler and queues", "scheduler.htm")] {
                    if ui.add_enabled(help(page, false), egui::Button::new(tr(t))).clicked() {
                        help(page, true);
                        ui.close_menu();
                    }
                }
                if ui.button(tr("Tip of the Day...")).clicked() {
                    let text = self.next_tip();
                    self.open(Dialog::Tip { text });
                    ui.close_menu();
                }
                ui.separator();
                // NDM's home is where its releases are published (UPDATE_REPO)
                for (t, page) in [("NDM Home Page", ""), ("Contact NDM Support", "/issues")] {
                    if ui.add_enabled(!UPDATE_REPO.is_empty(), egui::Button::new(tr(t))).clicked() {
                        explore(format!("https://github.com/{UPDATE_REPO}{page}"));
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(tr("Check for updates...")).clicked() {
                    self.check_updates(ui.ctx(), false);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(tr("About New Download Manager")).clicked() {
                    self.open(Dialog::About);
                    ui.close_menu();
                }
                if ui.button(tr("Tell a friend")).clicked() {
                    App::tell_friend();
                    ui.close_menu();
                }
            });
        });
    }

    fn ask_delete(&mut self, ids: Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        if self.core.settings().confirm_delete {
            self.open(Dialog::Delete { ids, files: false, dont_show: false });
        } else {
            self.remove(&ids, false);
        }
    }

    fn remove_completed(&mut self) {
        let ids: Vec<u64> = self.core.jobs().iter().map(|j| j.info()).filter(|i| i.status == Status::Complete).map(|i| i.id).collect();
        self.remove(&ids, false);
    }

    fn options(&mut self, tab: u8) {
        let mut s = self.core.settings().clone();
        // browsers installed since the list was saved are added ticked, like NDM's detection (296)
        for (name, exe) in win::installed_browsers() {
            if !s.browsers.iter().any(|b| b.exe.eq_ignore_ascii_case(&exe)) {
                s.browsers.push(core::Browser { name, exe, on: true });
            }
        }
        self.open(Dialog::Options { tab, s });
    }

    fn new_queue(&mut self, files: Vec<u64>) {
        let n = self.core.settings().queues.iter().map(|q| q.id).max().unwrap_or(0) + 1;
        self.open(Dialog::QueueName { name: i18n::trf("Queue # %ld", &[&n]), files });
    }

    /// "NDM categories" (template 204): add (None) or edit a category.
    fn edit_category(&mut self, orig: Option<String>) {
        let s = self.core.settings();
        let cat = orig.as_ref().and_then(|n| s.categories.iter().find(|c| c.name == *n).cloned()).unwrap_or_default();
        let folder = if cat.name.is_empty() { s.base_dir.clone() } else { s.folder_for(&cat.name) };
        drop(s);
        self.open(Dialog::Category { orig, cat, folder, error: String::new() });
    }

    fn save_category(&mut self, orig: Option<&str>, cat: core::Category, folder: &str) {
        let name = cat.name.clone();
        let mut s = self.core.settings();
        match orig.and_then(|o| s.categories.iter_mut().find(|c| c.name == o)) {
            Some(c) => *c = cat,
            None => s.categories.push(cat),
        }
        if let Some(o) = orig.filter(|o| *o != name) {
            s.cat_dirs.remove(o);
        }
        // only store a folder that differs from the default <base>\<name>
        let default = Path::new(&s.base_dir).join(&name).display().to_string();
        if folder.is_empty() || folder == default { s.cat_dirs.remove(&name) } else { s.cat_dirs.insert(name.clone(), folder.into()) };
        drop(s);
        if let Some(o) = orig.filter(|o| *o != name) {
            for j in self.core.jobs() {
                let mut i = j.info();
                if i.category == o {
                    i.category = name.clone();
                }
            }
            if let Cat::Files(_, Some(c)) = &mut self.cat {
                if c == o {
                    *c = name;
                }
            }
        }
        self.core.save();
        // an open Options window keeps its own copy: give it the new list (its OK would drop it otherwise)
        let (cats, dirs) = { let s = self.core.settings(); (s.categories.clone(), s.cat_dirs.clone()) };
        if self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::Options { .. })) {
            self.opt_patch.push(Box::new(move |s: &mut Settings| (s.categories, s.cat_dirs) = (cats, dirs)));
        }
    }

    fn open_scheduler(&mut self, tab: u8) {
        let s = self.core.settings().clone();
        let queue = s.queues[0].clone();
        self.open(Dialog::Scheduler { tab, queue, qsel: None, error: String::new() });
    }

    /// NDM's toolbar (RT_TOOLBAR 166: 12 buttons, 56x42 images, no separators). Labels and tooltips are
    /// NDM's strings; buttons share one width (0x45f560); Start/Stop Queue are BTNS_DROPDOWN buttons whose
    /// arrow lists the queues. Right click: NDM's toolbar menu (0x4613c0, popup +0x22c).
    fn toolbar(&mut self, ui: &mut egui::Ui, rows: &[Row], sel: &[u64]) {
        let chosen: Vec<&Row> = rows.iter().filter(|r| self.sel.contains(&r.id)).collect();
        let can_resume = chosen.iter().any(|r| !r.running && r.status != Status::Complete);
        let can_stop = chosen.iter().any(|r| r.running);
        let any_running = rows.iter().any(|r| r.running);
        let any_done = rows.iter().any(|r| r.status == Status::Complete);
        let queues: Vec<(u32, String, bool)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name), self.core.queue_running(q.id))).collect();
        let small = self.core.settings().small_buttons;
        let enabled = [true, can_resume, can_stop, any_running, !sel.is_empty(), any_done, true, true, queues.iter().any(|q| !q.2), queues.iter().any(|q| q.2), true, true];
        let buttons: Vec<TbButton> = TB_BUTTONS.iter().enumerate().map(|(k, (g, l, t))| (k, *g, tr(l), tr(t), enabled[k], k == 8 || k == 9)).collect();
        let key = (self.core.settings().tb_skin.clone(), self.dark);
        if self.tb_key != key {
            self.toolbar = Toolbar::load(ui.ctx(), &key.0, key.1);
            self.tb_key = key;
        }
        if !self.core.settings().classic {
            // NDM draws an etched line between the menu and the toolbar
            let (r, w) = (ui.max_rect(), ui.ctx().screen_rect().x_range());
            for (dy, g) in [(0.0, 240), (1.0, 160), (2.0, 255)] {
                ui.painter().with_clip_rect(ui.ctx().screen_rect()).hline(w, r.top() - 3.0 + dy + 0.5, egui::Stroke::new(1.0_f32, Color32::from_gray(g)));
            }
        }
        let clicked = if i18n::is_rtl() {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| self.tool_buttons(ui, &buttons, &queues, small)).inner
        } else {
            self.tool_buttons(ui, &buttons, &queues, small)
        };
        match clicked {
            Some(0) => self.add_url(),
            Some(1) => self.resume(sel),
            Some(2) => self.stop(sel),
            Some(3) => self.core.stop_all(),
            Some(4) => self.ask_delete(sel.to_vec()),
            Some(5) => self.remove_completed(),
            Some(6) => self.options(0),
            Some(7) => self.open_scheduler(0),
            // the button itself acts on the main queue
            Some(8) => self.core.start_queue(core::MAIN_QUEUE),
            Some(9) => self.core.stop_queue(core::MAIN_QUEUE),
            Some(10) => self.grabber(),
            Some(11) => App::tell_friend(),
            _ => {}
        }
    }

    /// The toolbar's buttons in the user's order (Customize...), separators as gaps. Returns the clicked button.
    fn tool_buttons(&mut self, ui: &mut egui::Ui, buttons: &[TbButton], queues: &[(u32, String, bool)], small: bool) -> Option<usize> {
        let font = segoe(11.6);
        // NDM draws the strip 1:1 in screen pixels at any DPI, every button image width + 19 px wide with the
        // label cut to "..."
        let ppp = ui.ctx().pixels_per_point();
        let (iw, ih) = if small { (32.0 / ppp, 32.0 / ppp) } else { (56.0 / ppp, 42.0 / ppp) };
        let bw = iw + 19.0 / ppp;
        let bh = ih + 28.0 / ppp;
        let arrow_w = 19.0 / ppp; // BTNS_DROPDOWN arrow: 19 px
        let dark = ui.visuals().dark_mode;
        let (hot_fill, hot_edge, down_fill) = if dark {
            (Color32::from_gray(0x45), Color32::from_gray(0x60), Color32::from_gray(0x55))
        } else {
            (Color32::from_rgb(229, 243, 255), Color32::from_rgb(204, 232, 255), Color32::from_rgb(204, 232, 255))
        };
        let mut clicked = None;
        let mut drop: Option<(usize, egui::Rect)> = None;
        let bar = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let layout = self.core.settings().tb_layout.clone();
            for b in layout {
                let Some((k, glyph, label, tip, enabled, dropdown)) = usize::try_from(b).ok().and_then(|b| buttons.get(b)).cloned() else {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, bh), Sense::hover());
                    ui.painter().vline(r.center().x, r.y_range().shrink(6.0), egui::Stroke::new(1.0_f32, Color32::from_gray(if dark { 90 } else { 190 })));
                    continue;
                };
                let w = bw + if dropdown { arrow_w } else { 0.0 };
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, bh), Sense::click());
                let resp = resp.on_hover_text(tip);
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &label));
                let arrow = egui::Rect::from_min_max(egui::pos2(rect.right() - arrow_w, rect.top()), rect.max);
                let on_arrow = dropdown && resp.hover_pos().is_some_and(|p| arrow.contains(p));
                let hot = enabled && resp.hovered();
                let p = ui.painter();
                if hot {
                    let fill = if resp.is_pointer_button_down_on() { down_fill } else { hot_fill };
                    p.rect_filled(rect.shrink(1.0), 2.0, fill);
                    p.rect_stroke(rect.shrink(1.0), 2.0, egui::Stroke::new(1.0_f32, hot_edge), egui::StrokeKind::Inside);
                    if dropdown {
                        p.vline(arrow.left(), arrow.y_range().shrink(3.0), egui::Stroke::new(1.0_f32, hot_edge));
                    }
                }
                let color = if enabled { ui.visuals().text_color() } else { Color32::from_gray(if dark { 110 } else { 150 }) };
                let body = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - if dropdown { arrow_w } else { 0.0 }, rect.bottom()));
                let icon = egui::Rect::from_center_size(egui::pos2(body.center().x, rect.top() + 4.0 + ih / 2.0), Vec2::new(iw, ih));
                match self.toolbar.as_ref().map(|t| t.strip(small, enabled, hot && !on_arrow)) {
                    Some(tex) => {
                        let u = egui::Rect::from_min_max(egui::pos2(k as f32 / TB_ICONS as f32, 0.0), egui::pos2((k + 1) as f32 / TB_ICONS as f32, 1.0));
                        p.image(tex, icon, u, Color32::WHITE);
                    }
                    None => {
                        p.text(icon.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(28.0), if enabled { BAR } else { color });
                    }
                }
                let mut job = egui::text::LayoutJob::simple_singleline(label.clone(), font.clone(), color);
                job.wrap = egui::text::TextWrapping { max_width: body.width() - 7.0 / ppp, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                let g = ui.painter().layout_job(job);
                p.galley(egui::pos2(body.center().x - g.size().x / 2.0, rect.bottom() - 9.0 - g.size().y / 2.0), g, color);
                if dropdown {
                    // NDM (measured at 125%): a solid 9x5 px near-black triangle, 3 px right of the arrow
                    // column's middle and 10 px below the icon's centre, black in dark mode too
                    let px = |v: f32| v / ppp;
                    let c = egui::pos2((arrow.center().x * ppp).round() / ppp + px(3.0), (icon.center().y * ppp).round() / ppp + px(10.0));
                    let fill = if enabled { Color32::from_gray(4) } else { Color32::from_gray(if dark { 110 } else { 150 }) };
                    let pts = vec![c + Vec2::new(px(-4.5), px(-2.5)), c + Vec2::new(px(4.5), px(-2.5)), c + Vec2::new(0.0, px(2.5))];
                    p.add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::NONE));
                }
                if enabled && resp.clicked() {
                    if on_arrow { drop = Some((k, rect)) } else { clicked = Some(k) }
                }
            }
        });
        bar.response.interact(Sense::click()).context_menu(|ui| self.toolbar_menu(ui));
        if let Some((k, rect)) = drop {
            let id = ui.id().with("tbdrop");
            ui.memory_mut(|m| m.toggle_popup(id));
            ui.data_mut(|m| m.insert_temp(id, (k, rect.left_bottom())));
        }
        let id = ui.id().with("tbdrop");
        if ui.memory(|m| m.is_popup_open(id)) {
            let (k, at): (usize, egui::Pos2) = ui.data(|m| m.get_temp(id)).unwrap_or((8, egui::Pos2::ZERO));
            let start = k == 8;
            let area = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    for (q, name, running) in queues {
                        if ui.add_enabled(*running != start, egui::Button::new(name)).clicked() {
                            if start { self.core.start_queue(*q) } else { self.core.stop_queue(*q) }
                            ui.memory_mut(|m| m.close_popup());
                        }
                    }
                });
            });
            if area.response.clicked_elsewhere() && drop.is_none() {
                ui.memory_mut(|m| m.close_popup());
            }
        }
        clicked
    }

    /// NDM's toolbar menu (0x4613c0, also View -> Toolbar): Customize..., Large/Small Buttons, Look for new
    /// toolbars, then Modern / Classic style.
    fn toolbar_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button(tr("Customize...")).clicked() {
            let layout = self.core.settings().tb_layout.clone();
            self.open(Dialog::TbCustomize { layout, avail: None, cur: None });
            ui.close_menu();
        }
        ui.separator();
        let small = self.core.settings().small_buttons;
        for (label, v) in [("Large Buttons", false), ("Small Buttons", true)] {
            if ui.radio(small == v, tr(label)).clicked() {
                self.core.settings().small_buttons = v;
                self.core.save();
                ui.close_menu();
            }
        }
        ui.separator();
        if ui.button(tr("Look for new toolbars")).clicked() {
            // new toolbar styles come with new NDM releases
            open_url(&format!("https://github.com/{UPDATE_REPO}/releases"));
            ui.close_menu();
        }
        ui.separator();
        // Modern (tree chevrons, list grid lines) or Classic (+/- tree boxes, 3D look)
        let classic = self.core.settings().classic;
        for (name, c) in [("Modern style", false), ("Classic style", true)] {
            if ui.radio(classic == c, tr(name)).clicked() {
                self.core.settings().classic = c;
                self.core.save();
                ui.close_menu();
            }
        }
    }

    /// NDM's category pane (template 102): "Categories" header frame with an x button over a
    /// SysTreeView32 (TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT): dotted lines, +/- boxes,
    /// All Downloads / Unfinished / Finished each holding the categories, Grabber projects, Queues.
    fn categories(&mut self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let (edge, text) = (if dark { Color32::from_gray(90) } else { Color32::from_gray(160) }, ui.visuals().text_color());
        // header: group frame [4,1,95,16] with "Categories" and the owner-drawn x [87,8,9,8]
        let head = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.4), Sense::hover()).0;
        ui.painter().rect_stroke(head, 0.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
        let rtl = i18n::is_rtl();
        if rtl {
            ui.painter().text(head.right_center() + Vec2::new(-5.0, 1.0), Align2::RIGHT_CENTER, tr("Categories"), egui::TextStyle::Body.resolve(ui.style()), text);
        } else {
            ui.painter().text(head.left_center() + Vec2::new(5.0, 1.0), Align2::LEFT_CENTER, tr("Categories"), egui::TextStyle::Body.resolve(ui.style()), text);
        }
        let xc = if rtl { head.left_center() + Vec2::new(10.0, 1.0) } else { head.right_center() - Vec2::new(10.0, -1.0) };
        let xr = egui::Rect::from_center_size(xc, Vec2::splat(13.0));
        let x = ui.interact(xr, ui.id().with("cats_x"), Sense::click()).on_hover_text(tr("Hide categories"));
        if x.hovered() {
            ui.painter().rect_filled(xr, 0.0, if dark { Color32::from_gray(80) } else { Color32::from_gray(220) });
        }
        // NDM's owner-drawn close cross
        for (a, b) in [(Vec2::new(-3.5, -3.5), Vec2::new(3.5, 3.5)), (Vec2::new(-3.5, 3.5), Vec2::new(3.5, -3.5))] {
            ui.painter().line_segment([xr.center() + a, xr.center() + b], egui::Stroke::new(1.2_f32, text));
        }
        if x.clicked() {
            self.show_cats = false;
        }

        // one settings() lock per statement: two in the same expression deadlock
        let cats: Vec<String> = self.core.settings().categories.iter().map(|c| c.name.clone()).collect();
        let queues: Vec<(u32, String, bool)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name), q.sync)).collect();
        let img = |id: u16| self.res.tree(id);
        let cat_img = |c: &str| match c {
            "Compressed" => img(225),
            "Documents" => img(223),
            "Music" => img(220),
            "Programs" => img(222),
            "Video" => img(221),
            _ => img(215),
        };
        // (depth, key for expand state or "", closed icon, open icon, label, value, has children)
        let mut nodes: Vec<(u8, &str, Option<egui::TextureId>, Option<egui::TextureId>, String, Cat, bool)> = Vec::new();
        for (which, key, label, (closed, open)) in [(Which::All, "all", "All Downloads", (215, 214)), (Which::Unfinished, "unf", "Unfinished", (219, 218)), (Which::Finished, "fin", "Finished", (217, 216))] {
            nodes.push((0, key, img(closed), img(open), tr(label), Cat::Files(which, None), !cats.is_empty()));
            if self.expanded.contains(key) {
                nodes.extend(cats.iter().map(|c| (1, "", cat_img(c), cat_img(c), tr(c), Cat::Files(which, Some(c.clone())), false)));
            }
        }
        let projects: Vec<String> = self.core.settings().grab_projects.iter().map(|p| p.opts.name.clone()).collect();
        nodes.push((0, "gp", img(282), img(282), tr("Grabber projects"), Cat::Grabber(None), !projects.is_empty()));
        if self.expanded.contains("gp") {
            nodes.extend(projects.into_iter().map(|n| (1, "", img(282), img(282), n.clone(), Cat::Grabber(Some(n)), false)));
        }
        nodes.push((0, "q", img(311), img(313), tr("Queues"), Cat::Queue(None), true));
        if self.expanded.contains("q") {
            nodes.extend(queues.iter().map(|(id, name, sync)| {
                let i = img([[311, 312], [315, 316]][*sync as usize][self.core.queue_running(*id) as usize]);
                (1, "", i, i, name.clone(), Cat::Queue(Some(*id)), false)
            }));
        }

        let mut queue_dlg = None;
        let mut grab_cmd: Option<(usize, String)> = None;
        let mut new_queue = false;
        let mut cat_dlg: Option<Option<String>> = None; // Some(None) = add, Some(Some(name)) = edit
        let mut toggle = None;
        let fill = if dark { DARK_TREE } else { Color32::WHITE };
        let line = if dark { Color32::from_gray(110) } else { Color32::from_gray(160) };
        // NDM's tree at 125%: 19 px rows, 21 px indent
        let (row_h, indent) = (15.2, 17.0);
        egui::Frame::new().fill(fill).stroke(egui::Stroke::new(1.0_f32, edge)).inner_margin(egui::Margin { left: 2, right: 2, top: 2, bottom: 2 }).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let modern = !self.core.settings().classic;
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (k, (depth, key, icon, open_icon, label, value, kids)) in nodes.iter().enumerate() {
                    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
                    let p = ui.painter();
                    // RTL languages mirror the tree as Windows does (WS_EX_LAYOUTRTL): levels grow leftwards
                    let dir = if rtl { -1.0 } else { 1.0 };
                    let cx = |d: u8| if rtl { row.right() - 7.0 - d as f32 * indent } else { row.left() + 7.0 + d as f32 * indent }; // centre of a level's +/- box
                    let my = row.center().y;
                    // dotted lines: to the item, down to the next sibling, and the parent's line passing by
                    let dots = |a: egui::Pos2, b: egui::Pos2| {
                        let n = ((b - a).length() / 2.0) as i32;
                        for s in (0..=n).step_by(2) {
                            let t = a + (b - a) * (s as f32 / n.max(1) as f32);
                            p.rect_filled(egui::Rect::from_min_size(t, Vec2::splat(1.0)), 0.0, line);
                        }
                    };
                    let next_sibling = nodes[k + 1..].iter().take_while(|n| n.0 >= *depth).any(|n| n.0 == *depth);
                    let first = k == 0;
                    dots(egui::pos2(cx(*depth), if first { my } else { row.top() }), egui::pos2(cx(*depth), if next_sibling { row.bottom() } else { my }));
                    dots(egui::pos2(cx(*depth).min(cx(*depth) + 10.0 * dir), my), egui::pos2(cx(*depth).max(cx(*depth) + 10.0 * dir), my));
                    if *depth == 1 && nodes[k + 1..].iter().any(|n| n.0 == 0) {
                        dots(egui::pos2(cx(0), row.top()), egui::pos2(cx(0), row.bottom()));
                    }
                    let open = !key.is_empty() && self.expanded.contains(*key);
                    if *kids && modern {
                        // NDM (Explorer-themed tree): a chevron instead of the +/- box
                        let c = egui::pos2(cx(*depth), my);
                        let v = |x: f32, y: f32| c + Vec2::new(x * dir, y);
                        let pts = if open { vec![v(-3.5, -1.5), v(0.0, 2.0), v(3.5, -1.5)] } else { vec![v(-1.5, -3.5), v(2.0, 0.0), v(-1.5, 3.5)] };
                        p.add(egui::Shape::line(pts, egui::Stroke::new(1.2_f32, Color32::from_gray(if self.dark { 190 } else { 90 }))));
                        if ui.interact(egui::Rect::from_center_size(c, Vec2::splat(13.0)), ui.id().with(("exp", k)), Sense::click()).clicked() {
                            toggle = Some(key.to_string());
                        }
                    } else if *kids {
                        let b = egui::Rect::from_center_size(egui::pos2(cx(*depth), my), Vec2::splat(9.0));
                        p.rect_filled(b, 0.0, fill);
                        p.rect_stroke(b, 0.0, egui::Stroke::new(1.0_f32, line), egui::StrokeKind::Inside);
                        p.hline(b.x_range().shrink(2.0), my, egui::Stroke::new(1.0_f32, text));
                        if !open {
                            p.vline(b.center().x, b.y_range().shrink(2.0), egui::Stroke::new(1.0_f32, text));
                        }
                        if ui.interact(b.expand(2.0), ui.id().with(("exp", k)), Sense::click()).clicked() {
                            toggle = Some(key.to_string());
                        }
                    }
                    let sel = self.cat == *value;
                    // x: the icon's edge nearest the line; in RTL the icon and label run leftwards from it
                    let mut x = cx(*depth) + 9.0 * dir;
                    if let Some(t) = if open || sel { open_icon.or(*icon) } else { *icon } {
                        let left = if rtl { x - 16.0 } else { x };
                        p.image(t, egui::Rect::from_min_size(egui::pos2(left, my - 6.5), Vec2::new(16.0, 13.0)), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    x += 17.0 * dir;
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let g = ui.painter().layout_no_wrap(label.clone(), font, if sel { Color32::WHITE } else { text });
                    let lw = g.size().x + 4.0;
                    let lr = egui::Rect::from_min_size(egui::pos2(if rtl { x - lw } else { x }, my - 8.0), Vec2::new(lw, 16.0));
                    if sel {
                        ui.painter().rect_filled(lr, 0.0, if dark { Color32::from_gray(0x50) } else { Color32::from_rgb(0, 120, 215) });
                    }
                    ui.painter().galley(egui::pos2(lr.left() + 2.0, my - g.size().y / 2.0), g, text);
                    let hit = if rtl { egui::Rect::from_min_max(egui::pos2(row.left(), row.top()), egui::pos2(x + 19.0, row.bottom())) } else { egui::Rect::from_min_max(egui::pos2(x - 19.0, row.top()), egui::pos2(row.right(), row.bottom())) };
                    let resp = ui.interact(hit, ui.id().with(("node", k)), Sense::click());
                    if resp.clicked() || resp.secondary_clicked() {
                        self.cat = value.clone();
                    }
                    if resp.double_clicked() && *kids {
                        toggle = Some(key.to_string());
                    }
                    // NDM's tree menus: category popup +0x220, grabber +0x224, queue +0x228.
                    resp.context_menu(|ui| match value {
                        Cat::Queue(q) => {
                            let id = q.unwrap_or(core::MAIN_QUEUE);
                            let on = self.core.queue_running(id);
                            if ui.add_enabled(q.is_some() && !on, egui::Button::new(tr("Start now"))).clicked() {
                                self.core.start_queue(id);
                                ui.close_menu();
                            }
                            if ui.add_enabled(q.is_some() && on, egui::Button::new(tr("Stop"))).clicked() {
                                self.core.stop_queue(id);
                                ui.close_menu();
                            }
                            ui.separator();
                            for (t, tab) in [("Edit queue", 1), ("Schedule", 0)] {
                                if ui.add_enabled(q.is_some(), egui::Button::new(t)).clicked() {
                                    queue_dlg = Some((id, tab));
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.add_enabled(q.is_some_and(|q| self.core.settings().queue(q).is_some_and(|x| !x.builtin())), egui::Button::new(tr("Delete"))).clicked() {
                                self.core.delete_queue(id);
                                self.cat = Cat::Queue(None);
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(tr("Create new queue")).clicked() {
                                new_queue = true;
                                ui.close_menu();
                            }
                        }
                        Cat::Grabber(p) => {
                            for (k, t) in ["Open project", "Schedule project", "Rename project", "Delete project"].into_iter().enumerate() {
                                if ui.add_enabled(p.is_some(), egui::Button::new(tr(t))).clicked() {
                                    grab_cmd = p.clone().map(|p| (k, p));
                                    ui.close_menu();
                                }
                            }
                        }
                        Cat::Files(_, c) => {
                            let c = c.as_deref().unwrap_or(GENERAL);
                            if ui.button(tr("Browse")).clicked() {
                                explore(self.core.settings().folder_for(c));
                                ui.close_menu();
                            }
                            if ui.add_enabled(c != GENERAL, egui::Button::new(tr("Properties"))).clicked() {
                                cat_dlg = Some(Some(c.to_string()));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(tr("Add category")).clicked() {
                                cat_dlg = Some(None);
                                ui.close_menu();
                            }
                            if ui.add_enabled(c != GENERAL, egui::Button::new(tr("Delete"))).clicked() {
                                self.open(Dialog::CatDelete { name: c.to_string() });
                                ui.close_menu();
                            }
                        }
                    });
                }
            });
        });
        if let Some((k, name)) = grab_cmd {
            self.project_cmd(k, &name);
        }
        if let Some(key) = toggle {
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.core.settings().expanded = self.expanded.iter().cloned().collect();
        }
        if let Some((id, tab)) = queue_dlg {
            self.open_scheduler(tab);
            if let Some((_, Dialog::Scheduler { queue, .. })) = self.dialogs.last_mut() {
                *queue = self.core.settings().queue(id).cloned().unwrap_or_default();
            }
        }
        if new_queue {
            self.new_queue(Vec::new());
        }
        if let Some(orig) = cat_dlg {
            self.edit_category(orig);
        }
    }

    fn table(&mut self, ui: &mut egui::Ui, rows: &[Row]) {
        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let mut sort = None;
        let mut action: Option<(usize, bool)> = None; // (row, double click)
        let mut menu_cmd: Option<(&str, u64)> = None;
        let mut columns_dlg = false;
        let mut dbl = None;
        let mut to_queue: Option<(Option<u32>, u64)> = None; // (queue or a new one, row)
        let queues: Vec<(u32, String)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name))).collect();
        let (mut cols, dbl_open): (Vec<(Col, &str, f32)>, bool) = {
            let s = self.core.settings();
            (s.columns.iter().filter_map(|(k, w)| COLS.get(*k as usize).map(|c| (c.0, c.1, *w as f32))).collect(), s.dbl_open)
        };
        if i18n::is_rtl() {
            cols.reverse(); // columns mirror, as the tree does
        }
        let icons: Vec<Option<egui::TextureId>> = rows.iter().map(|r| self.icon(ui.ctx(), &core::ext(&r.cell(Col::Name)))).collect();
        let modern = !self.core.settings().classic;
        let row_h = ROW_H.max(egui::TextStyle::Body.resolve(ui.style()).size * 1.5); // taller rows for a bigger font
        // Q column: the queue's tree picture (main / synchronization, idle / running) like NDM's list
        let qpics: HashMap<u32, egui::TextureId> = self.core.settings().queues.iter().filter_map(|q| {
            self.res.tree([[311, 312], [315, 316]][q.sync as usize][self.core.queue_running(q.id) as usize]).map(|t| (q.id, t))
        }).collect();
        let mut row_rects = Vec::new();
        // NDM's columns touch; cell text starts 7 px in (icon 8 px), measured at 125%
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let mut tb = TableBuilder::new(ui);
        if let Some(k) = self.scroll_to.take() {
            tb = tb.scroll_to_row(k, Some(Align::Center));
        }
        let mut tb = tb.striped(false).resizable(true).auto_shrink(false).sense(Sense::click()).cell_layout(Layout::left_to_right(Align::Center));
        for (k, (_, _, w)) in cols.iter().enumerate() {
            tb = tb.column(if k + 1 == cols.len() { Column::remainder().at_least(*w) } else { Column::initial(*w).at_least(20.0).clip(true) });
        }
        // NDM's list at 125%: 24 px header, 19 px rows
        tb.header(19.2, |mut h| {
            for (c, name, _) in &cols {
                let (_, resp) = h.col(|ui| {
                    ui.add_space(4.0);
                    let arrow = if self.sort.0 == *c { if self.sort.1 { " ⏶" } else { " ⏷" } } else { "" };
                    if ui.add(egui::Label::new(format!("{}{arrow}", tr(name))).selectable(false).sense(Sense::click())).clicked() {
                        sort = Some(*c);
                    }
                });
                // NDM's header menu has one item (0x4613c0, popup +0x230)
                resp.context_menu(|ui| {
                    if ui.button(tr("Columns...")).clicked() {
                        columns_dlg = true;
                        ui.close_menu();
                    }
                });
            }
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let k = row.index();
                let r = &rows[k];
                row.set_selected(self.sel.contains(&r.id));
                for (c, _, _) in &cols {
                    row.col(|ui| {
                        ui.add_space(if *c == Col::Name { 1.6 } else { 4.0 });
                        if *c == Col::Name {
                            if let Some(t) = icons[k] {
                                ui.add(egui::Image::new((t, Vec2::splat(16.0))));
                                ui.add_space(2.0);
                            }
                        }
                        if let Some(t) = qpics.get(&r.queue).filter(|_| *c == Col::Q && r.queued) {
                            ui.add(egui::Image::new((*t, Vec2::new(16.0, 13.0))));
                            return;
                        }
                        let text = r.cell(*c);
                        let text = if *c == Col::Status && matches!(r.status, Status::Error(_)) { RichText::new(text).color(Color32::RED) } else { RichText::new(text) };
                        ui.add(egui::Label::new(text).selectable(false).truncate());
                    });
                }
                let resp = row.response();
                if resp.rect.is_positive() {
                    row_rects.push(resp.rect); // a row egui didn't lay out has Rect::NOTHING (bottom = -inf)
                }
                if resp.double_clicked() {
                    action = Some((k, true));
                } else if resp.clicked() || (resp.secondary_clicked() && !self.sel.contains(&r.id)) {
                    action = Some((k, false));
                }
                // the list's context menu
                resp.context_menu(|ui| {
                    let done = r.status == Status::Complete;
                    let mut item = |ui: &mut egui::Ui, cmd: &'static str, enabled: bool| {
                        if ui.add_enabled(enabled, egui::Button::new(cmd)).clicked() {
                            menu_cmd = Some((cmd, r.id));
                            ui.close_menu();
                        }
                    };
                    item(ui, "Open", done);
                    item(ui, "Open with...", done);
                    item(ui, "Open folder", true);
                    ui.separator();
                    // NDM shows this item behind a setting (0x469250, DAT_00758d28); always on here
                    item(ui, "Move/Rename (Ctrl-M)", !r.running);
                    ui.separator();
                    item(ui, "Redownload", !r.running);
                    ui.separator();
                    item(ui, "Resume Download", !done && !r.running);
                    item(ui, "Stop Download", r.running);
                    ui.separator();
                    item(ui, "Refresh download address", !done);
                    ui.separator();
                    item(ui, "Remove", true);
                    ui.separator();
                    ui.add_enabled_ui(!done, |ui| {
                        ui.menu_button(tr("Add to download queue"), |ui| {
                            for (id, name) in &queues {
                                if ui.add_enabled(!(r.queued && r.queue == *id), egui::Button::new(name)).clicked() {
                                    to_queue = Some((Some(*id), r.id));
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.button(tr("Create new queue")).clicked() {
                                to_queue = Some((None, r.id));
                                ui.close_menu();
                            }
                        });
                    });
                    item(ui, "Delete from queue", r.queued);
                    ui.separator();
                    ui.menu_button(tr("On Double click"), |ui| {
                        for (on, label) in [(true, "Open"), (false, "Properties")] {
                            if ui.radio(dbl_open == on, label).clicked() {
                                dbl = Some(on);
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    item(ui, "Properties", true);
                });
            });
        });
        if modern {
            // NDM's list grid (LVS_EX_GRIDLINES / its dark-mode custom grid, 0x475710): a line under every row,
            // continued down the empty part of the list
            let area = ui.min_rect().intersect(ui.clip_rect()); // only the visible part
            // the fixed row pitch: a measured row rect can be 0 tall, and a 0 pitch never ends the loop below
            let pitch = row_h + ui.spacing().item_spacing.y;
            let mut y = row_rects.last().map_or(area.top() + 22.0 + ui.spacing().item_spacing.y, |r| r.bottom() + ui.spacing().item_spacing.y / 2.0).max(area.top());
            let stroke = egui::Stroke::new(1.0_f32, Color32::from_gray(if self.dark { 85 } else { 225 }));
            for r in &row_rects {
                ui.painter().hline(area.x_range(), r.bottom() + ui.spacing().item_spacing.y / 2.0, stroke);
            }
            while y < area.bottom() {
                ui.painter().hline(area.x_range(), y, stroke);
                y += pitch;
            }
        }
        if columns_dlg {
            self.open_columns();
        }
        if let Some(on) = dbl {
            self.core.settings().dbl_open = on;
        }
        if let Some((q, id)) = to_queue {
            let ids: Vec<u64> = if self.sel.contains(&id) { self.sel.iter().copied().collect() } else { vec![id] };
            match q {
                Some(q) => self.core.set_queue(&ids, Some(q)),
                None => self.new_queue(ids),
            }
        }
        if let Some(c) = sort {
            self.sort_by(c);
        }
        if let Some((k, double)) = action {
            let id = rows[k].id;
            if double {
                self.sel = HashSet::from([id]);
                if dbl_open && rows[k].status == Status::Complete {
                    explore(&rows[k].path);
                } else {
                    self.properties(&rows[k]);
                }
            } else if shift && self.anchor.is_some() {
                let a = rows.iter().position(|r| Some(r.id) == self.anchor).unwrap_or(k);
                self.sel = rows[a.min(k)..=a.max(k)].iter().map(|r| r.id).collect();
            } else if ctrl {
                if !self.sel.remove(&id) {
                    self.sel.insert(id);
                }
                self.anchor = Some(id);
            } else {
                self.sel = HashSet::from([id]);
                self.anchor = Some(id);
            }
        }
        if let Some((cmd, id)) = menu_cmd {
            let ids: Vec<u64> = if self.sel.contains(&id) { self.sel.iter().copied().collect() } else { vec![id] };
            let row = rows.iter().find(|r| r.id == id).unwrap();
            match cmd {
                "Open" => explore(&row.path),
                "Open with..." => {
                    open_with(&row.path);
                }
                "Open folder" => open_folder_of(&row.path),
                "Move/Rename (Ctrl-M)" => self.move_rename(id),
                "Refresh download address" => {
                    // "Downloading owner web page to refresh link address": open the page the link
                    // came from; the user clicks the link again and the browser hands it to us
                    let page = if row.parent.is_empty() { row.referrer.clone() } else { row.parent.clone() };
                    self.refreshing = Some(id);
                    self.open(Dialog::WaitLink { name: row.name.clone() });
                    if page.is_empty() {
                        self.open(Dialog::Info { title: "Refresh download address".into(), text: "The web page of this download is unknown. Open the page in your browser and click the download link again: New Download Manager will use the new address for this download.".into() });
                    } else {
                        explore(page);
                    }
                }
                "Resume Download" => self.resume(&ids),
                "Stop Download" => self.stop(&ids),
                "Redownload" => self.redownload(&ids),
                "Delete from queue" => self.core.set_queue(&ids, None),
                "Remove" => self.ask_delete(ids),
                _ => self.properties(row),
            }
        }
    }

    /// Move/Rename: pick a new path; a finished file is moved on disk, an unfinished one moves its .part.
    fn move_rename(&mut self, id: u64) {
        let Some(j) = self.core.find(id).filter(|j| !j.running()) else { return };
        let (path, done) = {
            let i = j.info();
            (i.path(), i.status == Status::Complete)
        };
        let mut fd = rfd::FileDialog::new().set_title("Move/Rename");
        if let Some(dir) = path.parent().filter(|d| d.exists()) {
            fd = fd.set_directory(dir);
        }
        if let Some(n) = path.file_name() {
            fd = fd.set_file_name(n.to_string_lossy());
        }
        let Some(to) = fd.save_file() else { return };
        if to == path {
            return;
        }
        let mut i = j.info();
        let from = if done { i.path() } else { i.part() };
        let mut moved_info = i.clone();
        moved_info.folder = to.parent().map(|p| p.display().to_string()).unwrap_or_default();
        moved_info.name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let target = if done { to.clone() } else { moved_info.part() };
        // rename, or copy + delete when moving to another drive
        let moved = !from.exists() || std::fs::rename(&from, &target).is_ok() || (std::fs::copy(&from, &target).is_ok() && std::fs::remove_file(&from).is_ok());
        if moved {
            i.folder = to.parent().map(|p| p.display().to_string()).unwrap_or_default();
            i.name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        } else {
            drop(i);
            self.open(Dialog::Info { title: "Move/Rename".into(), text: i18n::trf("Cannot move %s to %s.", &[&from.display(), &target.display()]) });
        }
    }

    /// "Properties": the progress window for unfinished downloads, the completion window otherwise.
    fn properties(&mut self, row: &Row) {
        let id = row.id;
        if row.status != Status::Complete {
            let details = self.core.settings().prog_details;
            return self.open(Dialog::Progress { id, tab: 0, details, min: false });
        }
        let Some(j) = self.core.find(id) else { return };
        let i = j.info().clone();
        let (url, login, pass) = split_login(&i.url);
        self.open(Dialog::FileProps { id, url, desc: i.description, referer: i.referrer, login, pass });
    }

    /// View -> Customize URL List / header menu -> Columns... (template 209).
    fn open_columns(&mut self) {
        let shown = self.core.settings().columns.clone();
        let mut list: Vec<(u8, bool, u16)> = shown.iter().map(|(k, w)| (*k, true, *w)).collect();
        list.extend(COLS.iter().enumerate().filter(|(k, _)| !shown.iter().any(|c| c.0 as usize == *k)).map(|(k, c)| (k as u8, false, c.2)));
        let width = list[0].2.to_string();
        self.open(Dialog::Columns { list, sel: 0, width });
    }

    // ---------- dialogs ----------

    /// Draw one dialog in its own OS window. Returns false once it should close.
    fn dialog(&mut self, ctx: &egui::Context, key: u64, d: &mut Dialog) -> bool {
        let tpl = |w: f32, h: f32| Some(dlu::size(w, h));
        let (title, fixed): (String, Option<Vec2>) = match d {
            Dialog::AddUrl { clip, .. } => (if *clip { "Download link was found in clipboard. Do you want to download it?" } else { "Enter new address to download" }.into(), tpl(373.0, 57.0)),
            Dialog::Cancelled { .. } => ("Cancelling an automatically started download".into(), tpl(283.0, 159.0)),
            Dialog::Exceptions { .. } => ("The list of address exceptions".into(), tpl(281.0, 182.0)),
            Dialog::ExcAdd { .. } => ("Add an address to exceptions list".into(), tpl(307.0, 46.0)),
            Dialog::MenuItems { .. } => ("Customize menu items in context menu of browsers".into(), tpl(275.0, 70.0)),
            Dialog::BrowserDetails { .. } => ("Browser details".into(), tpl(430.0, 94.0)),
            Dialog::Integrate { .. } => ("Browsers integration".into(), tpl(485.0, 200.0)),
            Dialog::Keys { .. } => ("Using special keys".into(), tpl(318.0, 234.0)),
            Dialog::PanelExc { .. } => ("Download panel exceptions".into(), tpl(254.0, 123.0)),
            Dialog::Panel { .. } => ("Customize New Download Manager Download panels in browsers".into(), tpl(336.0, 300.0)),
            Dialog::PanelType { .. } => ("Add file type".into(), tpl(126.0, 49.0)),
            Dialog::PanelSite { .. } => ("Enter site name".into(), tpl(186.0, 52.0)),
            Dialog::PanelMin { .. } => ("Minimum size".into(), tpl(239.0, 137.0)),
            Dialog::FileInfo { .. } => ("Download File Info".into(), tpl(380.0, 128.0)),
            Dialog::Progress { id, details, .. } => {
                // NDM (0x4a6d80) retitles when the whole percent changes: "N% name", the name alone at 0 and 100
                let t = self.core.find(*id).map(|j| {
                    let i = j.info();
                    let pct = i.size.filter(|s| *s > 0).map(|s| i.done() * 100 / s).unwrap_or(0);
                    let name = if i.name.is_empty() { &i.url } else { &i.name };
                    if pct == 0 || pct >= 100 { name.clone() } else { format!("{pct}% {name}") }
                });
                (t.unwrap_or_default(), Some(Vec2::new(353.0 * PROG_DX, dlu::size(1.0, if *details { 250.0 } else { 147.0 }).y)))
            }
            Dialog::Complete { .. } => ("Download complete".into(), tpl(303.0, 132.0)),
            Dialog::ProgCustom { .. } => ("Customize download progress dialog".into(), tpl(268.0, 145.0)),
            Dialog::Delete { .. } => ("Confirm deletion of downloads".into(), tpl(278.0, 86.0)),
            Dialog::Options { .. } => ("New Download Manager Configuration".replace("New Download Manager", "New Download Manager"), tpl(327.0, 309.0)),
            // NDM widens CNewSchedulerDlg past its template (434 DLU): 833 px at 125% (measured)
            Dialog::Scheduler { .. } => ("Scheduler".into(), tpl(478.0, 286.0)),
            Dialog::Batch { .. } => ("Batch download".into(), tpl(316.0, 238.0)),
            Dialog::QueuePick { .. } => (TITLE.into(), tpl(161.0, 89.0)),
            Dialog::GrabSave { .. } => ("Saving the project of NDM Grabber".into(), tpl(218.0, 75.0)),
            Dialog::GrabSchedAsk { .. } => ("Schedule the grabber project".into(), tpl(260.0, 60.0)),
            Dialog::GrabSched { .. } => ("Schedule the grabber project".into(), tpl(440.0, 219.0)),
            Dialog::GrabLogin { .. } => ("Connecting to the site".into(), tpl(273.0, 138.0)),
            Dialog::Export { .. } => ("Export download list".into(), tpl(214.0, 47.0)),
            Dialog::WaitLink { .. } => (TITLE.into(), tpl(240.0, 46.0)),
            Dialog::DialUp => ("Dial up".into(), tpl(186.0, 55.0)),
            Dialog::Help { .. } => ("New Download Manager Help".into(), None),
            Dialog::Limiter { .. } => ("Speed Limiter settings".into(), tpl(302.0, 89.0)),
            Dialog::Update { .. } => ("Quick Update".into(), tpl(186.0, 34.0)),
            Dialog::NewVersion { .. } => ("New version of New Download Manager is available".into(), Some(Vec2::new(520.0, 420.0))),
            Dialog::Virus { .. } => ("Virus checking".into(), tpl(379.0, 235.0)),
            Dialog::Columns { .. } => ("Columns".into(), tpl(236.0, 177.0)),
            Dialog::Find { .. } => ("Find file".into(), tpl(230.0, 148.0)),
            Dialog::ServerConn { .. } => ("Max. connections number for a server".into(), tpl(221.0, 89.0)),
            Dialog::Networks { nics, .. } => ("Networks".into(), tpl(240.0, 66.0 + 12.0 * nics.len().max(1) as f32)),
            Dialog::QueueName { .. } => ("Enter queue name".into(), tpl(186.0, 74.0)),
            Dialog::QuotaHit { .. } => ("Download limits exceeded!".into(), tpl(339.0, 93.0)),
            Dialog::Info { title, .. } => (title.clone(), tpl(260.0, 70.0)),
            Dialog::AdvProxy { .. } => ("Advanced proxy settings".into(), tpl(268.0, 285.0)),
            Dialog::Category { orig, .. } => (if orig.is_some() { "Editing category properties" } else { "Adding a category to New Download Manager categories list" }.to_string(), tpl(322.0, 177.0)),
            Dialog::CatDelete { .. } => (tr("Deleting a category from New Download Manager categories list"), tpl(260.0, 60.0)),
            Dialog::Duplicate { .. } => ("Duplicate download link".into(), tpl(286.0, 154.0)),
            Dialog::FileProps { .. } => ("File Properties".into(), tpl(318.0, 246.0)),
            Dialog::About => ("About New Download Manager".into(), tpl(260.0, 234.0)),
            Dialog::Tip { .. } => ("Tip of the Day".into(), tpl(253.0, 164.0)),
            Dialog::TbCustomize { .. } => ("Customize Toolbar".into(), tpl(357.0, 125.0)),
            // NDM puts its menu bar (menu 256) over both and the toolbar (260) over the results
            Dialog::Grabber { crawl: Some(_), .. } => ("NDM Site Grabber".into(), tpl(508.0, 286.0).map(|s| s + Vec2::new(0.0, GRAB_MENU + GRAB_TB))),
            Dialog::Grabber { step, adv, .. } => {
                let title = i18n::trf("NDM Site Grabber. Step %d --- %s", &[&(*step + 1), &tr(GRAB_STEPS[*step as usize % 4])]);
                (title, tpl(406.0, if *adv && *step != 1 { 307.0 } else { 210.0 }).map(|s| s + Vec2::new(0.0, GRAB_MENU)))
            }
            Dialog::GrabStats { .. } => ("NDM Grabber Statistics".into(), tpl(111.0, 103.0)),
            Dialog::GrabTpl { .. } => ("Template Name".into(), tpl(186.0, 70.0)),
            Dialog::GrabLoad { .. } => ("Load project".into(), tpl(257.0, 138.0)),
            Dialog::GrabSettings { .. } => ("Settings of NDM Grabber".into(), tpl(292.0, 139.0)),
            Dialog::GrabManual { .. } => ("Manual login page".into(), tpl(260.0, 70.0)),
        };
        let size = fixed.unwrap_or(if matches!(d, Dialog::Help { .. }) { Vec2::new(980.0, 660.0) } else { Vec2::new(380.0, 170.0) });
        let id = ViewportId::from_hash_of(("dlg", key));
        // centred on the main window when it is first shown; kept there so the user can move it
        if !self.dlg_pos.contains_key(&key) {
            let at = ctx.input(|i| {
                let v = i.viewport();
                let frame = size + Vec2::new(0.0, 31.0); // title bar ~31 pt over the client area
                v.outer_rect.filter(|r| r.min.x > -10000.0 && !v.minimized.unwrap_or(false) && win::visible(self.hwnd.load(Relaxed))).map(|r| r.center() - frame / 2.0)
            });
            // main window not shown yet (starting up) or in the tray: Windows places it; it moves to the
            // middle of the main window once that is on screen
            if let Some(p) = at {
                self.dlg_pos.insert(key, p);
            }
        }
        let mut vb = ViewportBuilder::default().with_title(i18n::tr_os(&title)).with_inner_size(size).with_resizable(fixed.is_none()).with_maximize_button(false).with_minimize_button(matches!(d, Dialog::Progress { .. }));
        if let Some(p) = self.dlg_pos.get(&key) {
            vb = vb.with_position(*p);
        }
        if matches!(d, Dialog::FileInfo { .. } | Dialog::Complete { .. }) {
            vb = vb.with_window_level(egui::WindowLevel::AlwaysOnTop); // pop over the browser
        }
        // NDM darkens only these (and message boxes); Options, Scheduler... stay light (0x407c90,
        // 0x4409e0, 0x4a528e, 0x444cf0, 0x5569c0, 0x4c3170 use the dark palette); NDM's help follows the theme too
        let dark = self.dark && matches!(d, Dialog::AddUrl { .. } | Dialog::FileInfo { .. } | Dialog::Progress { .. } | Dialog::Complete { .. } | Dialog::Info { .. } | Dialog::Help { .. });
        if self.dark && !dark {
            self.light_titles.push(i18n::tr_os(&title));
            ctx.set_theme(egui::ThemePreference::Light);
        }
        let r = ctx.show_viewport_immediate(id, vb, |ctx, _| {
            let mut keep = true;
            if let Dialog::Progress { min: min @ true, .. } = d {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                *min = false;
            }
            if let Some(sz) = fixed {
                // Resized dialogs (show/hide details, wizard -> results) snap to their template size.
                if ctx.input(|i| i.viewport().inner_rect.is_some_and(|r| (r.size() - sz).length() > 2.0)) {
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(sz));
                }
            }
            let frame = if fixed.is_some() { egui::Frame::new().fill(ctx.style().visuals.panel_fill) } else if matches!(d, Dialog::Help { .. }) { egui::Frame::new() } else { egui::Frame::central_panel(&ctx.style()) };
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| keep = self.dialog_body(ctx, ui, d));
            let closed = ctx.input(|i| i.viewport().close_requested());
            if let (true, Dialog::FileInfo { early: Some(j), .. }) = (closed, &*d) {
                discard(j);
            }
            keep && !closed
        });
        if self.dark && !dark {
            ctx.set_theme(egui::ThemePreference::Dark);
        }
        r
    }

    fn dialog_body(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, d: &mut Dialog) -> bool {
        match d {
            Dialog::AddUrl { url, auth, login, pass, .. } => {
                let mut d = dlu::D::new(ui);
                d.label(8.0, 9.0, 35.0, 9.0, "Address");
                d.edit(45.0, 7.0, 264.0, 14.0, url, true).request_focus();
                d.group(7.0, 24.0, 302.0, 29.0, "");
                d.check(13.0, 22.0, 80.0, 12.0, auth, "Use authorization");
                d.label_r(13.0, 36.0, 60.0, 9.0, "Login");
                d.edit(77.0, 34.0, 78.0, 14.0, login, *auth);
                d.label_r(160.0, 36.0, 64.0, 9.0, "Password");
                let r = d.rect(228.0, 34.0, 75.0, 14.0);
                d.ui.put(r, |ui: &mut egui::Ui| ui.add_enabled(*auth, egui::TextEdit::singleline(pass).password(true).desired_width(r.width()).margin(egui::Margin::symmetric(3, 2))));
                let enter = d.ui.input(|i| i.key_pressed(Key::Enter));
                let ok = d.button(316.0, 7.0, 50.0, 13.0, "OK", !url.trim().is_empty()) || (enter && !url.trim().is_empty());
                let cancel = d.button(316.0, 24.0, 50.0, 14.0, "Cancel", true);
                if ok {
                    let u = if *auth { with_login(url.trim(), login, pass) } else { url.trim().to_string() };
                    self.incoming(ctx, Incoming { url: u, force: true, ..Default::default() });
                }
                !(ok || cancel)
            }
            Dialog::FileInfo { info, path, probe, applied, error, hint, remember, replace, auto, early, dir } => {
                if let Some(j) = early.as_ref().filter(|_| !*applied) {
                    // the running download answers what the probe would
                    let i = j.info();
                    let res = match &i.status {
                        Status::Error(e) => Some(Err(e.clone())),
                        _ if i.probed => Some(Ok(engine::Probe { name: i.name.clone(), size: i.size, ranges: i.ranges, modified: i.modified.clone(), stream: i.stream })),
                        _ => None,
                    };
                    drop(i);
                    if let Some(r) = res {
                        *probe.lock().unwrap() = Some(r);
                    }
                }
                if !*applied {
                    if let Some(res) = probe.lock().unwrap().take() {
                        *applied = true;
                        match res {
                            Ok(p) => {
                                let name = if hint.is_empty() { p.name.clone() } else { hint.clone() };
                                info.category = self.core.settings().category_for(&name, &info.url);
                                info.folder = if dir.is_empty() { self.core.settings().folder_for(&info.category) } else { dir.clone() };
                                if replace.is_none() {
                                    let name = self.core.unique_name(&info.folder, &name);
                                    *path = Path::new(&info.folder).join(name).display().to_string();
                                }
                                (info.size, info.ranges, info.probed, info.stream) = (p.size, p.ranges, true, p.stream);
                            }
                            Err(e) => *error = e,
                        }
                    }
                }
                let icon = self.icon(ctx, &core::ext(path));
                let mut d = dlu::D::new(ui);
                d.label_r(5.0, 5.0, 54.0, 10.0, "URL");
                d.readonly(64.0, 3.0, 244.0, 14.0, &info.url);
                d.label_r(5.0, 22.0, 54.0, 10.0, "Category");
                let cat_names = self.core.settings().category_names();
                let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
                let changed = d.combo(64.0, 22.0, 116.0, 14.0, "cat", &mut info.category, &cats, true);
                if d.button(183.0, 21.0, 16.0, 14.0, "+", true) {
                    self.edit_category(None);
                }
                if changed {
                    let name = Path::new(path.as_str()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    *path = Path::new(&self.core.settings().folder_for(&info.category)).join(name).display().to_string();
                }
                d.label_r(5.0, 41.0, 54.0, 10.0, "Save As");
                d.edit(64.0, 39.0, 223.0, 14.0, path, true);
                if d.button(292.0, 39.0, 16.0, 14.0, "...", true) {
                    let p = Path::new(path.as_str());
                    let mut fd = rfd::FileDialog::new();
                    if let Some(dir) = p.parent().filter(|d| d.exists()) {
                        fd = fd.set_directory(dir);
                    }
                    if let Some(n) = p.file_name() {
                        fd = fd.set_file_name(n.to_string_lossy());
                    }
                    if let Some(chosen) = fd.save_file() {
                        *path = chosen.display().to_string();
                    }
                }
                d.group(64.0, 56.0, 244.0, 32.0, "");
                d.check(69.0, 53.0, 200.0, 13.0, remember, &i18n::trf("Remember this path for \"%s\" category", &[&tr(&info.category)]));
                let folder = Path::new(path.as_str()).parent().map(|f| f.display().to_string()).unwrap_or_default();
                d.readonly(69.0, 69.0, 232.0, 14.0, &folder);
                d.label_r(5.0, 93.0, 54.0, 10.0, "Description");
                d.edit(64.0, 91.0, 244.0, 14.0, &mut info.description, true);
                if let Some(t) = icon {
                    let r = d.rect(333.0, 28.0, 20.0, 20.0);
                    d.ui.put(Rect::from_center_size(r.center(), Vec2::splat(32.0)), egui::Image::new((t, Vec2::splat(32.0))));
                }
                let size_text = if !error.is_empty() { RichText::new(tr("Error")).color(Color32::RED) } else if !*applied { RichText::new(tr("")) } else { RichText::new(info.size.map_or(tr("Unknown"), human)) };
                let r = d.rect(311.0, 57.0, 64.0, 10.0);
                d.ui.put(r, egui::Label::new(size_text)).on_hover_text(error.as_str());
                if !*applied {
                    let r = d.rect(333.0, 72.0, 20.0, 12.0);
                    d.ui.put(r, egui::Spinner::new());
                }
                let later = d.button(64.0, 110.0, 74.0, 14.0, "Download Later", true);
                let now = d.button(149.0, 110.0, 74.0, 14.0, "Start Download", true);
                let cancel = d.button(234.0, 110.0, 74.0, 14.0, "Cancel", true);
                if later || now {
                    let p = PathBuf::from(path.trim());
                    let mut i = info.clone();
                    i.folder = p.parent().map(|f| f.display().to_string()).filter(|f| !f.is_empty()).unwrap_or_else(|| info.folder.clone());
                    i.name = p.file_name().map(|n| engine::sanitize(&n.to_string_lossy())).unwrap_or_default();
                    if let Some(old) = *replace {
                        // overwrite: the new download takes over the old one's file name (its .part is recreated)
                        self.core.detach(old);
                        self.sel.remove(&old);
                    } else if !i.name.is_empty() {
                        i.name = self.core.unique_name(&i.folder, &i.name);
                    }
                    let auto = { let s = self.core.settings(); s.categories.iter().find(|c| c.name == i.category).map_or(s.remember_general, |c| c.remember) };
                    if *remember || auto {
                        self.core.settings().cat_dirs.insert(i.category.clone(), i.folder.clone());
                    }
                    (i.queued, i.queue) = (later, core::MAIN_QUEUE);
                    self.cancels.remove(&i.url);
                    let job = match early.take() {
                        Some(j) => {
                            // keep what was downloaded: stop, move the .part to the chosen name, continue
                            j.stop();
                            while j.running() {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            let mut ji = j.info();
                            let from = ji.part();
                            (ji.folder, ji.name, ji.category, ji.description, ji.queued, ji.queue) = (i.folder.clone(), i.name.clone(), i.category.clone(), i.description.clone(), i.queued, i.queue);
                            if from != ji.part() {
                                let _ = std::fs::create_dir_all(&ji.folder);
                                if std::fs::rename(&from, ji.part()).is_err() {
                                    ji.segs.clear(); // couldn't move it: start over at the new place
                                    let _ = std::fs::remove_file(&from);
                                }
                            }
                            drop(ji);
                            self.core.adopt(j.clone());
                            j
                        }
                        None => self.core.add(i.clone()),
                    };
                    let id = i.id;
                    if later {
                        self.pick_queue(vec![id], false);
                    }
                    if now {
                        self.core.start(&job);
                        if self.core.settings().show_progress {
                            self.auto_progress(id);
                        }
                    }
                }
                if let Some(j) = early.take().filter(|_| cancel) {
                    discard(&j);
                }
                if cancel && *auto {
                    let n = self.cancels.entry(info.url.clone()).or_default();
                    *n += 1;
                    if *n >= 2 && self.core.settings().ask_exception {
                        self.cancels.remove(&info.url);
                        let site = url::Url::parse(&info.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                        self.open(Dialog::Cancelled { site, url: info.url.clone(), by_site: false, by_addr: true, dont_show: false });
                    }
                }
                !(later || now || cancel)
            }
            Dialog::Progress { id, tab, details, .. } => {
                let Some(job) = self.core.find(*id) else { return false };
                let speed = job.speed();
                let i = job.info().clone();
                let running = job.running();
                let done = i.done();
                let frac = i.size.filter(|s| *s > 0).map(|s| done as f64 / s as f64);
                let mut d = dlu::D::new(ui).with_dx(PROG_DX);
                // tabs the user hid (template 367 / "Hide tab", 0x4a46d0) are left out; `tab` indexes the shown ones
                let (sl_tab, c_tab, btns) = { let s = self.core.settings(); (s.prog_sl_tab, s.prog_c_tab, s.prog_tab_btns) };
                let pages: Vec<(u8, &str)> = [(0, "Download status"), (1, "Speed Limiter"), (2, "Options on completion")].into_iter().filter(|(p, _)| *p == 0 || (*p == 1 && sl_tab) || (*p == 2 && c_tab)).collect();
                *tab = (*tab).min(pages.len() as u8 - 1);
                let names: Vec<&str> = pages.iter().map(|p| p.1).collect();
                let (px, py, _, _) = d.tabs(7.0, 5.0, 338.0, 101.0, tab, &names);
                let (ox, oy) = (px - 7.0, py - 3.0); // page templates are laid out from (7, 3)
                let page = pages[*tab as usize].0;
                // "Hide tab" (templates 364/365, 0x4a41a0 clears bHideSLTabOnDwnlDlg)
                if page > 0 && btns && d.button(ox + 252.0, oy + 64.0, 65.0, 14.0, "Hide tab", true) {
                    let mut s = self.core.settings();
                    if page == 1 { s.prog_sl_tab = false } else { s.prog_c_tab = false }
                    drop(s);
                    self.core.save();
                    *tab = 0;
                }
                match page {
                    0 => {
                        let status = match &i.status {
                            Status::Error(e) => format!("{}: {e}", tr("Error")),
                            Status::Connecting => tr("Connecting..."),
                            Status::Downloading => tr("Receiving data..."),
                            Status::Complete => tr("Complete"),
                            Status::Stopped => tr("Stopped"),
                        };
                        let left = i.size.filter(|_| speed > 0.0).map(|s| time_left((s.saturating_sub(done) as f64 / speed) as u64));
                        // NDM's formats: "%s ( %d%s%02d %% )" until 100 %, "%s/%s" and " (Limited)" for the rate
                        let downloaded = match frac {
                            Some(f) if f < 1.0 => format!("{}  ( {} % )", size3(done), pct2(f * 100.0)),
                            _ => size3(done),
                        };
                        let limited = i.limit_kb > 0 || self.core.settings().limit_on;
                        let speed_text = if running { format!("{}/{}{}", size3(speed as u64), tr("sec"), if limited { format!(" {}", tr("(Limited)")) } else { String::new() }) } else { String::new() };
                        d.label(ox + 7.0, oy + 4.0, 329.0, 9.0, i.url.as_str());
                        for (y, k, vx, v) in [
                            (15.0, "Status", 53.0, status),
                            (28.0, "File size", 75.0, i.size.map_or(tr("(unknown)"), size3)),
                            (38.0, "Downloaded", 75.0, downloaded),
                            (48.0, "Transfer rate", 75.0, speed_text),
                            (58.0, "Time left", 75.0, left.unwrap_or_else(|| if running { "(unknown)".into() } else { String::new() })),
                            (68.0, "Resume capability", 127.0, if !i.probed { "(unknown)".into() } else if i.ranges { "Yes".into() } else { "No".into() }),
                        ] {
                            d.label(ox + 7.0, oy + y, vx - 9.0, 9.0, k);
                            d.label(ox + vx, oy + y, 260.0, 9.0, v);
                        }
                    }
                    1 => {
                        d.label(ox + 7.0, oy + 7.0, 63.0, 9.0, "Transfer rate");
                        d.label(ox + 75.0, oy + 7.0, 202.0, 9.0, if running { format!("{}/{}", size3(speed as u64), tr("sec")) } else { String::new() });
                        let mut on = i.limit_kb > 0;
                        let mut kb = if on { i.limit_kb.to_string() } else { "100".into() };
                        let toggled = d.check(ox + 7.0, oy + 20.0, 154.0, 10.0, &mut on, "Use Speed Limiter").changed();
                        d.label(ox + 7.0, oy + 32.0, 170.0, 9.0, "Maximum download speed:");
                        let edited = d.edit(ox + 7.0, oy + 43.0, 51.0, 14.0, &mut kb, on).changed();
                        d.label(ox + 64.0, oy + 45.0, 65.0, 9.0, "KBytes/sec");
                        if toggled || edited {
                            job.set_limit_kb(if on { kb.trim().parse().unwrap_or(100).max(1) } else { 0 });
                        }
                    }
                    _ => {
                        d.label(ox + 7.0, oy + 3.0, 316.0, 9.0, format!("{} {}", tr("Save To:"), i.path().display()));
                        let mut show = self.core.settings().show_complete;
                        if d.check(ox + 7.0, oy + 15.0, 169.0, 10.0, &mut show, "Show download complete dialog").changed() {
                            self.core.settings().show_complete = show;
                        }
                        d.hline(ox + 7.0, oy + 27.0, 316.0);
                        d.label(ox + 7.0, oy + 30.0, 316.0, 9.0, "Power options");
                        let flags = self.on_done.entry(*id).or_default();
                        d.check(ox + 7.0, oy + 41.0, 225.0, 10.0, &mut flags.0, "Exit New Download Manager when done");
                        d.check(ox + 7.0, oy + 52.0, 225.0, 10.0, &mut flags.1, "Turn off computer when done");
                    }
                }
                d.progress(7.0, 111.0, 338.0, 10.0, frac.unwrap_or(0.0) as f32);
                if d.button(15.0, 126.0, 93.0, 15.0, if *details { "<< Hide details" } else { "Show details >>" }, true) {
                    *details = !*details;
                    self.core.settings().prog_details = *details; // NDM saves dPrDtVis (0x4a3490)
                    self.core.save();
                }
                let finished = i.status == Status::Complete;
                let pause_label = if running { "Pause" } else { "Resume" };
                if d.button(203.0, 126.0, 56.0, 15.0, pause_label, running || !finished) {
                    if running { job.stop() } else { self.core.start(&job) }
                }
                let cancel = d.button(277.0, 126.0, 50.0, 15.0, "Cancel", true);
                if cancel {
                    job.stop();
                }
                if *details {
                    let r = d.rect(21.0, 143.0, 306.0, 9.0);
                    d.ui.put(r, egui::Label::new("Start positions and download progress by connections"));
                    let r = d.rect(7.0, 153.0, 338.0, 10.0);
                    let bars = self.res.bars.as_ref().map(|b| b.each_ref().map(|t| t.id()));
                    d.ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| segment_bar(ui, &i, r.height(), bars));
                    // NDM's connections list (id 1177): N. / Downloaded / Info and an empty fourth column, a row
                    // for every connection the download may open
                    let cols = [("N.", 18.5), ("Downloaded", 62.5), ("Info", 197.0), ("", 58.0)];
                    let rows = d.listview(7.0, 168.0, 338.0, 79.0, &cols);
                    let dx = d.dx();
                    let font = egui::TextStyle::Body.resolve(d.ui.style());
                    let ink = d.ui.visuals().text_color();
                    let n = i.conns.max(i.segs.len()).max(1);
                    d.ui.scope_builder(egui::UiBuilder::new().max_rect(rows), |ui| {
                        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                            for k in 0..n {
                                let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
                                let sg = i.segs.get(k);
                                let got = sg.map_or(0, |s| s.next - s.start);
                                let info = match sg {
                                    Some(s) if s.done() => tr("Disconnect."),
                                    Some(s) if s.net.is_empty() => tr(&s.info),
                                    Some(s) => format!("{} ({})", tr(&s.info), s.net),
                                    None => String::new(),
                                };
                                let mut x = row.left();
                                for (c, text) in [format!("{:2}", k + 1), if got > 0 { size3(got) } else { String::new() }, info].into_iter().enumerate() {
                                    let clip = egui::Rect::from_min_size(egui::pos2(x, row.top()), Vec2::new(cols[c].1 * dx - 4.0, row.height()));
                                    ui.painter().with_clip_rect(clip).text(egui::pos2(x + 6.0, row.center().y), Align2::LEFT_CENTER, text, font.clone(), ink);
                                    x += cols[c].1 * dx;
                                }
                            }
                        });
                    });
                }
                !cancel
            }
            Dialog::ProgCustom { view, sl, c, btns } => {
                // template 367, OK = 0x435710
                let mut d = dlu::D::new(ui);
                d.label(7.0, 6.0, 178.0, 10.0, "Start view:");
                let views = ["Show normally sized", "Show minimized", "Don't show"].map(tr);
                let mut v = views[*view as usize % 3].clone();
                if d.combo(7.0, 18.0, 90.0, 14.0, "pview", &mut v, &views.each_ref().map(String::as_str), true) {
                    *view = views.iter().position(|x| *x == v).unwrap_or(0) as u8;
                }
                d.text(7.0, 34.0, 254.0, 27.0, tr("Note: \"Start view\" option doesn't relate to queue processing. When NDM is processing queues,  \"Download progress\" dialog always starts minimized in system tray"));
                d.hline(7.0, 66.0, 254.0);
                d.check(7.0, 74.0, 254.0, 10.0, sl, "Show \"Speed Limiter\" tab");
                d.check(7.0, 88.0, 189.0, 10.0, c, "Show \"Options on completion\" tab");
                d.check(7.0, 101.0, 254.0, 10.0, btns, "Show \"Hide tab\" buttons");
                let ok = d.button(77.0, 124.0, 50.0, 14.0, "OK", true);
                if ok {
                    let v = (*view, *sl, *c, *btns);
                    self.opt_patch.push(Box::new(move |s: &mut Settings| (s.prog_view, s.prog_sl_tab, s.prog_c_tab, s.prog_tab_btns) = v));
                }
                !(ok || d.button(139.0, 124.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Complete { id, dont_show } => {
                let Some(job) = self.core.find(*id) else { return false };
                let i = job.info().clone();
                let path = i.path();
                let icon = self.icon(ctx, &core::ext(&i.name));
                let mut d = dlu::D::new(ui);
                if let Some(t) = icon {
                    let r = d.rect(7.0, 7.0, 21.0, 20.0);
                    d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
                }
                d.text(34.0, 7.0, 262.0, 32.0, format!("{}\n{}", tr("Download complete"), i18n::trf("Downloaded %s (%I64d Bytes)", &[&human(i.done()), &i.done()])));
                d.label(7.0, 38.0, 289.0, 9.0, "Address");
                d.readonly(7.0, 48.0, 289.0, 14.0, &i.url);
                d.label(7.0, 66.0, 289.0, 9.0, "The file saved as");
                d.readonly(7.0, 76.0, 289.0, 14.0, &path.display().to_string());
                let mut close = false;
                if d.button(7.0, 96.0, 61.0, 14.0, "Open", true) {
                    explore(&path);
                    close = true;
                }
                if d.button(76.0, 96.0, 61.0, 14.0, "Open with...", true) {
                    open_with(&path);
                    close = true;
                }
                if d.button(145.0, 96.0, 61.0, 14.0, "Open folder", true) {
                    open_folder_of(&path);
                    close = true;
                }
                close |= d.button(235.0, 96.0, 61.0, 14.0, "Close", true);
                d.check(16.0, 117.0, 238.0, 10.0, dont_show, "Don't show this dialog again");
                if close && *dont_show {
                    self.core.settings().show_complete = false;
                }
                !close
            }
            Dialog::Delete { ids, files, dont_show } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 6.0, 264.0, 18.0, tr("Are you sure you want to delete selected downloads from New Download Manager list of downloads?"));
                d.check(12.0, 26.0, 259.0, 18.0, files, "Delete completely downloaded files from your hard disk as well. Be careful !");
                d.check(12.0, 47.0, 247.0, 10.0, dont_show, "Don't show this dialog again");
                let yes = d.button(76.0, 65.0, 50.0, 14.0, "Yes", true);
                let no = d.button(151.0, 65.0, 50.0, 14.0, "No", true);
                if yes {
                    let (ids, files) = (ids.clone(), *files);
                    if *dont_show {
                        self.core.settings().confirm_delete = false;
                    }
                    self.remove(&ids, files);
                }
                !(yes || no)
            }
            Dialog::Options { tab, s } => {
                // CPropertySheet: tab control at (7,7) holding 268x257 DLU pages, OK/Cancel/Help below.
                let res = (self.res.icon(277), self.res.icon(295), self.res.icon(276), self.res.icon(293), self.res.icon(273), self.res.icon(278));
                let ext_dir = extension_dir();
                let mut d = dlu::D::new(ui);
                let names = ["General", "File types", "Save to", "Downloads", "Connection", "", "Proxy / Socks", "Sites Logins", "Dial Up / VPN", "Sounds"];
                if let Some(mut a) = self.adv_done.take() {
                    if let Some((on, h, p, u, pw)) = a.http.take() {
                        (s.proxy_http, s.proxy_host, s.proxy_port, s.proxy_user, s.proxy_pass) = (on, h, p, u, pw);
                        if on {
                            s.proxy_mode = 3;
                        }
                    }
                    s.adv = a;
                }
                for f in self.opt_patch.drain(..) {
                    f(s);
                }
                let mut adv = None;
                let mut open = None;
                let mut new_cat: Option<Option<String>> = None;
                let mut integrate: Vec<core::Browser> = Vec::new();
                // measured on NDM (125%): 570 px wide sheet, tab rows General..Connection / Proxy..Sounds,
                // pages laid out at PAGE_DX
                d.tabs(4.6, 5.5, 316.0, 280.5, tab, &names);
                let (ox, oy) = (6.3, 30.5);
                let e = |x: f32, y: f32| (ox + x, oy + y);
                match *tab {
                    0 => {
                        let (x, y) = e(0.0, 0.0);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(res.0, 48.0, "Browser/System Integration");
                        if d2.check(13.0, 25.0, 240.0, 10.0, &mut s.autostart, "Launch New Download Manager on startup").changed() {}
                        d2.check_off(13.0, 37.0, 248.0, 10.0, "Run module for click monitoring in IE-based browsers (MSN, Avant, etc)");
                        d2.check(13.0, 49.0, 241.0, 10.0, &mut s.clip_watch, "Automatically start downloading of URLs placed to clipboard");
                        d2.check(13.0, 61.0, 240.0, 10.0, &mut s.adv_integration, "Use advanced browser integration");
                        d2.group(4.0, 73.0, 261.0, 144.0, "Capture downloads from the following browsers:");
                        let list = d2.listview(13.0, 84.0, 241.0, 84.0, &[("Browser", 120.0), ("Type of integration", 119.0)]);
                        let sel_id = egui::Id::new("browser_sel");
                        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten().filter(|k| *k < s.browsers.len());
                        let icons: Vec<Option<egui::TextureId>> = s.browsers.iter().map(|b| self.exe_icon(ctx, &b.exe)).collect();
                        d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink2(Vec2::new(4.0, 0.0)).with_min_y(list.min.y + 20.0)), |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for (k, b) in s.browsers.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut b.on, tr(""));
                                        if let Some(t) = icons[k] {
                                            ui.image((t, Vec2::splat(16.0)));
                                        }
                                        let w = 120.0 * PAGE_DX - 48.0;
                                        if ui.add_sized([w, 16.0], egui::SelectableLabel::new(sel == Some(k), b.name.as_str())).clicked() {
                                            sel = Some(k);
                                        }
                                        ui.label(integration(b));
                                    });
                                }
                            });
                        });
                        if d2.button(13.0, 173.0, 72.0, 14.0, "Reintegrate All", ext_dir.is_some()) {
                            for b in s.browsers.iter().filter(|b| b.on && extension_page(b).is_some()) {
                                integrate.push(b.clone());
                            }
                        }
                        if d2.button(97.0, 173.0, 72.0, 14.0, "Add browser...", true) {
                            if let Some(p) = rfd::FileDialog::new().add_filter("Programs", &["exe"]).pick_file() {
                                let name = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                s.browsers.push(core::Browser { name, exe: p.display().to_string(), on: true });
                                sel = Some(s.browsers.len() - 1);
                            }
                        }
                        if d2.button(182.0, 173.0, 72.0, 14.0, "Details", sel.is_some()) {
                            open = Some(Dialog::BrowserDetails { name: s.browsers[sel.unwrap()].name.clone() });
                        }
                        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
                        d2.hline(16.0, 192.0, 238.0);
                        d2.label(10.0, 202.0, 190.0, 9.0, "Customize keys to prevent or force downloading with New Download Manager");
                        if d2.button(204.0, 198.0, 50.0, 14.0, "Keys...", true) {
                            open = Some(Dialog::Keys { k: s.keys.clone() });
                        }
                        d2.label(10.0, 224.0, 190.0, 9.0, "Customize menu items in context menu of browsers");
                        if d2.button(204.0, 221.0, 50.0, 14.0, "Edit...", true) {
                            open = Some(Dialog::MenuItems { dl: s.menu_dl, all: s.menu_all });
                        }
                        d2.label(12.0, 243.0, 188.0, 9.0, "Customize download panels in browsers");
                        if d2.button(204.0, 240.0, 50.0, 14.0, "...", true) {
                            open = Some(Dialog::Panel { tab: 0, types: s.panel_types.clone(), min: s.panel_min.clone(), mini: s.panel_mini, sel: None,
                                nocap: s.panel_nocap, drm: s.panel_drm, smode: s.sel_mode, sites: s.sel_sites.clone(), smini: s.sel_mini, ssel: None });
                        }
                    }
                    1 => {
                        let (x, y) = e(0.0, 0.0);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(res.1, 45.0, "Downloaded file types");
                        d2.label(7.0, 27.0, 237.0, 10.0, "Automatically start downloading the following file types:");
                        d2.edit(7.0, 39.0, 254.0, 36.0, &mut s.file_types, true);
                        if d2.button(203.0, 78.0, 50.0, 14.0, "Default", true) {
                            s.file_types = Settings::default().file_types;
                        }
                        d2.label(7.0, 100.0, 235.0, 10.0, "Don't start downloading automatically from the following sites:");
                        d2.edit(7.0, 112.0, 254.0, 30.0, &mut s.skip_sites, true);
                        d2.label(7.0, 144.0, 189.0, 10.0, "(separate names by spaces)");
                        if d2.button(203.0, 145.0, 50.0, 14.0, "Default", true) {
                            s.skip_sites.clear();
                        }
                        d2.label(7.0, 164.0, 254.0, 10.0, "Don't start downloading automatically from the following addresses:");
                        if d2.button(7.0, 176.0, 91.0, 14.0, "Edit list ...", true) {
                            open = Some(Dialog::Exceptions { list: s.skip_urls.clone(), sel: None, ask: s.ask_exception });
                        }
                    }
                    2 => {
                        let (x, y) = e(0.0, 0.0);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(res.2, 48.0, "Categories, file types, folders");
                        d2.group(7.0, 25.0, 253.0, 128.0, "Save To...");
                        d2.label(13.0, 42.0, 121.0, 10.0, "Category");
                        let mut cat = s.cat_dirs.keys().next().cloned().filter(|_| false).unwrap_or_else(|| d2.ui.data_mut(|m| m.get_temp::<String>(egui::Id::new("optcat")).unwrap_or_else(|| GENERAL.to_string())));
                        let cat_names = self.core.settings().category_names();
                let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
                        if d2.combo(13.0, 55.0, 141.0, 14.0, "optcat", &mut cat, &cats, true) {
                            d2.ui.data_mut(|m| m.insert_temp(egui::Id::new("optcat"), cat.clone()));
                        }
                        if d2.button(205.0, 35.0, 50.0, 14.0, "New", true) {
                            new_cat = Some(None);
                        }
                        if d2.button(205.0, 55.0, 50.0, 14.0, "Edit...", cat != GENERAL) {
                            new_cat = Some(Some(cat.clone()));
                        }
                        let exts = s.categories.iter().find(|c| c.name == cat).map_or(String::new(), |c| c.exts.clone());
                        let exts = exts.as_str();
                        d2.label(14.0, 74.0, 238.0, 10.0, i18n::trf("Automatically put in \"%s\" category the following file types:", &[&tr(&cat)]));
                        d2.readonly(14.0, 85.0, 241.0, 14.0, exts);
                        d2.label(13.0, 106.0, 242.0, 10.0, i18n::trf("Default download directory for \"%s\" category", &[&tr(&cat)]));
                        let mut dir = s.folder_for(&cat);
                        if d2.edit(14.0, 118.0, 187.0, 14.0, &mut dir, true).changed() {
                            if cat == GENERAL {
                                s.base_dir = dir.clone();
                            } else {
                                s.cat_dirs.insert(cat.clone(), dir.clone());
                            }
                        }
                        if d2.button(205.0, 118.0, 50.0, 14.0, "Browse", true) {
                            if let Some(p) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                                let p = p.display().to_string();
                                if cat == GENERAL { s.base_dir = p } else { s.cat_dirs.insert(cat.clone(), p); }
                            }
                        }
                        // = the category's "Remember last save path" (template 204)
                        let label = i18n::trf("Change folder for \"%s\" category on last selected", &[&tr(&cat)]);
                        match s.categories.iter_mut().find(|c| c.name == cat) {
                            Some(c) => { d2.at(15.0, 137.0, 240.0, 10.0, |ui| ui.checkbox(&mut c.remember, label)); }
                            None => { d2.at(15.0, 137.0, 240.0, 10.0, |ui| ui.checkbox(&mut s.remember_general, label)); }
                        }
                        d2.check(15.0, 159.0, 240.0, 10.0, &mut s.set_date, "Set file creation date as provided by the server");
                        d2.group(7.0, 173.0, 253.0, 77.0, "Temporary directory");
                        d2.edit(13.0, 186.0, 187.0, 14.0, &mut s.temp_dir, true);
                        if d2.button(204.0, 186.0, 50.0, 14.0, "Browse", true) {
                            if let Some(p) = rfd::FileDialog::new().set_directory(&s.temp_dir).pick_folder() {
                                s.temp_dir = p.display().to_string();
                            }
                        }
                        d2.text(13.0, 203.0, 242.0, 42.0, "Temporary directory is required for storing file parts during download.\n If you have several physical drives on your computer, you should select different physical drives for temporary directory and \"Save To\" folders for faster assembling of downloaded files.");
                    }
                    3 => {
                        let (x, y) = e(0.0, 0.0);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(res.3, 48.0, "Default download settings");
                        d2.label(7.0, 28.0, 191.0, 10.0, "Customize \"Download progress\" dialog");
                        if d2.button(205.0, 25.0, 50.0, 14.0, "Edit...", true) {
                            open = Some(Dialog::ProgCustom { view: s.prog_view, sl: s.prog_sl_tab, c: s.prog_c_tab, btns: s.prog_tab_btns });
                        }
                        d2.check(9.0, 44.0, 241.0, 10.0, &mut s.show_progress, "Show start download dialog");
                        d2.check(26.0, 57.0, 235.0, 10.0, &mut s.only_queue, "Do not start downloading, only add files to the queue");
                        d2.check(9.0, 69.0, 243.0, 10.0, &mut s.show_complete, "Show download complete dialog");
                        d2.label(14.0, 82.0, 237.0, 10.0, "Note: These settings don't relate to queue processing");
                        d2.hline(7.0, 93.0, 254.0);
                        d2.check(9.0, 96.0, 251.0, 19.0, &mut s.start_at_once, "Start downloading immediately while displaying \"Download File Info\" dialog");
                        d2.check(9.0, 119.0, 251.0, 10.0, &mut s.ask_queue_later, "Show queue selection panel on pressing \"Download Later\"");
                        d2.check(9.0, 134.0, 252.0, 10.0, &mut s.ask_queue_batch, "Show queue selection panel on adding batch downloads");
                        d2.check(9.0, 149.0, 251.0, 10.0, &mut s.ignore_date, "Ignore file modification time changes when resuming a download");
                        d2.hline(7.0, 164.0, 254.0);
                        d2.label(7.0, 170.0, 246.0, 10.0, "If a duplicate download link is added:");
                        let k = if s.dupl_ask { 0 } else { [3, 1, 2][s.dupl_action.min(2) as usize] };
                        let mut dup = DUPL[k].to_string();
                        if d2.combo(7.0, 180.0, 253.0, 14.0, "dup", &mut dup, &DUPL, true) {
                            match DUPL.iter().position(|x| *x == dup) {
                                Some(0) => s.dupl_ask = true,
                                Some(k) => (s.dupl_ask, s.dupl_action) = (false, [0, 1, 2, 0][k]),
                                None => {}
                            }
                        }
                        d2.hline(7.0, 197.0, 254.0);
                        d2.label(7.0, 203.0, 254.0, 9.0, "User-Agent for manually added downloads:");
                        d2.edit(7.0, 216.0, 254.0, 14.0, &mut s.user_agent, true);
                        d2.hline(7.0, 233.0, 254.0);
                        d2.label(7.0, 242.0, 188.0, 8.0, "Virus checking settings");
                        if d2.button(205.0, 239.0, 50.0, 14.0, "Edit...", true) {
                            open = Some(Dialog::Virus { exe: s.virus_exe.clone(), args: s.virus_args.clone() });
                        }
                    }
                    4 => {
                        let (x, y) = e(0.0, 0.0);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(res.4, 47.0, "Connections and Limits");
                        d2.group(7.0, 30.0, 253.0, 124.0, "Max. connections number");
                        d2.label_r(20.0, 42.0, 148.0, 10.0, "Default max. conn. number");
                        let mut conns = s.conns.to_string();
                        if d2.combo(172.0, 40.0, 67.0, 14.0, "conns", &mut conns, &["1", "2", "4", "8", "16", "24", "32"], true) {
                            s.conns = conns.parse().unwrap_or(8);
                        }
                        d2.hline(13.0, 58.0, 240.0);
                        d2.label(15.0, 61.0, 239.0, 10.0, "Exceptions:");
                        let list = d2.listview(13.0, 71.0, 187.0, 75.0, &[("Server", 120.0), ("Max. connections number", 66.0)]);
                        let sel_id = egui::Id::new("srvconn_sel");
                        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten().filter(|k| *k < s.server_conns.len());
                        let mut edit = None;
                        d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink2(Vec2::new(2.0, 0.0)).with_min_y(list.min.y + 18.0)), |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (k, (m, n)) in s.server_conns.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let a = ui.add_sized([120.0 * PAGE_DX - 4.0, 16.0], egui::SelectableLabel::new(sel == Some(k), m.as_str()));
                                    let b = ui.add_sized([66.0 * PAGE_DX - 8.0, 16.0], egui::SelectableLabel::new(sel == Some(k), n.to_string()));
                                    if a.clicked() || b.clicked() {
                                        sel = Some(k);
                                    }
                                    if a.double_clicked() || b.double_clicked() {
                                        edit = Some(k);
                                    }
                                });
                            }
                        });
                        if d2.button(205.0, 77.0, 50.0, 14.0, "New", true) {
                            self.open(Dialog::ServerConn { orig: None, server: String::new(), n: s.conns.to_string() });
                        }
                        if d2.button(205.0, 95.0, 50.0, 14.0, "Delete", sel.is_some()) {
                            s.server_conns.remove(sel.unwrap());
                            sel = None;
                        }
                        if d2.button(205.0, 113.0, 50.0, 14.0, "Edit", sel.is_some()) {
                            edit = sel;
                        }
                        if let Some(k) = edit {
                            let (server, n) = s.server_conns[k].clone();
                            self.open(Dialog::ServerConn { orig: Some(k), server, n: n.to_string() });
                        }
                        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
                        d2.group(7.0, 161.0, 253.0, 55.0, "");
                        // NDM's quota (template 141 group at [7,161]); the speed limit has its own dialog (337)
                        d2.check(18.0, 158.0, 64.0, 12.0, &mut s.quota_on, "Download limits");
                        let on = s.quota_on;
                        d2.label_r(14.0, 177.0, 93.0, 10.0, "Download no more than");
                        d2.drag(112.0, 174.0, 24.0, 12.0, &mut s.quota_mb, 1, 999_999);
                        d2.label(141.0, 177.0, 72.0, 10.0, "MBytes");
                        d2.label_r(55.0, 189.0, 52.0, 10.0, "every");
                        d2.drag(112.0, 187.0, 24.0, 12.0, &mut s.quota_hours, 1, 23);
                        d2.label(143.0, 189.0, 69.0, 10.0, "hours");
                        if on {
                            d2.check(18.0, 202.0, 209.0, 10.0, &mut s.quota_warn, "Show warning before stopping downloads");
                        } else {
                            d2.check_off(18.0, 202.0, 209.0, 10.0, "Show warning before stopping downloads");
                        }
                        if net::nic_supported() {
                            d2.check(11.0, 224.0, 190.0, 10.0, &mut s.multi_net, "Download over several network connections at once");
                            d2.label(23.0, 237.0, 178.0, 10.0, "Each network needs its own internet connection");
                        } else {
                            s.multi_net = false;
                            d2.check_off(11.0, 224.0, 190.0, 10.0, "Download over several network connections at once");
                            d2.label(23.0, 237.0, 178.0, 10.0, "Not available: Linux 5.7 or later is needed to choose the network");
                        }
                        if d2.button(205.0, 222.0, 50.0, 14.0, "Networks...", s.multi_net) {
                            self.open(Dialog::Networks { nics: win::nics(), off: s.nets_off.clone() });
                        }
                    }
                    6 => {
                        // Site Logins (template 145): credentials applied to matching download addresses.
                        let (x, y) = e(0.0, 0.0);
                        let icon = self.res.icon(272);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(icon, 42.0, "User names and passwords for servers/sites");
                        let list = d2.listview(7.0, 28.0, 255.0, 172.0, &[("Server/path", 110.0), ("User name", 75.0), ("Password", 70.0)]);
                        let sel_id = egui::Id::new("login_sel");
                        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten();
                        d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink2(Vec2::new(2.0, 0.0)).with_min_y(list.min.y + 18.0)), |ui| {
                            for (k, l) in s.logins.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    let w = |n: f32| n * PAGE_DX - 6.0;
                                    let a = ui.add(egui::TextEdit::singleline(&mut l.site).desired_width(w(110.0)).hint_text("example.com"));
                                    let b = ui.add(egui::TextEdit::singleline(&mut l.user).desired_width(w(75.0)));
                                    let c = ui.add(egui::TextEdit::singleline(&mut l.pass).password(true).desired_width(w(70.0)));
                                    if a.gained_focus() || b.gained_focus() || c.gained_focus() {
                                        sel = Some(k);
                                    }
                                    if ui.ctx().memory(|m| m.has_focus(a.id)) && ui.data(|m| m.get_temp::<bool>(sel_id.with("edit"))).unwrap_or(false) {
                                        ui.data_mut(|m| m.remove::<bool>(sel_id.with("edit")));
                                    }
                                    if sel == Some(k) && ui.data(|m| m.get_temp::<bool>(sel_id.with("edit"))).unwrap_or(false) {
                                        a.request_focus();
                                    }
                                });
                            }
                        });
                        if d2.button(13.0, 205.0, 50.0, 14.0, "New", true) {
                            s.logins.push(Default::default());
                            sel = Some(s.logins.len() - 1);
                            d2.ui.data_mut(|m| m.insert_temp(sel_id.with("edit"), true));
                        }
                        let has = sel.is_some_and(|k| k < s.logins.len());
                        if d2.button(76.0, 205.0, 50.0, 14.0, "Edit", has) {
                            d2.ui.data_mut(|m| m.insert_temp(sel_id.with("edit"), true));
                        }
                        if d2.button(139.0, 205.0, 50.0, 14.0, "Remove", has) {
                            s.logins.remove(sel.unwrap());
                            sel = None;
                        }
                        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
                    }
                    7 => {
                        // Dial Up / VPN (template 185, COptDialUp): Windows' connections; Apply stores the user
                        // name (and with "Save password" the password) in Windows, as NDM does (RasSetEntryDialParams).
                        let (x, y) = e(0.0, 0.0);
                        let icon = self.res.icon(275);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(icon, 47.0, "Dial up / VPN settings");
                        d2.check(8.0, 30.0, 247.0, 10.0, &mut s.dial_on, "Use Windows Dial Up / VPN Networking");
                        let on = s.dial_on;
                        d2.group(7.0, 44.0, 254.0, 110.0, "Connection options");
                        for (y, t) in [(62.0, "Connection:"), (84.0, "User name:"), (106.0, "Password:")] {
                            d2.label_r(16.0, y, 59.0, 10.0, if on { RichText::new(tr(t)) } else { RichText::new(tr(t)).weak() });
                        }
                        let id = egui::Id::new("dialup_form");
                        let (mut user, mut pass, mut save) = d2.ui.data_mut(|m| m.get_temp::<(String, String, bool)>(id)).unwrap_or_default();
                        let entries = win::ras_entries();
                        if s.dial_entry.is_empty() {
                            s.dial_entry = entries.first().cloned().unwrap_or_default();
                        }
                        let list: Vec<&str> = entries.iter().map(String::as_str).collect();
                        d2.combo(80.0, 59.0, 156.0, 14.0, "dialup", &mut s.dial_entry, &list, on);
                        d2.edit(80.0, 82.0, 117.0, 14.0, &mut user, on);
                        d2.password(80.0, 103.0, 117.0, 14.0, &mut pass, on);
                        d2.check(80.0, 123.0, 115.0, 10.0, &mut save, "Save password");
                        if d2.button(201.0, 134.0, 50.0, 14.0, "Apply", on && !s.dial_entry.is_empty()) {
                            if let Err(e) = win::ras_save(&s.dial_entry, &user, &pass, save) {
                                self.open(Dialog::Info { title: "Dial Up / VPN".into(), text: e });
                            }
                        }
                        d2.ui.data_mut(|m| m.insert_temp(id, (user, pass, save)));
                        d2.group(7.0, 158.0, 254.0, 54.0, "Redial options");
                        let weak = |t: &str| if on { RichText::new(tr(t)) } else { RichText::new(tr(t)).weak() };
                        d2.label_r(13.0, 172.0, 132.0, 10.0, weak("Redial attempts (zero if endlessly):"));
                        d2.drag(151.0, 169.0, 28.0, 14.0, &mut s.dial_redial, 0, 999);
                        d2.label(184.0, 172.0, 68.0, 10.0, weak("times"));
                        d2.label_r(14.0, 192.0, 131.0, 10.0, weak("Time between redial attempts:"));
                        d2.drag(151.0, 189.0, 28.0, 14.0, &mut s.dial_wait, 1, 3600);
                        d2.label(184.0, 192.0, 69.0, 10.0, weak("seconds"));
                    }
                    8 => {
                        // Sounds (template 195, CSoundsSettings 0x5120c0): a tick and a .wav per event; Browse and
                        // Play stay disabled until a row is picked. Like NDM the picked file is
                        // copied next to our state, so moving or deleting the original doesn't silence the event.
                        let (x, y) = e(0.0, 0.0);
                        let icon = self.res.icon(274);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(icon, 48.0, "Sound settings");
                        d2.group(10.0, 30.0, 255.0, 188.0, "Select sounds for New Download Manager events");
                        let list = d2.listview(15.0, 40.0, 244.0, 154.0, &[("Event", 120.0), ("Sound file", 124.0)]);
                        let sel_id = egui::Id::new("sound_sel");
                        let mut sel: Option<usize> = d2.ui.data(|m| m.get_temp(sel_id)).flatten();
                        d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink2(Vec2::new(4.0, 0.0)).with_min_y(list.min.y + 18.0)), |ui| {
                            for (k, (on, path)) in s.sounds.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.add_sized([120.0 * PAGE_DX - 6.0, 18.0], egui::Checkbox::new(on, core::SOUND_EVENTS[k]));
                                    let name = Path::new(path.as_str()).file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                                    if ui.add_sized([124.0 * PAGE_DX - 10.0, 18.0], egui::SelectableLabel::new(sel == Some(k), name)).clicked() {
                                        sel = Some(k);
                                    }
                                });
                            }
                        });
                        if d2.button(63.0, 199.0, 50.0, 14.0, "Browse...", sel.is_some()) {
                            let k = sel.unwrap();
                            if let Some(p) = rfd::FileDialog::new().add_filter(format!("{} (*.wav)", tr("Sound Files")), &["wav"]).pick_file() {
                                let dir = core::store_path().with_file_name("Sounds");
                                let copy = dir.join(p.file_name().unwrap_or_default());
                                let kept = copy == p || (std::fs::create_dir_all(&dir).is_ok() && std::fs::copy(&p, &copy).is_ok());
                                s.sounds[k] = (true, if kept { copy } else { p }.display().to_string());
                            }
                        }
                        if d2.button(153.0, 199.0, 50.0, 14.0, "Play", sel.is_some_and(|k| !s.sounds[k].1.is_empty())) {
                            win::play_sound(&s.sounds[sel.unwrap()].1);
                        }
                        d2.ui.data_mut(|m| m.insert_temp(sel_id, sel));
                    }
                    _ => {
                        let (x, y) = e(0.0, 0.0);
                        let mut d2 = dlu::D::new_offset(d.ui, x, y).with_dx(PAGE_DX);
                        d2.page_header(res.5, 36.0, "Proxy / socks configuration");
                        // below the page, off the header (whose title ends at 255 DLU)
                        if d2.button(197.0, 241.0, 63.0, 14.0, "Get System", true) {
                            // Internet Options' proxy into the manual fields ("h:p" or "http=h:p;https=h:p;...")
                            let (_, server, bypass) = win::system_proxy();
                            let hp = server.split(';').find_map(|p| p.strip_prefix("http=")).or_else(|| Some(server.as_str()).filter(|x| !x.contains('='))).unwrap_or("");
                            let (h, p) = hp.rsplit_once(':').unwrap_or((hp, ""));
                            (s.proxy_host, s.proxy_port, s.proxy_mode) = (h.trim().to_string(), p.trim().to_string(), 3);
                            (s.proxy_bypass_on, s.proxy_bypass) = (!bypass.is_empty(), bypass.replace(';', " ").replace("<local>", "").trim().to_string());
                        }
                        d2.check(13.0, 27.0, 243.0, 19.0, &mut s.proxy_browser, "Use proxy/socks from a browser's request if there are download errors for the downloads intercepted from the browser");
                        d2.hline(8.0, 50.0, 254.0);
                        if d2.radio(7.0, 55.0, 248.0, 10.0, s.proxy_mode == 0, "No proxy/socks") {
                            s.proxy_mode = 0;
                        }
                        d2.hline(8.0, 69.0, 254.0);
                        if d2.radio(7.0, 73.0, 249.0, 10.0, s.proxy_mode == 1, "Use system settings") {
                            s.proxy_mode = 1;
                        }
                        d2.hline(7.0, 86.0, 254.0);
                        if d2.radio(7.0, 90.0, 234.0, 10.0, s.proxy_mode == 2, "Use automatic configuration script") {
                            s.proxy_mode = 2;
                        }
                        d2.label(13.0, 105.0, 35.0, 9.0, "Address");
                        d2.edit(53.0, 102.0, 203.0, 14.0, &mut s.pac_url, s.proxy_mode == 2);
                        d2.hline(7.0, 120.0, 254.0);
                        if d2.radio(8.0, 124.0, 229.0, 10.0, s.proxy_mode == 3, "Manual proxy/socks configuration") {
                            s.proxy_mode = 3;
                        }
                        let m = s.proxy_mode == 3;
                        d2.label(13.0, 136.0, 84.0, 10.0, "Proxy server address");
                        d2.label(108.0, 136.0, 34.0, 10.0, "Port");
                        d2.label(146.0, 136.0, 52.0, 10.0, "UserName");
                        d2.label(202.0, 136.0, 54.0, 10.0, "Password");
                        d2.edit(13.0, 147.0, 91.0, 15.0, &mut s.proxy_host, m);
                        d2.edit(108.0, 147.0, 34.0, 15.0, &mut s.proxy_port, m);
                        d2.edit(146.0, 147.0, 52.0, 15.0, &mut s.proxy_user, m);
                        d2.password(201.0, 147.0, 55.0, 15.0, &mut s.proxy_pass, m);
                        d2.label(13.0, 164.0, 235.0, 10.0, "Use this proxy for the following protocols:");
                        d2.at(13.0, 176.0, 28.0, 10.0, |ui| ui.add_enabled(m, egui::Checkbox::new(&mut s.proxy_http, tr("http"))));
                        d2.at(58.0, 176.0, 31.0, 10.0, |ui| ui.add_enabled(m, egui::Checkbox::new(&mut s.proxy_https, tr("https"))));
                        d2.at(109.0, 176.0, 24.0, 10.0, |ui| ui.add_enabled(m, egui::Checkbox::new(&mut s.proxy_ftp, tr("ftp"))));
                        if d2.button(149.0, 179.0, 107.0, 14.0, "Advanced...", m) {
                            adv = Some(s.adv.clone());
                        }
                        d2.group(13.0, 197.0, 248.0, 41.0, "");
                        d2.at(18.0, 194.0, 238.0, 12.0, |ui| ui.add_enabled(m, egui::Checkbox::new(&mut s.proxy_bypass_on, tr("Do not use proxy server for addresses beginning with:"))));
                        d2.edit(22.0, 208.0, 234.0, 17.0, &mut s.proxy_bypass, m && s.proxy_bypass_on);
                        d2.label(20.0, 227.0, 228.0, 10.0, "Separate names by spaces");
                        d2.hline(7.0, 241.0, 254.0);
                        d2.check(11.0, 244.0, 180.0, 10.0, &mut s.ftp_pasv, "Use FTP in PASV mode");
                    }
                }
                if let Some(mut adv) = adv {
                    adv.http = Some((s.proxy_mode == 3 && s.proxy_http, s.proxy_host.clone(), s.proxy_port.clone(), s.proxy_user.clone(), s.proxy_pass.clone()));
                    self.open(Dialog::AdvProxy { adv });
                }
                if let Some(o) = open {
                    self.open(o);
                }
                if let Some(c) = new_cat {
                    self.edit_category(c);
                }
                for b in integrate {
                    self.open(Dialog::Integrate { b });
                }
                // "Add version..." in Browser details
                for b in self.browser_add.drain(..) {
                    s.browsers.push(b);
                }
                // Sheet buttons
                // NDM's sheet has OK and Help only; closing it (x) cancels
                let ok = d.button(203.0, 291.5, 56.0, 13.0, "OK", true);
                if d.button(265.0, 291.5, 56.0, 13.0, "Help", help("index.htm", false)) {
                    help("options.htm", true);
                }
                let cancel = false;
                if ok {
                    let autostart_changed = self.core.settings().autostart != s.autostart;
                    let dark_changed = self.core.settings().dark_support != s.dark_support;
                    *self.core.settings() = s.clone();
                    self.core.apply_settings();
                    self.core.apply_integration(s.adv_integration);
                    self.core.save();
                    if autostart_changed {
                        win::set_autostart(s.autostart);
                    }
                    if dark_changed {
                        self.theme_check = u32::MAX;
                    }
                }
                !(ok || cancel)
            }
            Dialog::Scheduler { tab, queue, qsel, error } => {
                // CNewSchedulerDlg (template 308): queue tree left, tab pages right, buttons along the bottom.
                let queue_icon = self.res.icon(331);
                let running = self.core.queue_running(queue.id);
                let queues: Vec<(u32, String, bool, bool)> = self.core.settings().queues.iter().map(|q| (q.id, tr(&q.name), self.core.queue_running(q.id), q.sync)).collect();
                if queue.id != LIMITS && !queues.iter().any(|q| q.0 == queue.id) {
                    *queue = self.core.settings().queues[0].clone(); // deleted from elsewhere
                }
                let mut pick = None;
                let mut d = dlu::D::new(ui);
                if let Some(t) = queue_icon {
                    let r = d.rect(7.0, 4.0, 20.0, 20.0);
                    d.ui.put(Rect::from_min_size(r.min, Vec2::splat(32.0)), egui::Image::new((t, Vec2::splat(32.0))));
                }
                let r = d.rect(32.0, 15.0, 61.0, 9.0);
                d.ui.put(r, egui::Label::new(tr("Queues")));
                let tree = d.listview(7.0, 27.0, 115.0, 233.0, &[]);
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(tree.shrink(3.0)), |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    // NDM's queue pictures: yellow folders, green for synchronization, a clock while running;
                    // last the "Download limits" node (red cross)
                    let limits = (LIMITS, tr("Download limits"), false, false);
                    for (id, name, on, sync) in queues.iter().chain([&limits]) {
                        ui.horizontal(|ui| {
                            let pic = if *id == LIMITS { 329 } else { [[311, 312], [315, 316]][*sync as usize][*on as usize] };
                            if let Some(t) = self.res.tree(pic) {
                                ui.add(egui::Image::new((t, Vec2::new(16.0, 13.0))));
                            }
                            if ui.selectable_label(*id == queue.id, name.as_str()).clicked() && *id != queue.id {
                                pick = Some(*id);
                            }
                        });
                    }
                });
                d.vline(132.0, 7.0, 272.0);
                let is_limits = queue.id == LIMITS;
                if is_limits {
                    // the scheduler's "Download limits" page: Options -> Connection's quota, applied at once
                    let mut s = self.core.settings();
                    // positions measured on NDM; the texts grey out with the box unticked
                    d.check(160.0, 20.5, 80.0, 10.0, &mut s.quota_on, "Download limits");
                    let on = s.quota_on;
                    let grey = |t: &str| RichText::new(tr(t)).color(if on { Color32::PLACEHOLDER } else { Color32::GRAY });
                    d.label_r(150.0, 41.0, 105.0, 10.0, grey("Download no more than"));
                    d.at(260.0, 41.0, 23.0, 10.0, |ui| ui.add_enabled(on, egui::DragValue::new(&mut s.quota_mb).range(1..=999_999)));
                    d.label(288.0, 41.0, 40.0, 10.0, grey("MBytes"));
                    d.label_r(200.0, 60.0, 55.0, 10.0, grey("every"));
                    d.at(260.0, 60.0, 23.0, 10.0, |ui| ui.add_enabled(on, egui::DragValue::new(&mut s.quota_hours).range(1..=23)));
                    d.label(288.0, 60.0, 40.0, 10.0, grey("hours"));
                    d.at(160.0, 78.5, 160.0, 10.0, |ui| ui.add_enabled(on, egui::Checkbox::new(&mut s.quota_warn, tr("Show warning before stopping downloads"))));
                }
                let builtin = self.core.settings().queue(queue.id).is_none_or(core::Queue::builtin);
                // shown like NDM's time pickers ("11:00:00 PM"); stored as HH:MM
                for t in [&mut queue.sched.start_at, &mut queue.sched.stop_at] {
                    if let Ok(v) = chrono::NaiveTime::parse_from_str(t, "%H:%M") {
                        *t = v.format("%-I:%M:%S %p").to_string();
                    }
                }
                let sync = &mut queue.sync;
                let (sched, max_active) = (&mut queue.sched, &mut queue.max_active);
                if !is_limits {
                // the queue's name centred over the tabs; pages are laid out 2.0 px/DLU across (1.6 at 100%)
                let r = d.rect(142.0, 3.0, 327.0, 9.0);
                d.ui.put(r, egui::Label::new(format!("{}{}", tr(&queue.name), if running { " \u{25b6}" } else { "" })));
                d.tabs(142.0, 15.5, 327.0, 246.0, tab, &["Schedule", "Files in the queue"]);
                let mut d2 = dlu::D::new_offset(d.ui, 145.6, 27.0).with_dx(PAGE_DX);
                if *tab == 0 {
                    // template 306; the built-in queues keep their type
                    let periodic = *sync;
                    for (x, w, v, t) in [(7.0, 126.0, false, "One-time downloading"), (141.0, 132.0, true, "Periodic synchronization")] {
                        if builtin && periodic != v {
                            d2.radio_off(x, 3.0, w, 10.0, t);
                        } else if d2.radio(x, 3.0, w, 10.0, periodic == v, t) {
                            *sync = v;
                        }
                    }
                    d2.hline(7.0, 16.0, 266.0);
                    d2.check(7.0, 20.0, 249.0, 10.0, &mut sched.on_startup, "Start download on New Download Manager startup");
                    d2.hline(7.0, 33.0, 266.0);
                    d2.check(7.0, 42.0, 97.0, 10.0, &mut sched.start_on, "Start download at");
                    d2.edit(110.0, 39.0, 59.0, 15.0, &mut sched.start_at, sched.start_on);
                    let on = sched.start_on;
                    if periodic {
                        // synchronization runs daily, starting again every N hours after a pass
                        sched.daily = true;
                        d2.at(7.0, 60.0, 90.0, 10.0, |ui| ui.add_enabled(on, egui::Checkbox::new(&mut sched.again_on, tr("Start again every"))));
                        let en = on && sched.again_on;
                        d2.at(100.0, 59.0, 19.0, 14.0, |ui| ui.add_enabled(en, egui::DragValue::new(&mut sched.again_h).range(0..=999)));
                        d2.label(122.0, 62.0, 25.0, 8.0, "hours");
                        d2.at(152.0, 59.0, 19.0, 14.0, |ui| ui.add_enabled(en, egui::DragValue::new(&mut sched.again_m).range(0..=59)));
                        d2.label(173.0, 62.0, 23.0, 8.0, "min");
                        d2.label(15.0, 87.0, 39.0, 9.0, RichText::new(tr("Daily")).color(Color32::GRAY));
                    } else {
                        if d2.radio(12.0, 60.0, 54.0, 10.0, on && !sched.daily, "Once at") && on {
                            sched.daily = false;
                        }
                        if d2.radio(12.0, 78.0, 45.0, 10.0, on && sched.daily, "Daily") && on {
                            sched.daily = true;
                        }
                    }
                    let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
                    let pos = [(62.0, 78.0), (62.0, 88.0), (62.0, 98.0), (135.0, 78.0), (135.0, 88.0), (135.0, 98.0), (209.0, 78.0)];
                    for (k, (x, y)) in pos.iter().enumerate() {
                        let en = on && sched.daily;
                        d2.at(*x, *y, 70.0, 10.0, |ui| ui.add_enabled(en, egui::Checkbox::new(&mut sched.days[k], days[k])));
                    }
                    d2.hline(8.0, 112.0, 265.0);
                    d2.check(8.0, 121.0, 99.0, 10.0, &mut sched.stop_on, "Stop download at");
                    d2.edit(111.0, 119.0, 59.0, 14.0, &mut sched.stop_at, sched.stop_on);
                    d2.check(8.0, 136.0, 204.0, 10.0, &mut sched.retry_on, "Number of retries for each file if downloading failed :");
                    d2.drag(216.0, 132.0, 32.0, 14.0, &mut sched.retries, 1, 999);
                    d2.hline(8.0, 151.0, 265.0);
                    d2.check(7.0, 156.0, 235.0, 10.0, &mut sched.open_on, "Open the following file when done:");
                    d2.edit(7.0, 168.0, 237.0, 14.0, &mut sched.open_file, sched.open_on);
                    if d2.button(249.0, 168.0, 15.0, 14.0, "...", sched.open_on) {
                        if let Some(p) = rfd::FileDialog::new().pick_file() {
                            sched.open_file = p.display().to_string();
                        }
                    }
                    d2.check(7.0, 186.0, 203.0, 10.0, &mut sched.hangup, "Hang up modem when done");
                    d2.check(7.0, 197.0, 207.0, 10.0, &mut sched.exit_done, "Exit New Download Manager when done");
                    d2.check(7.0, 208.0, 182.0, 10.0, &mut sched.off_done, "Turn off computer when done");
                    let kinds = ["Shut down", "Sleep", "Hibernate"];
                    let mut kind = kinds[sched.off_kind.min(2) as usize].to_string();
                    if d2.combo(194.0, 208.0, 79.0, 14.0, "offkind", &mut kind, &kinds, sched.off_done) {
                        sched.off_kind = kinds.iter().position(|k| *k == kind).unwrap_or(0) as u8;
                    }
                    d2.at(19.0, 219.0, 174.0, 10.0, |ui| ui.add_enabled(sched.off_done, egui::Checkbox::new(&mut sched.off_force, tr("Force processes to terminate"))));
                    if !error.is_empty() {
                        d2.label(7.0, 220.0, 266.0, 9.0, RichText::new(error.as_str()).color(Color32::RED));
                    }
                } else {
                    // template 307
                    d2.label(7.0, 8.0, 41.0, 10.0, "Download");
                    d2.drag(53.0, 5.0, 26.0, 14.0, max_active, 1, 20);
                    d2.label(83.0, 8.0, 99.0, 10.0, "files at the same time");
                    let queued: Vec<(u64, String, String, String)> = self
                        .core
                        .jobs()
                        .iter()
                        .map(|j| j.info())
                        .filter(|i| i.queued && i.queue == queue.id)
                        .map(|i| {
                            let st = match &i.status { Status::Complete => tr("Complete"), Status::Error(_) => tr("Error"), _ => String::new() };
                            (i.id, if i.name.is_empty() { i.url.clone() } else { i.name.clone() }, i.size.map(human).unwrap_or_default(), st)
                        })
                        .collect();
                    let list = d2.listview(8.0, 23.0, 264.0, 181.0, &[("File Name", 140.0), ("Size", 60.0), ("Status", 62.0)]);
                    d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
                        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                            for (id, name, size, st) in &queued {
                                ui.horizontal(|ui| {
                                    if ui.add_sized([140.0 * PAGE_DX - 6.0, 17.0], egui::SelectableLabel::new(*qsel == Some(*id), name.as_str())).clicked() {
                                        *qsel = Some(*id);
                                    }
                                    ui.add_sized([60.0 * PAGE_DX - 6.0, 17.0], egui::Label::new(size.as_str()));
                                    ui.label(st.as_str());
                                });
                            }
                        });
                    });
                    let has = qsel.is_some_and(|q| queued.iter().any(|x| x.0 == q));
                    for (k, (glyph, tip, dir)) in [("⏶", "Move up", -1), ("⏷", "Move down", 1), ("✖", "Delete from queue", 0)].into_iter().enumerate() {
                        let r = d2.rect([9.0, 29.0, 47.0][k], 210.0, 15.0, 14.0);
                        if d2.ui.put(r, |ui: &mut egui::Ui| ui.add_enabled(has, egui::Button::new(glyph).min_size(r.size()))).on_hover_text(tip).clicked() {
                            if dir == 0 { self.core.set_queue(&[qsel.unwrap()], None) } else { self.move_in_queue(qsel.unwrap(), dir) }
                        }
                    }
                }
                }
                let new = d.button(14.3, 266.5, 48.0, 12.5, "New queue", true);
                if d.button(67.7, 266.5, 48.5, 12.5, "Delete", !builtin) {
                    self.core.delete_queue(queue.id);
                    pick = Some(core::MAIN_QUEUE);
                }
                if d.button(144.0, 266.5, 55.0, 12.5, "Start now", !running && !is_limits) {
                    self.core.start_queue(queue.id);
                }
                if d.button(199.0, 266.5, 48.0, 12.5, "Stop", running) {
                    self.core.stop_queue(queue.id);
                }
                if d.button(261.0, 266.5, 48.0, 12.5, "Help", help("index.htm", false)) {
                    help("scheduler.htm", true);
                }
                if is_limits {
                    d.button(324.5, 266.5, 48.0, 12.5, "Apply", false);
                } else if d.button(324.5, 266.5, 48.0, 12.5, "Apply", true) {
                    let t = |s: &str| sched_time(s).map(|t| t.format("%H:%M").to_string());
                    match (t(&queue.sched.start_at), t(&queue.sched.stop_at)) {
                        (Some(a), Some(b)) => {
                            (queue.sched.start_at, queue.sched.stop_at) = (a, b);
                            if let Some(q) = self.core.settings().queues.iter_mut().find(|q| q.id == queue.id) {
                                *q = queue.clone();
                            }
                            self.core.save();
                            error.clear();
                            // 0x4ffe20: once ever, the first time a queue gets a start time
                            if queue.sched.start_on && !self.core.settings().tray_note_shown {
                                self.core.settings().tray_note_shown = true;
                                self.open(Dialog::Info { title: "NDM Queues".into(), text: "You have scheduled the start time of queue processing.\r\nPlease note that in order to start the queue at the specified time, it's necessary to have NDM running at that time at least in the taskbar notification area - tray (see the picture below)".into() });
                            }
                        }
                        _ => *error = "Times must be like 11:00:00 PM or 23:00".into(),
                    }
                }
                if let Some(id) = pick {
                    // like switching queues in NDM's tree: unapplied edits of the old one are dropped
                    *queue = if id == LIMITS {
                        core::Queue { id, name: "Download limits".into(), ..Default::default() }
                    } else {
                        self.core.settings().queue(id).cloned().unwrap_or_default()
                    };
                    (*qsel, *error) = (None, String::new());
                }
                if new {
                    self.new_queue(Vec::new());
                }
                !d.button(378.0, 266.5, 48.5, 12.5, "Close", true)
            }
            Dialog::Duplicate { inc, old, choice, remember } => {
                let mut d = dlu::D::new(ui);
                d.readonly(5.0, 7.0, 274.0, 14.0, &inc.url);
                d.text(7.0, 26.0, 272.0, 18.0, "This file already exists in your download list. Would you like to add the duplicate?");
                for (k, y, h) in [(1u8, 56.0, 10.0), (2, 73.0, 10.0), (0, 86.0, 18.0)] {
                    let label = DUPL[[3, 1, 2][k as usize]];
                    if d.at(7.0, y, 272.0, h, |ui| ui.radio(*choice == k, label)).clicked() {
                        *choice = k;
                    }
                }
                let ok = d.button(84.0, 112.0, 50.0, 14.0, "OK", true);
                let cancel = d.button(152.0, 112.0, 50.0, 14.0, "Cancel", true);
                d.check(11.0, 132.0, 260.0, 18.0, remember, "Remember my selection and don't show this dialog again.\n You may change it in New Download Manager Options->Downloads at a later time.");
                if ok {
                    {
                        let mut s = self.core.settings();
                        s.dupl_action = *choice;
                        s.dupl_ask = !*remember;
                    }
                    let (inc, old, c) = (inc.clone(), *old, *choice);
                    self.duplicate(ctx, inc, old, c);
                }
                !(ok || cancel)
            }
            Dialog::FileProps { id, url, desc, referer, login, pass } => {
                let Some(j) = self.core.find(*id) else { return false };
                let i = j.info().clone();
                let icon = self.icon(ctx, &core::ext(&i.name));
                let mut d = dlu::D::new(ui);
                if let Some(t) = icon {
                    let r = d.rect(7.0, 7.0, 20.0, 20.0);
                    d.ui.put(Rect::from_min_size(r.min, Vec2::splat(32.0)), egui::Image::new((t, Vec2::splat(32.0))));
                }
                d.label(58.0, 13.0, 253.0, 9.0, i.name.as_str());
                d.hline(7.0, 31.0, 304.0);
                d.label(7.0, 36.0, 44.0, 9.0, "Type:");
                d.label(58.0, 36.0, 253.0, 9.0, win::type_name(&core::ext(&i.name)));
                d.label(7.0, 48.0, 43.0, 9.0, "Status:");
                d.label(58.0, 48.0, 253.0, 9.0, "Complete");
                d.label(7.0, 60.0, 43.0, 9.0, "Size:");
                d.label(58.0, 60.0, 253.0, 9.0, i.size.map(|s| format!("{} ({s} Bytes)", human(s))).unwrap_or_default());
                d.label(7.0, 77.0, 45.0, 9.0, "Save To:");
                d.readonly(58.0, 74.0, 197.0, 14.0, &i.path().display().to_string());
                d.button(261.0, 74.0, 50.0, 14.0, "Browse...", false);
                d.label(7.0, 96.0, 44.0, 9.0, "Address:");
                d.edit(58.0, 93.0, 253.0, 14.0, url, true);
                d.label(7.0, 115.0, 47.0, 9.0, "Description:");
                d.edit(58.0, 112.0, 253.0, 14.0, desc, true);
                d.hline(7.0, 131.0, 304.0);
                d.label(7.0, 136.0, 304.0, 9.0, "The web page which this file was obtained from:");
                d.label(7.0, 149.0, 304.0, 9.0, if i.parent.is_empty() { "Unknown" } else { i.parent.as_str() });
                d.hline(7.0, 161.0, 304.0);
                d.label(7.0, 171.0, 47.0, 9.0, "Referer:");
                d.edit(58.0, 167.0, 253.0, 14.0, referer, true);
                d.label_r(23.0, 188.0, 50.0, 9.0, "Login");
                d.edit(80.0, 185.0, 126.0, 13.0, login, true);
                d.label_r(21.0, 205.0, 52.0, 9.0, "Password");
                d.password(80.0, 203.0, 126.0, 13.0, pass, true);
                d.hline(7.0, 220.0, 304.0);
                d.label(7.0, 225.0, 160.0, 9.0, format!("{} {}", tr("Last try date:").trim_end(), date(i.last_try)));
                d.label(7.0, 237.0, 160.0, 8.0, format!("{} {}", tr("Result: ").trim_end(), tr("Complete")));
                if d.button(172.0, 227.0, 50.0, 14.0, "Open", true) {
                    explore(i.path());
                }
                let ok = d.button(261.0, 227.0, 50.0, 14.0, "OK", !url.trim().is_empty());
                if ok {
                    let mut n = j.info();
                    (n.url, n.description, n.referrer) = (join_login(url, login, pass), desc.clone(), referer.clone());
                }
                !ok
            }
            Dialog::Category { orig, cat, folder, error } => {
                let mut d = dlu::D::new(ui);
                d.label(9.0, 3.0, 235.0, 10.0, "Category name");
                d.edit(9.0, 13.0, 238.0, 14.0, &mut cat.name, true);
                d.label(9.0, 33.0, 238.0, 10.0, "Automatically put in this category the following file types:");
                d.edit(9.0, 43.0, 238.0, 14.0, &mut cat.exts, true);
                d.label(9.0, 58.0, 302.0, 10.0, "Note: type file extensions separated by space (e.g. avi mpg mpeg)");
                d.hline(9.0, 72.0, 302.0);
                d.check(9.0, 78.0, 302.0, 10.0, &mut cat.sites_on, "Automatically put in this category the files from the following sites only:");
                d.edit(9.0, 90.0, 302.0, 14.0, &mut cat.sites, cat.sites_on);
                d.label(9.0, 106.0, 302.0, 10.0, "Separate sites by spaces. You may use asterisk as a wildcard pattern");
                d.hline(9.0, 121.0, 302.0);
                d.label(9.0, 127.0, 302.0, 10.0, "Save future downloads of this category to the following folder:");
                d.edit(9.0, 138.0, 302.0, 14.0, folder, true);
                d.check(9.0, 155.0, 228.0, 10.0, &mut cat.remember, "Remember last save path");
                if d.button(261.0, 155.0, 50.0, 14.0, "Browse...", true) {
                    if let Some(p) = rfd::FileDialog::new().set_directory(folder.as_str()).pick_folder() {
                        *folder = p.display().to_string();
                    }
                }
                if !error.is_empty() {
                    d.label(9.0, 166.0, 240.0, 9.0, RichText::new(error.as_str()).color(Color32::RED));
                }
                let ok = d.button(261.0, 13.0, 50.0, 14.0, "OK", true);
                let cancel = d.button(261.0, 34.0, 50.0, 14.0, "Cancel", true);
                if ok {
                    cat.name = cat.name.trim().to_string();
                    let taken = self.core.settings().category_names().iter().any(|n| n.eq_ignore_ascii_case(&cat.name) && Some(n) != orig.as_ref());
                    *error = if cat.name.is_empty() {
                        "Please fill in the \"Category name\" field.".into()
                    } else if taken {
                        format!("The \"{}\" category already exists.", cat.name)
                    } else {
                        String::new()
                    };
                    if error.is_empty() {
                        self.save_category(orig.as_deref(), cat.clone(), folder.trim());
                    }
                }
                !(ok && error.is_empty() || cancel)
            }
            Dialog::CatDelete { name } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 6.0, 246.0, 20.0, format!("Do you really want to delete \"{name}\" category from the categories list?"));
                let yes = d.button(70.0, 38.0, 50.0, 14.0, "Yes", true);
                if yes {
                    let mut s = self.core.settings();
                    s.categories.retain(|c| c.name != *name);
                    s.cat_dirs.remove(name.as_str());
                    drop(s);
                    // its downloads fall back to General (the files stay where they are)
                    for j in self.core.jobs() {
                        let mut i = j.info();
                        if i.category == *name {
                            i.category = GENERAL.into();
                        }
                    }
                    if matches!(&self.cat, Cat::Files(_, Some(c)) if c == name) {
                        self.cat = Cat::Files(Which::All, None);
                    }
                    self.core.save();
                }
                !(yes || d.button(140.0, 38.0, 50.0, 14.0, "No", true))
            }
            Dialog::AdvProxy { adv } => {
                let mut d = dlu::D::new(ui);
                // one group per protocol, 41 DLU apart: label row at +10, edits at +20
                let row = |d: &mut dlu::D, y: f32, on: bool, host: &mut String, port: &mut String, user: &mut String, pass: &mut String| {
                    d.label(13.0, y + 10.0, 76.0, 9.0, "Proxy");
                    d.label(108.0, y + 10.0, 34.0, 9.0, "Port");
                    d.label(146.0, y + 10.0, 52.0, 9.0, "UserName");
                    d.label(202.0, y + 10.0, 52.0, 9.0, "Password");
                    d.edit(13.0, y + 20.0, 91.0, 14.0, host, on);
                    d.edit(108.0, y + 20.0, 34.0, 14.0, port, on);
                    d.edit(146.0, y + 20.0, 52.0, 14.0, user, on);
                    d.password(202.0, y + 20.0, 52.0, 14.0, pass, on);
                };
                // the Proxy / Socks page's HTTP proxy, repeated here as in NDM
                d.group(7.0, 3.0, 254.0, 41.0, "");
                let mut http = adv.http.take().unwrap_or_default();
                d.check(13.0, 0.0, 69.0, 12.0, &mut http.0, "Use HTTP Proxy");
                let on = http.0;
                row(&mut d, 3.0, on, &mut http.1, &mut http.2, &mut http.3, &mut http.4);
                adv.http = Some(http);
                d.group(7.0, 50.0, 254.0, 41.0, "");
                d.check(13.0, 47.0, 74.0, 12.0, &mut adv.https_on, "Use HTTPS Proxy");
                let on = adv.https_on;
                row(&mut d, 50.0, on, &mut adv.https_host, &mut adv.https_port, &mut adv.https_user, &mut adv.https_pass);
                d.group(7.0, 99.0, 254.0, 41.0, "");
                d.check(13.0, 96.0, 63.0, 12.0, &mut adv.ftp_on, "Use FTP Proxy");
                let on = adv.ftp_on;
                row(&mut d, 99.0, on, &mut adv.ftp_host, &mut adv.ftp_port, &mut adv.ftp_user, &mut adv.ftp_pass);
                d.group(7.0, 148.0, 254.0, 87.0, "");
                d.check(13.0, 145.0, 52.0, 12.0, &mut adv.socks_on, "Use Socks");
                let on = adv.socks_on;
                row(&mut d, 148.0, on, &mut adv.socks_host, &mut adv.socks_port, &mut adv.socks_user, &mut adv.socks_pass);
                if d.at(13.0, 186.0, 42.0, 10.0, |ui| ui.add_enabled(on, egui::RadioButton::new(adv.socks5, tr("Socks 5")))).clicked() {
                    adv.socks5 = true;
                }
                if d.at(13.0, 199.0, 42.0, 10.0, |ui| ui.add_enabled(on, egui::RadioButton::new(!adv.socks5, tr("Socks 4")))).clicked() {
                    adv.socks5 = false;
                }
                let mut dns = !adv.local_dns;
                d.at(69.0, 186.0, 50.0, 10.0, |ui| ui.add_enabled(on && adv.socks5, egui::Checkbox::new(&mut dns, tr("Proxy DNS"))));
                adv.local_dns = !dns;
                d.text(14.0, 213.0, 237.0, 19.0, "Note: New Download Manager will use socks only for the protocols that are NOT checked above");
                if d.button(13.0, 240.0, 63.0, 14.0, "Get system", true) {
                    // Internet Options' per-protocol proxies ("https=h:p;ftp=h:p;socks=h:p")
                    let (_, server, _) = win::system_proxy();
                    let get = |k: &str| server.split(';').find_map(|p| p.strip_prefix(k)).map(|hp| hp.rsplit_once(':').map_or((hp.to_string(), String::new()), |(h, p)| (h.to_string(), p.to_string())));
                    if let Some((h, p)) = get("https=") {
                        (adv.https_on, adv.https_host, adv.https_port) = (true, h, p);
                    }
                    if let Some((h, p)) = get("ftp=") {
                        (adv.ftp_on, adv.ftp_host, adv.ftp_port) = (true, h, p);
                    }
                    if let Some((h, p)) = get("socks=") {
                        (adv.socks_on, adv.socks_host, adv.socks_port) = (true, h, p);
                    }
                }
                d.hline(7.0, 259.0, 254.0);
                let ok = d.button(73.0, 264.0, 50.0, 14.0, "OK", true);
                if ok {
                    self.adv_done = Some(adv.clone());
                }
                !(ok || d.button(144.0, 264.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Cancelled { site, url, by_site, by_addr, dont_show } => {
                let mut d = dlu::D::new(ui);
                d.text(6.0, 5.0, 271.0, 33.0, "You have cancelled this download twice.\n\nIf New Download Manager always takes over this download by mistake, you may add this address or this site to New Download Manager list of exceptions:");
                d.check(15.0, 46.0, 261.0, 10.0, by_site, "Don't start downloading automatically from this site:");
                d.label(27.0, 59.0, 248.0, 8.0, site.as_str());
                d.check(15.0, 72.0, 261.0, 17.0, by_addr, "Don't start downloading automatically from this address:");
                d.label(27.0, 89.0, 248.0, 8.0, url.as_str());
                d.text(7.0, 104.0, 269.0, 17.0, "Also you may edit exception lists on \"Options->File Types\" tab of the options dialog");
                let ok = d.button(82.0, 124.0, 50.0, 14.0, "OK", true);
                let cancel = d.button(151.0, 124.0, 50.0, 14.0, "Cancel", true);
                d.check(15.0, 142.0, 227.0, 10.0, dont_show, "Don't show this dialog again");
                if ok {
                    let mut s = self.core.settings();
                    if *by_site && !site.is_empty() && !s.skip_sites.split_whitespace().any(|x| x.eq_ignore_ascii_case(site)) {
                        s.skip_sites = format!("{} {site}", s.skip_sites).trim().to_string();
                    }
                    if *by_addr && !s.skip_urls.contains(url) {
                        s.skip_urls.push(url.clone());
                    }
                    s.ask_exception = !*dont_show;
                }
                !(ok || cancel)
            }
            Dialog::Exceptions { list, sel, ask } => {
                if let Some(a) = self.exc_add.take() {
                    list.push(a);
                    *sel = Some(list.len() - 1);
                }
                let mut d = dlu::D::new(ui);
                d.check(7.0, 6.0, 267.0, 17.0, ask, "Show the dialog to add an address to the list of exceptions for a twice-cancelled download");
                d.label(7.0, 26.0, 267.0, 9.0, "Don't start downloading automatically from the following addresses:");
                let r = d.listview(7.0, 37.0, 267.0, 107.0, &[]);
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (k, a) in list.iter().enumerate() {
                            if ui.add(egui::SelectableLabel::new(*sel == Some(k), a.as_str())).clicked() {
                                *sel = Some(k);
                            }
                        }
                    });
                });
                d.label(7.0, 146.0, 267.0, 9.0, "You may use the asterisk wildcard for the address pattern.");
                if d.button(16.0, 161.0, 50.0, 14.0, "Add", true) {
                    self.open(Dialog::ExcAdd { addr: String::new() });
                }
                if d.button(79.0, 161.0, 50.0, 14.0, "Delete", sel.is_some_and(|k| k < list.len())) {
                    list.remove(sel.take().unwrap());
                }
                let ok = d.button(215.0, 161.0, 50.0, 14.0, "OK", true);
                if ok {
                    let (list, ask) = (list.clone(), *ask);
                    self.opt_patch.push(Box::new(move |s: &mut Settings| (s.skip_urls, s.ask_exception) = (list, ask)));
                }
                !ok
            }
            Dialog::ExcAdd { addr } => {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 10.0, 35.0, 9.0, "Address");
                d.edit(45.0, 7.0, 253.0, 14.0, addr, true);
                let ok = d.button(191.0, 25.0, 50.0, 14.0, "OK", !addr.trim().is_empty());
                if ok {
                    self.exc_add = Some(addr.trim().to_string());
                }
                !(ok || d.button(248.0, 25.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::BrowserDetails { name } => {
                let mut d = dlu::D::new(ui);
                let list = d.listview(7.0, 7.0, 416.0, 59.0, &[("Browser", 110.0), ("Path", 230.0), ("Type of integration", 74.0)]);
                let rows: Vec<core::Browser> = self.dialogs.iter().find_map(|(_, x)| if let Dialog::Options { s, .. } = x { Some(s.browsers.clone()) } else { None })
                    .unwrap_or_else(|| self.core.settings().browsers.clone()).into_iter().filter(|b| b.name == *name).collect();
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink2(Vec2::new(4.0, 0.0)).with_min_y(list.min.y + 20.0)), |ui| {
                    for b in &rows {
                        ui.horizontal(|ui| {
                            ui.add_sized([110.0 * dlu::DX - 8.0, 16.0], egui::Label::new(b.name.as_str()).truncate());
                            ui.add_sized([230.0 * dlu::DX - 8.0, 16.0], egui::Label::new(b.exe.as_str()).truncate());
                            ui.label(integration(b));
                        });
                    }
                });
                if d.button(135.0, 73.0, 67.0, 14.0, "Add version...", true) {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Programs", &["exe"]).pick_file() {
                        self.browser_add.push(core::Browser { name: name.clone(), exe: p.display().to_string(), on: true });
                    }
                }
                !d.button(227.0, 73.0, 67.0, 14.0, "Close", true)
            }
            Dialog::Integrate { b } => {
                let page = extension_page(b).unwrap_or_default();
                let dir = extension_dir().map(|p| p.display().to_string()).unwrap_or_default();
                let mut d = dlu::D::new(ui);
                d.label(8.0, 3.0, 466.0, 12.0, RichText::new(b.name.as_str()).strong());
                d.group(6.0, 18.0, 473.0, 138.0, "");
                d.text(14.0, 28.0, 458.0, 60.0, if page.starts_with("about:") {
                    // ponytail: unsigned add-ons only load temporarily in release Firefox; signing on AMO makes it permanent
                    FIREFOX_GUIDE
                } else {
                    "To capture downloads from this browser, open its extensions page, turn on \"Developer mode\", press \"Load unpacked\" and select this folder:"
                });
                d.readonly(14.0, 92.0, 458.0, 14.0, &dir);
                if d.button(14.0, 162.0, 90.0, 14.0, "Open extensions page", !page.is_empty()) {
                    let mut c = std::process::Command::new(&b.exe);
                    win::clean_env(&mut c);
                    let _ = c.arg(&page).spawn();
                }
                if d.button(110.0, 162.0, 70.0, 14.0, "Open folder", !dir.is_empty()) {
                    explore(&dir);
                }
                !d.button(428.0, 180.0, 50.0, 14.0, "Close", true)
            }
            Dialog::Keys { k } => {
                // Template 229; the extension reads these through /config.
                let mut d = dlu::D::new(ui);
                d.hline(6.0, 6.0, 306.0);
                d.check(7.0, 13.0, 304.0, 10.0, &mut k.prevent, "Use the following key(s) to prevent downloading with New Download Manager for any links:");
                let p = k.prevent;
                d.at(89.0, 26.0, 34.0, 10.0, |ui| ui.add_enabled(p, egui::Checkbox::new(&mut k.alt_p, tr("Alt"))));
                d.at(127.0, 26.0, 35.0, 10.0, |ui| ui.add_enabled(p, egui::Checkbox::new(&mut k.shift_p, tr("Shift"))));
                d.at(165.0, 26.0, 35.0, 10.0, |ui| ui.add_enabled(p, egui::Checkbox::new(&mut k.ctrl_p, tr("Ctrl"))));
                d.at(203.0, 25.0, 41.0, 10.0, |ui| ui.add_enabled(p, egui::Checkbox::new(&mut k.del_p, tr("Del"))));
                d.hline(6.0, 41.0, 306.0);
                d.check(7.0, 50.0, 304.0, 10.0, &mut k.force, "Use the following key(s) to force downloading with New Download Manager for any links:");
                let f = k.force;
                d.at(89.0, 64.0, 34.0, 10.0, |ui| ui.add_enabled(f, egui::Checkbox::new(&mut k.alt_f, tr("Alt"))));
                d.at(127.0, 64.0, 35.0, 10.0, |ui| ui.add_enabled(f, egui::Checkbox::new(&mut k.shift_f, tr("Shift"))));
                d.at(165.0, 64.0, 35.0, 10.0, |ui| ui.add_enabled(f, egui::Checkbox::new(&mut k.ctrl_f, tr("Ctrl"))));
                d.at(203.0, 64.0, 41.0, 10.0, |ui| ui.add_enabled(f, egui::Checkbox::new(&mut k.ins_f, tr("Ins"))));
                d.text(7.0, 76.0, 304.0, 27.0, "Note: Sometimes after you click on a download link, a web page will load telling that the download starts in X seconds. In this case you will need to uncheck the following option to force downloading.");
                d.at(45.0, 104.0, 235.0, 17.0, |ui| ui.add_enabled(f, egui::Checkbox::new(&mut k.check_mouse, tr("Check for left mouse button clicked on a link along with the special key(s) pressed to force downloading the link"))));
                d.text(7.0, 130.0, 304.0, 50.0, "Note 2: Sometimes when you click on a link, your browser requests other files AT THE SAME TIME like Java scripts, web pages, pictures, etc. When you press and hold down a special key, New Download Manager intercepts these files, and the browser may not request the necessary file because it will not run a Javascript, which has been intercepted by New Download Manager erroneously. If you want to intercept and download only the necessary files with New Download Manager, please turn on the option below:");
                d.check(45.0, 183.0, 266.0, 17.0, &mut k.skip_html, "While holding down a special key DO NOT take over the downloads which are web-pages, pictures, scripts and etc.");
                d.hline(6.0, 205.0, 306.0);
                let ok = d.button(89.0, 213.0, 50.0, 14.0, "OK", true);
                if ok {
                    let k = k.clone();
                    self.opt_patch.push(Box::new(move |s: &mut Settings| s.keys = k));
                }
                !(ok || d.button(178.0, 213.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::MenuItems { dl, all } => {
                // Template 334; its two FLV video items are hidden in NDM too (no WS_VISIBLE).
                let mut d = dlu::D::new(ui);
                d.label(7.0, 11.0, 261.0, 9.0, "Selected menu items will appear in browser context menu:");
                d.check(7.0, 27.0, 261.0, 10.0, dl, "Download with New Download Manager");
                d.check(7.0, 42.0, 261.0, 10.0, all, "Download all links with New Download Manager");
                let ok = d.button(152.0, 54.0, 50.0, 14.0, "OK", true);
                if ok {
                    let (dl, all) = (*dl, *all);
                    self.opt_patch.push(Box::new(move |s: &mut Settings| (s.menu_dl, s.menu_all) = (dl, all)));
                }
                !(ok || d.button(210.0, 54.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Panel { tab, types, min, mini, sel, nocap, drm, smode, sites, smini, ssel } => {
                if let Some(s) = self.panel_site.take() {
                    if !sites.contains(&s) {
                        sites.push(s);
                    }
                }
                if let Some(t) = self.panel_add.take() {
                    match types.iter().position(|x| x.0.eq_ignore_ascii_case(&t)) {
                        Some(k) => types[k].1 = true,
                        None => types.push((t, true)),
                    }
                }
                if let Some((t, kb)) = self.panel_min.take() {
                    if kb == 0 { min.remove(&t) } else { min.insert(t, kb) };
                }
                let mut d = dlu::D::new(ui);
                let (px, py, _, _) = d.tabs(7.0, 7.0, 322.0, 262.0, tab, &["For web-players", "For selected files"]);
                let mut d2 = dlu::D::new_offset(d.ui, px - 2.0, py - 2.0);
                if *tab == 0 {
                    d2.text(7.0, 4.0, 308.0, 18.0, "New Download Manager can show its Download panel on a web-player in a browser when New Download Manager detects a multimedia request from the web-player");
                    d2.hline(7.0, 26.0, 308.0);
                    d2.label(7.0, 30.0, 79.0, 9.0, "Panel view:");
                    if d2.radio(22.0, 43.0, 65.0, 10.0, !*mini, "Full mode") {
                        *mini = false;
                    }
                    if d2.radio(22.0, 59.0, 64.0, 10.0, *mini, "Mini mode") {
                        *mini = true;
                    }
                    d2.hline(7.0, 75.0, 308.0);
                    d2.label(7.0, 80.0, 308.0, 9.0, "Show download panel for the following file types:");
                    let list = d2.listview(28.0, 92.0, 148.0, 88.0, &[]);
                    d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink(3.0)), |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (k, (t, on)) in types.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.checkbox(on, tr(""));
                                    let size = min.get(t.as_str()).map(|kb| format!("  ({} {kb} KB)", tr("Min. size"))).unwrap_or_default();
                                    if ui.add(egui::SelectableLabel::new(*sel == Some(k), format!("{t}{size}"))).clicked() {
                                        *sel = Some(k);
                                    }
                                });
                            }
                        });
                    });
                    if d2.button(190.0, 93.0, 99.0, 14.0, "Add", true) {
                        self.open(Dialog::PanelType { name: String::new() });
                    }
                    let picked = sel.and_then(|k| types.get(k)).map(|t| t.0.clone());
                    if d2.button(190.0, 111.0, 99.0, 14.0, "Minimum size", picked.is_some()) {
                        let ext = picked.unwrap();
                        let kb = min.get(&ext).copied().unwrap_or(0).to_string();
                        self.open(Dialog::PanelMin { ext, kb });
                    }
                    if d2.button(190.0, 129.0, 99.0, 14.0, "Check All", true) {
                        types.iter_mut().for_each(|t| t.1 = true);
                    }
                    if d2.button(190.0, 147.0, 99.0, 14.0, "Clear All", true) {
                        types.iter_mut().for_each(|t| t.1 = false);
                    }
                    if d2.button(190.0, 165.0, 99.0, 14.0, "Exceptions", true) {
                        let text = self.dialogs.iter().find_map(|(_, x)| if let Dialog::Options { s, .. } = x { Some(s.panel_skip.clone()) } else { None }).unwrap_or_default();
                        self.open(Dialog::PanelExc { text });
                    }
                    d2.check(50.0, 221.0, 251.0, 9.0, nocap, "Don't capture downloads from web-players automatically");
                    d2.hline(7.0, 234.0, 308.0);
                    d2.check(22.0, 238.0, 279.0, 16.0, drm, "Show download panel for protected content which New Download Manager may not download");
                } else {
                    // Template 348: a panel over selected text that holds links (extension's content.js)
                    d2.text(7.0, 5.0, 309.0, 32.0, "New Download Manager can show Download panel on a web-page when you select a text that contains download links.");
                    d2.hline(7.0, 39.0, 309.0);
                    d2.label(7.0, 44.0, 79.0, 9.0, "Panel view:");
                    if d2.radio(22.0, 58.0, 65.0, 10.0, !*smini, "Full mode") {
                        *smini = false;
                    }
                    if d2.radio(22.0, 74.0, 64.0, 10.0, *smini, "Mini mode") {
                        *smini = true;
                    }
                    d2.hline(7.0, 92.0, 309.0);
                    for (y, h, v, t) in [(101.0, 10.0, 0, "Show Download panel for any selected links"), (115.0, 10.0, 1, "Don't show Download panel for selected links"), (127.0, 14.0, 2, "Show Download panel for selected links for the following sites only:")] {
                        if d2.radio(22.0, y, 288.0, h, *smode == v, t) {
                            *smode = v;
                        }
                    }
                    let on = *smode == 2;
                    let list = d2.listview(38.0, 148.0, 188.0, 74.0, &[]);
                    d2.ui.scope_builder(egui::UiBuilder::new().max_rect(list.shrink(3.0)), |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (k, s) in sites.iter().enumerate() {
                                if ui.add_enabled(on, egui::SelectableLabel::new(*ssel == Some(k), s.as_str())).clicked() {
                                    *ssel = Some(k);
                                }
                            }
                        });
                    });
                    if d2.button(235.0, 152.0, 56.0, 14.0, "Add", on) {
                        self.open(Dialog::PanelSite { name: String::new() });
                    }
                    if d2.button(235.0, 171.0, 56.0, 14.0, "Delete", on && ssel.is_some_and(|k| k < sites.len())) {
                        sites.remove(ssel.take().unwrap());
                    }
                }
                let ok = d.button(218.0, 280.0, 50.0, 14.0, "OK", true);
                if ok {
                    let (t, m, v) = (types.clone(), min.clone(), *mini);
                    let sel_panel = (*nocap, *drm, *smode, sites.clone(), *smini);
                    self.opt_patch.push(Box::new(move |s: &mut Settings| {
                        (s.panel_types, s.panel_min, s.panel_mini) = (t, m, v);
                        (s.panel_nocap, s.panel_drm, s.sel_mode, s.sel_sites, s.sel_mini) = sel_panel;
                    }));
                }
                !(ok || d.button(276.0, 280.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::PanelSite { name } => {
                let mut d = dlu::D::new(ui);
                d.edit(7.0, 7.0, 172.0, 14.0, name, true);
                let site = name.trim().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/').to_ascii_lowercase();
                let ok = d.button(60.0, 31.0, 50.0, 14.0, "OK", !site.is_empty() && !site.contains(' '));
                if ok {
                    self.panel_site = Some(site);
                }
                !(ok || d.button(120.0, 31.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::PanelType { name } => {
                let mut d = dlu::D::new(ui);
                d.edit(7.0, 7.0, 112.0, 14.0, name, true);
                let t = name.trim().trim_start_matches('.').to_ascii_uppercase();
                let ok = d.button(9.0, 28.0, 50.0, 14.0, "OK", !t.is_empty() && !t.contains([' ', '.', '/', '\\']));
                if ok {
                    self.panel_add = Some(t);
                }
                !(ok || d.button(66.0, 28.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::PanelMin { ext, kb } => {
                let mut d = dlu::D::new(ui);
                // NDM's own wording for this setting (21149); the type is shown on its own line
                d.text(7.0, 7.0, 225.0, 32.0, "Do not show \"Download this video\" panel if media file size is below the following minimum size in kilobytes");
                d.label_r(40.0, 46.0, 58.0, 8.0, ext.as_str());
                d.edit(103.0, 44.0, 48.0, 14.0, kb, true);
                d.label(155.0, 46.0, 43.0, 8.0, "KB");
                let v = kb.trim().parse::<u64>();
                let ok = d.button(59.0, 116.0, 50.0, 14.0, "OK", v.is_ok());
                if ok {
                    self.panel_min = Some((ext.clone(), v.unwrap_or(0)));
                }
                !(ok || d.button(130.0, 116.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::PanelExc { text } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 6.0, 240.0, 18.0, "Don't show download panel for the following sites:");
                d.edit(7.0, 27.0, 240.0, 51.0, text, true);
                d.text(7.0, 81.0, 240.0, 18.0, "Separate names by spaces. You may use asterisk as a wildcard pattern");
                let ok = d.button(67.0, 102.0, 50.0, 14.0, "OK", true);
                if ok {
                    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    self.opt_patch.push(Box::new(move |s: &mut Settings| s.panel_skip = t));
                }
                !(ok || d.button(136.0, 102.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Networks { nics, off } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 5.0, 226.0, 26.0, "NDM spreads each download's connections over the ticked networks. Two networks on the same router add no speed.");
                if nics.is_empty() {
                    d.label(15.0, 34.0, 210.0, 10.0, "No network with an internet connection was found");
                }
                for (k, n) in nics.iter().enumerate() {
                    let mut on = !off.contains(&n.name);
                    let ips = n.ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>().join(", ");
                    d.at(15.0, 34.0 + 12.0 * k as f32, 210.0, 10.0, |ui| ui.checkbox(&mut on, format!("{}   {ips}", n.name)));
                    if on {
                        off.retain(|o| *o != n.name);
                    } else if !off.contains(&n.name) {
                        off.push(n.name.clone());
                    }
                }
                let y = 44.0 + 12.0 * nics.len().max(1) as f32;
                let ok = d.button(64.0, y, 50.0, 14.0, "OK", true);
                if ok {
                    let off = off.clone();
                    self.opt_patch.push(Box::new(move |s: &mut Settings| s.nets_off = off));
                }
                !(ok || d.button(126.0, y, 50.0, 14.0, "Cancel", true))
            }
            Dialog::ServerConn { orig, server, n } => {
                let mut d = dlu::D::new(ui);
                d.label(50.0, 3.0, 150.0, 9.0, "Server");
                let mut scheme = "http://".to_string();
                d.combo(7.0, 15.0, 40.0, 13.0, "sc_scheme", &mut scheme, &["http://"], false);
                d.edit(50.0, 15.0, 164.0, 13.0, server, true);
                d.label(50.0, 29.0, 164.0, 9.0, "You may use asterisk as a wildcard pattern");
                d.label_r(33.0, 49.0, 128.0, 9.0, "Max. connections number");
                d.combo(166.0, 47.0, 48.0, 14.0, "sc_n", n, &["1", "2", "4", "8", "16", "24", "32"], true);
                let host = server.trim().trim_start_matches("http://").trim_start_matches("https://").trim_start_matches("ftp://").trim_end_matches('/').to_string();
                let ok = d.button(53.0, 68.0, 50.0, 14.0, "OK", !host.is_empty());
                if ok {
                    let (orig, v) = (*orig, (host, n.parse().unwrap_or(8)));
                    self.opt_patch.push(Box::new(move |s: &mut Settings| match orig {
                        Some(k) if k < s.server_conns.len() => s.server_conns[k] = v,
                        _ => s.server_conns.push(v),
                    }));
                }
                !(ok || d.button(119.0, 68.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Find { text, flags } => {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 7.0, 216.0, 8.0, "Find:");
                d.edit(7.0, 19.0, 216.0, 14.0, text, true);
                for (y, h, bit, t) in [(36.0, 10.0, 4, "File name or part of the name"), (50.0, 10.0, 8, "Description or part of the description"), (64.0, 19.0, 0x10, "Site name/Download link/Parent web page/Referer"), (97.0, 10.0, 1, "Match case"), (110.0, 10.0, 2, "Match whole string only")] {
                    let mut on = *flags & bit != 0;
                    d.check(if bit < 4 { 15.0 } else { 7.0 }, y, 216.0, h, &mut on, t);
                    *flags = if on { *flags | bit } else { *flags & !bit };
                }
                d.hline(7.0, 88.0, 216.0);
                let ok = d.button(48.0, 127.0, 50.0, 14.0, "Find", !text.is_empty());
                if ok && *flags & 0x1c == 0 {
                    // 0x4b0dc0: no search target ticked -> warn and tick "File name"
                    *flags |= 4;
                    self.open(Dialog::Info { title: "NDM Search settings".into(), text: "The checkbox for at least one of the search targets must be enabled".into() });
                    return true;
                }
                if ok {
                    let mut s = self.core.settings();
                    (s.search, s.search_flags) = (text.clone(), *flags);
                    drop(s);
                    self.core.save();
                    self.find_pos = 0;
                    self.find_next();
                }
                !(ok || d.button(131.0, 127.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Info { text, .. } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 7.0, 246.0, 38.0, text.as_str());
                !d.button(105.0, 50.0, 50.0, 14.0, "OK", true)
            }
            Dialog::Update { slot, quiet } => {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 4.0, 172.0, 9.0, "Checking for new version");
                let t = d.ui.input(|i| i.time) as f32;
                d.progress(7.0, 16.0, 172.0, 11.0, (t * 0.5).fract()); // marquee-like while waiting
                let res = slot.lock().unwrap().take();
                if res.is_some() {
                    self.core.settings().update_checked = chrono::Local::now().timestamp();
                }
                match res {
                    Some(Ok(Some(rel))) => self.open(Dialog::NewVersion { rel, get: None }),
                    Some(Ok(None)) if !*quiet => self.open(Dialog::Info { title: "Quick Update".into(), text: "You are using the latest version of New Download Manager. Please check back again for updates at a later time.".into() }),
                    Some(Err(e)) if !*quiet => self.open(Dialog::Info { title: "Quick Update".into(), text: e }),
                    Some(_) => {}
                    None => return true,
                }
                false
            }
            Dialog::NewVersion { rel, get } => {
                let v = ui.visuals().clone();
                let accent = if v.dark_mode { Color32::from_rgb(110, 170, 255) } else { BAR };
                let (paper, line) = if v.dark_mode { (Color32::from_gray(26), Color32::from_gray(55)) } else { (Color32::WHITE, Color32::from_rgb(226, 230, 236)) };
                let logo = help_logo(ctx);
                let mut keep = true;
                egui::Frame::new().inner_margin(egui::Margin::same(18)).show(ui, |ui| {
                    // header: logo, the new version and the one installed
                    ui.horizontal(|ui| {
                        if let Some(t) = &logo {
                            ui.add(egui::Image::new((t.id(), Vec2::splat(48.0))));
                        }
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(tr("A new version is available")).size(19.0).strong().color(v.strong_text_color()));
                            ui.label(RichText::new(format!("{} {} \u{2192} {}", tr("Version"), env!("CARGO_PKG_VERSION"), rel.version)).size(13.0).color(accent));
                        });
                    });
                    ui.add_space(12.0);
                    ui.label(RichText::new(tr("What's new")).size(13.0).strong());
                    ui.add_space(4.0);
                    // the release notes (GitHub markdown: headings, bullets, **bold**) in a card
                    egui::Frame::new().fill(paper).stroke(egui::Stroke::new(1.0_f32, line)).corner_radius(8).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical().max_height(ui.available_height() - 80.0).auto_shrink(false).show(ui, |ui| {
                            let notes = if rel.notes.trim().is_empty() { tr("No release notes.") } else { rel.notes.clone() };
                            let plain = |t: &str| t.replace(['*', '`'], "");
                            for l in notes.lines().map(str::trim_end) {
                                if let Some(h) = l.strip_prefix('#') {
                                    ui.add_space(4.0);
                                    ui.label(RichText::new(plain(h.trim_start_matches('#').trim())).size(14.0).strong().color(v.strong_text_color()));
                                } else if let Some(b) = l.trim_start().strip_prefix("* ").or_else(|| l.trim_start().strip_prefix("- ")) {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new("\u{2022}").color(accent));
                                        ui.label(plain(b));
                                    });
                                } else if !l.is_empty() {
                                    ui.label(plain(l));
                                }
                            }
                        });
                    });
                    ui.add_space(14.0);
                    if let Some((got, total, done)) = get {
                        // downloading: a progress bar and the amount in place of the buttons
                        let (g, t) = (got.load(Relaxed), total.load(Relaxed));
                        let amount = if t > 0 { format!("{} / {}", human(g), human(t)) } else { human(g) };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr("Downloading the update")).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(amount).color(v.weak_text_color())));
                        });
                        ui.add(egui::ProgressBar::new(if t > 0 { g as f32 / t as f32 } else { 0.0 }).fill(accent).desired_height(8.0).corner_radius(4).animate(t == 0));
                        ui.add_space(4.0);
                        ui.label(RichText::new(tr("New Download Manager restarts by itself when the update is installed.")).size(11.5).color(v.weak_text_color()));
                        let res = done.lock().unwrap().take();
                        match res {
                            Some(Ok(())) => {
                                // the setup (or the new copy) waits for this process to end: close the main
                                // window, not this dialog's, and exit anyway should that not happen
                                self.quitting = true;
                                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
                                let core = self.core.clone();
                                std::thread::spawn(move || {
                                    std::thread::sleep(Duration::from_secs(10));
                                    core.shutdown();
                                    std::process::exit(0);
                                });
                                keep = false;
                            }
                            Some(Err(e)) => {
                                self.open(Dialog::Info { title: "Quick Update".into(), text: format!("{1}{0}{e}", if e.is_empty() { "" } else { "\r\n\r\n" }, tr("NDM cannot download the update file.\r\nYou can download a new version of NDM from NDM home site and install it over your current version manually")) });
                                keep = false;
                            }
                            None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
                        }
                        return;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let big = Vec2::new(110.0, 30.0);
                        let now = ui.add_enabled(!rel.exe.is_empty(), egui::Button::new(RichText::new(tr("Update now")).strong().color(Color32::WHITE)).fill(accent).corner_radius(6).min_size(big));
                        if now.clicked() {
                            let (got, total, done) = (Arc::new(std::sync::atomic::AtomicU64::new(0)), Arc::new(std::sync::atomic::AtomicU64::new(0)), Arc::new(Mutex::new(None)));
                            let (agent, url, g, t, dn, c) = (self.core.agent(&rel.exe), rel.exe.clone(), got.clone(), total.clone(), done.clone(), ctx.clone());
                            std::thread::spawn(move || {
                                *dn.lock().unwrap() = Some(install_update(&agent, &url, &g, &t));
                                c.request_repaint();
                            });
                            *get = Some((got, total, done));
                        }
                        if ui.add(egui::Button::new(tr("Later")).corner_radius(6).min_size(big)).clicked() {
                            keep = false;
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // the release's page, to download by hand (the only way when no program for this system is attached)
                            if ui.link(tr("Download page")).clicked() {
                                open_url(&rel.page);
                            }
                        });
                    });
                });
                keep
            }
            Dialog::Virus { exe, args } => {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 10.0, 248.0, 10.0, "Virus scanner program");
                d.edit(7.0, 20.0, 299.0, 14.0, exe, true);
                if d.button(322.0, 20.0, 50.0, 14.0, "Browse", true) {
                    if let Some(p) = rfd::FileDialog::new().add_filter("exe", &["exe"]).pick_file() {
                        *exe = p.display().to_string();
                    }
                }
                d.label(7.0, 38.0, 248.0, 10.0, "Command line parameters");
                d.edit(7.0, 48.0, 299.0, 14.0, args, true);
                d.text(7.0, 68.0, 365.0, 142.0, format!("{}\n\n{}", tr("If you would like to run antiviral software on download completion, you should type or browse the complete path to its executable file. If NDM knows about necessary parameters for a given executable file name, it will set them automatically. Otherwise, you will have to set them manually in \"Command line parameters\" according to antiviral software documentation. The scanned file among different parameters should be written as\r\n[File]\r\n(you should type square brackets as well). If no parameters are set, NDM will execute antiviral software appending the complete path of a downloaded file."), tr("Note that all modern antiviral software have \"real-time protection\" feature that checks all operations with files on your computer, and it includes all files downloaded in NDM. Thus, you need to add antiviral software scanner to NDM settings only when real-time protection is turned off in antiviral software settings.\r\nAlso, in some cases, real time protection may significantly slow down the assembly of files made by NDM. Then you need to add ndm.exe process to exceptions according to antiviral software documentation, and set up antiviral checking on this dialog.")));
                let ok = d.button(110.0, 214.0, 50.0, 14.0, "OK", true);
                if ok {
                    let v = (exe.trim().to_string(), args.trim().to_string());
                    self.opt_patch.push(Box::new(move |s: &mut Settings| (s.virus_exe, s.virus_args) = v));
                }
                !(ok || d.button(219.0, 214.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Export { ef2, mode } => {
                let mut d = dlu::D::new(ui);
                for (k, y, t) in [(0, 6.0, "Export download queue"), (1, 18.0, "Export selected files"), (2, 30.0, "Export all files")] {
                    if d.radio(11.0, y, 141.0, 10.0, *mode == k, t) {
                        *mode = k;
                    }
                }
                let ok = d.button(157.0, 6.0, 50.0, 14.0, "OK", true);
                if ok {
                    self.export(*ef2, *mode);
                }
                !(ok || d.button(157.0, 24.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Help { page, anchor, back, fwd, search } => {
                let Some(dir) = help_dir() else { return false };
                let read = |f: &str| help_parse(&std::fs::read_to_string(dir.join(f)).unwrap_or_default());
                let mut go = None::<String>;
                // the topics: Home, then index.htm's list (link, name, summary)
                let (_, index) = read("index.htm");
                let topics: Vec<(String, String, String)> = std::iter::once(("index.htm".to_string(), "Home".to_string(), String::new()))
                    .chain(index.iter().filter(|p| p.kind == HelpKind::Item).filter_map(|p| {
                        let first = p.runs.first()?;
                        let link = first.link.clone().filter(|l| !l.contains("://"))?;
                        Some((link, first.text.trim().to_string(), p.runs[1..].iter().map(|r| r.text.as_str()).collect::<String>().trim().to_string()))
                    }))
                    .collect();
                let file = page.split('#').next().unwrap_or("index.htm").to_string();
                let (_, paras) = read(&file);
                let icons = help_icons(ctx);
                let v = ui.visuals().clone();
                // blue is for links only; everything else is the text colours and greys
                let link = if v.dark_mode { Color32::from_rgb(120, 175, 255) } else { Color32::from_rgb(0, 95, 184) };
                let (text, strong, weak) = (v.text_color(), v.strong_text_color(), v.weak_text_color());
                let (side, paper, line, row, chip) = if v.dark_mode {
                    (Color32::from_gray(30), Color32::from_gray(22), Color32::from_gray(55), Color32::from_gray(48), Color32::from_gray(55))
                } else {
                    (Color32::from_gray(246), Color32::WHITE, Color32::from_gray(222), Color32::from_gray(230), Color32::from_gray(236))
                };
                let hand = egui::CursorIcon::PointingHand;
                let searching = !search.trim().is_empty();

                // sidebar: search, then the contents, the current topic's sections under it
                egui::SidePanel::left("help_nav").resizable(false).exact_width(228.0).show_separator_line(true)
                    .frame(egui::Frame::new().fill(side).inner_margin(egui::Margin::symmetric(12, 14)))
                    .show_inside(ui, |ui| {
                        egui::Frame::new().fill(paper).stroke(egui::Stroke::new(1.0_f32, line)).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if let Some(t) = &icons {
                                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                    help_icon(ui, t, HELP_SEARCH, r, weak);
                                }
                                ui.add(egui::TextEdit::singleline(search).hint_text(tr("Search help")).frame(false).desired_width(f32::INFINITY));
                            });
                        });
                        ui.add_space(14.0);
                        ui.label(RichText::new(tr("Contents")).size(13.0).strong().color(strong));
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                            for (f, name, _) in &topics {
                                let on = !searching && *f == file;
                                let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
                                if on {
                                    ui.painter().rect_filled(rect, 3.0, row);
                                } else if r.hovered() {
                                    ui.painter().rect_filled(rect, 3.0, row.gamma_multiply(0.5));
                                }
                                ui.painter().text(rect.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, tr(name), FontId::proportional(13.5), if on { strong } else { text });
                                if on {
                                    // no bold face is loaded: a second pass a hair to the right makes it bold
                                    ui.painter().text(rect.left_center() + Vec2::new(8.5, 0.0), Align2::LEFT_CENTER, tr(name), FontId::proportional(13.5), strong);
                                }
                                if r.on_hover_cursor(hand).clicked() {
                                    go = Some(f.clone());
                                }
                                if on {
                                    for h in paras.iter().filter(|p| p.kind == HelpKind::Heading && p.id.is_some()) {
                                        ui.horizontal(|ui| {
                                            ui.add_space(20.0);
                                            if ui.add(egui::Link::new(RichText::new(tr(&h.text())).size(12.5).color(link))).on_hover_cursor(hand).clicked() {
                                                *anchor = h.id.clone();
                                            }
                                        });
                                    }
                                    ui.add_space(4.0);
                                }
                            }
                            ui.add_space(12.0);
                            if ui.link(RichText::new(format!("{} \u{2197}", tr("NDM on GitHub"))).size(12.5).color(link)).clicked() {
                                go = Some(format!("https://github.com/{UPDATE_REPO}"));
                            }
                        });
                    });

                // top bar: back, forward, where you are
                let pos = topics.iter().position(|t| t.0 == file);
                egui::TopBottomPanel::top("help_bar").show_separator_line(true)
                    .frame(egui::Frame::new().fill(paper).inner_margin(egui::Margin::symmetric(14, 8)))
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (i, is_back) in [(HELP_BACK, true), (HELP_FWD, false)] {
                                let on = if is_back { !back.is_empty() } else { !fwd.is_empty() };
                                let (rect, r) = ui.allocate_exact_size(Vec2::splat(28.0), if on { Sense::click() } else { Sense::hover() });
                                if on && r.hovered() {
                                    ui.painter().rect_filled(rect, 3.0, row);
                                }
                                if let Some(t) = &icons {
                                    help_icon(ui, t, i, Rect::from_center_size(rect.center(), Vec2::splat(14.0)), if on { strong } else { weak.gamma_multiply(0.5) });
                                }
                                if on && r.on_hover_text(tr(if is_back { "Back" } else { "Forward" })).clicked() {
                                    let (from, to) = if is_back { (&mut *back, &mut *fwd) } else { (&mut *fwd, &mut *back) };
                                    if let Some(p) = from.pop() {
                                        to.push(page.clone());
                                        (*page, *anchor) = (p, None);
                                        search.clear();
                                    }
                                }
                            }
                            ui.add_space(8.0);
                            if ui.link(RichText::new(tr("Help")).size(13.0).color(link)).clicked() {
                                go = Some("index.htm".into());
                            }
                            if file != "index.htm" || searching {
                                let here = if searching { tr("Search") } else { pos.map_or(String::new(), |i| tr(&topics[i].1)) };
                                ui.label(RichText::new("\u{203A}").size(13.0).color(weak));
                                ui.label(RichText::new(here).size(13.0).color(text));
                            }
                        });
                    });

                // the page, in a reading column of about 65 characters
                egui::CentralPanel::default().frame(egui::Frame::new().fill(paper)).show_inside(ui, |ui| {
                    let query = search.trim().to_lowercase();
                    egui::ScrollArea::vertical().auto_shrink(false).id_salt(("help", file.clone(), query.is_empty())).show(ui, |ui| {
                        let width = (ui.available_width() - 64.0).clamp(300.0, 600.0);
                        let pad = ((ui.available_width() - width) / 2.0).max(24.0);
                        ui.horizontal(|ui| {
                            ui.add_space(pad);
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                ui.add_space(22.0);
                                if !query.is_empty() {
                                    help_search(ui, &topics, &read, &query, search.trim(), (text, strong, weak, link, row), &mut go);
                                    return;
                                }
                                for p in &paras {
                                    let resp = match p.kind {
                                        HelpKind::Title => {
                                            let r = ui.label(RichText::new(tr(&p.text())).size(26.0).strong().color(strong));
                                            ui.add_space(4.0);
                                            r
                                        }
                                        HelpKind::Lead => {
                                            ui.add_space(4.0);
                                            help_runs(ui, &p.runs, 15.0, text, strong, (link, chip), &mut go)
                                        }
                                        HelpKind::Heading => {
                                            ui.add_space(22.0);
                                            let r = ui.label(RichText::new(tr(&p.text())).size(18.0).strong().color(strong));
                                            ui.add_space(2.0);
                                            r
                                        }
                                        HelpKind::Text => {
                                            ui.add_space(7.0);
                                            help_runs(ui, &p.runs, 14.0, text, strong, (link, chip), &mut go)
                                        }
                                        HelpKind::Tip | HelpKind::Note => {
                                            // an ordinary paragraph that starts with a bold "Tip:" / "Note:"
                                            ui.add_space(7.0);
                                            let label = tr(if p.kind == HelpKind::Tip { "Tip:" } else { "Note:" });
                                            let runs: Vec<HelpRun> = std::iter::once(HelpRun { text: format!("{label} "), bold: true, ..Default::default() }).chain(p.runs.iter().cloned()).collect();
                                            help_runs(ui, &runs, 14.0, text, strong, (link, chip), &mut go)
                                        }
                                        HelpKind::Item => {
                                            ui.add_space(3.0);
                                            ui.horizontal(|ui| {
                                                let mark = p.num.map_or("\u{2022}".to_string(), |n| format!("{n}."));
                                                let (r, _) = ui.allocate_exact_size(Vec2::new(22.0, 19.0), Sense::hover());
                                                ui.painter().text(egui::pos2(r.right() - 7.0, r.top() + 1.0), Align2::RIGHT_TOP, mark, FontId::proportional(14.0), text);
                                                ui.vertical(|ui| help_runs(ui, &p.runs, 14.0, text, strong, (link, chip), &mut go)).inner
                                            })
                                            .inner
                                        }
                                        HelpKind::Code => {
                                            ui.add_space(8.0);
                                            let r = egui::Frame::new().fill(chip).corner_radius(3).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                                                ui.set_width(ui.available_width());
                                                ui.label(RichText::new(p.text()).monospace().size(13.0).color(strong));
                                            });
                                            r.response
                                        }
                                        HelpKind::Image => {
                                            // scaled to the column, never above its own size; a thin border and the caption below
                                            ui.add_space(12.0);
                                            let tex = p.src.as_deref().and_then(|src| help_image(ctx, &dir, src));
                                            let r = match &tex {
                                                Some(t) => {
                                                    let natural = t.size_vec2() / ctx.pixels_per_point();
                                                    let size = natural * (ui.available_width() / natural.x).min(1.0);
                                                    let r = ui.add(egui::Image::new((t.id(), size)));
                                                    ui.painter().rect_stroke(r.rect, 0.0, egui::Stroke::new(1.0_f32, line), egui::StrokeKind::Outside);
                                                    r
                                                }
                                                None => ui.label(RichText::new(format!("[{}]", p.text())).size(12.5).color(weak)),
                                            };
                                            ui.add_space(4.0);
                                            ui.label(RichText::new(tr(&p.text())).size(12.5).color(weak));
                                            ui.add_space(14.0);
                                            r
                                        }
                                    };
                                    if p.id.is_some() && p.id == *anchor {
                                        resp.scroll_to_me(Some(Align::TOP));
                                        *anchor = None;
                                    }
                                }
                                // previous / next topic: one line under a rule
                                if let Some(i) = pos {
                                    ui.add_space(28.0);
                                    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0_f32, line));
                                    ui.add_space(12.0);
                                    ui.horizontal(|ui| {
                                        if let Some(j) = i.checked_sub(1) {
                                            if ui.add(egui::Link::new(RichText::new(format!("\u{2039} {}", tr(&topics[j].1))).size(13.5).color(link))).on_hover_cursor(hand).clicked() {
                                                go = Some(topics[j].0.clone());
                                            }
                                        }
                                        if let Some(j) = Some(i + 1).filter(|j| *j < topics.len()) {
                                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                if ui.add(egui::Link::new(RichText::new(format!("{} \u{203A}", tr(&topics[j].1))).size(13.5).color(link))).on_hover_cursor(hand).clicked() {
                                                    go = Some(topics[j].0.clone());
                                                }
                                            });
                                        }
                                    });
                                }
                                ui.add_space(28.0);
                            });
                        });
                    });
                });
                if let Some(l) = go {
                    if l.contains("://") {
                        open_url(&l);
                    } else {
                        let (f, a) = l.split_once('#').map_or((l.clone(), None), |(f, a)| (f.to_string(), Some(a.to_string())));
                        if f != file || !search.is_empty() {
                            back.push(page.clone());
                            fwd.clear();
                        }
                        (*page, *anchor) = (f, a);
                        search.clear();
                    }
                }
                true
            }
            Dialog::DialUp => {
                let Some(text) = self.core.dial_text.lock().unwrap().clone() else { return false };
                let mut d = dlu::D::new(ui);
                d.text(7.0, 7.0, 172.0, 22.0, text.as_str());
                // Redial shows while waiting between attempts
                if text.contains('\n') && d.button(6.0, 34.0, 50.0, 14.0, "Redial", true) {
                    self.core.dial_now.store(true, Relaxed);
                }
                if d.button(63.0, 34.0, 50.0, 14.0, "Cancel", true) {
                    self.core.dial_cancel.store(true, Relaxed);
                }
                ui.ctx().request_repaint_after(Duration::from_millis(250));
                true
            }
            Dialog::WaitLink { name } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 5.0, 226.0, 17.0, format!("{}\n{name}", tr("NDM is waiting for the new address to resume the file:")));
                if d.button(66.0, 25.0, 107.0, 14.0, "Stop waiting", true) {
                    self.refreshing = None;
                }
                self.refreshing.is_some()
            }
            Dialog::QuotaHit { text } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 7.0, 325.0, 60.0, text.as_str());
                !d.button(144.0, 72.0, 50.0, 14.0, "OK", true)
            }
            Dialog::QueueName { name, files } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 6.0, 166.0, 18.0, "Enter a name for the new queue that will be displayed in the list of queues");
                d.edit(7.0, 28.0, 172.0, 14.0, name, true);
                let ok = d.button(36.0, 53.0, 50.0, 14.0, "OK", !name.trim().is_empty());
                if ok {
                    let q = self.core.add_queue(name);
                    self.core.set_queue(files, Some(q));
                    self.core.save();
                }
                !(ok || d.button(98.0, 53.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::GrabSave { name, orig, quiet, proj, sched } => {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 6.0, 204.0, 9.0, "Save this project with the following name");
                d.edit(7.0, 19.0, 204.0, 14.0, name, true);
                if orig.is_none() {
                    d.check(18.0, 35.0, 185.0, 10.0, quiet, "Don't show this dialog if the name is unique");
                }
                let ok = d.button(53.0, 54.0, 50.0, 14.0, "OK", !name.trim().is_empty());
                if ok {
                    if orig.is_none() {
                        self.core.settings().grab_quiet = *quiet;
                    }
                    let p = proj.take().map(|p| *p);
                    let n = name.trim().to_string();
                    let taken = orig.is_none() && p.as_ref().is_some_and(|p| p.opts.name.trim() != n) && self.core.settings().grab_projects.iter().any(|x| x.opts.name == n);
                    if taken {
                        // 20615: another project already has this name
                        *proj = p.map(Box::new);
                        self.open(Dialog::Info { title: "Saving the project of NDM Grabber".into(), text: i18n::trf("The project with \"%s\" name already exists. Please select another name.", &[&n]) });
                        return true;
                    }
                    if p.is_some() {
                        self.grab_saved_as = Some(n.clone());
                    }
                    self.save_project(&n, orig.as_deref(), p);
                    if *sched {
                        self.project_cmd(1, &n);
                    }
                }
                !(ok || d.button(113.0, 54.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::GrabSchedAsk { proj } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 7.0, 246.0, 20.0, "To use project scheduler you should save the project at first. Save the project now?");
                let yes = d.button(140.0, 40.0, 50.0, 14.0, "Yes", true);
                if yes {
                    let p = (**proj).clone();
                    let name = p.opts.name.trim().to_string();
                    let quiet = self.core.settings().grab_quiet;
                    self.open(Dialog::GrabSave { name, orig: None, quiet, proj: Some(Box::new(p)), sched: true });
                }
                !(yes || d.button(203.0, 40.0, 50.0, 14.0, "No", true))
            }
            Dialog::GrabSched { p } => {
                let mut d = dlu::D::new(ui);
                if let Some(t) = self.res.icon(285) {
                    let r = d.rect(7.0, 7.0, 20.0, 20.0);
                    d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
                }
                d.label(41.0, 6.0, 124.0, 9.0, "Project:");
                d.label(42.0, 18.0, 127.0, 9.0, p.opts.name.clone());
                d.group(7.0, 32.0, 190.0, 43.0, "Step 1. Select type");
                for (y, v, t) in [(45.0, false, "One-time exploring/downloading"), (58.0, true, "Periodic synchronization")] {
                    if d.radio(11.0, y, 182.0, 10.0, p.periodic == v, t) {
                        p.periodic = v;
                    }
                }
                d.group(7.0, 82.0, 191.0, 58.0, "Step 2. Select action");
                for (y, v, t) in [(97.0, 0, "Explore only"), (111.0, 1, "Explore site and download matched files"), (123.0, 2, "Download checked files")] {
                    if d.radio(11.0, y, 179.0, 10.0, p.action == v, t) {
                        p.action = v;
                    }
                }
                d.group(207.0, 7.0, 223.0, 133.0, "Step 3. Schedule");
                let sc = &mut p.sched;
                d.check(217.0, 19.0, 95.0, 10.0, &mut sc.start_on, "Start download at");
                d.edit(318.0, 17.0, 59.0, 15.0, &mut sc.start_at, sc.start_on);
                if p.periodic {
                    d.check(217.0, 36.0, 90.0, 10.0, &mut sc.again_on, "Start again every");
                    let (mut h, mut m) = (sc.again_h.to_string(), sc.again_m.to_string());
                    if d.edit(310.0, 35.0, 19.0, 14.0, &mut h, sc.again_on).changed() {
                        sc.again_h = h.trim().parse().unwrap_or(0);
                    }
                    d.label(332.0, 38.0, 25.0, 8.0, "hours");
                    if d.edit(362.0, 35.0, 19.0, 14.0, &mut m, sc.again_on).changed() {
                        sc.again_m = m.trim().parse().unwrap_or(0);
                    }
                    d.label(383.0, 38.0, 23.0, 8.0, "min");
                } else {
                    if d.radio(225.0, 36.0, 54.0, 10.0, !sc.daily, "Once at") {
                        sc.daily = false;
                    }
                    if d.radio(225.0, 52.0, 45.0, 10.0, sc.daily, "Daily") {
                        sc.daily = true;
                    }
                    let days = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
                    for (k, day) in days.iter().enumerate() {
                        let (x, y) = if k < 4 { (275.0, [52.0, 63.0, 73.0, 83.0][k]) } else { (353.0, [52.0, 63.0, 73.0][k - 4]) };
                        let daily = sc.daily;
                        d.at(x, y, 72.0, 10.0, |ui| ui.add_enabled(daily, egui::Checkbox::new(&mut sc.days[k], tr(day))));
                    }
                }
                d.hline(217.0, 94.0, 197.0);
                d.check(217.0, 100.0, 97.0, 10.0, &mut sc.stop_on, "Stop download at");
                d.edit(318.0, 97.0, 59.0, 14.0, &mut sc.stop_at, sc.stop_on);
                d.hline(218.0, 114.0, 197.0);
                d.text(212.0, 118.0, 214.0, 18.0, "Note: NDM should be running in system tray at the specified time to start a scheduled project");
                d.check(9.0, 145.0, 185.0, 10.0, &mut sc.hangup, "Hang up modem when done");
                d.check(9.0, 158.0, 191.0, 10.0, &mut sc.exit_done, "Exit New Download Manager when done");
                d.check(9.0, 170.0, 188.0, 10.0, &mut sc.off_done, "Turn off computer when done");
                let kinds = ["Shut down", "Sleep", "Hibernate"].map(tr);
                let mut kind = kinds[sc.off_kind as usize % 3].clone();
                if d.combo(23.0, 182.0, 104.0, 14.0, "goff", &mut kind, &kinds.each_ref().map(String::as_str), sc.off_done) {
                    sc.off_kind = kinds.iter().position(|k| *k == kind).unwrap_or(0) as u8;
                }
                let off = sc.off_done;
                d.at(23.0, 200.0, 158.0, 10.0, |ui| ui.add_enabled(off, egui::Checkbox::new(&mut sc.off_force, tr("Force processes to terminate"))));
                d.check(212.0, 146.0, 213.0, 10.0, &mut sc.open_on, "Open the following file when done:");
                d.edit(211.0, 158.0, 198.0, 14.0, &mut sc.open_file, sc.open_on);
                if d.button(412.0, 158.0, 15.0, 14.0, "...", sc.open_on) {
                    if let Some(f) = rfd::FileDialog::new().pick_file() {
                        sc.open_file = f.display().to_string();
                    }
                }
                let t = |s: &str| sched_time(s).map(|t| t.format("%H:%M").to_string());
                let times = (t(&sc.start_at), t(&sc.stop_at));
                let ok = d.button(296.0, 198.0, 50.0, 14.0, "OK", times.0.is_some() && times.1.is_some());
                if ok {
                    (sc.start_at, sc.stop_at) = (times.0.unwrap(), times.1.unwrap());
                    let mut p = (**p).clone();
                    p.opts.overwrite |= p.periodic; // grabber.chm "Scheduler": synchronization replaces changed files
                    let periodic_now = p.periodic && p.sched.again_on && !p.sched.start_on;
                    if let Some(x) = self.core.settings().grab_projects.iter_mut().find(|x| x.opts.name == p.opts.name) {
                        *x = p.clone();
                    }
                    self.core.save();
                    if periodic_now {
                        self.core.run_project(p); // no start time: the first pass now, then every N hours
                    }
                }
                !(ok || d.button(357.0, 198.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::GrabLogin { realm, site, user, pass, save } => {
                let mut d = dlu::D::new(ui);
                if let Some(t) = self.res.icon(272) {
                    let r = d.rect(10.0, 7.0, 20.0, 20.0);
                    d.ui.put(r, egui::Image::new((t, Vec2::splat(32.0))));
                }
                d.label(40.0, 9.0, 223.0, 10.0, "Type your user name and password.");
                d.label(40.0, 28.0, 225.0, 10.0, format!("{} {realm}", tr("Realm:")));
                d.label(40.0, 48.0, 73.0, 10.0, "User name:");
                d.edit(117.0, 46.0, 146.0, 14.0, user, true);
                d.label(40.0, 69.0, 72.0, 10.0, "Password:");
                d.password(117.0, 66.0, 146.0, 14.0, pass, true);
                d.check(40.0, 93.0, 216.0, 10.0, save, "Save this password in NDM password list");
                let ok = d.button(155.0, 117.0, 50.0, 14.0, "OK", !user.is_empty());
                if ok {
                    if *save {
                        self.core.settings().logins.push(core::Login { site: site.clone(), user: user.clone(), pass: pass.clone(), ..Default::default() });
                        self.core.save();
                    }
                    self.grab_auth = Some((user.clone(), pass.clone()));
                }
                !(ok || d.button(212.0, 117.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::QueuePick { ids, q, start, never, batch } => {
                // template 345; "+" = new queue (310) for these files
                let mut d = dlu::D::new(ui);
                d.label(7.0, 7.0, 143.0, 9.0, "Add files to the queue:");
                let names: Vec<String> = self.core.settings().queues.iter().map(|x| x.name.clone()).collect();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                d.combo(7.0, 18.0, 126.0, 14.0, "qpick", q, &refs, true);
                let plus = d.button(138.0, 17.0, 16.0, 14.0, "+", true);
                if plus {
                    self.open(Dialog::QueueName { name: String::new(), files: ids.clone() });
                }
                d.check(7.0, 34.0, 147.0, 10.0, start, "Start queue processing");
                d.check(7.0, 74.0, 147.0, 10.0, never, "Don't ask me again");
                let ok = d.button(55.0, 53.0, 50.0, 14.0, "OK", true);
                if ok {
                    let qid = self.core.settings().queues.iter().find(|x| x.name == *q).map_or(core::MAIN_QUEUE, |x| x.id);
                    self.core.set_queue(ids, Some(qid));
                    if *never {
                        let mut s = self.core.settings();
                        if *batch { s.ask_queue_batch = false } else { s.ask_queue_later = false }
                    }
                    if *start {
                        self.core.start_queue(qid);
                    }
                    self.core.save();
                }
                !(ok || plus)
            }
            Dialog::Batch { url, from, to, width, letters, lf, lt, auth, login, pass } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 4.0, 302.0, 37.0, tr("It's possible to add a group of sequential file names like img001.jpg, img002.jpg, etc., img100.jpg to New Download Manager download queue. Use the asterisk wildcard for the file name pattern. \nFor example:   http://www.example.com/pictures/img*.jpg"));
                d.label(7.0, 47.0, 30.0, 10.0, "Address");
                d.edit(39.0, 44.0, 270.0, 14.0, url, true);
                d.group(7.0, 64.0, 302.0, 55.0, "Replace asterisk to");
                if d.radio(15.0, 79.0, 66.0, 10.0, !*letters, "Numbers") {
                    *letters = false;
                }
                if d.radio(92.0, 79.0, 65.0, 10.0, *letters, "Letters") {
                    *letters = true;
                }
                d.label(11.0, 100.0, 33.0, 10.0, "From:");
                d.label(107.0, 100.0, 22.0, 10.0, "To:");
                d.label_r(203.0, 100.0, 61.0, 10.0, "Wildcard size");
                if *letters {
                    // one letter each, a..z or A..Z; no wildcard size
                    d.edit(53.0, 95.0, 38.0, 16.0, lf, true);
                    d.edit(136.0, 95.0, 38.0, 16.0, lt, true);
                    d.edit(268.0, 95.0, 34.0, 16.0, &mut "1".to_string(), false);
                } else {
                    d.drag(53.0, 95.0, 38.0, 16.0, from, 0, 999_999);
                    d.drag(136.0, 95.0, 38.0, 16.0, to, 0, 999_999);
                    d.drag(268.0, 95.0, 34.0, 16.0, width, 1, 9);
                }
                d.group(7.0, 126.0, 302.0, 29.0, "");
                d.check(13.0, 124.0, 70.0, 12.0, auth, "Use authorization");
                d.label_r(13.0, 139.0, 60.0, 10.0, "Login");
                d.edit(77.0, 136.0, 78.0, 14.0, login, *auth);
                d.label_r(160.0, 139.0, 64.0, 10.0, "Password");
                d.password(228.0, 136.0, 75.0, 14.0, pass, *auth);
                let base = if *auth { with_login(url, login, pass) } else { url.clone() };
                let range = batch_range(*letters, *from, *to, *width, lf, lt);
                let ok = url.contains('*') && !range.is_empty() && range.len() <= 10_000;
                let gen = |k: usize| base.replacen('*', &range[k], 1);
                let show = |k: usize| if ok && k < range.len() { gen(k) } else { String::new() };
                d.label(7.0, 166.0, 58.0, 10.0, "First file:");
                d.readonly(68.0, 165.0, 241.0, 11.0, &show(0));
                d.label(7.0, 179.0, 54.0, 10.0, "Second file:");
                d.readonly(68.0, 178.0, 241.0, 11.0, &show(1));
                d.label(15.0, 189.0, 8.0, 8.0, "...");
                d.label(7.0, 201.0, 54.0, 10.0, "Last file:");
                d.readonly(68.0, 199.0, 241.0, 11.0, &show(range.len().saturating_sub(1)));
                let add = d.button(182.0, 217.0, 50.0, 14.0, "OK", ok);
                if add {
                    let ids = self.add_queued((0..range.len()).map(gen).collect());
                    self.pick_queue(ids, true);
                }
                !(add || d.button(247.0, 217.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Grabber { step, adv, node, opts, save_mode, save_cat, folder, crawl } => {
                if let Some(n) = self.grab_load.take() {
                    self.project_cmd(0, &n);
                    return false;
                }
                if let Some(n) = self.grab_saved_as.take() {
                    opts.name = n;
                }
                if let Some(ck) = self.grab_cookies.take() {
                    opts.cookies = ck;
                    *crawl = Some(self.crawl(ctx, opts));
                }
                if let Some(c) = crawl.clone() {
                    let (finished, login) = { let s = c.state.lock().unwrap(); (s.done, s.need_login.clone()) };
                    if let Some(realm) = login {
                        // "Connecting to the site" (template 279): ask once, then explore again with the login
                        c.state.lock().unwrap().need_login = None;
                        let site = url::Url::parse(opts.start.trim()).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                        self.open(Dialog::GrabLogin { realm, site, user: opts.login.clone(), pass: String::new(), save: false });
                    }
                    if let Some((user, pass)) = self.grab_auth.take() {
                        (opts.auth, opts.login, opts.pass) = (true, user, pass);
                        *crawl = Some(self.crawl(ctx, opts));
                        return true;
                    }
                    if opts.start_all && finished && !c.auto_started.swap(true, Relaxed) {
                        self.grab(&c, opts, *save_mode, save_cat, folder, true);
                    }
                    let (keep, back, again) = self.grabber_results(ctx, ui, &c, opts, *save_mode, save_cat, folder, node);
                    if back {
                        c.stop.store(true, Relaxed);
                        (*crawl, *step) = (None, 0);
                    }
                    if again {
                        c.stop.store(true, Relaxed);
                        *crawl = Some(self.crawl(ctx, opts));
                    }
                    if !keep && opts.add_on_close {
                        self.grab(&c, opts, *save_mode, save_cat, folder, false);
                    }
                    if !keep && !opts.name.trim().is_empty() {
                        // closing the results window saves the project (template 269 unless "don't show")
                        let st = c.state.lock().unwrap();
                        let proj = core::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), files: st.files.clone(), pages: st.pages, ..Default::default() };
                        drop(st);
                        let name = opts.name.trim().to_string();
                        let (quiet, taken) = { let s = self.core.settings(); (s.grab_quiet, s.grab_projects.iter().any(|p| p.opts.name == name)) };
                        if quiet && !taken {
                            self.save_project(&name, None, Some(proj));
                        } else {
                            self.open(Dialog::GrabSave { name, orig: None, quiet, proj: Some(Box::new(proj)), sched: false });
                        }
                    }
                    return keep;
                }
                // NDM's wizard (template 239 + pages 257 / 259 / 264 / 240) under its menu bar (menu 256)
                let saved = self.core.settings().grab_projects.iter().any(|p| p.opts.name == opts.name.trim());
                let full = ui.max_rect();
                let cmd = {
                    let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(full.min, Vec2::new(full.width(), GRAB_MENU))));
                    self.grab_menu(&mut bar)
                };
                match cmd {
                    Some(GrabCmd::New) => {
                        (*opts, *step, *adv) = (self.new_grab_opts(), 0, false);
                        return true;
                    }
                    Some(GrabCmd::Save) => {
                        let proj = core::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), ..Default::default() };
                        self.open(Dialog::GrabSave { name: opts.name.trim().to_string(), orig: None, quiet: false, proj: Some(Box::new(proj)), sched: false });
                    }
                    Some(GrabCmd::SaveTpl) => {
                        let proj = core::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), ..Default::default() };
                        self.open(Dialog::GrabTpl { name: String::new(), proj: Box::new(proj), replace: false });
                    }
                    Some(GrabCmd::Recent(n)) => {
                        self.project_cmd(0, &n);
                        return false;
                    }
                    Some(GrabCmd::Schedule) => {
                        let proj = core::GrabProject { opts: opts.clone(), save_mode: *save_mode, save_cat: save_cat.clone(), folder: folder.clone(), ..Default::default() };
                        self.grab_schedule(proj, saved);
                    }
                    Some(GrabCmd::Close) => return false,
                    None => {}
                }
                let mut body = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(full.min + Vec2::new(0.0, GRAB_MENU), full.max)));
                let mut d = dlu::D::new(&mut body);
                // the picture on the left (static 1801, in a black frame), cropped to fill it without stretching
                let pic = d.rect(7.0, 7.0, 85.0, 160.0);
                match self.res.grab_pic.as_ref() {
                    Some(t) => {
                        let [w, h] = t.size().map(|v| v as f32);
                        let (box_a, img_a) = (pic.width() / pic.height(), w / h);
                        let uv = if img_a > box_a {
                            let k = box_a / img_a;
                            Rect::from_min_max(egui::pos2((1.0 - k) / 2.0, 0.0), egui::pos2((1.0 + k) / 2.0, 1.0))
                        } else {
                            let k = img_a / box_a;
                            Rect::from_min_max(egui::pos2(0.0, (1.0 - k) / 2.0), egui::pos2(1.0, (1.0 + k) / 2.0))
                        };
                        d.ui.painter().image(t.id(), pic, uv, Color32::WHITE);
                    }
                    None => drop(d.ui.painter().rect_filled(pic, 0.0, Color32::from_rgb(28, 93, 178))),
                }
                d.ui.painter().rect_stroke(pic, 0.0, egui::Stroke::new(1.0_f32, Color32::BLACK), egui::StrokeKind::Inside);
                let adv_label = if *adv { format!("<< {}", tr("Advanced")) } else { format!("{} >>", tr("Advanced")) };
                // NDM's own English where its language files still carry older words
                let en = |new: &str, old: &str| if i18n::current().is_empty() { new.to_string() } else { tr(old) };
                match *step {
                    0 => {
                        d.label(106.0, 2.0, 164.0, 9.0, "Grabber Project Name");
                        d.edit(106.0, 12.0, 166.0, 14.0, &mut opts.name, true);
                        d.label(106.0, 29.0, 287.0, 9.0, "Start page/address");
                        d.edit(106.0, 39.0, 289.0, 14.0, &mut opts.start, true);
                        d.label(106.0, 56.0, 147.0, 9.0, "Project template");
                        let mut names: Vec<String> = grabber::TEMPLATES.iter().map(|t| t.to_string()).collect();
                        names.extend(self.core.settings().grab_templates.iter().map(|t| t.opts.name.clone()));
                        let shown: Vec<String> = names.iter().enumerate().map(|(k, n)| if k < grabber::TEMPLATES.len() { tr(n) } else { n.clone() }).collect();
                        let cur_k = names.iter().position(|n| *n == opts.template).unwrap_or(0);
                        let mut cur = shown[cur_k].clone();
                        if d.combo(106.0, 66.0, 289.0, 14.0, "gtpl", &mut cur, &shown.iter().map(String::as_str).collect::<Vec<_>>(), true) {
                            if let Some(k) = shown.iter().position(|s| *s == cur) {
                                self.apply_template(&names[k], opts, save_mode, save_cat);
                            }
                        }
                        d.text(106.0, 80.0, 290.0, 26.0, "If you select a project template, the wizard will make the required project settings for the selected template on the next steps. Note that you may always change any settings manually.");
                        d.hline(106.0, 111.0, 289.0);
                        d.check(106.0, 115.0, 161.0, 10.0, &mut opts.auth, "Use authorization");
                        d.label_r(107.0, 131.0, 50.0, 9.0, "Login");
                        d.edit(160.0, 128.0, 74.0, 14.0, &mut opts.login, opts.auth);
                        d.label_r(240.0, 131.0, 61.0, 9.0, "Password");
                        d.password(305.0, 128.0, 73.0, 14.0, &mut opts.pass, opts.auth);
                        d.text(106.0, 150.0, 188.0, 17.0, "Press Advanced button to enable manual login or to disable a logout page");
                        if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                            *adv = !*adv;
                        }
                        if *adv {
                            d.hline(7.0, 176.0, 396.0);
                            d.check(105.0, 184.0, 289.0, 10.0, &mut opts.manual_on, "Enter login and password manually at the following web page:");
                            d.edit(105.0, 196.0, 290.0, 14.0, &mut opts.manual, opts.manual_on);
                            d.check(106.0, 220.0, 288.0, 10.0, &mut opts.logout_on, "Don't open the logout page:");
                            d.edit(105.0, 232.0, 290.0, 14.0, &mut opts.logout, opts.logout_on);
                            d.text(106.0, 248.0, 286.0, 24.0, "Many sites have a logout page(s) that the Grabber should not open.  You may use asterisk wildcard here to specify a pattern for different logout pages.");
                        }
                    }
                    1 => {
                        d.group(103.0, 1.0, 294.0, 123.0, "Save To");
                        if d.radio(110.0, 8.0, 268.0, 22.0, *save_mode == 0, &en("Every file to a folder according to NDM category of the file", "Every file to the directory according to the category of the file")) {
                            *save_mode = 0;
                        }
                        if d.radio(110.0, 33.0, 270.0, 10.0, *save_mode == 1, &en("All files to the folder associated with the following NDM category:", "All files to one category")) {
                            *save_mode = 1;
                        }
                        let cat_names = self.core.settings().category_names();
                        let cats: Vec<&str> = cat_names.iter().map(String::as_str).collect();
                        d.combo(130.0, 43.0, 130.0, 14.0, "gcat", save_cat, &cats, *save_mode == 1);
                        let cat_dir = self.core.settings().folder_for(save_cat);
                        d.edit(130.0, 58.0, 256.0, 14.0, &mut cat_dir.clone(), false);
                        if d.radio(110.0, 79.0, 186.0, 10.0, *save_mode == 2, &en("All files to the following folder", "All files to one directory")) {
                            *save_mode = 2;
                        }
                        let m2 = *save_mode == 2;
                        d.edit(130.0, 89.0, 256.0, 14.0, folder, m2);
                        d.at(130.0, 107.0, 198.0, 10.0, |ui| ui.add_enabled(m2, egui::Checkbox::new(&mut opts.subfolders, tr("Use original relative subfolders"))));
                        if d.button(331.0, 106.0, 55.0, 14.0, "Browse...", m2) {
                            if let Some(p) = rfd::FileDialog::new().set_directory(folder.as_str()).pick_folder() {
                                *folder = p.display().to_string();
                            }
                        }
                        d.check(110.0, 128.0, 280.0, 10.0, &mut opts.convert, "Convert the links in downloaded html files to the local files for offline browsing");
                        d.check(110.0, 144.0, 282.0, 10.0, &mut opts.overwrite, "Overwrite existing files");
                        d.check(110.0, 156.0, 284.0, 19.0, &mut opts.add_on_close, "Add checked files to NDM main list and to download queue on closing the grabber");
                    }
                    2 => {
                        d.text(106.0, 7.0, 283.0, 25.0, "At this step you should specify what web pages to explore to find the required files. At the next step, you will be able to set file types, location, and other filters.");
                        if d.radio(106.0, 35.0, 206.0, 10.0, opts.whole_site, "Explore the whole site") {
                            opts.whole_site = true;
                        }
                        if d.radio(106.0, 48.0, 241.0, 10.0, !opts.whole_site, "Explore the specified number of link levels:") {
                            opts.whole_site = false;
                        }
                        let ws = opts.whole_site;
                        let r = d.rect(119.0, 60.0, 20.0, 14.0);
                        d.ui.put(r, |ui: &mut egui::Ui| ui.add_enabled(!ws, egui::DragValue::new(&mut opts.levels).range(0..=20)));
                        d.label(141.0, 63.0, 113.0, 9.0, "levels within the base site");
                        d.drag(267.0, 60.0, 20.0, 14.0, &mut opts.levels_other, 0, 5);
                        d.label(289.0, 63.0, 89.0, 9.0, "levels on other sites");
                        // SS_NOTIFY statics drawn as links (0x50020100): they open the grabber's help
                        let link = |d: &mut dlu::D, y: f32, w: f32, t: &str, c: Color32| d.at(119.0, y, w, 9.0, |ui| ui.add(egui::Label::new(RichText::new(tr(t)).color(c)).sense(Sense::click()))).clicked();
                        if link(&mut d, 77.0, 261.0, "What is the number of link levels?", Color32::from_rgb(0, 0, 255)) {
                            help("grabber.htm", true);
                        }
                        let mut ignore = !opts.popups;
                        if d.check(109.0, 91.0, 240.0, 10.0, &mut ignore, "Ignore popup windows").changed() {
                            opts.popups = !ignore;
                        }
                        d.at(109.0, 104.0, 240.0, 10.0, |ui| ui.add_enabled(ws, egui::Checkbox::new(&mut opts.no_parent, tr("Don't explore parent directories"))));
                        d.check(109.0, 117.0, 240.0, 10.0, &mut opts.main_domain, "Explore all sites within the main domain");
                        d.check(109.0, 130.0, 240.0, 10.0, &mut opts.js, "Process JavaScript");
                        if link(&mut d, 141.0, 274.0, "Use this option carefully. What is the risk?", Color32::GRAY) {
                            help("grabber.htm", true);
                        }
                        if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                            *adv = !*adv;
                        }
                        if *adv {
                            d.hline(7.0, 176.0, 396.0);
                            d.check(105.0, 182.0, 271.0, 10.0, &mut opts.explore_in_on, "Explore web pages within the following paths/domains only:");
                            d.edit(105.0, 193.0, 291.0, 27.0, &mut opts.explore_in, opts.explore_in_on);
                            d.check(105.0, 224.0, 254.0, 10.0, &mut opts.explore_not_on, "Don't explore web pages within the following paths/domains:");
                            d.edit(105.0, 235.0, 290.0, 27.0, &mut opts.explore_not, opts.explore_not_on);
                            d.label(105.0, 264.0, 298.0, 9.0, "Use semicolons ( ; ) to separate entries.");
                        }
                    }
                    _ => {
                        d.label(106.0, 6.0, 216.0, 9.0, "Download the following files (file types)");
                        self.grab_filter(&mut d, 16.0, &mut opts.masks, false);
                        d.label(106.0, 36.0, 198.0, 9.0, "Don't download the following files (file types)");
                        self.grab_filter(&mut d, 46.0, &mut opts.exclude, true);
                        d.check(106.0, 69.0, 237.0, 10.0, &mut opts.files_same_site, "Search files on this site only");
                        let sub = opts.subfolders && *save_mode == 2;
                        d.at(106.0, 87.0, 294.0, 10.0, |ui| ui.add_enabled(!sub, egui::Checkbox::new(&mut opts.hide_dups, tr("Hide duplicate files found in different locations"))));
                        d.check(106.0, 105.0, 262.0, 10.0, &mut opts.start_all, "Start downloading all matched files at once");
                        d.text(106.0, 116.0, 292.0, 34.0, "You can explore the site at first, check the files that you need, and download them in the main Grabber window, or add them to the main list of NDM. This option downloads all explored files immediately.");
                        if d.button(305.0, 153.0, 73.0, 14.0, &adv_label, true) {
                            *adv = !*adv;
                        }
                        if *adv {
                            d.hline(7.0, 177.0, 393.0);
                            d.group(10.0, 183.0, 101.0, 76.0, "Download if file size is");
                            let units: Vec<String> = grabber::SIZE_UNITS.iter().map(|u| tr(u)).collect();
                            let unit_refs: Vec<&str> = units.iter().map(String::as_str).collect();
                            for (k, y, t) in [(0usize, 195.0, "Not less than"), (1, 228.0, "Not more than")] {
                                let on = if k == 0 { &mut opts.min_on } else { &mut opts.max_on };
                                d.check(20.0, y, 81.0, 10.0, on, t);
                                let on = *on;
                                let mut n = opts.size[k].to_string();
                                if d.edit(20.0, y + 11.0, 33.0, 13.0, &mut n, on).changed() {
                                    opts.size[k] = n.trim().parse().unwrap_or(0);
                                }
                                let mut u = units[opts.units[k] as usize % 3].clone();
                                if d.combo(56.0, y + 11.0, 41.0, 13.0, ["gmin", "gmax"][k], &mut u, &unit_refs, on) {
                                    opts.units[k] = units.iter().position(|x| *x == u).unwrap_or(0) as u8;
                                }
                            }
                            d.check(126.0, 182.0, 268.0, 10.0, &mut opts.files_in_on, "Download the files located within the following paths/domains only :");
                            d.edit(126.0, 193.0, 268.0, 27.0, &mut opts.files_in, opts.files_in_on);
                            d.check(126.0, 225.0, 268.0, 10.0, &mut opts.files_not_on, "Don't download the files located within the following paths/domains:");
                            d.edit(126.0, 236.0, 268.0, 27.0, &mut opts.files_not, opts.files_not_on);
                            d.label(126.0, 264.0, 267.0, 9.0, "Use semicolons ( ; ) to separate entries.");
                        }
                    }
                }
                // the advanced half (274 DLU pages) pushes the buttons down by 97 DLU
                let o = if *adv && *step != 1 { 97.0 } else { 0.0 };
                d.hline(7.0, 183.0 + o, 392.0);
                let ok = url::Url::parse(opts.start.trim()).is_ok_and(|u| u.scheme().starts_with("http") && u.host_str().is_some());
                if d.button(155.0, 190.0 + o, 60.0, 14.0, "< Back", *step > 0) {
                    (*step, *adv) = (*step - 1, false);
                }
                if d.button(221.0, 190.0 + o, 60.0, 14.0, "Next >", ok) {
                    if *step < 3 {
                        (*step, *adv) = (*step + 1, false);
                    } else if opts.manual_on {
                        // NDM opens the page in the user's browser and the
                        // extension hands back the site's cookies once the user has logged in
                        match url::Url::parse(opts.manual.trim()).ok().filter(|u| u.scheme().starts_with("http")) {
                            Some(u) => {
                                *self.core.login_wait.lock().unwrap() = u.host_str().map(str::to_ascii_lowercase);
                                self.core.login_cookies.lock().unwrap().clear();
                                open_url(u.as_str());
                                self.open(Dialog::GrabManual { url: u.to_string() });
                            }
                            None => self.open(Dialog::Info { title: "Manual login page".into(), text: "Please enter manual login page or uncheck the corresponding checkbox".into() }),
                        }
                    } else {
                        *crawl = Some(self.crawl(ctx, opts));
                    }
                }
                !d.button(324.0, 190.0 + o, 60.0, 14.0, "Close", true)
            }
            Dialog::GrabTpl { name, proj, replace } => {
                // template 268; built-in names are reserved, an existing one is replaced on Yes
                let mut d = dlu::D::new(ui);
                if *replace {
                    d.text(7.0, 7.0, 172.0, 18.0, "The template with such name already exists. Do you want to replace the template?");
                } else {
                    d.text(7.0, 7.0, 172.0, 18.0, "Please enter a name for the grabber template");
                }
                d.edit(6.0, 29.0, 173.0, 14.0, name, !*replace);
                let n = name.trim().to_string();
                let ok = d.button(34.0, 49.0, 50.0, 14.0, if *replace { "Yes" } else { "OK" }, !n.is_empty());
                let cancel = d.button(102.0, 49.0, 50.0, 14.0, if *replace { "No" } else { "Cancel" }, true);
                if ok && grabber::TEMPLATES.iter().any(|t| t.eq_ignore_ascii_case(&n) || tr(t).eq_ignore_ascii_case(&n)) {
                    self.open(Dialog::Info { title: "Template Name".into(), text: "This name is reserved. Please select another name for your template.".into() });
                    return true;
                }
                let exists = self.core.settings().grab_templates.iter().any(|t| t.opts.name == n);
                if ok && exists && !*replace {
                    *replace = true;
                    return true;
                }
                if ok {
                    let mut t = (**proj).clone();
                    (t.opts.name, t.opts.start, t.opts.login, t.opts.pass, t.opts.cookies, t.opts.template, t.files, t.pages) = (n.clone(), String::new(), String::new(), String::new(), String::new(), String::new(), vec![], 0);
                    let mut s = self.core.settings();
                    s.grab_templates.retain(|x| x.opts.name != n);
                    s.grab_templates.push(t);
                    drop(s);
                    self.core.save();
                }
                if cancel && *replace {
                    *replace = false;
                    return true;
                }
                !(ok || cancel)
            }
            Dialog::GrabLoad { sel } => {
                // template 270: the saved projects; OK or a double click opens one in the grabber
                let mut d = dlu::D::new(ui);
                let names: Vec<String> = self.core.settings().grab_projects.iter().map(|p| p.opts.name.clone()).collect();
                let lr = d.listview(7.0, 7.0, 186.0, 124.0, &[]);
                let mut open = false;
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(lr.shrink(2.0)), |ui| {
                    egui::ScrollArea::vertical().id_salt("gload").auto_shrink(false).show(ui, |ui| {
                        for (k, n) in names.iter().enumerate() {
                            let r = ui.selectable_label(*sel == Some(k), n.as_str());
                            if r.clicked() {
                                *sel = Some(k);
                            }
                            if r.double_clicked() {
                                (*sel, open) = (Some(k), true);
                            }
                        }
                    });
                });
                open |= d.button(200.0, 7.0, 50.0, 14.0, "OK", true);
                let cancel = d.button(200.0, 24.0, 50.0, 14.0, "Cancel", true);
                if open {
                    match sel.and_then(|k| names.get(k)) {
                        Some(n) => self.grab_load = Some(n.clone()),
                        None => {
                            self.open(Dialog::Info { title: "Load project".into(), text: "Please select the project name!".into() });
                            return true;
                        }
                    }
                }
                !(open || cancel)
            }
            Dialog::GrabSettings { explore, dl, link, iec, adv } => {
                // template 262, checked (explore 1-20, download 1-10)
                let mut d = dlu::D::new(ui);
                d.edit(7.0, 6.0, 21.0, 14.0, explore, true);
                d.label(33.0, 10.0, 239.0, 10.0, "files to explore at the same time (1 to 10)");
                d.edit(7.0, 25.0, 22.0, 14.0, dl, true);
                d.label(33.0, 28.0, 239.0, 8.0, "files to download at the same time (1 to 10)");
                d.text(7.0, 43.0, 278.0, 18.0, "Please note that a web server may reject requests if you set a large number of files to explore (download) at the same time.");
                d.hline(7.0, 66.0, 276.0);
                d.check(7.0, 75.0, 278.0, 10.0, iec, "Look up files in IE cache before downloading");
                d.check(7.0, 89.0, 278.0, 10.0, link, "Use link texts as download descriptions when adding files to NDM main list");
                d.check(7.0, 103.0, 278.0, 8.0, adv, "Use Advanced web page processing");
                let ok = d.button(164.0, 118.0, 50.0, 14.0, "OK", true);
                if ok {
                    let (e, n) = (explore.trim().parse::<usize>().ok().filter(|v| (1..=20).contains(v)), dl.trim().parse::<usize>().ok().filter(|v| (1..=10).contains(v)));
                    let bad = if e.is_none() { Some("Invalid number of files explored at the same time. Please set a correct number.") } else if n.is_none() { Some("Invalid number of files downloaded at the same time. Please set a correct number.") } else { None };
                    if let Some(text) = bad {
                        self.open(Dialog::Info { title: "Settings of NDM Grabber".into(), text: text.into() });
                        return true;
                    }
                    let mut s = self.core.settings();
                    (s.grab_explore, s.grab_dl, s.grab_link_text, s.grab_ie_cache, s.grab_adv) = (e.unwrap(), n.unwrap(), *link, *iec, *adv);
                    drop(s);
                    self.core.save();
                }
                !(ok || d.button(235.0, 118.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::GrabManual { url } => {
                let mut d = dlu::D::new(ui);
                d.label(7.0, 7.0, 246.0, 9.0, "Enter login and password manually at the following web page:");
                d.label(7.0, 20.0, 246.0, 9.0, url.clone());
                let ok = d.button(140.0, 49.0, 50.0, 14.0, "OK", true);
                let cancel = d.button(203.0, 49.0, 50.0, 14.0, "Cancel", true);
                if ok || cancel {
                    *self.core.login_wait.lock().unwrap() = None;
                    let ck = std::mem::take(&mut *self.core.login_cookies.lock().unwrap());
                    if ok {
                        self.grab_cookies = Some(ck);
                    }
                }
                !(ok || cancel)
            }
            Dialog::GrabStats { crawl } => {
                // template 271
                let st = crawl.state.lock().unwrap();
                let (done, pages, total, explored) = (st.done, st.pages, st.files.len(), st.explored);
                let urls: HashSet<String> = st.files.iter().map(|f| f.url.clone()).collect();
                drop(st);
                let downloaded = self.core.jobs().iter().filter(|j| {
                    let i = j.info();
                    i.status == Status::Complete && urls.contains(&i.url)
                }).count();
                let mut d = dlu::D::new(ui);
                d.label_r(3.0, 3.0, 36.0, 9.0, "Status:");
                d.label(41.0, 3.0, 63.0, 9.0, if done { tr("Complete") } else { tr("Exploring site") });
                d.hline(5.0, 14.0, 101.0);
                d.label(7.0, 17.0, 97.0, 9.0, "Web pages processed");
                d.label_r(7.0, 28.0, 51.0, 9.0, "Simple");
                d.label(65.0, 29.0, 39.0, 9.0, pages.to_string());
                d.label_r(7.0, 37.0, 51.0, 9.0, "Advanced");
                d.label(65.0, 38.0, 39.0, 9.0, "0");
                d.hline(5.0, 49.0, 101.0);
                d.label(7.0, 52.0, 97.0, 9.0, "Files");
                for (y, k, v) in [(63.0, "Total", total), (72.0, "Explored", explored), (81.0, "Matched", total), (90.0, "Downloaded", downloaded)] {
                    d.label_r(7.0, y, 54.0, 9.0, k);
                    d.label(65.0, y, 38.0, 9.0, v.to_string());
                }
                true
            }
            Dialog::Limiter { kb, startup } => {
                let mut d = dlu::D::new(ui);
                d.label_r(7.0, 7.0, 170.0, 9.0, "Maximum download speed for one file");
                d.edit(183.0, 4.0, 33.0, 14.0, kb, true);
                d.label(224.0, 7.0, 65.0, 9.0, "KBytes/sec");
                d.text(7.0, 23.0, 288.0, 26.0, "Note: Internet servers may break connection when the speed is too limited! Thus it's not recommended to use Speed Limiter to download from servers that do not support \"resume\" feature.");
                d.check(7.0, 52.0, 288.0, 10.0, startup, "Always turn on Speed Limiter on New Download Manager startup");
                let v = kb.trim().parse::<u64>().ok().filter(|v| *v > 0);
                let ok = d.button(189.0, 68.0, 50.0, 14.0, "OK", v.is_some());
                if ok {
                    let mut s = self.core.settings();
                    (s.limit_kb, s.limit_startup) = (v.unwrap(), *startup);
                    drop(s);
                    self.core.apply_settings();
                    self.core.save();
                }
                !(ok || d.button(245.0, 68.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::Columns { list, sel, width } => {
                let mut d = dlu::D::new(ui);
                d.text(7.0, 7.0, 222.0, 25.0, "Check the columns that you would like visible in this list. Use the Move Up and Move Down buttons to reorder the columns however you like.");
                let r = d.listview(7.0, 36.0, 139.0, 87.0, &[]);
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (k, (c, vis, w)) in list.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                // File Name can't be hidden
                                ui.add_enabled(*c != 0, egui::Checkbox::without_text(vis));
                                if ui.add_sized([ui.available_width(), 16.0], egui::SelectableLabel::new(*sel == k, COLS[*c as usize].1)).clicked() {
                                    (*sel, *width) = (k, w.to_string());
                                }
                            });
                        }
                    });
                });
                let n = list.len();
                if d.button(158.0, 36.0, 71.0, 14.0, "Move Up", *sel > 0) {
                    list.swap(*sel, *sel - 1);
                    *sel -= 1;
                }
                if d.button(158.0, 54.0, 71.0, 14.0, "Move Down", *sel + 1 < n) {
                    list.swap(*sel, *sel + 1);
                    *sel += 1;
                }
                let (c, vis, _) = list[*sel];
                if d.button(158.0, 72.0, 71.0, 14.0, "Show", !vis) {
                    list[*sel].1 = true;
                }
                if d.button(158.0, 90.0, 71.0, 14.0, "Hide", vis && c != 0) {
                    list[*sel].1 = false;
                }
                if d.button(158.0, 108.0, 71.0, 14.0, "Reset", true) {
                    *list = core::DEFAULT_COLUMNS.iter().map(|(k, w)| (*k, true, *w)).collect();
                    list.extend(COLS.iter().enumerate().filter(|(k, _)| !core::DEFAULT_COLUMNS.iter().any(|c| c.0 as usize == *k)).map(|(k, c)| (k as u8, false, c.2)));
                    (*sel, *width) = (0, list[0].2.to_string());
                }
                d.label_r(7.0, 132.0, 123.0, 10.0, "The selected column should be");
                if d.edit(136.0, 130.0, 20.0, 14.0, width, true).changed() {
                    if let Some(w) = width.trim().parse::<u16>().ok().filter(|w| (10..=2000).contains(w)) {
                        list[*sel].2 = w;
                    }
                }
                d.label(159.0, 132.0, 66.0, 10.0, "pixels wide");
                d.hline(7.0, 149.0, 222.0);
                let ok = d.button(107.0, 155.0, 50.0, 14.0, "OK", true);
                if ok {
                    self.core.settings().columns = list.iter().filter(|c| c.1).map(|c| (c.0, c.2)).collect();
                    self.core.save();
                }
                !(ok || d.button(179.0, 155.0, 50.0, 14.0, "Cancel", true))
            }
            Dialog::About => {
                let icon = texture(ctx, "app_icon", &app_icon());
                let mut d = dlu::D::new(ui);
                d.ui.put(d.rect(7.0, 6.0, 20.0, 20.0), egui::Image::new((icon.id(), Vec2::splat(32.0))));
                d.at(33.0, 3.0, 183.0, 16.0, |ui| ui.label(RichText::new(TITLE.trim_end_matches(" 1.0")).size(14.0).strong()));
                d.label(32.0, 19.0, 208.0, 8.0, format!("{} {}", tr("Version:"), env!("CARGO_PKG_VERSION")));
                if d.button(75.0, 31.0, 99.0, 14.0, "Check for Update", true) {
                    self.check_updates(ctx, false);
                }
                !d.button(102.0, 212.0, 55.0, 14.0, "Close", true)
            }
            Dialog::TbCustomize { layout, avail, cur } => {
                let mut d = dlu::D::new(ui);
                // available: a separator always, plus the buttons not on the bar (comctl32's lists)
                let left: Vec<i8> = std::iter::once(-1).chain((0..TB_ICONS as i8).filter(|b| !layout.contains(b))).collect();
                let strip = self.toolbar.as_ref().map(|t| t.strip(true, true, false));
                let list = |ui: &mut egui::Ui, items: &[i8], sel: &mut Option<usize>| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (k, b) in items.iter().enumerate() {
                            let r = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
                            if *sel == Some(k) {
                                ui.painter().rect_filled(r.0, 0.0, ui.visuals().selection.bg_fill);
                            }
                            let label = match usize::try_from(*b) {
                                Ok(b) => {
                                    if let Some(t) = strip {
                                        let u = egui::Rect::from_min_max(egui::pos2(b as f32 / TB_ICONS as f32, 0.0), egui::pos2((b + 1) as f32 / TB_ICONS as f32, 1.0));
                                        ui.painter().image(t, egui::Rect::from_min_size(r.0.min + Vec2::new(2.0, 2.0), Vec2::splat(18.0)), u, Color32::WHITE);
                                    }
                                    tr(TB_BUTTONS[b].1)
                                }
                                Err(_) => tr("Separator"),
                            };
                            ui.painter().text(r.0.left_center() + Vec2::new(26.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), ui.visuals().text_color());
                            if r.1.clicked() {
                                *sel = Some(k);
                            }
                        }
                    });
                };
                d.label(4.0, 5.0, 84.0, 10.0, "Available toolbar buttons:");
                let r = d.listview(4.0, 17.0, 120.0, 100.0, &[]);
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| list(ui, &left, avail));
                d.label(182.0, 5.0, 78.0, 10.0, "Current toolbar buttons:");
                let r = d.listview(182.0, 17.0, 120.0, 100.0, &[]);
                d.ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink(2.0)), |ui| list(ui, layout, cur));
                let before = layout.clone();
                if d.button(131.0, 42.0, 44.0, 14.0, &format!("{} ->", tr("Add")), avail.is_some_and(|a| a < left.len())) {
                    // inserted before the selected current button, like comctl32
                    let at = cur.map_or(layout.len(), |c| c.min(layout.len()));
                    layout.insert(at, left[avail.unwrap()]);
                    (*avail, *cur) = (None, Some(at));
                }
                if d.button(131.0, 62.0, 44.0, 14.0, &format!("<- {}", tr("Remove")), cur.is_some_and(|c| c < layout.len())) {
                    layout.remove(cur.unwrap());
                    *cur = None;
                }
                let close = d.button(308.0, 6.0, 44.0, 14.0, "Close", true);
                if d.button(308.0, 23.0, 44.0, 14.0, "Reset", true) {
                    *layout = core::Settings::default().tb_layout;
                    (*avail, *cur) = (None, None);
                }
                if d.button(308.0, 40.0, 44.0, 14.0, "Help", help("index.htm", false)) {
                    help("customization.htm", true);
                }
                if let Some(c) = cur.filter(|c| *c < layout.len()) {
                    if d.button(308.0, 74.0, 44.0, 14.0, "Move Up", c > 0) {
                        layout.swap(c, c - 1);
                        *cur = Some(c - 1);
                    }
                    if d.button(308.0, 91.0, 44.0, 14.0, "Move Down", c + 1 < layout.len()) {
                        layout.swap(c, c + 1);
                        *cur = Some(c + 1);
                    }
                } else {
                    d.button(308.0, 74.0, 44.0, 14.0, "Move Up", false);
                    d.button(308.0, 91.0, 44.0, 14.0, "Move Down", false);
                }
                // comctl32 applies every change to the bar at once
                if *layout != before {
                    self.core.settings().tb_layout = layout.clone();
                    self.core.save();
                }
                !close
            }
            Dialog::Tip { text } => {
                let mut d = dlu::D::new(ui);
                // NDM's OnPaint (0x54d200): the SS_BLACKFRAME box, white from the bulb static (20,17) to the
                // frame's far corner less the same inset, bitmap 104 at its own size; black text on the white
                let edge = dlu::pal(d.ui).edge;
                d.ui.painter().rect_stroke(d.rect(12.0, 11.0, 228.0, 123.0), 0.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
                d.ui.painter().rect_filled(d.rect(20.0, 17.0, 212.0, 111.0), 0.0, Color32::WHITE);
                if let Some(t) = self.res.tree.get(&104) {
                    let at = d.rect(20.0, 17.0, 1.0, 1.0).min;
                    d.ui.painter().image(t.id(), egui::Rect::from_min_size(at, t.size_vec2() / ctx.pixels_per_point()), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
                d.label(55.0, 29.0, 168.0, 12.0, RichText::new(tr("Did you know...")).color(Color32::BLACK));
                d.text(28.0, 57.0, 196.0, 66.0, RichText::new(text.as_str()).color(Color32::BLACK));
                let mut on = self.core.settings().tips_startup;
                if d.check(12.0, 146.0, 101.0, 10.0, &mut on, "Show Tips on StartUp").changed() {
                    self.core.settings().tips_startup = on;
                    self.core.save();
                }
                if d.button(121.0, 143.0, 54.0, 14.0, "Next Tip", true) {
                    *text = self.next_tip();
                }
                !d.button(183.0, 143.0, 54.0, 14.0, "Close", true)
            }
        }
    }

    /// The next tip (MFC GetNextTipString: wraps at the end), or NDM's message when there is no tips file.
    fn next_tip(&self) -> String {
        let tips = i18n::tips();
        if tips.is_empty() {
            return tr("Tips file does not exist in the prescribed directory");
        }
        let mut s = self.core.settings();
        let t = tips[s.tip_next % tips.len()].clone();
        s.tip_next = (s.tip_next + 1) % tips.len();
        drop(s);
        self.core.save();
        t
    }

    /// The grabber window after the wizard: site tree on the left, found files on the right.
    /// Returns (keep open, back to wizard).
    /// New wizard settings: the start page from the clipboard, the file types of Options -> File types.
    fn new_grab_opts(&self) -> grabber::Opts {
        let masks = self.core.settings().file_types.split_whitespace().map(|e| format!("*.{}", e.trim_start_matches('.'))).collect::<Vec<_>>().join(",");
        let start = links(&clipboard()).into_iter().next().unwrap_or_else(|| "https://".into());
        let n = self.core.settings().grab_projects.len() + 1;
        grabber::Opts { name: format!("project_{n}"), start, levels: 2, no_parent: true, masks, main_domain: true, hide_dups: true, size: [1, 10], units: [0, 1], ..Default::default() }
    }

    /// The grabber's menu bar.
    fn grab_menu(&mut self, ui: &mut egui::Ui) -> Option<GrabCmd> {
        let (mut cmd, mut load, mut settings) = (None, false, false);
        let recent: Vec<String> = {
            let s = self.core.settings();
            s.grab_recent.iter().filter(|n| s.grab_projects.iter().any(|p| p.opts.name == **n)).cloned().collect()
        };
        egui::menu::bar(ui, |ui| {
            ui.menu_button(tr("Project"), |ui| {
                if ui.button(tr("New")).clicked() {
                    cmd = Some(GrabCmd::New);
                    ui.close_menu();
                }
                if ui.button(tr("Load...")).clicked() {
                    load = true;
                    ui.close_menu();
                }
                if ui.button(tr("Save")).clicked() {
                    cmd = Some(GrabCmd::Save);
                    ui.close_menu();
                }
                if ui.button(tr("Save current settings as a template")).clicked() {
                    cmd = Some(GrabCmd::SaveTpl);
                    ui.close_menu();
                }
                ui.separator();
                // "1 name"... "4 name", or a greyed "Recent..." when there are none
                ui.menu_button(tr("Recent Projects"), |ui| {
                    if recent.is_empty() {
                        ui.add_enabled(false, egui::Button::new(tr("Recent...")));
                    }
                    for (k, n) in recent.iter().enumerate() {
                        if ui.button(format!("{} {n}", k + 1)).clicked() {
                            cmd = Some(GrabCmd::Recent(n.clone()));
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                if ui.button(tr("Close")).clicked() {
                    cmd = Some(GrabCmd::Close);
                    ui.close_menu();
                }
            });
            ui.menu_button(tr("Options"), |ui| {
                if ui.button(tr("Grabber Settings")).clicked() {
                    settings = true;
                    ui.close_menu();
                }
                if ui.button(tr("Schedule the project")).clicked() {
                    cmd = Some(GrabCmd::Schedule);
                    ui.close_menu();
                }
            });
            if ui.button(tr("Help")).clicked() {
                help("grabber.htm", true);
            }
        });
        if load {
            if self.core.settings().grab_projects.is_empty() {
                self.open(Dialog::Info { title: "Load project".into(), text: "There are no saved projects of NDM grabber".into() });
            } else {
                self.open(Dialog::GrabLoad { sel: None });
            }
        }
        if settings {
            let s = self.core.settings();
            let d = Dialog::GrabSettings { explore: s.grab_explore.to_string(), dl: s.grab_dl.to_string(), link: s.grab_link_text, iec: s.grab_ie_cache, adv: s.grab_adv };
            drop(s);
            self.open(d);
        }
        cmd
    }

    /// Project template `name` (combo 1511): one of NDM's built-ins, or one the
    /// user saved. The project's name, start page and login stay.
    fn apply_template(&self, name: &str, opts: &mut grabber::Opts, save_mode: &mut u8, save_cat: &mut String) {
        let mask = |f: &str| GRAB_FILTERS.iter().find(|x| x.0 == f).map_or("*.*", |x| x.1).to_string();
        let cat = |c: &str, cur: &str| if self.core.settings().category_names().iter().any(|n| n == c) { c.to_string() } else { cur.to_string() };
        opts.template = name.to_string();
        let Some(k) = grabber::TEMPLATES.iter().position(|t| *t == name) else {
            let Some(t) = self.core.settings().grab_templates.iter().find(|t| t.opts.name == name).cloned() else { return };
            let keep = opts.clone();
            *opts = grabber::Opts { name: keep.name, start: keep.start, auth: keep.auth, login: keep.login, pass: keep.pass, manual_on: keep.manual_on, manual: keep.manual, cookies: keep.cookies, template: keep.template, ..t.opts };
            (*save_mode, *save_cat) = (t.save_mode, t.save_cat);
            return;
        };
        if let Some((mode, c)) = grabber::builtin_template(k, opts, &mask) {
            *save_mode = mode;
            if let Some(c) = c {
                *save_cat = cat(c, save_cat);
            }
        }
    }

    /// A file-type filter: NDM's drop-down list ("All Files ( *.* )",...) and its "Add Filter" button,
    /// which here takes masks typed into a small popup. `exclude`: the first entry is "Don't use exclude filter".
    fn grab_filter(&mut self, d: &mut dlu::D, y: f32, masks: &mut String, exclude: bool) {
        let label = |m: &str| match GRAB_FILTERS.iter().find(|f| f.1 == m) {
            Some(f) => format!("{}  ( {} )", tr(f.0), f.1),
            None if m.is_empty() => tr("Don't use exclude filter"),
            None => format!("( {m} )"),
        };
        let mut choices: Vec<String> = GRAB_FILTERS.iter().map(|f| label(f.1)).collect();
        if exclude {
            choices.insert(0, label(""));
        }
        let mut cur = label(masks);
        if !choices.contains(&cur) {
            choices.push(cur.clone());
        }
        let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
        if d.combo(106.0, y, 194.0, 14.0, if exclude { "gexc" } else { "ginc" }, &mut cur, &refs, true) {
            *masks = if exclude && cur == label("") { String::new() } else { GRAB_FILTERS.iter().find(|f| label(f.1) == cur).map_or_else(|| masks.clone(), |f| f.1.to_string()) };
        }
        let r = d.rect(305.0, y - 1.0, 73.0, 14.0);
        let id = d.ui.id().with(("gaddf", exclude));
        let resp = d.ui.put(r, egui::Button::new(tr("Add Filter")).min_size(r.size()));
        if resp.clicked() {
            self.grab_filter_buf = masks.clone();
            d.ui.memory_mut(|m| m.toggle_popup(id));
        }
        let buf = &mut self.grab_filter_buf;
        egui::popup_below_widget(d.ui, id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(260.0);
            ui.add(egui::TextEdit::singleline(buf).hint_text("*.zip;*.pdf"));
            if ui.button(tr("OK")).clicked() {
                *masks = buf.split([',', ';', ' ']).map(str::trim).filter(|m| !m.is_empty()).collect::<Vec<_>>().join(";");
                ui.memory_mut(|m| m.close_popup());
            }
        });
    }

    /// Grabber "Thumbnails View": the found pictures in a grid, drawn from the file once it is downloaded.
    fn grab_thumbs(&mut self, ui: &mut egui::Ui, rect: Rect, items: &[(String, Option<PathBuf>)]) {
        const CELL: f32 = 104.0;
        let mut decoded = 0;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            egui::ScrollArea::vertical().id_salt("gthumbs").auto_shrink(false).show(ui, |ui| {
                let cols = ((rect.width() / CELL) as usize).max(1);
                for row in items.chunks(cols) {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(rect.width() - 14.0, CELL + 16.0), Sense::hover());
                    for (k, (name, path)) in row.iter().enumerate() {
                        let cell = Rect::from_min_size(r.min + Vec2::new(k as f32 * CELL + 4.0, 4.0), Vec2::splat(CELL - 8.0));
                        ui.painter().rect_stroke(cell, 0.0, egui::Stroke::new(1.0_f32, Color32::from_gray(190)), egui::StrokeKind::Inside);
                        // ponytail: decoded on the UI thread, at most 3 a frame; cached for the session
                        let tex = path.as_ref().and_then(|p| {
                            if !self.thumbs.contains_key(p) && decoded < 3 {
                                decoded += 1;
                                let t = image::open(p).ok().map(|img| {
                                    let px = img.thumbnail(192, 192).to_rgba8();
                                    ui.ctx().load_texture(p.display().to_string(), egui::ColorImage::from_rgba_unmultiplied([px.width() as usize, px.height() as usize], px.as_raw()), egui::TextureOptions::LINEAR)
                                });
                                self.thumbs.insert(p.clone(), t);
                            }
                            self.thumbs.get(p).cloned().flatten()
                        });
                        match tex {
                            Some(t) => {
                                let [w, h] = t.size().map(|v| v as f32);
                                let s = ((cell.width() - 6.0) / w).min((cell.height() - 6.0) / h).min(1.0);
                                ui.painter().image(t.id(), Rect::from_center_size(cell.center(), Vec2::new(w, h) * s), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => drop(ui.painter().text(cell.center(), Align2::CENTER_CENTER, "🖼", FontId::proportional(28.0), Color32::GRAY)),
                        }
                        let clip = Rect::from_min_size(egui::pos2(cell.left(), cell.bottom()), Vec2::new(cell.width(), 16.0));
                        ui.painter().with_clip_rect(clip).text(clip.center(), Align2::CENTER_CENTER, name, FontId::proportional(11.0), ui.visuals().text_color());
                    }
                }
            });
        });
        if decoded > 0 {
            ui.ctx().request_repaint();
        }
    }

    /// The Grabber's main window (template 241 under menu 256 and toolbar 260): the explored files with NDM's
    /// columns, a tree to filter them (All Files, Link View by page, Folder View by site), the toolbar's
    /// commands. Returns (keep the window, back to the wizard, explore again).
    #[allow(clippy::too_many_arguments)]
    fn grabber_results(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, c: &Arc<grabber::Crawl>, opts: &grabber::Opts, save_mode: u8, save_cat: &str, folder: &str, node: &mut usize) -> (bool, bool, bool) {
        let (done, total, checked, pages) = {
            let s = c.state.lock().unwrap();
            let mut pages: Vec<String> = s.page_list.clone();
            for f in &s.files {
                if !f.page.is_empty() && !pages.contains(&f.page) {
                    pages.push(f.page.clone());
                }
            }
            (s.done, s.files.len(), s.files.iter().filter(|f| f.checked).count(), pages)
        };
        let urls: HashSet<String> = c.state.lock().unwrap().files.iter().map(|f| f.url.clone()).collect();
        let jobs: HashMap<String, Arc<engine::Job>> = self.core.jobs().into_iter().filter(|j| urls.contains(&j.info().url)).map(|j| {
            let u = j.info().url.clone();
            (u, j)
        }).collect();
        let saved = self.core.settings().grab_projects.iter().any(|p| p.opts.name == opts.name.trim());
        let full = ui.max_rect();
        let cmd = {
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(full.min, Vec2::new(full.width(), GRAB_MENU))));
            self.grab_menu(&mut bar)
        };
        let (mut keep, mut back, mut again) = (true, false, false);
        let mut action = None;
        match cmd {
            Some(GrabCmd::New) => back = true,
            Some(GrabCmd::Save) => {
                let st = c.state.lock().unwrap();
                let proj = core::GrabProject { opts: opts.clone(), save_mode, save_cat: save_cat.into(), folder: folder.into(), files: st.files.clone(), pages: st.pages, ..Default::default() };
                drop(st);
                self.open(Dialog::GrabSave { name: opts.name.trim().to_string(), orig: None, quiet: false, proj: Some(Box::new(proj)), sched: false });
            }
            Some(GrabCmd::SaveTpl) => {
                let proj = core::GrabProject { opts: opts.clone(), save_mode, save_cat: save_cat.into(), folder: folder.into(), ..Default::default() };
                self.open(Dialog::GrabTpl { name: String::new(), proj: Box::new(proj), replace: false });
            }
            Some(GrabCmd::Recent(n)) => {
                self.project_cmd(0, &n);
                keep = false;
            }
            Some(GrabCmd::Schedule) => action = Some(8),
            Some(GrabCmd::Close) => keep = false,
            None => {}
        }
        // toolbar 260: NDM's 24 px strip (bitmaps 265 / 266), one image per command, a separator after the second
        let ppp = ctx.pixels_per_point();
        let tb = Rect::from_min_size(full.min + Vec2::new(0.0, GRAB_MENU), Vec2::new(full.width(), GRAB_TB));
        ui.painter().hline(tb.x_range(), tb.top(), egui::Stroke::new(1.0_f32, Color32::from_gray(200)));
        let running = jobs.values().any(|j| j.running());
        let buttons: [(usize, &str, bool); 10] = [
            (0, "Start Exploring", done),
            (1, "Stop Exploring", !done),
            (2, "Check All Files", total > 0),
            (3, "Uncheck All Files", total > 0),
            (4, "Start Download checked files", checked > 0),
            (5, "Stop download checked files", running),
            (6, "Add checked files to the main NDM list and to the download queue", checked > 0),
            (7, "Update all", done),
            (8, "Schedule the project", true),
            (9, "Show grabber statistics", true),
        ];
        let size = Vec2::splat(24.0 / ppp);
        let mut x = tb.left() + 4.0;
        for (k, (img, tip, enabled)) in buttons.into_iter().enumerate() {
            if k == 2 {
                ui.painter().vline(x + 3.0, tb.y_range().shrink(5.0), egui::Stroke::new(1.0_f32, Color32::from_gray(170)));
                x += 8.0;
            }
            let r = Rect::from_min_size(egui::pos2(x, tb.center().y - size.y / 2.0 - 3.0), size + Vec2::splat(6.0));
            let resp = ui.interact(r, ui.id().with(("gtb", k)), if enabled { Sense::click() } else { Sense::hover() }).on_hover_text(tr(tip));
            if enabled && resp.hovered() {
                ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, Color32::from_gray(160)), egui::StrokeKind::Inside);
            }
            let uv = Rect::from_min_max(egui::pos2(img as f32 / 10.0, 0.0), egui::pos2((img + 1) as f32 / 10.0, 1.0));
            if let Some([n, h, g]) = self.res.grab_tb.as_ref() {
                let t = if !enabled { g } else if resp.hovered() { h } else { n };
                ui.painter().image(t.id(), Rect::from_center_size(r.center(), size), uv, Color32::WHITE);
            } else {
                ui.painter().text(r.center(), Align2::CENTER_CENTER, ["⌕", "■", "☑", "☐", "▶", "⏹", "⏬", "⟳", "⏰", "ℹ"][img], FontId::proportional(14.0), if enabled { ui.visuals().text_color() } else { Color32::GRAY });
            }
            if resp.clicked() {
                action = Some(img);
            }
            x += r.width() + 1.0;
        }
        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(egui::pos2(full.left(), tb.bottom()), full.max)));
        let mut d = dlu::D::new(&mut body);

        // tree (1431): Thumbnails View, All Files, Link View > pages, Folder View > main domain / other sites
        let host = url::Url::parse(opts.start.trim()).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let domain = host.rsplitn(3, '.').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(".");
        let (pic_eye, fold, fold_open) = (self.res.tree(282), self.res.tree(215), self.res.tree(214));
        let page_icon = self.icon(ctx, "html");
        let short = |u: &str| u.split_once("://").map_or(u, |x| x.1).to_string();
        let mut nodes: Vec<(u8, usize, Option<egui::TextureId>, String)> = vec![(0, 0, pic_eye, tr("Thumbnails View")), (0, 1, if *node == 1 { fold_open } else { fold }, tr("All Files")), (0, 2, page_icon, tr("Link View"))];
        nodes.extend(pages.iter().enumerate().map(|(k, p)| (1, 10 + k, page_icon, short(p))));
        nodes.push((0, 3, fold_open, tr("Folder View")));
        nodes.push((1, 4, fold, format!("*.{domain} {}", tr("sites"))));
        nodes.push((1, 5, fold, tr("Other sites")));
        let tree = d.listview(7.0, 7.0, 101.0, 244.0, &[]);
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(tree.shrink(2.0)), |ui| {
            egui::ScrollArea::both().id_salt("gtree").auto_shrink(false).show(ui, |ui| {
                for (depth, id, icon, label) in &nodes {
                    let (row, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(150.0), 16.0), Sense::click());
                    let mut x = row.left() + 3.0 + *depth as f32 * 16.0;
                    if let Some(t) = icon {
                        ui.painter().image(*t, Rect::from_min_size(egui::pos2(x, row.center().y - 6.5), Vec2::new(16.0, 13.0)), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    x += 19.0;
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let g = ui.painter().layout_no_wrap(label.clone(), font, if *node == *id { Color32::WHITE } else { ui.visuals().text_color() });
                    let lr = Rect::from_min_size(egui::pos2(x - 2.0, row.top()), Vec2::new(g.size().x + 4.0, row.height()));
                    if *node == *id {
                        ui.painter().rect_filled(lr, 0.0, Color32::from_rgb(0, 120, 215));
                    }
                    ui.painter().galley(egui::pos2(x, row.center().y - g.size().y / 2.0), g, ui.visuals().text_color());
                    if resp.clicked() {
                        *node = *id;
                    }
                }
            });
        });

        // list (1023): NDM's seven columns, horizontally scrolled
        const COLS: [(&str, f32); 7] = [("File Name", 111.0), ("File Type", 154.0), ("Size", 57.0), ("Status", 57.0), ("Link Text", 57.0), ("Download from", 86.0), ("Save to", 86.0)];
        let dx = d.dx();
        let lr = d.listview(115.0, 7.0, 386.0, 244.0, &[]);
        let width: f32 = COLS.iter().map(|c| c.1 * dx).sum();
        let shown = |f: &grabber::Found| -> bool {
            let fh = url::Url::parse(&f.url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
            let ours = !domain.is_empty() && (fh == domain || fh.ends_with(&format!(".{domain}")));
            match *node {
                0 => ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg"].contains(&core::ext(&f.name).as_str()),
                4 => ours,
                5 => !ours,
                k if k >= 10 => pages.get(k - 10).is_some_and(|p| *p == f.page),
                _ => true,
            }
        };
        let rows: Vec<(usize, String, String)> = c.state.lock().unwrap().files.iter().enumerate().filter(|(_, f)| shown(f)).map(|(k, f)| (k, f.name.clone(), core::ext(&f.name))).collect();
        let icons: Vec<Option<egui::TextureId>> = rows.iter().map(|r| self.icon(ctx, &r.2)).collect();
        let types: Vec<String> = rows.iter().map(|r| self.type_names.entry(r.2.clone()).or_insert_with(|| win::type_name(&r.2)).clone()).collect();
        let font = egui::TextStyle::Body.resolve(d.ui.style());
        let ink = d.ui.visuals().text_color();
        let line = dlu::pal(d.ui).line;
        let save_dir = |name: &str, url: &str| self.core.grab_dir(name, url, save_mode, save_cat, folder, opts.subfolders);
        if *node != 0 {
        d.ui.scope_builder(egui::UiBuilder::new().max_rect(lr.shrink(1.0)), |ui| {
            egui::ScrollArea::horizontal().id_salt("glist").auto_shrink(false).show(ui, |ui| {
                ui.set_min_width(width);
                let (head, _) = ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::hover());
                let mut x = head.left();
                for (name, w) in COLS {
                    ui.painter().text(egui::pos2(x + 6.0, head.center().y), Align2::LEFT_CENTER, tr(name), font.clone(), ink);
                    x += w * dx;
                    ui.painter().vline(x - 1.0, head.y_range().shrink(3.0), egui::Stroke::new(1.0_f32, line));
                }
                ui.painter().hline(head.x_range(), head.bottom(), egui::Stroke::new(1.0_f32, line));
                egui::ScrollArea::vertical().id_salt("glistv").auto_shrink(false).show(ui, |ui| {
                    let mut s = c.state.lock().unwrap();
                    for (n, (k, name, _)) in rows.iter().enumerate() {
                        let f = &mut s.files[*k];
                        let (row, _) = ui.allocate_exact_size(Vec2::new(width, 19.0), Sense::hover());
                        let cb = Rect::from_min_size(row.left_center() + Vec2::new(4.0, -7.0), Vec2::splat(14.0));
                        ui.put(cb, egui::Checkbox::without_text(&mut f.checked));
                        if let Some(t) = icons[n] {
                            ui.painter().image(t, Rect::from_min_size(row.left_center() + Vec2::new(22.0, -8.0), Vec2::splat(16.0)), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        let job = jobs.get(&f.url).map(|j| j.info().clone());
                        let status = job.as_ref().map_or(String::new(), |i| match &i.status {
                            Status::Complete => tr("Complete"),
                            Status::Error(_) => tr("Error"),
                            Status::Downloading | Status::Connecting => format!("{}%", i.size.filter(|s| *s > 0).map_or(0, |s| i.done() * 100 / s)),
                            Status::Stopped => String::new(),
                        });
                        let cells = [String::new(), types[n].clone(), job.as_ref().and_then(|i| i.size).or(f.size).map(human).unwrap_or_default(), status, f.text.clone(), f.url.clone(), Path::new(&save_dir(name, &f.url)).join(name).display().to_string()];
                        let mut x = row.left();
                        for (ci, text) in cells.iter().enumerate() {
                            let w = COLS[ci].1 * dx;
                            let (tx, t) = if ci == 0 { (x + 42.0, name.clone()) } else { (x + 6.0, text.clone()) };
                            let clip = Rect::from_min_max(egui::pos2(x, row.top()), egui::pos2(x + w - 4.0, row.bottom()));
                            ui.painter().with_clip_rect(clip.intersect(ui.clip_rect())).text(egui::pos2(tx, row.center().y), Align2::LEFT_CENTER, t, font.clone(), ink);
                            x += w;
                        }
                    }
                });
            });
        });
        } else {
            let items: Vec<(String, Option<PathBuf>)> = {
                let s = c.state.lock().unwrap();
                rows.iter().map(|(k, name, _)| (name.clone(), jobs.get(&s.files[*k].url).map(|j| j.info().clone()).filter(|i| i.status == Status::Complete).map(|i| i.path()))).collect()
            };
            self.grab_thumbs(d.ui, lr.shrink(1.0), &items);
        }
        d.hline(7.0, 258.0, 494.0);
        // statics 1501-1506: five small sunken boxes along the bottom
        for k in 0..5 {
            let r = d.rect(18.0 + k as f32 * 20.0, 267.0, 11.0, 10.0);
            d.ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, Color32::from_gray(160)), egui::StrokeKind::Inside);
        }
        if d.button(250.0, 265.0, 60.0, 14.0, "< Back", true) {
            back = true;
        }
        d.button(316.0, 265.0, 60.0, 14.0, "Next >", false);
        if d.button(441.0, 265.0, 60.0, 14.0, "Close", true) {
            keep = false;
        }
        match action {
            Some(0) | Some(7) => again = true,
            Some(1) => c.stop.store(true, Relaxed),
            Some(k @ (2 | 3)) => c.state.lock().unwrap().files.iter_mut().for_each(|f| f.checked = k == 2),
            Some(4) => self.grab(c, opts, save_mode, save_cat, folder, true),
            Some(5) => {
                let ids: HashSet<u64> = jobs.values().map(|j| j.info().id).collect();
                self.core.grab_dl_ids.lock().unwrap().retain(|id| !ids.contains(id));
                jobs.values().filter(|j| j.running()).for_each(|j| j.stop());
            }
            Some(6) => self.grab(c, opts, save_mode, save_cat, folder, false),
            Some(8) => {
                let st = c.state.lock().unwrap();
                let proj = core::GrabProject { opts: opts.clone(), save_mode, save_cat: save_cat.into(), folder: folder.into(), files: st.files.clone(), pages: st.pages, ..Default::default() };
                drop(st);
                self.grab_schedule(proj, saved);
            }
            Some(9) => self.open(Dialog::GrabStats { crawl: c.clone() }),
            _ => {}
        }
        if !keep {
            c.stop.store(true, Relaxed);
        }
        (keep, back, again)
    }

    fn move_in_queue(&self, id: u64, dir: i32) {
        let mut jobs = self.core.jobs.lock().unwrap();
        let Some(q) = jobs.iter().find(|j| j.info().id == id).map(|j| j.info().queue) else { return };
        let queued: Vec<usize> = (0..jobs.len()).filter(|k| { let i = jobs[*k].info(); i.queued && i.queue == q }).collect();
        let Some(p) = queued.iter().position(|k| jobs[*k].info().id == id) else { return };
        let q = p as i32 + dir;
        if q >= 0 && (q as usize) < queued.len() {
            jobs.swap(queued[p], queued[q as usize]);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        no_zoom(ctx);
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if self.hwnd.load(Relaxed) == 0 {
            if let Ok(RawWindowHandle::Win32(h)) = frame.window_handle().map(|h| h.as_raw()) {
                self.hwnd.store(h.hwnd.get(), Relaxed);
                win::pump(h.hwnd.get());
                win::dark_title(h.hwnd.get(), self.dark);
                let c = ctx.clone();
                win::accept_drops(h.hwnd.get(), move || c.request_repaint());
            }
        }
        // Closing the window keeps NDM in the notification area; without a tray it minimizes while
        // something downloads, so the downloads aren't lost, and quits otherwise.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            let queues: Vec<u32> = self.core.settings().queues.iter().map(|q| q.id).collect();
            let busy = self.core.jobs().iter().any(|j| j.running()) || queues.iter().any(|q| self.core.queue_running(*q));
            match close_action(self.tray.is_some() || LINUX_TRAY.load(Relaxed), busy) {
                Close::Hide => {
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    win::hide(self.hwnd.load(Relaxed));
                    #[cfg(not(windows))]
                    hide_linux(ctx);
                    return;
                }
                Close::Minimize => {
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                    let shown = std::mem::replace(&mut self.core.settings().close_note_shown, true);
                    if !shown {
                        self.open(Dialog::Info { title: TITLE.into(), text: "New Download Manager keeps downloading while its window is minimized. To quit, use Tasks → Exit.".into() });
                    }
                }
                Close::Quit => {}
            }
        }
        if let Some((tray, limit, _)) = &self.tray {
            let on = self.core.settings().limit_on;
            if limit.is_checked() != on {
                limit.set_checked(on);
            }
            let total: f64 = self.core.jobs().iter().map(|j| j.speed()).sum();
            let _ = tray.set_tooltip(Some(if total > 0.0 { format!("{TITLE}\n{}", rate(total)) } else { TITLE.to_string() }));
        }
        // eframe shows the window after its first frame: started for the browser, it goes back to the tray
        static TRAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let h = self.hwnd.load(Relaxed);
        if tray_start() && win::visible(h) && !TRAYED.swap(true, Relaxed) {
            win::hide(h);
        }
        #[cfg(not(windows))]
        if LINUX_TRAY.load(Relaxed) {
            static TRAYED_LINUX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if tray_start() && !TRAYED_LINUX.swap(true, Relaxed) {
                hide_linux(ctx);
            }
            // the tray menu's Speed Limiter tick follows the setting (its item lives on the GTK thread)
            static LIMIT_SHOWN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
            let on = self.core.settings().limit_on;
            if LIMIT_SHOWN.swap(on as u8, Relaxed) != on as u8 {
                win::gtk_spawn(move || LIMIT_ITEM.with(|l| l.borrow().as_ref().map(|l| l.set_checked(on))).unwrap_or_default());
            }
        }
        // a link dropped on the drop target, or its double click, brings a hidden main window back
        #[cfg(not(windows))]
        if win::take_show() {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        while let Ok(inc) = self.rx.try_recv() {
            self.incoming(ctx, inc);
        }
        // links dropped on the drop target or the main window: as if added by hand (Add URL)
        for url in win::take_drops() {
            self.incoming(ctx, Incoming { url, force: true, ..Default::default() });
        }
        let want_help = HELP_REQ.lock().unwrap().take();
        if let Some(p) = want_help {
            let (f, a) = p.split_once('#').map_or((p.clone(), None), |(f, a)| (f.to_string(), Some(a.to_string())));
            match self.dialogs.iter_mut().find_map(|(_, d)| if let Dialog::Help { page, anchor, .. } = d { Some((page, anchor)) } else { None }) {
                Some((page, anchor)) => (*page, *anchor) = (f, a),
                None => self.open(Dialog::Help { page: f, anchor: a, back: vec![], fwd: vec![], search: String::new() }),
            }
        }
        // after the incoming downloads: their dialogs need frames while the main window is in the tray
        win::PUMP.store(!self.dialogs.is_empty(), Relaxed);
        let want = self.core.settings().drop_target;
        if want != self.drop_shown {
            // the drop target shows NDM's icon
            let px = app_icon();
            win::drop_target(want, &px);
            self.drop_shown = want;
        } else if want && !win::drop_target_visible() {
            // closed from its own right click
            self.core.settings().drop_target = false;
            self.drop_shown = false;
        }
        for j in self.core.jobs() {
            if j.just_completed.swap(false, Relaxed) {
                let id = j.info().id;
                self.dialogs.retain(|(_, d)| !matches!(d, Dialog::Progress { id: i, .. } if *i == id));
                let (exe, args) = { let s = self.core.settings(); (s.virus_exe.clone(), s.virus_args.clone()) };
                if !exe.is_empty() {
                    let mut c = std::process::Command::new(&exe);
                    win::clean_env(&mut c);
                    if let Err(e) = c.args(virus_args(&args, &j.info().path())).spawn() {
                        self.open(Dialog::Info { title: "Virus checking".into(), text: i18n::trf("Can not start virus checking program.\n%s", &[&e]) });
                    }
                }
                if self.core.settings().show_complete {
                    self.open(Dialog::Complete { id, dont_show: false });
                }
                if let Some((exit, off)) = self.on_done.remove(&id) {
                    if off {
                        // 60 s grace period; `shutdown /a` cancels it
                        #[cfg(windows)]
                        let _ = std::process::Command::new("shutdown").args(["/s", "/t", "60", "/c", "Downloads complete. The computer will shut down in 60 seconds."]).spawn();
                        #[cfg(not(windows))]
                        win::power_off(0, false);
                    }
                    if exit || off {
                        self.quitting = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
            }
        }

        let rows = self.rows();
        let visible: HashSet<u64> = rows.iter().map(|r| r.id).collect();
        self.sel.retain(|id| visible.contains(id));
        let sel: Vec<u64> = rows.iter().filter(|r| self.sel.contains(&r.id)).map(|r| r.id).collect();

        if !ctx.wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Delete)) && !sel.is_empty() {
                self.ask_delete(sel.clone());
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::A)) {
                self.sel = visible;
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
            self.open_find();
        }
        if ctx.input(|i| i.key_pressed(Key::F3)) {
            self.find_next();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::M)) && self.sel.len() == 1 {
            let id = *self.sel.iter().next().unwrap();
            self.move_rename(id);
        }

        // NDM's menu bar: 24 px tall, first item 3 px from the edge
        let menu_frame = egui::Frame::new().inner_margin(egui::Margin { left: 2, right: 2, top: 1, bottom: 0 }).fill(if self.dark { DARK_TREE } else { ctx.style().visuals.panel_fill });
        egui::TopBottomPanel::top("menu").frame(menu_frame).show(ctx, |ui| {
            if self.dark {
                ui.visuals_mut().widgets.hovered.weak_bg_fill = DARK_MENU_HOT;
                ui.visuals_mut().widgets.open.weak_bg_fill = DARK_MENU_HOT;
            }
            // right-to-left languages: menu bar, toolbar, panes and columns run from the right
            // mirrored window (egui's horizontal layouts follow the parent's direction)
            if i18n::is_rtl() {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| self.menu(ctx, ui, &sel));
            } else {
                self.menu(ctx, ui, &sel)
            }
        });
        // NDM's toolbar starts at the window edge; the tree pane is 145 px wide, 8 px in, the list 7 px right of
        // it (measured at 125%).
        let m = |l: f32, r: f32, t: f32, b: f32| egui::Margin { left: l as i8, right: r as i8, top: t as i8, bottom: b as i8 };
        let fill = ctx.style().visuals.panel_fill;
        egui::TopBottomPanel::top("toolbar").frame(egui::Frame::new().fill(fill).inner_margin(m(0.0, 0.0, 0.0, 3.0))).show_separator_line(false).show(ctx, |ui| self.toolbar(ui, &rows, &sel));
        let main_font = |ui: &mut egui::Ui| _ = ui.style_mut().text_styles.insert(egui::TextStyle::Body, FontId::proportional(10.0));
        if self.show_cats {
            let side = if i18n::is_rtl() { egui::SidePanel::right("cats") } else { egui::SidePanel::left("cats") };
            side.frame(egui::Frame::new().fill(fill).inner_margin(m(6.0, 0.0, 3.0, 3.0))).show_separator_line(false).resizable(true).default_width(122.0).min_width(80.0).show(ctx, |ui| {
                main_font(ui);
                self.categories(ui)
            });
        }
        let edge = if self.dark { Color32::from_gray(70) } else { Color32::from_gray(170) };
        let list = egui::Frame::new().fill(if self.dark { DARK_DLG } else { Color32::WHITE }).stroke(egui::Stroke::new(1.0_f32, edge)).inner_margin(2.0);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(fill).inner_margin(m(5.0, 6.0, 3.0, 3.0))).show(ctx, |ui| {
            let px = self.core.settings().list_font_px;
            ui.style_mut().text_styles.insert(egui::TextStyle::Body, FontId::new(px, egui::FontFamily::Name("list".into())));
            list.show(ui, |ui| self.table(ui, &rows));
        });

        let mut ds = std::mem::take(&mut self.dialogs);
        ds.retain_mut(|(key, d)| self.dialog(ctx, *key, d));
        ds.append(&mut self.dialogs);
        self.dialogs = ds;
        let open: HashSet<u64> = self.dialogs.iter().map(|(k, _)| *k).collect();
        self.dlg_pos.retain(|k, _| open.contains(k));
        if !self.opt_patch.is_empty() && !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::Options { .. })) {
            // Options was closed first: the change goes straight to the settings
            let mut s = self.core.settings().clone();
            self.opt_patch.drain(..).for_each(|f| f(&mut s));
            *self.core.settings() = s;
            self.core.save();
        }
        self.watch_clipboard();
        // Follow Windows' app mode (checked about every 2 s) when "Dark Mode support" is on.
        self.theme_check = self.theme_check.saturating_add(1);
        let mut retitle = false;
        if self.theme_check > 6 {
            self.theme_check = 0;
            let dark = self.core.settings().dark_support && win::system_dark();
            if dark != self.dark {
                self.dark = dark;
                style(ctx, dark);
                win::dark_title(self.hwnd.load(Relaxed), dark);
                retitle = true;
            }
        }
        let light = std::mem::take(&mut self.light_titles);
        win::own_dialogs(self.hwnd.load(Relaxed), self.dark, retitle, &light);
        if self.core.dial_text.lock().unwrap().is_some() && !self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::DialUp)) {
            self.open(Dialog::DialUp);
        }
        let quota_msg = self.core.quota_msg.lock().unwrap().take();
        if let Some(text) = quota_msg {
            self.open(Dialog::QuotaHit { text });
        }
        let finished: Vec<u32> = std::mem::take(&mut *self.core.queue_finished.lock().unwrap());
        let mut done: Vec<core::Sched> = finished.iter().filter_map(|q| self.core.settings().queue(*q).map(|q| q.sched.clone())).collect();
        done.append(&mut self.core.sched_done.lock().unwrap()); // scheduled grabber projects
        for s in done {
            if s.open_on && !s.open_file.trim().is_empty() {
                explore(s.open_file.trim());
            }
            if s.hangup {
                win::ras_hangup_all();
            }
            if s.off_done {
                win::power_off(s.off_kind, s.off_force);
            }
            // sleep and hibernate come back to a running app
            if s.exit_done || (s.off_done && s.off_kind == 0) {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }

        ctx.request_repaint_after(Duration::from_millis(300));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        win::exit_splash(&i18n::tr_os("Exit NDM"), &i18n::tr_os("Saving settings..."));
        self.core.shutdown();
    }
}

/// What the window's close button does.
#[derive(Debug, PartialEq)]
enum Close {
    Hide,
    Minimize,
    Quit,
}

/// To the tray when there is one; without one, minimized while downloads or queues run, else quit.
fn close_action(tray: bool, busy: bool) -> Close {
    match (tray, busy) {
        (true, _) => Close::Hide,
        (false, true) => Close::Minimize,
        (false, false) => Close::Quit,
    }
}

/// Hides the main window on Linux; Wayland can't hide a window, so there it is minimized.
#[cfg(not(windows))]
fn hide_linux(ctx: &egui::Context) {
    ctx.send_viewport_cmd(if win::prefer_x11() { ViewportCommand::Visible(false) } else { ViewportCommand::Minimized(true) });
}

/// NDM's windows are laid out in dialog units and break when zoomed, so the UI never zooms.
fn no_zoom(ctx: &egui::Context) {
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    if ctx.zoom_factor() != 1.0 {
        ctx.set_zoom_factor(1.0);
    }
}

/// Batch download's replacements for `*`: numbers `from..=to` padded to `width`, or letters `lf..=lt`
/// (one letter each, same case). Empty when the range is invalid.
fn batch_range(letters: bool, from: u32, to: u32, width: u32, lf: &str, lt: &str) -> Vec<String> {
    let one = |s: &str| {
        let mut c = s.trim().chars();
        c.next().filter(|x| x.is_ascii_alphabetic() && c.next().is_none())
    };
    match (letters, one(lf), one(lt)) {
        (false, ..) => (from..=to).map(|n| format!("{n:0w$}", w = width as usize)).collect(),
        (true, Some(a), Some(b)) if a.is_ascii_lowercase() == b.is_ascii_lowercase() => (a..=b).map(String::from).collect(),
        _ => vec![],
    }
}

/// Does download `r` match the search: each ticked target is compared as a
/// substring, or whole with flag 2; case-insensitive unless flag 1.
fn find_hit(flags: u32, needle: &str, r: &Row) -> bool {
    let up = |t: &str| if flags & 1 != 0 { t.to_string() } else { t.to_uppercase() };
    let n = up(needle);
    let hit = |t: &str| if flags & 2 != 0 { up(t) == n } else { up(t).contains(&n) };
    (flags & 4 != 0 && hit(&r.name)) || (flags & 8 != 0 && hit(&r.desc)) || (flags & 0x10 != 0 && [&r.url, &r.parent, &r.referrer].iter().any(|t| hit(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn folder_update_replaces_files() {
        let base = std::env::temp_dir().join(format!("ndm-test-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (old, src) = (base.join("ndm-1.0"), base.join("src/ndm-1.1"));
        for (d, v) in [(&old, "old"), (&src, "new")] {
            std::fs::create_dir_all(d.join("help")).unwrap();
            std::fs::write(d.join("ndm"), v).unwrap();
            std::fs::write(d.join("help/index.htm"), v).unwrap();
        }
        std::fs::write(old.join("help/gone.htm"), "old").unwrap();
        std::fs::write(old.join("state-like.json"), "kept").unwrap();
        let tgz = base.join("u.tar.gz");
        assert!(std::process::Command::new("tar").arg("-czf").arg(&tgz).arg("-C").arg(base.join("src")).arg("ndm-1.1").status().unwrap().success());
        update_folder(&std::fs::read(&tgz).unwrap(), &old).unwrap();
        let read = |p: &str| std::fs::read_to_string(old.join(p)).unwrap();
        assert_eq!((read("ndm").as_str(), read("help/index.htm").as_str(), read("state-like.json").as_str()), ("new", "new", "kept"));
        assert!(!old.join("help/gone.htm").exists(), "folders are replaced whole");
        assert!(update_folder(b"\x1f\x8bnot really", &old).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn close_rules() {
        assert_eq!(close_action(true, false), Close::Hide);
        assert_eq!(close_action(true, true), Close::Hide);
        assert_eq!(close_action(false, true), Close::Minimize);
        assert_eq!(close_action(false, false), Close::Quit);
    }

    #[test]
    fn ui_never_zooms() {
        let ctx = egui::Context::default();
        let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, 5.0), modifiers: egui::Modifiers::COMMAND };
        let input = egui::RawInput { modifiers: egui::Modifiers::COMMAND, events: vec![key(Key::Plus), key(Key::Equals), wheel, egui::Event::Zoom(1.5)], ..Default::default() };
        for _ in 0..3 {
            let _ = ctx.run(input.clone(), no_zoom);
        }
        assert_eq!(ctx.zoom_factor(), 1.0);
    }

    #[test]
    fn sched_times() {
        let hm = |s: &str| sched_time(s).map(|t| t.format("%H:%M").to_string());
        assert_eq!(hm("11:00:00 PM").as_deref(), Some("23:00"));
        assert_eq!(hm("7:30 am").as_deref(), Some("07:30"));
        assert_eq!(hm("02:30").as_deref(), Some("02:30"));
        assert_eq!(hm("25:00"), None);
    }

    #[test]
    fn find() {
        let mut r = Row::default();
        (r.name, r.desc, r.url) = ("setup.exe".into(), "tool".into(), "http://ex.com/s".into());
        assert!(find_hit(4, "setup", &r));
        assert!(!find_hit(4 | 1, "SETUP", &r) && find_hit(4, "SETUP", &r));
        assert!(!find_hit(4 | 2, "setup", &r) && find_hit(4 | 2, "setup.exe", &r));
        assert!(!find_hit(8, "setup", &r) && find_hit(8, "TOOL", &r));
        assert!(find_hit(0x10, "ex.com", &r) && !find_hit(4, "ex.com", &r));
    }

    #[test]
    fn login_fields() {
        let (u, l, p) = split_login("http://me:p%20w@h.com/a.zip");
        assert_eq!((u.as_str(), l.as_str(), p.as_str()), ("http://h.com/a.zip", "me", "p w"));
        assert_eq!(join_login(&u, &l, &p), "http://me:p%20w@h.com/a.zip");
        assert_eq!(join_login("http://h.com/a.zip", "", "x"), "http://h.com/a.zip");
        assert_eq!(split_login("not a url").0, "not a url");
    }
}

#[test]
fn batch_ranges() {
    assert_eq!(batch_range(false, 8, 10, 3, "", ""), ["008", "009", "010"]);
    assert_eq!(batch_range(true, 0, 0, 1, "x", "z"), ["x", "y", "z"]);
    assert!(batch_range(true, 0, 0, 1, "a", "Z").is_empty());
    assert!(batch_range(false, 5, 1, 1, "", "").is_empty());
}
