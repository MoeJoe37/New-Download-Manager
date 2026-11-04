mod dialogs;
mod help;
pub(crate) mod resources;
mod window;

use self::resources::{app_icon, DARK_DLG, help_logo, tray_picture, texture, Toolbar, TB_ICONS, TB_BUTTONS, TB_BUILTIN, toolbars_dir, toolbar_sets, cat_icon_sets, cat_icon_files, tb_picture, ALL_CJK, LIST_FONT, style, Res, segoe, DARK_TREE, DARK_MENU_HOT, BAR};
use self::window::table::{Col, COLS, Row};
use self::window::categories::{Which, Cat};
use self::window::menu::{menu_layout};

use crate::i18n::{tr, tr_os};
use crate::startup::{tray_start, PAINTED, TITLE};
use crate::{core, dlu, engine, grabber, i18n, net, stream, win, ytdlp};
#[cfg(target_os = "macos")]
use crate::mac_capture;
use self::dialogs::{Dialog, PlaylistPicker, ProbeSlot};
use self::help::{help, help_body, take_help_request};
use crate::core::{Core, Incoming, Settings, GENERAL};
use crate::diagnostics::{crash_diagnostics, issue_text};
use crate::update::{check_update, install_update, Release, UPDATE_REPO};
use crate::engine::{Info, Probe, Status};
use crate::file_ops::copy_dir;
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

/// The Linux tray icon is up (its GTK thread owns it): closing the window hides it instead of quitting.
static LINUX_TRAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The main window's X11 window (0 on Wayland, or before the first frame).
#[cfg(not(windows))]
static MAIN_XID: AtomicIsize = AtomicIsize::new(0);
// The Linux tray's Speed Limiter tick and its other items with their English text, on the GTK thread.
#[cfg(target_os = "linux")]
thread_local!(static LIMIT_ITEM: std::cell::RefCell<Option<(CheckMenuItem, Vec<(MenuItem, &'static str)>)>> = const { std::cell::RefCell::new(None) });

// ---------- formatting ----------

fn human(n: u64) -> String {
    i18n::line(&human_os(n))
}

/// `human` in reading order, for a line it is part of.
fn human_os(n: u64) -> String {
    let n = n as f64;
    match n {
        _ if n < 1024.0 => format!("{n} {}", tr_os("Bytes")),
        _ if n < 1024.0 * 1024.0 => format!("{:.2} {}", n / 1024.0, tr_os("KB")),
        _ if n < 1024.0 * 1024.0 * 1024.0 => format!("{:.2} {}", n / 1048576.0, tr_os("MB")),
        _ => format!("{:.2} {}", n / 1073741824.0, tr_os("GB")),
    }
}

fn rate(bps: f64) -> String {
    if bps <= 0.0 { String::new() } else { i18n::line(&format!("{}/{}", human_os(bps as u64), tr_os("sec"))) }
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
    i18n::line(&size3_os(n))
}

/// `size3` in reading order, for a line it is part of.
fn size3_os(n: u64) -> String {
    let (b, kb, mb, gb) = (n % 1024, n / 1024 % 1024, n / 1_048_576 % 1024, n / 1_073_741_824);
    let (whole, frac, unit) = if gb == 0 && mb < 1000 {
        if mb == 0 && kb < 1000 {
            if kb == 0 {
                return format!("{b}  {}", tr_os("Bytes"));
            }
            (kb, b * 1000 / 1024, "KB")
        } else {
            (mb, kb * 1000 / 1024, "MB")
        }
    } else {
        (gb, mb * 1000 / 1024, "GB")
    };
    format!("{whole}{}{frac:03}  {}", win::decimal_sep(), tr_os(unit))
}

/// A percentage as NDM prints it: whole part, separator, two truncated decimals.
fn pct2(p: f64) -> String {
    let whole = p.trunc();
    format!("{}{}{:02}", whole as u64, win::decimal_sep(), ((p - whole) * 100.0).trunc() as u64)
}

/// A date and time as NDM's English prints it, or in the language file's format (its "%b %d %H:%M:%S %Y"
/// line: the languages shipped with NDM use numbers rather than English month names).
fn date(ts: i64) -> String {
    use chrono::TimeZone;
    const EN: &str = "%b %d %H:%M:%S %Y";
    if ts == 0 {
        return String::new();
    }
    // a format chrono can't read would panic when printed
    let f = Some(tr_os(EN)).filter(|f| !chrono::format::StrftimeItems::new(f).any(|i| matches!(i, chrono::format::Item::Error))).unwrap_or_else(|| EN.into());
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.format(&f).to_string()).unwrap_or_default()
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

/// The browser-extension window's instructions (English keys): Firefox's temporary add-on, the other
/// browsers' unpacked extension.
const GUIDES: [&str; 2] = [
    "To capture downloads from Firefox, open its extensions page (about:debugging), click \"This Firefox\", press \"Load Temporary Add-on...\" and select manifest.json in this folder. Firefox removes the add-on when it closes, so load it again after a restart:",
    "To capture downloads from this browser, open its extensions page, turn on \"Developer mode\", press \"Load unpacked\" and select this folder:",
];

fn integration(b: &core::Browser) -> &'static str {
    if extension_page(b).is_some() { "Extension" } else { "Not supported" }
}

/// RichText keeps the filename's already-shaped text out of the translation and bidi pass in D::label.
fn file_info_name(name: &str) -> RichText {
    RichText::new(i18n::shown(name)).strong()
}

#[test]
fn file_info_renders_both_logical_hebrew_names() {
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400.0, 200.0))),
        ..Default::default()
    }, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut d = dlu::D::new(ui);
            d.label(7.0, 7.0, 200.0, 12.0, file_info_name("שלום"));
            d.label(7.0, 24.0, 200.0, 12.0, file_info_name("םולש"));
        });
    });
    let rendered: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Text(text) => Some(text.galley.text()),
        _ => None,
    }).collect();
    assert_eq!(rendered, ["םולש", "שלום"]);
}

