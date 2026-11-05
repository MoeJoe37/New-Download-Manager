mod download;
mod options;
pub(super) mod scheduler;

use self::download::file_info_rows;
use self::misc::info_height;
mod playlist;
#[cfg(target_os = "macos")]
mod mac;
mod limits;
mod report;
mod checksum;
mod browser;
mod grabber;
mod misc;

use crate::app::{App, Arc, GRAB_MENU, GRAB_STEPS, GRAB_TB, Incoming, Info, Mutex, PROG_DX, Probe, Receiver, Relaxed, Release, Settings, TITLE, Vec2, ViewportBuilder, ViewportCommand, ViewportId, core, discard, dlu, egui, engine, help_body, i18n, net, tr_os, win, ytdlp};

pub(super) type ProbeSlot = Arc<Mutex<Option<Result<Probe, String>>>>;

#[derive(Default)]
pub(super) struct PlaylistPicker {
    url: String,
    browser: String,
    pending: Option<PlaylistLookup>,
    list: Option<ytdlp::playlist::Playlist>,
    selected: Vec<bool>,
    error: String,
}
impl PlaylistPicker {
    pub(super) fn new(url: String) -> Self {
        Self { url, ..Default::default() }
    }
}

struct PlaylistLookup {
    job: Arc<engine::Job>,
    results: Receiver<Result<ytdlp::playlist::Playlist, String>>,
}

impl Drop for PlaylistLookup {
    fn drop(&mut self) {
        self.job.stop();
    }
}

#[derive(Default)]
pub(super) struct ChecksumSource {
    url: String,
    pending: Option<Receiver<Result<String, String>>>,
    preview: Option<Result<String, String>>,
}

