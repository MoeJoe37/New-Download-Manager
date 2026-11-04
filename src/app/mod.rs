mod display;
mod downloads;
mod download_list;
mod browser_setup;
mod shell;
mod grabber_window;
mod events;

use self::display::{human, human_os, rate, time_left, size3, size3_os, pct2, date};
use self::downloads::{with_login, split_login, join_login, clipboard, links, discard, DUPL};
use self::browser_setup::{extension_page, GUIDES, integration, extension_dir};
use self::shell::{virus_args, explore, open_folder_of, open_url, open_with};
use self::grabber_window::{GRAB_MENU, GRAB_TB, GRAB_STEPS, GrabCmd};
#[cfg(test)]
use self::events::{close_action, Close, no_zoom};
use self::dialogs::scheduler::sched_time;
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
use crate::core::{Core, browser_bridge::Incoming, settings::Settings, settings::GENERAL};
use crate::diagnostics::{crash_diagnostics, issue_text};
use crate::update::{check_update, install_update, Release, UPDATE_REPO};
use crate::engine::{Info, probe::Probe, Status};
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

/// Horizontal DLU scale of the pages NDM creates inside its Options sheet and Scheduler (2.0 px per DLU at
/// 125%, measured; their frames use dlu::DX).
const PAGE_DX: f32 = 1.6;

/// Download progress dialog 103 across: NDM lays it out 723 px wide for 353 DLU at 125% (measured).
const PROG_DX: f32 = 723.0 / 353.0 / 1.25;

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
    adv_done: Option<core::routing::AdvProxy>,
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
    browser_add: Vec<core::settings::Browser>,
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
        if let Some(inc) = core::browser_bridge::cli_args(&std::env::args().skip(1).collect::<Vec<_>>()) {
            let _ = tx.send(inc);
        }
        let c = ctx.clone();
        core.spawn_background(move || c.request_repaint());
        // NDM's own yt-dlp (Downloads -> Set yt-dlp) stays up to date
        ytdlp::install::keep_updated(core.clone());
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
            let r = ytdlp::install::install(&agent, &g, &t, true);
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

    fn options(&mut self, tab: u8) {
        let mut s = self.core.settings().clone();
        // browsers installed since the list was saved are added ticked, like NDM's detection (296)
        for (name, exe) in win::installed_browsers() {
            if !s.browsers.iter().any(|b| b.exe.eq_ignore_ascii_case(&exe)) {
                s.browsers.push(core::settings::Browser { name, exe, on: true });
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