/// The rows File Info adds: the name as it reads (`i18n::shown`) when the edit box can't show it so
/// (right-to-left letters), and a video page's Quality, Subtitles and Thumbnail.
fn file_info_rows(info: &Info, path: &str) -> (f32, f32) {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let shown = if i18n::shown(&name) != name { 12.0 } else { 0.0 };
    let video = if info.ytdlp || ytdlp::is_video_page(&info.url) { 17.0 } else { 0.0 };
    (shown, video)
}

/// File Info's Quality choices and the `YtOpts::quality` each stands for.
const QUALITIES: [(&str, &str); 9] = [
    ("Best", ""),
    ("2160p", "2160"),
    ("1440p", "1440"),
    ("1080p", "1080"),
    ("720p", "720"),
    ("480p", "480"),
    ("360p", "360"),
    ("Audio only (M4A)", "m4a"),
    ("Audio only (MP3)", "mp3"),
];

/// Stop an early download and delete its partial data.
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

/// The file's folder with the file selected (Explorer, Finder, the Linux file manager); its folder when the
/// file isn't there (yet).
fn open_folder_of(path: &Path) {
    if path.is_file() {
        win::show_in_folder(path);
    } else if let Some(p) = path.ancestors().skip(1).find(|p| p.is_dir()) {
        explore(p);
    }
}