// a handful of dialogs is open at a time: their size doesn't matter, boxing each would only add noise
#[allow(clippy::large_enum_variant)]
pub(super) enum Dialog {
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
    Integrate { b: core::settings::Browser },
    /// "Using special keys" (template 229).
    Keys { k: core::settings::Keys },
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
    Options { tab: u8, s: Box<Settings> },
    /// `queue` is the edited copy of the queue selected in the tree; Apply stores it.
    Scheduler { tab: u8, queue: core::queue::Queue, qsel: Option<u64>, error: String },
    /// Duplicate download link (template 298): the new link and the download already in the list.
    Duplicate { inc: Incoming, old: u64, choice: u8, remember: bool },
    /// File Properties (template 135) for a finished download.
    FileProps { id: u64, url: String, desc: String, referer: String, login: String, pass: String },
    /// File Properties -> Checksum...: the file's SHA-256, SHA-1 and MD5, and a checksum to compare them with.
    Checksum { name: String, run: Arc<crate::checksum::Run>, expect: String, source: ChecksumSource },
    Playlist(Box<PlaylistPicker>),
    ProblemReport { text: String },
    Http1Hosts { hosts: String },
    #[cfg(target_os = "macos")]
    MacCapture {
        services: Vec<(String, String)>,
        selected: Option<usize>,
        pending: Option<Receiver<Result<(), String>>>,
        message: String,
    },
    /// Downloads -> Set yt-dlp: downloading it (bytes so far, of, the version installed or the error).
    YtDlp { got: Arc<std::sync::atomic::AtomicU64>, total: Arc<std::sync::atomic::AtomicU64>, done: Arc<Mutex<Option<Result<String, String>>>> },
    /// NDM categories (template 204). `orig` = name of the edited category, None when adding.
    Category { orig: Option<String>, cat: core::settings::Category, folder: String, error: String },
    /// "Do you really want to delete ... category" confirmation.
    CatDelete { name: String },
    /// Advanced proxy settings (template 369).
    AdvProxy { adv: core::routing::AdvProxy },
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
    GrabSave { name: String, orig: Option<String>, quiet: bool, proj: Option<Box<core::settings::GrabProject>>, sched: bool },
    /// "To use project scheduler you should save the project at first. Save the project now?"
    /// 0x4e8c80, Yes / No) for Schedule the project on an unsaved project.
    GrabSchedAsk { proj: Box<core::settings::GrabProject> },
    /// "Schedule the grabber project" (template 284), edited copy.
    GrabSched { p: Box<core::settings::GrabProject> },
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
    Grabber { step: u8, adv: bool, node: usize, opts: crate::grabber::Opts, save_mode: u8, save_cat: String, folder: String, crawl: Option<Arc<crate::grabber::Crawl>> },
    /// "NDM Grabber Statistics" (template 271), from the grabber toolbar's last button.
    GrabStats { crawl: Arc<crate::grabber::Crawl> },
    /// "Template Name" (template 268) for Project -> "Save current settings as a template"; `replace` =
    /// asking "The template with such name already exists..." (20603).
    GrabTpl { name: String, proj: Box<core::settings::GrabProject>, replace: bool },
    /// "Load project" (template 270): the saved projects.
    GrabLoad { sel: Option<usize> },
    /// "Settings of NDM Grabber" (template 262), edited copy.
    GrabSettings { explore: String, dl: String, link: bool, iec: bool, adv: bool },
    /// "Manual login page": the page is open in the browser; OK explores with the browser's cookies.
    GrabManual { url: String },
    About,
    /// View -> Custom scale: the toolbar, categories pane and text sizes when it opened (Cancel restores them).
    Scale { orig: [f32; 3] },
    /// "Customize Toolbar": the edited button order (-1 = separator) and the selected rows of both lists.
    TbCustomize { layout: Vec<i8>, avail: Option<usize>, cur: Option<usize> },
    /// Tip of the Day (template 105, CTipDlg): the tip shown.
    Tip { text: String },
    /// Speed Limiter settings (template 337).
    Limiter { kb: String, startup: bool, sched: bool, from: String, to: String },
    /// The list's "Speed limit...": the chosen downloads' own limit (`Info::limit_kb`).
    SpeedLimit { ids: Vec<u64>, on: bool, kb: String },
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

fn dialog_panel_frame(ctx: &egui::Context, d: &Dialog, fixed: Option<Vec2>) -> egui::Frame {
    if fixed.is_some() { egui::Frame::new().fill(ctx.style().visuals.panel_fill) } else if matches!(d, Dialog::Help { .. }) { egui::Frame::new() } else { egui::Frame::central_panel(&ctx.style()) }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod dialog_snapshots;

impl App {
    fn dialog_frame(&self, d: &Dialog) -> (String, Option<Vec2>, Vec2) {
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
            Dialog::FileInfo { info, path, .. } => {
                let (name, video) = file_info_rows(info, path);
                ("Download File Info".into(), tpl(380.0, 128.0 + name + video))
            }
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
            Dialog::Options { .. } => ("New Download Manager Configuration".into(), tpl(327.0, 309.0)),
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
            Dialog::Limiter { .. } => ("Speed Limiter settings".into(), tpl(302.0, 107.0)),
            Dialog::SpeedLimit { .. } => ("Speed limit".into(), tpl(230.0, 46.0)),
            Dialog::Update { .. } => ("Quick Update".into(), tpl(186.0, 34.0)),
            Dialog::NewVersion { .. } => ("New version of New Download Manager is available".into(), Some(Vec2::new(520.0, 420.0))),
            Dialog::Virus { .. } => ("Virus checking".into(), tpl(379.0, 235.0)),
            Dialog::Columns { .. } => ("Columns".into(), tpl(236.0, 177.0)),
            Dialog::Find { .. } => ("Find file".into(), tpl(230.0, 148.0)),
            Dialog::ServerConn { .. } => ("Max. connections number for a server".into(), tpl(221.0, 89.0)),
            Dialog::Networks { nics, .. } => ("Networks".into(), tpl(240.0, 66.0 + 12.0 * nics.len().max(1) as f32)),
            Dialog::QueueName { .. } => ("Enter queue name".into(), tpl(186.0, 74.0)),
            Dialog::QuotaHit { .. } => ("Download limits exceeded!".into(), tpl(339.0, 93.0)),
            Dialog::Info { title, text } => (title.clone(), tpl(260.0, 32.0 + info_height(text))),
            Dialog::AdvProxy { .. } => ("Advanced proxy settings".into(), tpl(268.0, 285.0)),
            Dialog::Category { orig, .. } => (if orig.is_some() { "Editing category properties" } else { "Adding a category to New Download Manager categories list" }.to_string(), tpl(322.0, 177.0)),
            Dialog::CatDelete { .. } => ("Deleting a category from New Download Manager categories list".into(), tpl(260.0, 60.0)),
            Dialog::Duplicate { .. } => ("Duplicate download link".into(), tpl(286.0, 154.0)),
            Dialog::FileProps { .. } => ("File Properties".into(), tpl(318.0, 246.0)),
            Dialog::Checksum { .. } => ("Verify checksum".into(), tpl(300.0, 220.0)),
            Dialog::Playlist(_) => ("Download video playlist".into(), None),
            Dialog::ProblemReport { .. } => ("Report a problem".into(), None),
            Dialog::Http1Hosts { .. } => ("HTTP/1.1 hosts".into(), tpl(270.0, 75.0)),
            #[cfg(target_os = "macos")]
            Dialog::MacCapture { .. } => ("macOS HTTP capture".into(), None),
            Dialog::YtDlp { .. } => ("Set yt-dlp".into(), tpl(186.0, 44.0)),
            Dialog::About => ("About New Download Manager".into(), tpl(260.0, 234.0)),
            Dialog::Scale { .. } => ("Custom scale".into(), tpl(232.0, 100.0)),
            Dialog::Tip { .. } => ("Tip of the Day".into(), tpl(253.0, 164.0)),
            Dialog::TbCustomize { .. } => ("Customize Toolbar".into(), tpl(357.0, 125.0)),
            // NDM puts its menu bar (menu 256) over both and the toolbar (260) over the results
            Dialog::Grabber { crawl: Some(_), .. } => ("NDM Site Grabber".into(), tpl(508.0, 286.0).map(|s| s + Vec2::new(0.0, GRAB_MENU + GRAB_TB))),
            Dialog::Grabber { step, adv, .. } => {
                let title = i18n::trf_os("NDM Site Grabber. Step %d --- %s", &[&(*step + 1), &tr_os(GRAB_STEPS[*step as usize % 4])]);
                (title, tpl(406.0, if *adv && *step != 1 { 307.0 } else { 210.0 }).map(|s| s + Vec2::new(0.0, GRAB_MENU)))
            }
            Dialog::GrabStats { .. } => ("NDM Grabber Statistics".into(), tpl(111.0, 103.0)),
            Dialog::GrabTpl { .. } => ("Template Name".into(), tpl(186.0, 70.0)),
            Dialog::GrabLoad { .. } => ("Load project".into(), tpl(257.0, 138.0)),
            Dialog::GrabSettings { .. } => ("Settings of NDM Grabber".into(), tpl(292.0, 139.0)),
            Dialog::GrabManual { .. } => ("Manual login page".into(), tpl(260.0, 70.0)),
        };
        let size = fixed.unwrap_or(match d {
            Dialog::Help { .. } => Vec2::new(980.0, 660.0),
            Dialog::Playlist(_) | Dialog::ProblemReport { .. } => Vec2::new(550.0, 430.0),
            #[cfg(target_os = "macos")]
            Dialog::MacCapture { .. } => Vec2::new(500.0, 250.0),
            _ => Vec2::new(380.0, 170.0),
        });
        (title, fixed, size)
    }

    /// Draw one dialog in its own OS window. Returns false once it should close.
    pub(super) fn dialog(&mut self, ctx: &egui::Context, key: u64, d: &mut Dialog) -> bool {
        let (title, fixed, size) = self.dialog_frame(d);
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
        // NDM Style darkens every window
        let dark = self.dark && (dlu::ndm() || matches!(d, Dialog::AddUrl { .. } | Dialog::FileInfo { .. } | Dialog::Progress { .. } | Dialog::Complete { .. } | Dialog::Info { .. } | Dialog::Help { .. }));
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
            let frame = dialog_panel_frame(ctx, d, fixed);
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| keep = self.dialog_body(ctx, ui, d));
            let closed = ctx.input(|i| i.viewport().close_requested());
            if let (true, Dialog::FileInfo { early: Some(j), .. }) = (closed, &*d) {
                discard(j);
            }
            if let (true, Dialog::Scale { orig }) = (closed, &*d) {
                let mut s = self.core.settings();
                [s.scale_toolbar, s.scale_tree, s.scale_text] = *orig;
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
            Dialog::AddUrl { url, auth, login, pass, .. } => self.dialog_add_url(ctx, ui, url, auth, login, pass),
            Dialog::FileInfo { info, path, probe, applied, error, hint, remember, replace, auto, early, dir } => self.dialog_file_info(ctx, ui, info, path, probe, applied, error, hint, remember, replace, auto, early, dir),
            Dialog::Progress { id, tab, details, .. } => self.dialog_progress(ui, id, tab, details),
            Dialog::ProgCustom { view, sl, c, btns } => self.dialog_prog_custom(ui, view, sl, c, btns),
            Dialog::Complete { id, dont_show } => self.dialog_complete(ctx, ui, id, dont_show),
            Dialog::Delete { ids, files, dont_show } => self.dialog_delete(ui, ids, files, dont_show),
            Dialog::Options { tab, s } => self.dialog_options(ctx, ui, tab, s),
            Dialog::Scheduler { tab, queue, qsel, error } => self.dialog_scheduler(ui, tab, queue, qsel, error),
            Dialog::Duplicate { inc, old, choice, remember } => self.dialog_duplicate(ctx, ui, inc, old, choice, remember),
            Dialog::FileProps { id, url, desc, referer, login, pass } => self.dialog_file_props(ctx, ui, id, url, desc, referer, login, pass),
            Dialog::Playlist(pick) => self.dialog_playlist(ctx, ui, pick),
            #[cfg(target_os = "macos")]
            Dialog::MacCapture { services, selected, pending, message } => self.dialog_mac_capture(ctx, ui, services, selected, pending, message),
            Dialog::Http1Hosts { hosts } => self.dialog_http1_hosts(ui, hosts),
            Dialog::ProblemReport { text } => Self::dialog_problem_report(ui, text),
            Dialog::Checksum { name, run, expect, source } => self.dialog_checksum(ctx, ui, name, run, expect, source),
            Dialog::Category { orig, cat, folder, error } => self.dialog_category(ui, orig, cat, folder, error),
            Dialog::CatDelete { name } => self.dialog_cat_delete(ui, name),
            Dialog::AdvProxy { adv } => self.dialog_adv_proxy(ui, adv),
            Dialog::Cancelled { site, url, by_site, by_addr, dont_show } => self.dialog_cancelled(ui, site, url, by_site, by_addr, dont_show),
            Dialog::Exceptions { list, sel, ask } => self.dialog_exceptions(ui, list, sel, ask),
            Dialog::ExcAdd { addr } => self.dialog_exc_add(ui, addr),
            Dialog::BrowserDetails { name } => self.dialog_browser_details(ui, name),
            Dialog::Integrate { b } => Self::dialog_integrate(ui, b),
            Dialog::Keys { k } => self.dialog_keys(ui, k),
            Dialog::MenuItems { dl, all } => self.dialog_menu_items(ui, dl, all),
            Dialog::Panel { tab, types, min, mini, sel, nocap, drm, smode, sites, smini, ssel } => self.dialog_panel(ui, tab, types, min, mini, sel, nocap, drm, smode, sites, smini, ssel),
            Dialog::PanelSite { name } => self.dialog_panel_site(ui, name),
            Dialog::PanelType { name } => self.dialog_panel_type(ui, name),
            Dialog::PanelMin { ext, kb } => self.dialog_panel_min(ui, ext, kb),
            Dialog::PanelExc { text } => self.dialog_panel_exc(ui, text),
            Dialog::Networks { nics, off } => self.dialog_networks(ui, nics, off),
            Dialog::ServerConn { orig, server, n } => self.dialog_server_conn(ui, orig, server, n),
            Dialog::Find { text, flags } => self.dialog_find(ui, text, flags),
            Dialog::Info { text, .. } => Self::dialog_info(ui, text),
            Dialog::YtDlp { got, total, done } => self.dialog_yt_dlp(ui, got, total, done),
            Dialog::Update { slot, quiet } => self.dialog_update(ui, slot, quiet),
            Dialog::NewVersion { rel, get } => self.dialog_new_version(ctx, ui, rel, get),
            Dialog::Virus { exe, args } => self.dialog_virus(ui, exe, args),
            Dialog::Export { ef2, mode } => self.dialog_export(ui, ef2, mode),
            Dialog::Help { page, anchor, back, fwd, search } => help_body(ctx, ui, page, anchor, back, fwd, search),
            Dialog::DialUp => self.dialog_dial_up(ui),
            Dialog::WaitLink { name } => self.dialog_wait_link(ui, name),
            Dialog::QuotaHit { text } => Self::dialog_quota_hit(ui, text),
            Dialog::QueueName { name, files } => self.dialog_queue_name(ui, name, files),
            Dialog::GrabSave { name, orig, quiet, proj, sched } => self.dialog_grab_save(ui, name, orig, quiet, proj, sched),
            Dialog::GrabSchedAsk { proj } => self.dialog_grab_sched_ask(ui, proj),
            Dialog::GrabSched { p } => self.dialog_grab_sched(ui, p),
            Dialog::GrabLogin { realm, site, user, pass, save } => self.dialog_grab_login(ui, realm, site, user, pass, save),
            Dialog::QueuePick { ids, q, start, never, batch } => self.dialog_queue_pick(ui, ids, q, start, never, batch),
            Dialog::Batch { url, from, to, width, letters, lf, lt, auth, login, pass } => self.dialog_batch(ui, url, from, to, width, letters, lf, lt, auth, login, pass),
            Dialog::Grabber { step, adv, node, opts, save_mode, save_cat, folder, crawl } => self.dialog_grabber(ctx, ui, step, adv, node, opts, save_mode, save_cat, folder, crawl),
            Dialog::GrabTpl { name, proj, replace } => self.dialog_grab_tpl(ui, name, proj, replace),
            Dialog::GrabLoad { sel } => self.dialog_grab_load(ui, sel),
            Dialog::GrabSettings { explore, dl, link, iec, adv } => self.dialog_grab_settings(ui, explore, dl, link, iec, adv),
            Dialog::GrabManual { url } => self.dialog_grab_manual(ui, url),
            Dialog::GrabStats { crawl } => self.dialog_grab_stats(ui, crawl),
            Dialog::Limiter { kb, startup, sched, from, to } => self.dialog_limiter(ui, kb, startup, sched, from, to),
            Dialog::SpeedLimit { ids, on, kb } => self.dialog_speed_limit(ui, ids, on, kb),
            Dialog::Columns { list, sel, width } => self.dialog_columns(ui, list, sel, width),
            Dialog::Scale { orig } => self.dialog_scale(ui, orig),
            Dialog::About => self.dialog_about(ctx, ui),
            Dialog::TbCustomize { layout, avail, cur } => self.dialog_tb_customize(ui, layout, avail, cur),
            Dialog::Tip { text } => self.dialog_tip(ctx, ui, text),
        }
    }
}