/// The unpacked browser extension: next to the exe or in one of its parent folders (cargo builds).
fn extension_dir() -> Option<PathBuf> {
    let bundled = i18n::beside_exe("extension", "manifest.json")?;
    // an AppImage, and a Mac app (whose bundle the update replaces), give browsers a copy that stays put
    if std::env::var_os("APPIMAGE").is_none() && !cfg!(target_os = "macos") {
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

/// The height (DLU) of a message window's text: room for about 55 characters a line, at least the 38 of NDM's
/// own message windows (translations run longer, so the translated text is measured).
fn info_height(text: &str) -> f32 {
    let lines: usize = tr(text).split('\n').map(|l| l.chars().count() / 55 + 1).sum();
    (lines as f32 * 9.0).max(38.0)
}

/// Tasks -> Export extension...: a copy of NDM's browser extension in a folder the user picks, to load in any
/// browser (its extensions page, "Load unpacked"; Firefox: about:debugging), with the steps in a message.
impl App {
    fn export_extension(&mut self) {
        let Some(src) = extension_dir() else {
            return self.open(Dialog::Info { title: "Export extension".into(), text: "The browser extension isn't in NDM's folder: install NDM again to get it.".into() });
        };
        let Some(to) = rfd::FileDialog::new().set_title(tr_os("Export extension")).pick_folder() else { return };
        let dest = to.join("NDM extension");
        let text = match copy_dir(&src, &dest) {
            Ok(()) => {
                explore(&dest);
                i18n::trf_os("The browser extension is in this folder:\n%s\n\nChrome, Edge, Brave, Opera, Vivaldi: open the browser's extensions page, turn on \"Developer mode\", press \"Load unpacked\" and select the folder.\nFirefox: open about:debugging, click \"This Firefox\", press \"Load Temporary Add-on...\" and select manifest.json in the folder.", &[&i18n::ltr(&dest.display().to_string())])
            }
            Err(e) => i18n::trf_os("The extension couldn't be saved there: %s", &[&e]),
        };
        self.open(Dialog::Info { title: "Export extension".into(), text });
    }
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

/// Options -> Downloads "If a duplicate download link is added:" in NDM's combo order.
const DUPL: [&str; 4] = [
    "Show a dialog and ask what to do.",
    "Add the duplicate with a numbered file name",
    "Add the duplicate and overwrite the existing file",
    "If existing file is complete, show download complete dialog; otherwise resume it.",
];

// ---------- dialogs ----------

pub(crate) struct App {
    core: Arc<Core>,
    rx: Receiver<Incoming>,
    cat: Cat,
    sel: HashSet<u64>,
    anchor: Option<u64>,
    sort: (Col, bool),
    /// Titles of this frame's dialogs that NDM keeps light in dark mode (their title bars).
    light_titles: Vec<String>,
    /// The filter box over the download list: rows whose name, address or description has this text.
    filter: String,
    /// Find Next starts at this row of the list, and the row to scroll to.
    find_pos: usize,
    scroll_to: Option<usize>,
    show_cats: bool,
    dialogs: Vec<(u64, Dialog)>,
    /// Where each open dialog first appeared: centred on the main window.
    dlg_pos: HashMap<u64, egui::Pos2>,
    serial: u64,
    quitting: bool,
    /// Closing for an update: the setup's own window shows it, so no "Exit NDM" note.
    updating: bool,
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
    /// Custom category icons loaded so far: (set, file name) -> picture (None: the set has no such file).
    cat_pics: HashMap<(String, String), Option<egui::TextureHandle>>,
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
    /// What the toolbar's pictures were made for: (set, NDM Style, scale %).
    tb_key: (String, bool, u32),
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
        // the file's start at the right in right-to-left languages, as the progress bar fills
        let rtl = i18n::is_rtl();
        let x = |b: u64| if rtl { inner.right() - inner.width() * (b as f32 / size as f32) } else { inner.left() + inner.width() * (b as f32 / size as f32) };
        let span = |t: egui::TextureId, a: f32, b: f32| col(t, a.min(b), a.max(b));
        let dir = if rtl { -px } else { px };
        for s in &info.segs {
            let done = if info.status == Status::Complete { s.stop.unwrap_or(s.next) } else { s.next };
            if done > s.start {
                span(blue, x(s.start) + dir, x(done));
            }
            span(red, x(s.start), x(s.start) + dir);
        }
        return;
    }
    p.rect_filled(rect, 0.0, c.field);
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, c.edge), egui::StrokeKind::Inside);
    let Some(size) = info.size.filter(|s| *s > 0) else { return };
    let inner = rect.shrink(1.0);
    let rtl = i18n::is_rtl();
    let x = |b: u64| if rtl { inner.right() - inner.width() * (b as f32 / size as f32) } else { inner.left() + inner.width() * (b as f32 / size as f32) };
    for s in &info.segs {
        let done = if info.status == Status::Complete { s.stop.unwrap_or(s.next) } else { s.next };
        let (a, b) = (x(s.start), x(done));
        p.rect_filled(egui::Rect::from_x_y_ranges(a.min(b)..=a.max(b), inner.y_range()), 0.0, BAR);
    }
}

impl App {
    pub(crate) fn new(ctx: &egui::Context) -> App {
        let core = Core::load();
        #[cfg(not(target_os = "macos"))]
        {
            let on = core.settings().adv_integration;
            core.apply_integration(on);
        }
        let dark =core.settings().dark_support && win::system_dark();
        *LIST_FONT.lock().unwrap() = core.settings().list_font.clone();
        i18n::load(&core.settings().language.clone());
        dlu::NDM_STYLE.store(core.settings().ndm_style, Relaxed);
        style(ctx, dark);
        let hwnd = Arc::new(AtomicIsize::new(0));
        let (tx, rx) = channel();
        if let Some(inc) = core::cli_args(&std::env::args().skip(1).collect::<Vec<_>>()) {
            let _ = tx.send(inc);
        }
        let c = ctx.clone();
        core.spawn_background(move || c.request_repaint());
        // NDM's own yt-dlp (Downloads -> Set yt-dlp) stays up to date
        ytdlp::keep_updated(core.clone());
        let (c, h) = (ctx.clone(), hwnd.clone());
        core.spawn_server(tx, move || {
            // only the download's dialog shows; the main window stays in the tray (win::pump)
            win::wake(h.load(Relaxed));
            c.request_repaint();
        });
        let tray = App::tray(ctx, &core, &hwnd);
        let mut app = Self::build(ctx, core, hwnd, rx, tray, dark);
        #[cfg(target_os = "macos")]
        if let Err(error) = mac_capture::restore() {
            app.open(Dialog::Info { title: "Restore macOS web proxy".into(), text: error });
        }
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

    fn build(
        ctx: &egui::Context, core: Arc<Core>, hwnd: Arc<AtomicIsize>, rx: Receiver<Incoming>,
        tray: Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)>, dark: bool,
    ) -> App {
        let expanded = core.settings().expanded.iter().cloned().collect();
        App {
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
            cat_pics: HashMap::new(),
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
            filter: String::new(),
            find_pos: 0,
            scroll_to: None,
            show_cats: true,
            dialogs: vec![],
            dlg_pos: HashMap::new(),
            serial: 0,
            quitting: false,
            updating: false,
            toolbar: None,
            tb_key: (String::new(), false, 0),
            icons: HashMap::new(),
            hwnd,
            tray,
            on_done: HashMap::new(),
            res: Res::load(ctx),
            dark,
            theme_check: 0,
            langs: i18n::list(),
        }
    }

    /// Linux: an AppIndicator on the GTK thread (tray-icon needs GTK's loop and keeps its items there), only
    /// where the desktop shows one; else closing quits. Sets LINUX_TRAY; the items stay on that thread
    /// (`LIMIT_ITEM`, for the Speed Limiter tick and a language change).
    #[cfg(target_os = "linux")]
    fn tray(ctx: &egui::Context, core: &Arc<Core>, _: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let style = core.settings().tray_icon;
        // without the indicator library tray-icon panics on the GTK thread, which aborts NDM
        if style == 3 || !win::tray_host() || !win::indicator_lib() {
            return None;
        }
        let (core, ctx) = (core.clone(), ctx.clone());
        win::gtk_spawn(move || {
            // the desktop lays the menu out itself (right-to-left languages too)
            let open = MenuItem::new(i18n::tr_os("Open"), true, None);
            let startq = MenuItem::new(i18n::tr_os("Start queue"), true, None);
            let stopq = MenuItem::new(i18n::tr_os("Stop queue"), true, None);
            let limit = CheckMenuItem::new(i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
            let exit = MenuItem::new(i18n::tr_os("Exit"), true, None);
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
            let items = vec![(open, "Open"), (startq, "Start queue"), (stopq, "Stop queue"), (exit, "Exit")];
            LIMIT_ITEM.with(|l| *l.borrow_mut() = Some((limit, items)));
            std::mem::forget(tray); // lives as long as the app
        });
        None
    }

    /// macOS: NDM's item in the menu bar, made on the main thread like the window; its menu shows on a click.
    /// The window is hidden and shown with egui's viewport commands.
    #[cfg(target_os = "macos")]
    fn tray(ctx: &egui::Context, core: &Arc<Core>, _: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let open = MenuItem::new(i18n::tr_os("Open"), true, None);
        let startq = MenuItem::new(i18n::tr_os("Start queue"), true, None);
        let stopq = MenuItem::new(i18n::tr_os("Stop queue"), true, None);
        let limit = CheckMenuItem::new(i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
        let exit = MenuItem::new(i18n::tr_os("Exit"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &startq, &stopq, &limit, &PredefinedMenuItem::separator(), &exit]).ok()?;
        let style = core.settings().tray_icon;
        if style == 3 {
            return None;
        }
        let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(TITLE).with_icon(tray_picture(style)?).build().map_err(|e| eprintln!("tray: {e}")).ok()?;
        let ids = (open.id().clone(), startq.id().clone(), stopq.id().clone(), limit.id().clone(), exit.id().clone());
        let (core2, ctx2) = (core.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let (open, startq, stopq, limit, exit) = &ids;
            if e.id == *open {
                ctx2.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx2.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx2.send_viewport_cmd(ViewportCommand::Focus);
            } else if e.id == *startq {
                core2.start_queue(core::MAIN_QUEUE);
            } else if e.id == *stopq {
                core2.stop_queue(core::MAIN_QUEUE);
            } else if e.id == *limit {
                let on = !core2.settings().limit_on;
                core2.settings().limit_on = on;
                core2.apply_settings();
            } else if e.id == *exit {
                core2.shutdown();
                std::process::exit(0);
            }
            ctx2.request_repaint();
        }));
        let items = vec![(open, "Open"), (startq, "Start queue"), (stopq, "Stop queue"), (exit, "Exit")];
        Some((tray, limit, items))
    }

    /// Notification-area icon. Its handlers run on the UI thread's message loop even while
    /// the window is hidden, so they act on Core and the HWND directly.
    #[cfg(windows)]
    fn tray(ctx: &egui::Context, core: &Arc<Core>, hwnd: &Arc<AtomicIsize>) -> Option<(TrayIcon, CheckMenuItem, Vec<(MenuItem, &'static str)>)> {
        let open = MenuItem::new(i18n::tr_os("Open"), true, None);
        let startq = MenuItem::new(i18n::tr_os("Start queue"), true, None);
        let stopq = MenuItem::new(i18n::tr_os("Stop queue"), true, None);
        let limit = CheckMenuItem::new(i18n::tr_os("Speed Limiter"), true, core.settings().limit_on, None);
        let exit = MenuItem::new(i18n::tr_os("Exit"), true, None);
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

    fn add_url(&mut self) {
        let url = links(&clipboard()).into_iter().next().unwrap_or_default();
        self.open(Dialog::AddUrl { url, auth: false, login: String::new(), pass: String::new(), clip: false });
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

    /// Downloads -> Set yt-dlp: NDM's own copy, downloaded (or brought up to date) on a thread.
    fn set_ytdlp(&mut self, ctx: &egui::Context) {
        if self.dialogs.iter().any(|(_, d)| matches!(d, Dialog::YtDlp { .. })) {
            return;
        }
        let (got, total, done) = (Arc::new(std::sync::atomic::AtomicU64::new(0)), Arc::new(std::sync::atomic::AtomicU64::new(0)), Arc::new(Mutex::new(None)));
        let (agent, g, t, d, c) = (self.core.agent("https://github.com/"), got.clone(), total.clone(), done.clone(), ctx.clone());
        let core = self.core.clone();
        std::thread::spawn(move || {
            let r = ytdlp::install(&agent, &g, &t, true);
            if r.is_ok() {
                core.settings().ytdlp_checked = chrono::Local::now().timestamp();
            }
            *d.lock().unwrap() = Some(r);
            c.request_repaint();
        });
        self.open(Dialog::YtDlp { got, total, done });
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
            i.ytdlp = inc.video;
            i.stream = inc.stream;
            i.media_output = inc.output.clone();
            i.media_variant = inc.variant;
            (i.referrer, i.cookies, i.browser_proxy) = (inc.referrer.clone(), inc.cookies.clone(), inc.proxy.clone());
            if !inc.folder.is_empty() {
                i.folder = inc.folder.clone();
            }
            i.name = inc.filename.clone();
            // Options -> Downloads "Do not start downloading, only add files to the queue": a download
            // handed over without questions goes to the main queue instead of starting
            let later = inc.later || (!inc.cli && self.core.settings().only_queue);
            i.queued = later;
            let id = i.id;
            let job = self.core.add(i);
            if !later {
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
        // a video page (YouTube..., or any page from the download panel): the Video category until yt-dlp
        // names the file from the video's title
        info.ytdlp = inc.video;
        info.stream = inc.stream;
        info.media_output = inc.output.clone();
        info.media_variant = inc.variant;
        let video_page = info.ytdlp || ytdlp::is_video_page(&info.url);
        let name = if !hint.is_empty() {
            hint.clone()
        } else if video_page {
            "video.mp4".into()
        } else {
            engine::sanitize(&engine::url_name(&info.url))
        };
        if video_page {
            info.category = self.core.settings().category_for(&name, &info.url);
            info.folder = self.core.settings().folder_for(&info.category);
        }
        if !inc.folder.is_empty() {
            info.folder = inc.folder.clone();
        }
        let path = Path::new(&info.folder).join(&name).display().to_string();
        let at_once = {
            let settings = self.core.settings();
            settings.start_at_once && !settings.only_queue && !video_page
        };
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
                let found = if video_page { ytdlp::probe(&agent, &url, &r, &k) } else { engine::probe(&agent, &url, &r, &k) };
                // a web page from any of the sites yt-dlp knows (well beyond the ones NDM names): its video
                let found = match found {
                    Ok(p) if p.page => ytdlp::probe(&agent, &url, &r, &k).or(Ok(p)),
                    // Some extractors can read a page whose ordinary HTTP request is rejected.
                    Err(e) if !video_page && (url.starts_with("https://") || url.starts_with("http://")) => ytdlp::probe(&agent, &url, &r, &k).map_err(|_| e),
                    other => other,
                };
                *s.lock().unwrap() = Some(found);
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
        let Some(p) = rfd::FileDialog::new().add_filter(tr_os(name), &[ext]).pick_file() else { return };
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
        self.open(Dialog::Options { tab, s: Box::new(s) });
    }

    fn new_queue(&mut self, files: Vec<u64>) {
        let n = self.core.settings().queues.iter().map(|q| q.id).max().unwrap_or(0) + 1;
        self.open(Dialog::QueueName { name: i18n::trf("Queue # %ld", &[&n]), files });
    }

    fn open_scheduler(&mut self, tab: u8) {
        let s = self.core.settings().clone();
        let queue = s.queues[0].clone();
        self.open(Dialog::Scheduler { tab, queue, qsel: None, error: String::new() });
    }

    // ---------- dialogs ----------

    /// The next tip (MFC GetNextTipString: wraps at the end), or NDM's message when there is no tips file.
    fn next_tip(&self) -> String {
        let tips = i18n::tips();
        if tips.is_empty() {
            return tr_os("Tips file does not exist in the prescribed directory");
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

    /// The grabber's menu bar (in a `menu_layout` Ui).
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
            Some(f) => i18n::line(&format!("{}  ( {} )", tr_os(f.0), f.1)),
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
            let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(full.min, Vec2::new(full.width(), GRAB_MENU))).layout(menu_layout()));
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
        nodes.push((1, 4, fold, i18n::line(&format!("{} {}", i18n::ltr(&format!("*.{domain}")), tr_os("sites")))));
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
        let types: Vec<String> = rows.iter().map(|r| self.type_names.entry(r.2.clone()).or_insert_with(|| i18n::line(&win::type_name(&r.2))).clone()).collect();
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
        PAINTED.store(true, Relaxed);
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
        // Linux and macOS: links dropped on the main window (X11; Wayland's own windows take none)
        #[cfg(not(windows))]
        {
            static XDND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !XDND.load(Relaxed) {
                if let Ok(raw) = frame.window_handle().map(|h| h.as_raw()) {
                    XDND.store(true, Relaxed);
                    let xid = match raw {
                        RawWindowHandle::Xlib(h) => h.window as isize,
                        RawWindowHandle::Xcb(h) => h.window.get() as isize,
                        // macOS: the window's view; links dropped on it come through mac::accept_drops
                        #[cfg(target_os = "macos")]
                        RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as isize,
                        _ => 0,
                    };
                    MAIN_XID.store(xid, Relaxed);
                    let c = ctx.clone();
                    win::accept_drops(xid, move || c.request_repaint());
                }
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
                    hide_main(ctx);
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
        // eframe shows the main window after its first frame whatever NDM asks. Started for the browser,
        // that show is held back (Windows) or made minimized (X11) by `keep_hidden`, then the window goes to
        // the tray, so only the download's dialog comes up.
        static FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        static TRAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let n = FRAME.fetch_add(1, Relaxed);
        let h = self.hwnd.load(Relaxed);
        #[cfg(windows)]
        let native = h;
        #[cfg(not(windows))]
        let native = MAIN_XID.load(Relaxed);
        if tray_start() && !TRAYED.load(Relaxed) {
            if n == 0 {
                win::keep_hidden(native, true);
                // the next frame comes even though the window stays hidden
                win::wake(h);
                ctx.request_repaint();
            } else {
                #[cfg(windows)]
                {
                    win::hide(h);
                    TRAYED.store(true, Relaxed);
                }
                // Linux: to the tray once it is up (it starts on its own thread); without one it stays minimized
                #[cfg(target_os = "linux")]
                {
                    let tray = LINUX_TRAY.load(Relaxed);
                    if tray {
                        hide_main(ctx);
                    }
                    if tray || n > 20 {
                        win::keep_hidden(native, false);
                        TRAYED.store(true, Relaxed);
                    } else {
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                }
                // macOS: to the menu bar item
                #[cfg(target_os = "macos")]
                {
                    if self.tray.is_some() {
                        hide_main(ctx);
                    }
                    TRAYED.store(true, Relaxed);
                }
            }
        }
        #[cfg(target_os = "linux")]
        if LINUX_TRAY.load(Relaxed) {
            // the tray menu's Speed Limiter tick follows the setting (its item lives on the GTK thread)
            static LIMIT_SHOWN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
            let on = self.core.settings().limit_on;
            if LIMIT_SHOWN.swap(on as u8, Relaxed) != on as u8 {
                win::gtk_spawn(move || LIMIT_ITEM.with(|l| l.borrow().as_ref().map(|(l, _)| l.set_checked(on))).unwrap_or_default());
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
        let want_help = take_help_request();
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
                // a file whose name didn't say what it is: named and filed by its contents
                if self.core.classify(&j) {
                    self.core.save();
                }
                let id = j.info().id;
                self.dialogs.retain(|(_, d)| !matches!(d, Dialog::Progress { id: i, .. } if *i == id));
                let (exe, args) = { let s = self.core.settings(); (s.virus_exe.clone(), s.virus_args.clone()) };
                if !exe.is_empty() {
                    let mut c = std::process::Command::new(&exe);
                    win::clean_env(&mut c);
                    if let Err(e) = c.args(virus_args(&args, &j.info().path())).spawn() {
                        self.open(Dialog::Info { title: "Virus checking".into(), text: i18n::trf_os("Can not start virus checking program.\n%s", &[&e]) });
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
        let nt = dlu::ndm().then(|| dlu::theme(self.dark));
        let menu_frame = match &nt {
            Some(t) => egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 3, bottom: 2 }).fill(t.surface),
            None => egui::Frame::new().inner_margin(egui::Margin { left: 2, right: 2, top: 1, bottom: 0 }).fill(if self.dark { DARK_TREE } else { ctx.style().visuals.panel_fill }),
        };
        egui::TopBottomPanel::top("menu").frame(menu_frame).show_separator_line(false).show(ctx, |ui| {
            if self.dark && nt.is_none() {
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
        let tb_frame = match &nt {
            Some(t) => egui::Frame::new().fill(t.surface).inner_margin(m(6.0, 6.0, 2.0, 4.0)),
            None => egui::Frame::new().fill(fill).inner_margin(m(0.0, 0.0, 0.0, 3.0)),
        };
        let tb = egui::TopBottomPanel::top("toolbar").frame(tb_frame).show_separator_line(false).show(ctx, |ui| self.toolbar(ui, &rows, &sel));
        if let Some(t) = &nt {
            // one hairline under the menu and toolbar
            let y = tb.response.rect.bottom() - 0.5;
            ctx.layer_painter(egui::LayerId::background()).hline(ctx.screen_rect().x_range(), y, egui::Stroke::new(1.0_f32, t.border));
        }
        if let Some(t) = &nt {
            self.status_bar(ctx, t, &rows);
        }
        let main_font = |ui: &mut egui::Ui| _ = ui.style_mut().text_styles.insert(egui::TextStyle::Body, FontId::proportional(10.0));
        if self.show_cats {
            let side = if i18n::is_rtl() { egui::SidePanel::right("cats") } else { egui::SidePanel::left("cats") };
            let side_frame = match &nt {
                Some(t) => egui::Frame::new().fill(t.surface).inner_margin(m(6.0, 6.0, 6.0, 6.0)),
                None => egui::Frame::new().fill(fill).inner_margin(m(6.0, 0.0, 3.0, 3.0)),
            };
            let sp = side.frame(side_frame).show_separator_line(false).resizable(true).default_width(if nt.is_some() { 170.0 } else { 122.0 }).min_width(80.0).show(ctx, |ui| {
                main_font(ui);
                self.categories(ui)
            });
            if let Some(t) = &nt {
                let r = sp.response.rect;
                let x = if i18n::is_rtl() { r.left() + 0.5 } else { r.right() - 0.5 };
                ctx.layer_painter(egui::LayerId::background()).vline(x, r.y_range(), egui::Stroke::new(1.0_f32, t.border));
            }
        }
        let list = match &nt {
            Some(t) => egui::Frame::new().fill(t.surface).stroke(egui::Stroke::new(1.0_f32, t.border)).corner_radius(dlu::R_CARD + 1.0).inner_margin(egui::Margin { left: 2, right: 2, top: 2, bottom: 6 }),
            None => {
                let edge = if self.dark { Color32::from_gray(70) } else { Color32::from_gray(170) };
                egui::Frame::new().fill(if self.dark { DARK_DLG } else { Color32::WHITE }).stroke(egui::Stroke::new(1.0_f32, edge)).inner_margin(2.0)
            }
        };
        let central = if nt.is_some() { egui::Frame::new().fill(fill).inner_margin(m(10.0, 10.0, 10.0, 8.0)) } else { egui::Frame::new().fill(fill).inner_margin(m(5.0, 6.0, 3.0, 3.0)) };
        egui::CentralPanel::default().frame(central).show(ctx, |ui| {
            // View -> Custom scale: the list's text
            let px = { let s = self.core.settings(); s.list_font_px * s.scale_text };
            // the filter box: the list narrows to the downloads with the text in their name, address or description
            ui.with_layout(menu_layout(), |ui| {
                ui.add_space(if dlu::ndm() { 6.0 } else { 2.0 });
                let edit = egui::TextEdit::singleline(&mut self.filter).hint_text(tr("Filter the list")).desired_width(240.0);
                let r = ui.add(edit);
                if !self.filter.is_empty() && (ui.small_button("\u{2716}").on_hover_text(tr("Clear")).clicked() || (r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)))) {
                    self.filter.clear();
                }
            });
            ui.add_space(3.0);
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
        if !self.updating {
            win::exit_splash(&i18n::tr_os("Exit NDM"), &i18n::tr_os("Saving settings..."));
        }
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

/// Hides the main window on Linux and macOS; Wayland can't hide a window, so there it is minimized.
#[cfg(not(windows))]
fn hide_main(ctx: &egui::Context) {
    let hide = cfg!(target_os = "macos") || win::prefer_x11();
    ctx.send_viewport_cmd(if hide { ViewportCommand::Visible(false) } else { ViewportCommand::Minimized(true) });
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::window::table::find_hit;

    #[cfg(not(windows))]
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
