//! App state shared by the UI and a background thread that runs the download queue,
//! the scheduler, autosave and the localhost endpoint the browser extension talks to.
use crate::engine::{self, Info, Job, Limiter, Status};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub const PORT: u16 = 9614;
/// "Use advanced browser integration": Windows' proxy for http points here (see `apply_integration`).
pub const PROXY_PORT: u16 = 9615;

/// NDM's default categories (and their file types); the user edits them in "NDM categories" (template 204).
pub const CATEGORIES: [(&str, &str); 5] = [
    ("Compressed", "zip rar r0 r1 arj gz sit sitx sea ace bz2 7z tar tgz xz zst iso"),
    ("Documents", "doc docx pdf ppt pps pptx xls xlsx odt ods rtf txt epub"),
    ("Music", "mp3 wav wma mpa ram ra aac aif m4a flac ogg opus"),
    ("Programs", "exe msi msix apk dmg deb rpm appimage"),
    ("Video", "avi mpg mpe mpeg asf wmv mov qt rm mp4 flv m4v webm ogv mkv ts 3gp"),
];
pub const GENERAL: &str = "General";

pub fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// A download category (template 204).
#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Category {
    pub name: String,
    /// "Automatically put in category file types" (space separated).
    pub exts: String,
    /// "Automatically put in this category files following sites only" + the site patterns (`*` wildcards).
    pub sites_on: bool,
    pub sites: String,
    /// "Remember last save path": the folder chosen in Download File Info becomes the category folder.
    pub remember: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sched {
    pub start_on: bool,
    pub start_at: String,
    pub daily: bool,
    /// Days the daily schedule runs, Sunday first.
    pub days: [bool; 7],
    pub stop_on: bool,
    pub stop_at: String,
    pub on_startup: bool,
    pub exit_done: bool,
    pub off_done: bool,
    /// "Number of retries for each file if downloading failed" (template 306, edit 1681).
    pub retry_on: bool,
    pub retries: u32,
    /// Synchronization queues: "Start again every" hours/min after a pass.
    pub again_on: bool,
    pub again_h: u32,
    pub again_m: u32,
    /// "Open the following file when done".
    pub open_on: bool,
    pub open_file: String,
    /// "Turn off computer when done": 0 Shut down, 1 Sleep, 2 Hibernate; "Force processes to terminate".
    pub off_kind: u8,
    pub off_force: bool,
    /// "Hang up modem when done" (Dial Up / VPN).
    pub hangup: bool,
}

impl Default for Sched {
    fn default() -> Self {
        // NDM's defaults: 11:00 PM / 7:30 AM, 10 retries, start again every 2 hours
        Sched {
            start_on: false,
            start_at: "23:00".into(),
            daily: true,
            days: [true; 7],
            stop_on: false,
            stop_at: "07:30".into(),
            on_startup: false,
            exit_done: false,
            off_done: false,
            retry_on: false,
            retries: 10,
            again_on: false,
            again_h: 2,
            again_m: 0,
            open_on: false,
            open_file: String::new(),
            off_kind: 0,
            off_force: false,
            hangup: false,
        }
    }
}

/// A download queue. Each has its own schedule and "files at the same time".
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Queue {
    pub id: u32,
    pub name: String,
    pub sched: Sched,
    pub max_active: usize,
    /// "Periodic synchronization": finished files stay in the queue and
    /// each pass downloads again those that changed on the server.
    pub sync: bool,
    /// NDM's built-in Synchronization queue: can't be deleted.
    pub fixed: bool,
}

impl Default for Queue {
    fn default() -> Self {
        Queue { id: MAIN_QUEUE, name: "Main download queue".into(), sched: Sched::default(), max_active: 3, sync: false, fixed: false }
    }
}

impl Queue {
    /// NDM's built-in second queue.
    fn sync_default(id: u32) -> Queue {
        Queue { id, name: "Synchronization queue".into(), sync: true, fixed: true, ..Default::default() }
    }

    /// Main and synchronization queues are built in: they can't be deleted or change type.
    pub fn builtin(&self) -> bool {
        self.id == MAIN_QUEUE || self.fixed
    }
}

/// A saved Site Grabber project: the wizard's settings, what the last run found, and its schedule
/// ("Schedule the grabber project", template 284).
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GrabProject {
    pub opts: crate::grabber::Opts,
    /// Save To: 0 by category, 1 one category, 2 one folder.
    pub save_mode: u8,
    pub save_cat: String,
    pub folder: String,
    pub files: Vec<crate::grabber::Found>,
    pub pages: usize,
    /// Step 1 "Periodic synchronization" (else one-time); step 2: 0 explore only, 1 explore and download
    /// matched files, 2 download checked files; step 3 + when-done options in `sched`.
    pub periodic: bool,
    pub action: u8,
    pub sched: Sched,
}

/// NDM's "Main download queue": always there, can't be deleted.
pub const MAIN_QUEUE: u32 = 0;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub base_dir: String,
    pub conns: usize,
    pub limit_on: bool,
    pub limit_kb: u64,
    pub show_progress: bool,
    pub show_complete: bool,
    pub file_types: String,
    /// Main download queue first.
    pub queues: Vec<Queue>,
    /// User categories ("General" is implicit: everything that matches none of these).
    pub categories: Vec<Category>,
    /// Before queues existed the main queue's settings lived here; read once on load.
    #[serde(rename = "sched", skip_serializing)]
    pub old_sched: Option<Sched>,
    #[serde(rename = "max_active", skip_serializing)]
    pub old_max_active: Option<usize>,
    /// Folders the user chose to remember per category ("Remember this path for ... category").
    pub cat_dirs: std::collections::BTreeMap<String, String>,
    pub confirm_delete: bool,
    pub autostart: bool,
    /// Options -> General -> "Use advanced browser integration".
    pub adv_integration: bool,
    /// Dial Up / VPN page: the connection to dial,
    /// redial attempts (0 = endlessly) and seconds between them. User name and password live in Windows.
    pub dial_on: bool,
    pub dial_entry: String,
    pub dial_redial: u32,
    pub dial_wait: u32,
    /// Windows' proxy (on, server, bypass) from before NDM's was set, put back when it's turned off.
    pub saved_proxy: Option<(bool, String, String)>,
    pub only_queue: bool,
    pub user_agent: String,
    /// Don't take over browser downloads from these sites (space separated).
    pub skip_sites: String,
    pub proxy_mode: u8, // 0 none, 1 system, 2 script, 3 manual
    /// "Use automatic configuration script" address (a PAC file).
    pub pac_url: String,
    /// "Use FTP in PASV mode".
    pub ftp_pasv: bool,
    /// "Use this proxy for the following protocols: http https".
    pub proxy_http: bool,
    pub proxy_https: bool,
    /// "ftp": ftp:// addresses go through the manual HTTP proxy (which does the FTP).
    pub proxy_ftp: bool,
    /// Connection page: "Download over several network connections at once", and the networks the
    /// user unticked in the Networks dialog.
    pub multi_net: bool,
    pub nets_off: Vec<String>,
    /// Proxy / Socks page: "Use proxy/socks from a browser's request if there are download errors for the
    /// downloads intercepted from the browser" (`Info::browser_proxy`, `Core::start_now`).
    pub proxy_browser: bool,
    /// "Do not use proxy server for addresses beginning with:" (space separated, `*` wildcards).
    pub proxy_bypass_on: bool,
    pub proxy_bypass: String,
    /// Advanced proxy settings (template 369): a separate HTTPS proxy and a SOCKS server.
    pub adv: AdvProxy,
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub proxy_pass: String,
    pub dark_support: bool,
    /// View -> Language: a Languages\ndm_*.lng file stem, "" = English.
    pub language: String,
    /// Options -> Site Logins: credentials used for downloads from these servers.
    pub logins: Vec<Login>,
    /// "Always turn on Speed Limiter on startup".
    pub limit_startup: bool,
    /// Main list columns as, in display order (View -> Customize URL List).
    pub columns: Vec<(u8, u16)>,
    /// List context menu "On Double click": open the file instead of showing properties.
    pub dbl_open: bool,
    /// Duplicate download link: NDM's DuplLinksA (0 resume / show complete, 1 numbered name, 2 overwrite)
    /// and !RememberDuplLinksA (ask with dialog 298)..
    pub dupl_action: u8,
    pub dupl_ask: bool,
    /// View -> Toolbar -> Small Buttons.
    pub small_buttons: bool,
    /// Options -> Connection "Download limits": no more than `quota_mb` every `quota_hours` (1-23).
    /// Scheduler\isLimitEnabled, m_MBytes, m_hours, showLimitExceededWarning.
    pub quota_on: bool,
    pub quota_mb: u64,
    pub quota_hours: u64,
    pub quota_warn: bool,
    /// Connection page "Exceptions": (server mask, max connections), template 226.
    pub server_conns: Vec<(String, usize)>,
    /// Save to page: give finished files the server's date; temporary directory for .part files.
    pub set_date: bool,
    pub temp_dir: String,
    /// Downloads page: "Ignore file modification time changes when resuming a download".
    pub ignore_date: bool,
    /// Downloads page: "Start downloading immediately while displaying \"Download File Info\" dialog".
    pub start_at_once: bool,
    /// Options -> Sounds: (on, .wav path) per `SOUND_EVENTS`, all off by default.
    pub sounds: [(bool, String); 4],
    /// Options -> General "Automatically start downloading of URLs placed to clipboard" (MonitorUrlClipboard).
    pub clip_watch: bool,
    /// Options -> File types "Edit list..." (template 303): addresses never taken over automatically,
    /// `*` wildcards.
    pub skip_urls: Vec<String>,
    /// Template 303 "Show the dialog to add an address to the list of exceptions for a twice-cancelled download" (302).
    pub ask_exception: bool,
    /// Browser context menu items (template 334): "Download with", "Download all links with".
    pub menu_dl: bool,
    pub menu_all: bool,
    /// Download panel exceptions (template 370, PanelExceptionServers): sites with no video panel.
    pub panel_skip: String,
    /// Options -> General "Capture downloads from the following browsers": the ticked ones are taken over.
    pub browsers: Vec<Browser>,
    /// Options -> General "Keys..." (template 229).
    pub keys: Keys,
    /// Download panel on web players (template 340, `HKCU\Software\DownloadManager\DwnlPanel`): file types
    /// with ticks, minimum size per type in KB (DwnlPanel\minsize), Full/Mini view (PanelView).
    pub panel_types: Vec<(String, bool)>,
    pub panel_min: BTreeMap<String, u64>,
    pub panel_mini: bool,
    /// Web-players page: "Don't capture downloads from web-players automatically" (off = a media request
    /// of a ticked type is handed over by itself) and "Show download panel for protected content".
    pub panel_nocap: bool,
    pub panel_drm: bool,
    /// "For selected files" (template 348): 0 any page, 1 never, 2 only `sel_sites`; its Full/Mini view.
    pub sel_mode: u8,
    pub sel_sites: Vec<String>,
    pub sel_mini: bool,
    /// Tip of the Day (template 105): "Show Tips on StartUp" and the next tip's index.
    /// ponytail: index, not NDM's byte offset TipFilePos + TipTimeStamp reset; a changed file only shifts the tip.
    pub tips_startup: bool,
    pub tip_next: usize,
    /// View -> "Classic style" (3D toolbar, +/- tree boxes) instead of "Modern style".
    pub classic: bool,
    /// View -> "NDM tray icon": 1 3D style (app icon), 2 Classic style (2D icon), 3 Don't show.
    pub tray_icon: u8,
    /// "Customize download progress dialog" (template 367, 0x435a00): Start view 0 normal / 1 minimized /
    /// 2 don't show (PrgrDlgVisiblity), Speed Limiter tab, Options on completion tab, "Hide tab" buttons.
    pub prog_view: u8,
    /// Options -> Downloads "Show queue selection panel on pressing Download Later" / "...on adding batch
    /// downloads" (template 345 pops up).
    pub ask_queue_later: bool,
    pub ask_queue_batch: bool,
    /// The "NDM Queues" note about keeping the app in the tray was shown.
    pub tray_note_shown: bool,
    /// "Change folder for General category on last selected" (the other categories keep it in `remember`).
    pub remember_general: bool,
    /// Options -> Downloads "Virus checking settings" (template 494): scanner and its parameters
    /// (`[File]` = the downloaded file; none = the path is appended).
    pub virus_exe: String,
    pub virus_args: String,
    /// View -> Font -> Choose Font...: the download list's face and size (px); "" = NDM's default Tahoma.
    pub list_font: String,
    pub list_font_px: f32,
    /// Expanded nodes of the category tree ("all", "unf", "fin", "q"), kept between runs.
    pub expanded: Vec<String>,
    /// Saved Site Grabber projects (the "Grabber projects" tree node) and "Don't show this dialog if the
    /// name is unique" of "Saving the project of NDM Grabber" (template 269).
    pub grab_projects: Vec<GrabProject>,
    pub grab_quiet: bool,
    /// Project -> Recent Projects: the last 4 projects opened or saved, newest first.
    pub grab_recent: Vec<String>,
    /// "Save current settings as a template" (34020, name box 268): `opts.name` is the template's name.
    pub grab_templates: Vec<GrabProject>,
    /// Grabber Settings (template 262, saved as GrabberSts nATSTFiles 1-20,
    /// nDwnlATSTFiles 1-10, bUseTL, bUseIEC, bUseAdvWPP). Defaults from CSpiderSettings' constructor 0x53c050.
    pub grab_explore: usize,
    pub grab_dl: usize,
    pub grab_link_text: bool,
    pub grab_ie_cache: bool,
    pub grab_adv: bool,
    /// File -> Show drop target.
    pub drop_target: bool,
    /// When updates were last checked.
    pub update_checked: i64,
    pub prog_sl_tab: bool,
    pub prog_c_tab: bool,
    pub prog_tab_btns: bool,
    /// Download progress dialog shows the connections.
    pub prog_details: bool,
    /// Toolbar skin: "Neon" or a `<NDM>\Toolbar\*.tbi` name.
    pub tb_skin: String,
    /// Toolbar buttons in order, -1 = separator.
    pub tb_layout: Vec<i8>,
    /// Find file: 1 match case, 2 whole
    /// string, 4 file name, 8 description, 0x10 site/link/page/referer.
    pub search: String,
    pub search_flags: u32,
}

/// NDM's default download panel file types (its DwnlPanel values after install), all ticked.
pub const PANEL_TYPES: &str = "FLV MP3 MP4 M4V F4V M4A MPG MPEG AVI WMV WMA WAV ASF RM OGG OGV MOV 3GP QT WEBM TS MKV AAC VTT TTML TTML2 DFXP SRT";

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Browser {
    pub name: String,
    pub exe: String,
    pub on: bool,
}

/// Which browser a name, exe path or User-Agent belongs to; the extension's requests carry the
/// browser's User-Agent. Chrome is last: the others' User-Agents contain "Chrome/" too.
pub fn browser_kind(text: &str) -> &'static str {
    let t = text.to_ascii_lowercase();
    [("edg", "edge"), ("opr/", "opera"), ("opera", "opera"), ("vivaldi", "vivaldi"), ("yabrowser", "yandex"), ("yandex", "yandex"), ("firefox", "firefox"), ("brave", "brave"), ("chrom", "chrome")]
        .iter()
        .find(|(k, _)| t.contains(k))
        .map_or("", |(_, v)| v)
}

/// Special keys (template 229; SpecialKeys\UseKeyToPrevent, AltP... InsF, CheckMouse, SkipHtml,
/// The browser extension applies them.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Keys {
    pub prevent: bool,
    pub alt_p: bool,
    pub shift_p: bool,
    pub ctrl_p: bool,
    pub del_p: bool,
    pub force: bool,
    pub alt_f: bool,
    pub shift_f: bool,
    pub ctrl_f: bool,
    pub ins_f: bool,
    pub check_mouse: bool,
    pub skip_html: bool,
}

impl Default for Keys {
    fn default() -> Self {
        Keys { prevent: true, alt_p: false, shift_p: false, ctrl_p: false, del_p: true, force: false, alt_f: false, shift_f: false, ctrl_f: false, ins_f: true, check_mouse: false, skip_html: true }
    }
}

pub const SOUND_EVENTS: [&str; 4] = ["Download complete", "Download failed", "Queue processing started", "Queue processing stopped/finished"];

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AdvProxy {
    /// The Proxy / Socks page's manual HTTP proxy (on, host, port, user, password), which NDM's advanced
    /// dialog repeats as "Use HTTP Proxy"; only carried while the dialog is open.
    #[serde(skip)]
    pub http: Option<(bool, String, String, String, String)>,
    pub https_on: bool,
    pub https_host: String,
    pub https_port: String,
    pub https_user: String,
    pub https_pass: String,
    pub socks_on: bool,
    pub socks_host: String,
    pub socks_port: String,
    pub socks_user: String,
    pub socks_pass: String,
    /// Socks 5 (else 4).
    pub socks5: bool,
    /// "Proxy DNS" unticked: NDM looks names up itself and gives the SOCKS 5 server the address.
    /// Missing in older state files, which keeps their behaviour (the proxy looks names up).
    pub local_dns: bool,
    /// FTP proxy (logs in as user@host).
    pub ftp_on: bool,
    pub ftp_host: String,
    pub ftp_port: String,
    pub ftp_user: String,
    pub ftp_pass: String,
}

/// `scheme://user:pass@host:port`; `host` may already carry ":port" and a scheme prefix.
fn proxy_url(scheme: &str, host: &str, port: &str, user: &str, pass: &str) -> String {
    let host = host.trim();
    let host = host.split_once("://").map_or(host, |(_, h)| h).trim_end_matches('/');
    let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
    let auth = if user.is_empty() { String::new() } else { format!("{}:{}@", enc(user), enc(pass)) };
    let port = port.trim();
    if port.is_empty() || host.contains(':') { format!("{scheme}://{auth}{host}") } else { format!("{scheme}://{auth}{host}:{port}") }
}

/// The host matches one of the space separated patterns ("10.*", "intranet", "*.corp.com").
fn bypassed(patterns: &str, host: &str) -> bool {
    !host.is_empty() && patterns.split_whitespace().any(|p| crate::grabber::mask_match(p, host) || host.starts_with(p))
}

/// Download-limits bookkeeping: the window start, the bytes counter there, and a stop in force.
#[derive(Default)]
pub struct Quota {
    start: Option<(chrono::DateTime<Local>, u64)>,
    /// Downloads stopped by the quota and when they resume.
    pub stopped: Option<(chrono::DateTime<Local>, Vec<u64>)>,
}

/// NDM stops at 98% of the limit: m_MBytes * 1 MiB / 100 * 98 (0x49d4d0).
pub fn quota_bytes(mb: u64) -> u64 {
    mb * 1024 * 1024 / 100 * 98
}

/// NDM's default visible columns: File Name, Q, Size, Status, Time left, Transfer rate, Last Try Date, Description.
pub const DEFAULT_COLUMNS: [(u8, u16); 8] = [(0, 150), (2, 20), (3, 75), (4, 65), (5, 75), (6, 95), (7, 65), (8, 200)];

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Login {
    pub site: String,
    pub user: String,
    pub pass: String,
}

impl Default for Settings {
    fn default() -> Self {
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
        let mut types: Vec<&str> = CATEGORIES.iter().flat_map(|c| c.1.split(' ')).filter(|e| *e != "txt").collect();
        types.extend(["bin", "img", "torrent"]);
        Settings {
            base_dir: Path::new(&home).join("Downloads").display().to_string(),
            conns: 8,
            limit_on: false,
            limit_kb: 500,
            show_progress: true,
            show_complete: true,
            file_types: types.join(" "),
            queues: vec![Queue::default(), Queue::sync_default(1)],
            categories: CATEGORIES.iter().map(|(n, e)| Category { name: n.to_string(), exts: e.to_string(), ..Default::default() }).collect(),
            old_sched: None,
            old_max_active: None,
            cat_dirs: Default::default(),
            confirm_delete: true,
            autostart: false,
            adv_integration: false,
            dial_on: false,
            dial_entry: String::new(),
            dial_redial: 10,
            dial_wait: 30,
            saved_proxy: None,
            only_queue: false,
            user_agent: String::new(),
            skip_sites: String::new(),
            proxy_mode: 0,
            pac_url: String::new(),
            ftp_pasv: true,
            proxy_http: true,
            proxy_https: true,
            proxy_ftp: false,
            multi_net: false,
            nets_off: vec![],
            proxy_browser: false,
            proxy_bypass_on: false,
            proxy_bypass: String::new(),
            adv: AdvProxy { socks5: true, ..Default::default() },
            proxy_host: String::new(),
            proxy_port: "8080".into(),
            proxy_user: String::new(),
            proxy_pass: String::new(),
            dark_support: true,
            language: String::new(),
            logins: Vec::new(),
            limit_startup: false,
            columns: DEFAULT_COLUMNS.to_vec(),
            dbl_open: false,
            dupl_action: 0,
            dupl_ask: true,
            small_buttons: false,
            quota_on: false,
            quota_mb: 200,
            quota_hours: 5,
            quota_warn: true,
            server_conns: Vec::new(),
            set_date: false,
            temp_dir: String::new(),
            ignore_date: false,
            start_at_once: false,
            sounds: Default::default(),
            clip_watch: false,
            skip_urls: Vec::new(),
            ask_exception: true,
            menu_dl: true,
            menu_all: true,
            panel_skip: String::new(),
            browsers: Vec::new(),
            keys: Keys::default(),
            panel_types: PANEL_TYPES.split(' ').map(|t| (t.to_string(), true)).collect(),
            // NDM's DwnlPanel\minsize defaults: MP3 0xc800 and OGG 0x19000 bytes
            panel_min: BTreeMap::from([("MP3".to_string(), 50), ("OGG".to_string(), 100)]),
            panel_mini: false,
            panel_nocap: true,
            panel_drm: true,
            sel_mode: 0,
            sel_sites: Vec::new(),
            sel_mini: false,
            tips_startup: true,
            tip_next: 0,
            classic: false,
            tray_icon: 1,
            prog_view: 0,
            ask_queue_later: true,
            ask_queue_batch: true,
            tray_note_shown: false,
            remember_general: false,
            virus_exe: String::new(),
            virus_args: String::new(),
            list_font: String::new(),
            list_font_px: 10.0,
            expanded: vec!["all".into()],
            grab_projects: Vec::new(),
            grab_quiet: false,
            grab_templates: Vec::new(),
            grab_recent: Vec::new(),
            // +0xb0 = 4, +0x11c = 4, +0x112 / +0x113 / +0x114 = 1
            grab_explore: 4,
            grab_dl: 4,
            grab_link_text: true,
            grab_ie_cache: true,
            grab_adv: true,
            drop_target: false,
            update_checked: 0,
            prog_sl_tab: true,
            prog_c_tab: true,
            prog_tab_btns: true,
            prog_details: true,
            tb_skin: "Neon".into(),
            tb_layout: (0..12).collect(),
            search: String::new(),
            search_flags: 4,
        }
    }
}

impl Settings {
    /// Proxy URL for the engine from NDM-style manual proxy fields; empty = direct.
    /// The proxy to use for `url` ("" = direct), following NDM's Proxy / Socks page: manual
    /// HTTP proxy for the ticked protocols, the advanced HTTPS proxy for https, SOCKS for the rest
    ///, or Windows' settings.
    pub fn proxy_for(&self, url: &str) -> String {
        let u = url::Url::parse(url).ok();
        let (scheme, host) = (u.as_ref().map_or("", |u| u.scheme()), u.as_ref().and_then(|u| u.host_str()).unwrap_or(""));
        match self.proxy_mode {
            // PAC: Windows' WinHTTP runs FindProxyForURL; the first proxy it names is used
            2 => crate::win::pac_proxy(&self.pac_url, url).map_or(String::new(), |hp| proxy_url("http", &hp, "", "", "")),
            1 => {
                // NDM's own downloads don't go through NDM's proxy
                let (on, server, bypass) = self.saved_proxy.clone().unwrap_or_else(crate::win::system_proxy);
                if !on || bypassed(&bypass.replace(';', " ").replace("<local>", ""), host) {
                    return String::new();
                }
                // "host:port" for all, or "http=h:p;https=h:p;socks=h:p"
                let pick = |k: &str| server.split(';').find_map(|p| p.strip_prefix(&format!("{k}="))).map(str::to_string);
                if !server.contains('=') {
                    return proxy_url("http", server.trim(), "", "", "");
                }
                pick(scheme).map(|hp| proxy_url("http", &hp, "", "", "")).or_else(|| pick("socks").map(|hp| proxy_url("socks5h", &hp, "", "", ""))).unwrap_or_default()
            }
            3 => {
                if self.proxy_bypass_on && bypassed(&self.proxy_bypass, host) {
                    return String::new();
                }
                let a = &self.adv;
                if scheme == "ftp" && a.ftp_on && !a.ftp_host.trim().is_empty() {
                    return proxy_url("ftp", &a.ftp_host, &a.ftp_port, &a.ftp_user, &a.ftp_pass);
                }
                if scheme == "https" && a.https_on && !a.https_host.trim().is_empty() {
                    return proxy_url("http", &a.https_host, &a.https_port, &a.https_user, &a.https_pass);
                }
                let ticked = (scheme == "http" && self.proxy_http) || (scheme == "https" && self.proxy_https) || (scheme == "ftp" && self.proxy_ftp);
                if ticked && !self.proxy_host.trim().is_empty() {
                    return proxy_url("http", &self.proxy_host, &self.proxy_port, &self.proxy_user, &self.proxy_pass);
                }
                if a.socks_on && !a.socks_host.trim().is_empty() {
                    let scheme = if !a.socks5 { "socks4" } else if a.local_dns { "socks5" } else { "socks5h" };
                    return proxy_url(scheme, &a.socks_host, &a.socks_port, &a.socks_user, &a.socks_pass);
                }
                String::new()
            }
            _ => String::new(),
        }
    }

    /// Several networks for this download: only http(s) that goes direct, since a proxy has one way out.
    pub fn use_lanes(&self, url: &str) -> bool {
        self.multi_net && !url.trim().get(..6).is_some_and(|p| p.eq_ignore_ascii_case("ftp://")) && self.proxy_for(url).is_empty()
    }

    /// Adds the saved site login to a URL that has none. A site matches its host and subdomains.
    pub fn with_login(&self, url: &str) -> String {
        let Ok(mut u) = url::Url::parse(url.trim()) else { return url.trim().into() };
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        let site = |l: &Login| l.site.trim().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/').to_ascii_lowercase();
        if u.username().is_empty() {
            if let Some(l) = self.logins.iter().find(|l| !site(l).is_empty() && (host == site(l) || host.ends_with(&format!(".{}", site(l))))) {
                let _ = u.set_username(&l.user);
                let _ = u.set_password(Some(&l.pass));
            }
        }
        u.into()
    }

    /// First category whose file types contain the extension (and whose sites match, if restricted).
    pub fn category_for(&self, name: &str, url: &str) -> String {
        let e = ext(name);
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        self.categories
            .iter()
            .find(|c| !e.is_empty() && c.exts.split_whitespace().any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(&e)) && (!c.sites_on || crate::grabber::mask_match(&c.sites, &host)))
            .map_or(GENERAL.into(), |c| c.name.clone())
    }

    /// "General" then the user categories, for combo boxes.
    pub fn category_names(&self) -> Vec<String> {
        std::iter::once(GENERAL.to_string()).chain(self.categories.iter().map(|c| c.name.clone())).collect()
    }

    pub fn queue(&self, id: u32) -> Option<&Queue> {
        self.queues.iter().find(|q| q.id == id)
    }

    pub fn folder_for(&self, category: &str) -> String {
        if let Some(d) = self.cat_dirs.get(category) {
            d.clone()
        } else if category == GENERAL {
            self.base_dir.clone()
        } else {
            Path::new(&self.base_dir).join(category).display().to_string()
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    settings: Settings,
    items: Vec<Info>,
    next_id: u64,
}

/// A download handed over by the browser extension.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Incoming {
    pub url: String,
    pub referrer: String,
    pub cookies: String,
    pub filename: String,
    pub mime: String,
    pub force: bool,
    /// The browser's proxy (`Info::browser_proxy`).
    pub proxy: String,
    /// "Download all links": the page's links; `url` is the page.
    pub links: Vec<Link>,
    /// From the command line:
    /// folder (/p), silent (/n), add to queue only (/a), exit when done (/q), start the main queue (/s).
    pub cli: bool,
    pub folder: String,
    pub silent: bool,
    pub later: bool,
    pub quit: bool,
    pub start_queue: bool,
}

/// Command-line arguments as an `Incoming` (None when there are none).
pub fn cli_args(args: &[String]) -> Option<Incoming> {
    let mut inc = Incoming { cli: true, force: true, ..Default::default() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_ascii_lowercase().as_str() {
            "/d" => inc.url = it.next()?.clone(),
            "/p" => inc.folder = it.next()?.clone(),
            "/f" => inc.filename = it.next()?.clone(),
            "/n" => inc.silent = true,
            "/a" => inc.later = true,
            "/q" => inc.quit = true,
            "/s" => inc.start_queue = true,
            _ => {} // /h hangs up a dial-up connection: nothing to do
        }
    }
    (!inc.url.is_empty() || inc.start_queue).then_some(inc)
}

#[test]
fn command_line() {
    let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let i = cli_args(&a("/n /d http://x.com/a.zip /p C:\\dl /f b.zip /a")).unwrap();
    assert!((i.silent, i.later, i.start_queue) == (true, true, false) && i.url == "http://x.com/a.zip" && i.folder == "C:\\dl" && i.filename == "b.zip");
    assert!(cli_args(&a("/s")).unwrap().start_queue);
    assert!(cli_args(&[]).is_none() && cli_args(&a("/d")).is_none());
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Link {
    pub url: String,
    pub text: String,
}

pub struct Core {
    pub quota: Mutex<Quota>,
    /// Text of NDM's "Download limits exceeded!" message (template 194) for the UI to show.
    pub quota_msg: Mutex<Option<String>>,
    /// Queues that ran out of work; the UI applies their "when done" options.
    pub queue_finished: Mutex<Vec<u32>>,
    pub jobs: Mutex<Vec<Arc<Job>>>,
    pub settings: Mutex<Settings>,
    running: Mutex<BTreeSet<u32>>,
    pub limiter: Arc<Limiter>,
    /// One HTTP agent per proxy in use ("" = direct); rebuilt when the settings change.
    agents: Mutex<HashMap<String, ureq::Agent>>,
    next_id: AtomicU64,
    /// Per queue: the days its start / stop time last fired.
    fired: Mutex<HashMap<u32, (Option<NaiveDate>, Option<NaiveDate>)>>,
    /// Synchronization: files checked in the current pass (job id -> queue); next pass per queue.
    synced: Mutex<HashMap<u64, u32>>,
    again: Mutex<HashMap<u32, Instant>>,
    checking: Arc<Mutex<HashSet<u64>>>,
    store: PathBuf,
    /// Grabber projects: the days their start / stop time fired, periodic next runs, running projects,
    /// and the downloads a scheduled run added (for its when-done options).
    grab_fired: Mutex<HashMap<String, (Option<NaiveDate>, Option<NaiveDate>)>>,
    grab_again: Mutex<HashMap<String, Instant>>,
    grab_running: Mutex<HashSet<String>>,
    grab_watch: Mutex<Vec<(Vec<u64>, Sched)>>,
    /// Grabbed batches (job id, address) whose pages get their links converted once all have finished.
    grab_convert: Mutex<Vec<Vec<(u64, String)>>>,
    /// "Start Download checked files": downloads the grabber runs itself, Grabber Settings' N at a time.
    pub grab_dl_ids: Mutex<Vec<u64>>,
    /// Manual login page: the site whose cookies the browser extension should send (`POST /cookies`), and
    /// the latest it sent.
    pub login_wait: Mutex<Option<String>>,
    pub login_cookies: Mutex<String>,
    /// When-done options (exit / power off / open a file) due now, applied by the UI like a finished queue's.
    pub sched_done: Mutex<Vec<Sched>>,
    /// Dial Up / VPN (`ensure_online`): one dial at a time, its status for the "Dial up" box, Cancel / Redial.
    dial_lock: Mutex<()>,
    pub dial_text: Mutex<Option<String>>,
    pub dial_cancel: std::sync::atomic::AtomicBool,
    pub dial_now: std::sync::atomic::AtomicBool,
}

pub fn store_path() -> PathBuf {
    // %APPDATA% on Windows, ~/.config elsewhere
    let base = std::env::var("APPDATA").or_else(|_| std::env::var("XDG_CONFIG_HOME")).or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config"))).unwrap_or_else(|_| ".".into());
    Path::new(&base).join("NewDownloadManager").join("state.json")
}

impl Core {
    pub fn load() -> Arc<Core> {
        let store = store_path();
        let text = std::fs::read_to_string(&store).ok();
        let saved: Saved = text.as_deref().map_or(Ok(Saved::default()), serde_json::from_str).unwrap_or_else(|e| {
            // keep the unreadable list instead of letting autosave overwrite it with an empty one
            eprintln!("state.json unreadable ({e}), kept as state.json.bad");
            let _ = std::fs::copy(&store, store.with_extension("json.bad"));
            Saved::default()
        });
        Core::from_saved(saved, store)
    }

    fn from_saved(mut saved: Saved, store: PathBuf) -> Arc<Core> {
        let s = &mut saved.settings;
        s.limit_on |= s.limit_startup;
        if !s.queues.iter().any(|q| q.id == MAIN_QUEUE) {
            s.queues.insert(0, Queue::default());
        }
        if !s.queues.iter().any(|q| q.fixed) {
            let id = s.queues.iter().map(|q| q.id).max().unwrap_or(0) + 1;
            s.queues.insert(1, Queue::sync_default(id));
        }
        if let Some(old) = s.old_sched.take() {
            s.queues[0].sched = old;
        }
        if s.grab_recent.is_empty() {
            // lists saved before Recent Projects was kept: the newest saved projects
            s.grab_recent = s.grab_projects.iter().rev().take(4).map(|p| p.opts.name.clone()).collect();
        }
        if let Some(old) = s.old_max_active.take() {
            s.queues[0].max_active = old;
        }
        let on_startup: BTreeSet<u32> = s.queues.iter().filter(|q| q.sched.on_startup).map(|q| q.id).collect();
        let jobs = saved
            .items
            .into_iter()
            .map(|mut i| {
                if matches!(i.status, Status::Connecting | Status::Downloading) {
                    i.status = Status::Stopped;
                }
                Job::new(i)
            })
            .collect();
        let core = Arc::new(Core {
            jobs: Mutex::new(jobs),
            agents: Mutex::default(),
            settings: Mutex::new(saved.settings),
            running: Mutex::new(on_startup),
            queue_finished: Mutex::default(),
            quota: Mutex::default(),
            quota_msg: Mutex::default(),
            limiter: Arc::new(Limiter::default()),
            next_id: AtomicU64::new(saved.next_id.max(1)),
            fired: Mutex::default(),
            synced: Mutex::default(),
            again: Mutex::default(),
            checking: Arc::default(),
            store,
            grab_fired: Mutex::default(),
            grab_again: Mutex::default(),
            grab_running: Mutex::default(),
            grab_watch: Mutex::default(),
            grab_convert: Mutex::default(),
            grab_dl_ids: Mutex::default(),
            login_wait: Mutex::default(),
            login_cookies: Mutex::default(),
            sched_done: Mutex::default(),
            dial_lock: Mutex::default(),
            dial_text: Mutex::default(),
            dial_cancel: Default::default(),
            dial_now: Default::default(),
        });
        core.apply_settings();
        core
    }

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().unwrap()
    }

    pub fn jobs(&self) -> Vec<Arc<Job>> {
        self.jobs.lock().unwrap().clone()
    }

    pub fn find(&self, id: u64) -> Option<Arc<Job>> {
        self.jobs().into_iter().find(|j| j.info().id == id)
    }

    /// Push limiter/proxy settings into the running engine.
    pub fn apply_settings(&self) {
        let s = self.settings().clone();
        self.limiter.rate.store(if s.limit_on { s.limit_kb * 1024 } else { 0 }, Relaxed);
        self.agents.lock().unwrap().clear();
        let pasv = s.ftp_pasv;
        let routes = s;
        *engine::FTP.lock().unwrap() = engine::FtpCfg { pasv, route: Some(Arc::new(move |u: &str| routes.proxy_for(u))) };
    }

    /// The agent for downloading `url`, with the proxy the settings choose for it.
    pub fn agent(&self, url: &str) -> ureq::Agent {
        let (proxy, ua) = {
            let s = self.settings();
            (s.proxy_for(url), s.user_agent.clone())
        };
        let proxy = crate::net::agent_proxy(&proxy);
        self.agents.lock().unwrap().entry(proxy.clone()).or_insert_with(|| engine::agent(&proxy, &ua)).clone()
    }

    /// The networks `url` is downloaded over, each with its agent: none unless the setting is on, the
    /// address goes direct, NDM can choose the network and at least two ticked networks are up.
    pub fn lanes(&self, url: &str) -> Vec<(String, ureq::Agent)> {
        let (on, off, ua) = {
            let s = self.settings();
            (s.use_lanes(url), s.nets_off.clone(), s.user_agent.clone())
        };
        if !on || !crate::net::nic_supported() {
            return vec![];
        }
        let nets: Vec<String> = crate::win::nics().into_iter().map(|n| n.name).filter(|n| !off.contains(n)).collect();
        if nets.len() < 2 {
            return vec![];
        }
        let mut agents = self.agents.lock().unwrap();
        nets.into_iter()
            .map(|n| {
                let proxy = crate::net::local_proxy(&format!("nic://{}", percent_encoding::utf8_percent_encode(&n, percent_encoding::NON_ALPHANUMERIC)));
                let a = agents.entry(proxy.clone()).or_insert_with(|| engine::agent(&proxy, &ua)).clone();
                (n, a)
            })
            .collect()
    }

    pub fn new_info(&self, url: &str) -> Info {
        let s = self.settings();
        Info {
            id: self.next_id.fetch_add(1, Relaxed),
            url: s.with_login(url),
            conns: s.conns,
            category: GENERAL.into(),
            folder: s.base_dir.clone(),
            temp: s.temp_dir.trim().to_string(),
            added: Local::now().timestamp(),
            ..Default::default()
        }
    }

    /// Name not used by any other download heading to the same folder.
    pub fn unique_name(&self, folder: &str, name: &str) -> String {
        let jobs = self.jobs();
        engine::unique_name(folder, name, &|n| jobs.iter().any(|j| {
            let i = j.info();
            i.folder == folder && i.name == n && i.status != Status::Complete
        }))
    }

    /// Queue-added downloads with an unknown name get one (and a category) from the URL.
    pub fn add(&self, mut info: Info) -> Arc<Job> {
        if info.name.is_empty() {
            let name = engine::sanitize(&engine::url_name(&info.url));
            if name != "download" {
                info.category = self.settings().category_for(&name, &info.url);
                info.folder = self.settings().folder_for(&info.category);
                info.name = self.unique_name(&info.folder, &name);
            }
        }
        let job = Job::new(info);
        self.jobs.lock().unwrap().push(job.clone());
        job
    }

    /// Put a download that already exists (started from "Download File Info") in the list.
    pub fn adopt(&self, job: Arc<Job>) {
        self.jobs.lock().unwrap().push(job);
    }

    /// Take a download out of the list, leaving its files alone. Waits (briefly) until it stopped writing.
    pub fn detach(&self, id: u64) {
        let Some(j) = self.find(id) else { return };
        j.stop();
        let until = Instant::now() + Duration::from_secs(3);
        while j.running() && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
        self.jobs.lock().unwrap().retain(|x| !Arc::ptr_eq(x, &j));
    }

    /// "Use Windows Dial Up / VPN Networking" (Options page 185):
    /// before a download connects, the chosen connection is dialled unless one is up, with the
    /// user name and password Windows keeps for it. A failed dial is redialled after "Time between redial
    /// attempts" (attempts 0 = endlessly), shown in NDM's "Dial up" box (184, `dial_text`). Downloads
    /// starting meanwhile wait for the same dial.
    pub fn ensure_online(&self) -> Result<(), String> {
        let (on, entry, tries, wait) = {
            let s = self.settings();
            (s.dial_on, s.dial_entry.clone(), s.dial_redial, s.dial_wait)
        };
        if !on || entry.is_empty() {
            return Ok(());
        }
        let _one = self.dial_lock.lock().unwrap();
        if crate::win::ras_connected() {
            return Ok(());
        }
        self.dial_cancel.store(false, Relaxed);
        let say = |t: Option<String>| *self.dial_text.lock().unwrap() = t;
        let mut n = 1;
        let r = loop {
            say(Some(crate::i18n::tr("Connecting ...")));
            let e = match crate::win::ras_dial(&entry) {
                Ok(()) => break Ok(()),
                Err(e) => e,
            };
            if tries > 0 && n >= tries {
                break Err(e);
            }
            n += 1;
            // ponytail: a dial in progress can't be cancelled (RasDial runs synchronously); the wait can
            for left in (1..=wait.max(1)).rev() {
                if self.dial_cancel.load(Relaxed) || self.dial_now.swap(false, Relaxed) {
                    break;
                }
                say(Some(format!("{e}\n{}", crate::i18n::trf("Redial  %ld", &[&left]))));
                thread::sleep(Duration::from_secs(1));
            }
            if self.dial_cancel.load(Relaxed) {
                break Err(e);
            }
        };
        say(None);
        r
    }

    pub fn start(self: &Arc<Self>, job: &Arc<Job>) {
        if self.settings().dial_on && !crate::win::ras_connected() {
            // dial first, off the UI thread; the download starts once connected
            let (core, job) = (self.clone(), job.clone());
            job.info().status = Status::Connecting;
            thread::spawn(move || match core.ensure_online() {
                Ok(()) => core.start_now(&job),
                Err(e) => job.info().status = Status::Error(e),
            });
            return;
        }
        self.start_now(job);
    }

    fn start_now(&self, job: &Arc<Job>) {
        let url = job.info().url.clone();
        {
            let s = self.settings();
            job.set_date.store(s.set_date, Relaxed);
            job.ignore_date.store(s.ignore_date, Relaxed);
            let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
            if let Some((_, n)) = s.server_conns.iter().find(|(m, _)| crate::grabber::mask_match(m, &host)) {
                job.info().conns = *n;
            }
        }
        // a download from the browser that failed before goes through the browser's own proxy
        let bp = job.info().browser_proxy.clone();
        let (ua, on) = { let s = self.settings(); (s.user_agent.clone(), s.proxy_browser) }; // one lock: a second one in the same statement deadlocks
        let bp = match bp.strip_prefix("pac:") {
            Some(pac) => crate::win::pac_proxy(pac, &url).map_or(String::new(), |hp| proxy_url("http", &hp, "", "", "")),
            None => bp,
        };
        // ponytail: used from the next start (Resume, queue retries), not by restarting the failed one at once
        let browser = on && !bp.is_empty() && job.fails.load(Relaxed) > 0;
        let agent = if browser { engine::agent(&bp, &ua) } else { self.agent(&url) };
        *job.lanes.lock().unwrap() = if browser { vec![] } else { self.lanes(&url) };
        job.start(agent, self.limiter.clone());
    }

    pub fn queue_running(&self, q: u32) -> bool {
        self.running.lock().unwrap().contains(&q)
    }

    /// Play the sound chosen for event `ev` (index into `SOUND_EVENTS`), if any.
    pub fn sound(&self, ev: usize) {
        let (on, path) = self.settings().sounds[ev].clone();
        if on {
            crate::win::play_sound(&path);
        }
    }

    pub fn start_queue(&self, q: u32) {
        if self.running.lock().unwrap().insert(q) {
            self.synced.lock().unwrap().retain(|_, x| *x != q); // a new pass checks every file again
            self.sound(2);
        }
    }

    pub fn stop_queue(&self, q: u32) {
        if self.running.lock().unwrap().remove(&q) {
            self.sound(3);
        }
        for j in self.jobs() {
            let i = j.info();
            if i.queued && i.queue == q {
                j.stop();
            }
        }
    }

    /// "Stop All": every queue and every download.
    pub fn stop_all(&self) {
        self.running.lock().unwrap().clear();
        self.jobs().iter().for_each(|j| j.stop());
    }

    /// Scheduler "New queue": NDM names it "Queue # <id>" before the user renames it.
    pub fn add_queue(&self, name: &str) -> u32 {
        let mut s = self.settings();
        let id = s.queues.iter().map(|q| q.id).max().unwrap_or(0) + 1;
        let name = if name.trim().is_empty() { format!("Queue # {id}") } else { name.trim().to_string() };
        s.queues.push(Queue { id, name, ..Default::default() });
        id
    }

    /// Delete a queue; its files stay in the list but leave the queue. The main queue can't be deleted.
    pub fn delete_queue(&self, q: u32) {
        if self.settings().queue(q).is_none_or(Queue::builtin) {
            return;
        }
        self.stop_queue(q);
        for j in self.jobs() {
            let mut i = j.info();
            if i.queue == q {
                (i.queued, i.queue) = (false, MAIN_QUEUE);
            }
        }
        self.settings().queues.retain(|x| x.id != q);
    }

    /// Put downloads in queue `q`, or take them out of their queue (None).
    pub fn set_queue(&self, ids: &[u64], q: Option<u32>) {
        let sync = q.is_some_and(|q| self.settings().queue(q).is_some_and(|x| x.sync));
        for j in ids.iter().filter_map(|id| self.find(*id)) {
            let mut i = j.info();
            i.queued = q.is_some() && (sync || i.status != Status::Complete);
            i.queue = q.unwrap_or(MAIN_QUEUE);
        }
    }

    /// Options -> Connection "Download limits". The window starts with the first counted byte and lasts
    /// `quota_hours`; reaching 98% of the quota stops every download until the window ends, then the
    /// stopped downloads resume.
    fn tick_quota(self: &Arc<Self>) {
        let (on, mb, hours, warn) = {
            let s = self.settings();
            (s.quota_on, s.quota_mb, s.quota_hours.clamp(1, 23), s.quota_warn)
        };
        let now = Local::now();
        let got = self.limiter.received.load(Relaxed);
        let mut q = self.quota.lock().unwrap();
        if !on {
            // turning the limit off lifts a stop in force, like NDM's "change settings... and press Resume"
            q.start = None;
            q.stopped = None;
            return;
        }
        let (start, base) = *q.start.get_or_insert((now, got));
        let end = start + chrono::Duration::hours(hours as i64);
        if now >= end {
            q.start = Some((now, got));
            if let Some((_, ids)) = q.stopped.take() {
                drop(q);
                ids.iter().filter_map(|id| self.find(*id)).for_each(|j| self.start(&j));
            }
            return;
        }
        if q.stopped.is_some() {
            // keep everything stopped until the window ends (a manual resume is stopped again)
            self.jobs().iter().filter(|j| j.running()).for_each(|j| j.stop());
            return;
        }
        if got.saturating_sub(base) >= quota_bytes(mb) {
            let ids: Vec<u64> = self.jobs().iter().filter(|j| j.running()).map(|j| j.info().id).collect();
            ids.iter().filter_map(|id| self.find(*id)).for_each(|j| j.stop());
            q.stopped = Some((end, ids));
            if warn {
                let hm = |t: chrono::DateTime<Local>| t.format("%H:%M").to_string();
                *self.quota_msg.lock().unwrap() = Some(format!(
                    "From {} to {} you downloaded {} MB. All downloads have been stopped at {} because you had exceeded your download limits set in 'Options->Connection'!\r\n\r\nAll stopped downloads will be resumed automatically at {}. If you want to resume downloads immediately, change settings in Download Limits and press on Resume.",
                    hm(start), hm(now), (got - base) / (1024 * 1024), hm(now), hm(end)
                ));
            }
        }
    }

    pub fn quota_blocked(&self) -> bool {
        self.quota.lock().unwrap().stopped.is_some()
    }

    fn tick_queue(self: &Arc<Self>) {
        if self.quota_blocked() {
            return;
        }
        let running: Vec<u32> = self.running.lock().unwrap().iter().copied().collect();
        let jobs = self.jobs();
        for q in running {
            let (max, retries, sync, again) = self.settings().queue(q).map_or((1, 0, false, None), |x| {
                let again = Some(x.sched.again_h as u64 * 3600 + x.sched.again_m as u64 * 60).filter(|s| x.sync && x.sched.again_on && *s > 0);
                (x.max_active.max(1), if x.sched.retry_on { x.sched.retries } else { 0 }, x.sync, again)
            });
            let mine: Vec<&Arc<Job>> = jobs.iter().filter(|j| {
                let i = j.info();
                i.queued && i.queue == q
            }).collect();
            let mut active = mine.iter().filter(|j| j.running()).count();
            let mut pending = false;
            for j in &mine {
                let status = j.info().status.clone();
                // failed after the queue's retries: the user decides
                let failed = matches!(status, Status::Error(_)) && j.fails.load(Relaxed) > retries;
                let id = j.info().id;
                if status == Status::Complete && sync && !self.synced.lock().unwrap().contains_key(&id) {
                    // synchronization: ask the server whether the file changed (size or date); if so it is
                    // downloaded again on a later tick
                    self.synced.lock().unwrap().insert(id, q);
                    self.checking.lock().unwrap().insert(id);
                    let (j, agent, checking) = ((*j).clone(), self.agent(&j.info().url), self.checking.clone());
                    thread::spawn(move || {
                        let (url, referrer, cookies) = {
                            let i = j.info();
                            (i.url.clone(), i.referrer.clone(), i.cookies.clone())
                        };
                        if let Ok(p) = engine::probe(&agent, &url, &referrer, &cookies) {
                            let i = j.info().clone();
                            if p.size != i.size || (!p.modified.is_empty() && p.modified != i.modified) || !i.path().exists() {
                                j.reset();
                            }
                        }
                        checking.lock().unwrap().remove(&id);
                    });
                }
                if self.checking.lock().unwrap().contains(&id) {
                    pending = true;
                    continue;
                }
                if j.running() || failed || status == Status::Complete {
                    continue;
                }
                pending = true;
                if active < max {
                    self.start(j);
                    active += 1;
                }
            }
            if !pending && active == 0 {
                if let Some(secs) = again {
                    self.again.lock().unwrap().insert(q, Instant::now() + Duration::from_secs(secs));
                }
                self.running.lock().unwrap().remove(&q);
                self.queue_finished.lock().unwrap().push(q);
                self.sound(3);
            }
        }
    }

    /// Download complete / failed sounds; NDM plays them where it finishes a download (0x4725c0).
    fn tick_sounds(&self) {
        for j in self.jobs() {
            if j.ended.swap(false, Relaxed) {
                self.sound(if j.info().status == Status::Complete { 0 } else { 1 });
            }
        }
    }

    fn tick_scheduler(&self) {
        let now = Local::now();
        let (hm, today) = (now.format("%H:%M").to_string(), now.date_naive());
        use chrono::Datelike;
        let (mut start, mut stop) = (vec![], vec![]);
        {
            let mut s = self.settings();
            let mut fired = self.fired.lock().unwrap();
            for q in &mut s.queues {
                let f = fired.entry(q.id).or_default();
                let sc = &mut q.sched;
                let day_ok = !sc.daily || sc.days[now.weekday().num_days_from_sunday() as usize];
                if sc.start_on && day_ok && hm == sc.start_at && f.0 != Some(today) {
                    f.0 = Some(today);
                    sc.start_on = sc.daily;
                    start.push(q.id);
                }
                if sc.stop_on && hm == sc.stop_at && f.1 != Some(today) {
                    f.1 = Some(today);
                    sc.stop_on = sc.daily;
                    stop.push(q.id);
                }
            }
        }
        let due: Vec<u32> = {
            let mut again = self.again.lock().unwrap();
            let due = again.iter().filter(|(_, t)| **t <= Instant::now()).map(|(q, _)| *q).collect::<Vec<_>>();
            due.iter().for_each(|q| _ = again.remove(q));
            due
        };
        start.into_iter().chain(due).for_each(|q| self.start_queue(q));
        stop.into_iter().for_each(|q| self.stop_queue(q));
    }

    pub fn grab_tuning(&self) -> crate::grabber::Tuning {
        let s = self.settings();
        crate::grabber::Tuning { explore: s.grab_explore, adv: s.grab_adv }
    }

    /// Folder of a grabbed file. Save To: 0 = its category's folder, 1 = `save_cat`'s, 2 = `folder`
    /// (+ the site's own folders with "Use original relative subfolders").
    pub fn grab_dir(&self, name: &str, url: &str, save_mode: u8, save_cat: &str, folder: &str, subfolders: bool) -> String {
        let s = self.settings();
        match save_mode {
            1 => s.folder_for(save_cat),
            2 if subfolders => url::Url::parse(url).map_or(folder.into(), |u| Path::new(folder).join(crate::grabber::site_dir(&u)).display().to_string()),
            2 => folder.into(),
            _ => s.folder_for(&s.category_for(name, url)),
        }
    }

    /// Add the grabber's checked files to the main list, unticking them: queued in the main queue, or with
    /// `start` downloaded by the grabber itself (Grabber Settings' N at a time, `tick_grabber`).
    /// Existing names get NDM's grabber numbering (index_2.html) unless "Overwrite existing files".
    pub fn grab_files(&self, files: &mut [crate::grabber::Found], opts: &crate::grabber::Opts, save_mode: u8, save_cat: &str, folder: &str, start: bool) -> Vec<u64> {
        let mut batch = vec![];
        let (link_text, ie_cache) = { let s = self.settings(); (s.grab_link_text, s.grab_ie_cache) };
        for f in files.iter_mut().filter(|f| f.checked) {
            f.checked = false;
            let mut i = self.new_info(&f.url);
            (i.queued, i.cookies, i.ie_cache) = (!start, opts.cookies.clone(), ie_cache);
            if link_text {
                i.description = f.text.clone();
            }
            i.folder = self.grab_dir(&f.name, &f.url, save_mode, save_cat, folder, opts.subfolders);
            i.category = self.settings().category_for(&f.name, &f.url);
            i.name = if opts.overwrite { f.name.clone() } else { crate::grabber::numbered(&f.name, &|n| self.unique_name(&i.folder, n) == n) };
            batch.push((i.id, f.url.clone()));
            self.add(i);
        }
        let ids = batch.iter().map(|b| b.0).collect();
        if opts.convert && !batch.is_empty() {
            self.grab_convert.lock().unwrap().push(batch);
        }
        if start {
            self.grab_dl_ids.lock().unwrap().extend(&ids);
        }
        ids
    }

    /// Scheduled grabber projects (template 284): start / stop times like a queue's, "Start again every"
    /// for periodic ones; a run explores and/or downloads, then its when-done options wait for the files.
    fn tick_grabber(self: &Arc<Self>) {
        let now = Local::now();
        let (hm, today) = (now.format("%H:%M").to_string(), now.date_naive());
        use chrono::Datelike;
        let mut due = vec![];
        let mut stop = false;
        {
            let mut s = self.settings();
            let mut fired = self.grab_fired.lock().unwrap();
            let mut again = self.grab_again.lock().unwrap();
            for p in &mut s.grab_projects {
                let f = fired.entry(p.opts.name.clone()).or_default();
                let sc = &p.sched;
                let day_ok = !sc.daily || sc.days[now.weekday().num_days_from_sunday() as usize];
                let timed = sc.start_on && day_ok && hm == sc.start_at && f.0 != Some(today);
                let periodic = again.get(&p.opts.name).is_some_and(|t| *t <= Instant::now());
                if timed || periodic {
                    f.0 = Some(today);
                    p.sched.start_on = p.sched.daily || p.periodic;
                    again.remove(&p.opts.name);
                    due.push(p.clone());
                }
                if p.sched.stop_on && hm == p.sched.stop_at && f.1 != Some(today) {
                    f.1 = Some(today);
                    p.sched.stop_on = p.sched.daily;
                    stop = true;
                }
            }
        }
        if stop {
            self.stop_queue(MAIN_QUEUE);
        }
        for p in due {
            self.run_project(p);
        }
        // when-done options once every file of a scheduled run has finished (or failed)
        let mut watch = self.grab_watch.lock().unwrap();
        watch.retain(|(ids, sc)| {
            let over = ids.iter().filter_map(|id| self.find(*id)).all(|j| matches!(j.info().status, Status::Complete | Status::Error(_)));
            if over {
                self.sched_done.lock().unwrap().push(sc.clone());
            }
            !over
        });
        drop(watch);
        // the grabber's own downloads: N at a time; finished, failed, stopped by the user or deleted leave the list
        let n = self.settings().grab_dl.clamp(1, 10);
        let mut dl = self.grab_dl_ids.lock().unwrap();
        let mut active = 0;
        dl.retain(|id| {
            let Some(j) = self.find(*id) else { return false };
            let (st, started) = { let i = j.info(); (i.status.clone(), i.done() > 0 || j.running()) };
            if j.running() {
                active += 1;
                return true;
            }
            if st != Status::Stopped || started {
                return false;
            }
            if active < n && !self.quota_blocked() {
                self.start(&j);
                active += 1;
            }
            true
        });
        drop(dl);
        // ponytail: pending conversions live in memory only; a restart before the batch ends skips them
        let mut conv = self.grab_convert.lock().unwrap();
        conv.retain(|batch| {
            let infos: Vec<(String, Info)> = batch.iter().filter_map(|(id, u)| Some((u.clone(), self.find(*id)?.info().clone()))).collect();
            if !infos.iter().all(|(_, i)| matches!(i.status, Status::Complete | Status::Error(_))) {
                return true;
            }
            let local: HashMap<String, PathBuf> = infos.iter().filter(|(_, i)| i.status == Status::Complete).map(|(u, i)| (u.clone(), i.path())).collect();
            for (u, p) in &local {
                let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                let (Ok(base), true) = (url::Url::parse(u), ["htm", "html", "shtml", "xhtml", "php", "asp", "aspx", "jsp"].contains(&ext.as_str())) else { continue };
                let Ok(bytes) = std::fs::read(p) else { continue };
                // non-UTF-8 pages go through Latin-1 so every byte comes back unchanged (new links are ASCII)
                let utf8 = std::str::from_utf8(&bytes).is_ok();
                let html: String = if utf8 { String::from_utf8_lossy(&bytes).into() } else { bytes.iter().map(|&b| b as char).collect() };
                let out = crate::grabber::localize(&html, &base, p, &|l| local.get(l.as_str()).cloned());
                let _ = std::fs::write(p, if utf8 { out.into_bytes() } else { out.chars().map(|c| c as u8).collect() });
            }
            false
        });
    }

    /// One scheduled run of a grabber project, in the background.
    pub fn run_project(self: &Arc<Self>, p: GrabProject) {
        if !self.grab_running.lock().unwrap().insert(p.opts.name.clone()) {
            return;
        }
        let core = self.clone();
        thread::spawn(move || {
            let mut files = p.files.clone();
            if p.action != 2 {
                let c = Arc::new(crate::grabber::Crawl::default());
                crate::grabber::run(p.opts.clone(), core.agent(&p.opts.start), c.clone(), core.grab_tuning());
                let st = c.state.lock().unwrap();
                // a periodic project only fetches what is new since its last run
                let old: HashSet<&str> = p.files.iter().map(|f| f.url.as_str()).collect();
                files = st.files.iter().filter(|f| !p.periodic || !old.contains(f.url.as_str())).cloned().collect();
                if let Some(x) = core.settings().grab_projects.iter_mut().find(|x| x.opts.name == p.opts.name) {
                    let mut all = x.files.clone();
                    all.extend(st.files.iter().filter(|f| !old.contains(f.url.as_str())).cloned().map(|mut f| { f.checked = false; f }));
                    (x.files, x.pages) = (all, st.pages);
                }
            }
            if p.action != 0 {
                let ids = core.grab_files(&mut files, &p.opts, p.save_mode, &p.save_cat, &p.folder, true);
                let sc = &p.sched;
                if sc.exit_done || sc.off_done || sc.open_on || sc.hangup {
                    core.grab_watch.lock().unwrap().push((ids, sc.clone()));
                }
            }
            if p.periodic && p.sched.again_on {
                let secs = (p.sched.again_h as u64 * 3600 + p.sched.again_m as u64 * 60).max(60);
                core.grab_again.lock().unwrap().insert(p.opts.name.clone(), Instant::now() + Duration::from_secs(secs));
            }
            core.grab_running.lock().unwrap().remove(&p.opts.name);
            core.save();
        });
    }

    pub fn save(&self) {
        let saved = Saved {
            settings: self.settings().clone(),
            items: self.jobs().iter().map(|j| j.info().clone()).collect(),
            next_id: self.next_id.load(Relaxed),
        };
        let Ok(json) = serde_json::to_string_pretty(&saved) else { return };
        let tmp = self.store.with_extension("tmp");
        let r = std::fs::create_dir_all(self.store.parent().unwrap())
            .and_then(|_| std::fs::write(&tmp, json))
            .and_then(|_| std::fs::rename(&tmp, &self.store));
        if let Err(e) = r {
            eprintln!("save failed: {e}");
        }
    }

    /// "Use advanced browser integration" (`on`), done without NDM's network driver: Windows' proxy for
    /// plain http is set to NDM's (`proxy_conn`) and the user's own proxy kept for the other protocols;
    /// off puts the old setting back. Run at start (on), on Options OK and at exit (off, so http works
    /// while NDM isn't running).
    pub fn apply_integration(&self, on: bool) {
        let mut s = self.settings();
        match (on, s.saved_proxy.clone()) {
            (true, None) => {
                let old = crate::win::system_proxy();
                let mut server = vec![format!("http=127.0.0.1:{PROXY_PORT}")];
                if old.0 && old.1.contains('=') {
                    server.extend(old.1.split(';').filter(|p| !p.trim().is_empty() && !p.starts_with("http=")).map(String::from));
                } else if old.0 && !old.1.trim().is_empty() {
                    server.extend(["https", "ftp"].map(|k| format!("{k}={}", old.1.trim())));
                }
                let bypass = if old.0 && !old.2.is_empty() { old.2.clone() } else { "<local>".into() };
                crate::win::set_system_proxy(true, &server.join(";"), &bypass);
                s.saved_proxy = Some(old);
            }
            (false, Some((was_on, server, bypass))) => {
                crate::win::set_system_proxy(was_on, &server, &bypass);
                s.saved_proxy = None;
            }
            _ => {}
        }
    }

    /// Puts the user's proxy back (every exit path comes here), stops the downloads and saves.
    pub fn shutdown(&self) {
        self.apply_integration(false);
        let jobs = self.jobs();
        jobs.iter().for_each(|j| j.stop());
        let until = Instant::now() + Duration::from_secs(5);
        while jobs.iter().any(|j| j.running()) && Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
        self.save();
    }

    pub fn spawn_background(self: &Arc<Self>, notify: impl Fn() + Send + 'static) {
        let core = self.clone();
        thread::spawn(move || {
            let mut last_save = Instant::now();
            loop {
                core.tick_scheduler();
                core.tick_grabber();
                core.tick_quota();
                core.tick_queue();
                core.tick_sounds();
                if last_save.elapsed() > Duration::from_secs(5) {
                    core.save();
                    last_save = Instant::now();
                }
                notify();
                thread::sleep(Duration::from_millis(500));
            }
        });
    }

    /// Is the browser that sent a request ticked in "Capture downloads from the following browsers"?
    /// Browsers not in the list (or not recognised) are captured.
    pub fn captures(&self, ua: &str) -> bool {
        let kind = browser_kind(ua);
        let s = self.settings();
        let mine: Vec<&Browser> = s.browsers.iter().filter(|b| !kind.is_empty() && browser_kind(&format!("{} {}", b.name, b.exe)) == kind).collect();
        mine.is_empty() || mine.iter().any(|b| b.on)
    }

    /// Should a download the browser started be taken over?
    pub fn wants(&self, inc: &Incoming) -> bool {
        if !inc.url.starts_with("http://") && !inc.url.starts_with("https://") {
            return false;
        }
        if inc.force {
            return true;
        }
        let host = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
        let (h, r) = (host(&inc.url), host(&inc.referrer));
        if self.settings().skip_urls.iter().any(|p| crate::grabber::mask_match(p, &inc.url)) {
            return false;
        }
        if self.settings().skip_sites.split_whitespace().map(str::to_ascii_lowercase).any(|s| (!h.is_empty() && h.ends_with(&s)) || (!r.is_empty() && r.ends_with(&s))) {
            return false;
        }
        let name = if inc.filename.is_empty() { engine::url_name(&inc.url) } else { inc.filename.replace('\\', "/") };
        let e = ext(name.rsplit('/').next().unwrap_or(""));
        if e.is_empty() {
            return !inc.mime.starts_with("text/") && !inc.mime.starts_with("image/");
        }
        self.settings().file_types.split_whitespace().any(|t| t.trim_start_matches('.').eq_ignore_ascii_case(&e))
    }

    /// Localhost endpoint for the browser extension: POST /add with `X-NDM: 1`.
    /// Web pages can't send that header cross-origin without a CORS preflight, which we refuse,
    /// and we also reject any Origin that isn't a browser extension.
    pub fn spawn_server(self: &Arc<Self>, tx: Sender<Incoming>, notify: impl Fn() + Send + Sync + 'static) {
        let notify = Arc::new(notify);
        if let Ok(l) = TcpListener::bind(("127.0.0.1", PROXY_PORT)) {
            let (core, tx, notify) = (self.clone(), tx.clone(), notify.clone());
            thread::spawn(move || {
                for c in l.incoming().flatten() {
                    let (core, tx, notify) = (core.clone(), tx.clone(), notify.clone());
                    thread::spawn(move || {
                        if let Some(inc) = proxy_conn(&core, c) {
                            let _ = tx.send(inc);
                            notify();
                        }
                    });
                }
            });
        }
        let core = self.clone();
        thread::spawn(move || {
            let l = match TcpListener::bind(("127.0.0.1", PORT)) {
                Ok(l) => l,
                Err(e) => return eprintln!("browser integration disabled, port {PORT}: {e}"),
            };
            for s in l.incoming().flatten() {
                let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
                let req = handle(&s);
                let reply = |status: &str, body: &str| {
                    let _ = (&s).write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                };
                if req.as_ref().is_some_and(|(path, ..)| path == "/config") {
                    // what the extension shows: context menu items (334) and download panel exceptions (370)
                    let c = {
                        let st = core.settings();
                        let types: Vec<&str> = st.panel_types.iter().filter(|t| t.1).map(|t| t.0.as_str()).collect();
                        serde_json::json!({ "menu_dl": st.menu_dl, "menu_all": st.menu_all, "panel_skip": st.panel_skip, "keys": st.keys,
                            "panel_types": types, "panel_min": st.panel_min, "panel_mini": st.panel_mini, "panel_nocap": st.panel_nocap,
                            "panel_drm": st.panel_drm, "sel_mode": st.sel_mode, "sel_sites": st.sel_sites, "sel_mini": st.sel_mini,
                            "want_cookies": *core.login_wait.lock().unwrap() })
                    };
                    reply("200 OK", &c.to_string());
                    continue;
                }
                if let Some((_, body, _)) = req.as_ref().filter(|(path, ..)| path == "/cookies") {
                    // the grabber's manual login page: the browser's cookies for the site the user logged in to
                    let inc: Incoming = serde_json::from_slice(body).unwrap_or_default();
                    let host = url::Url::parse(&inc.url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
                    let want = core.login_wait.lock().unwrap().clone().unwrap_or_default();
                    if !want.is_empty() && crate::grabber::same_domain(&host, &want) {
                        *core.login_cookies.lock().unwrap() = inc.cookies;
                    }
                    reply("200 OK", "");
                    continue;
                }
                let accepted = req.and_then(|(_, body, ua)| serde_json::from_slice::<Incoming>(&body).ok().map(|i| (i, ua)))
                    .filter(|(inc, ua)| inc.cli || core.captures(ua) && core.wants(inc))
                    .map(|(inc, _)| inc);
                reply(if accepted.is_some() { "200 OK" } else { "204 No Content" }, "");
                if let Some(inc) = accepted {
                    let _ = tx.send(inc);
                    notify();
                }
            }
        });
    }
}

/// One program's connection to NDM's http proxy ("Use advanced browser integration"): the request goes
/// to the server; a download the browser-capture rules want (`captures`, `wants`) is returned for the app
/// and the program gets "HTTP/1.0 204 Intercepted by NDM"; anything else is passed through unchanged.
fn proxy_conn(core: &Core, client: std::net::TcpStream) -> Option<Incoming> {
    let head = |r: &mut dyn BufRead| -> Option<(String, Vec<(String, String)>)> {
        let mut first = String::new();
        r.read_line(&mut first).ok()?;
        let mut h = vec![];
        loop {
            let mut l = String::new();
            if r.read_line(&mut l).ok()? == 0 || l.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                h.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        Some((first.trim_end().to_string(), h))
    };
    let get = |h: &[(String, String)], k: &str| h.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map_or(String::new(), |x| x.1.clone());
    let hop = |k: &str| ["connection", "proxy-connection", "keep-alive", "proxy-authorization"].iter().any(|x| k.eq_ignore_ascii_case(x));
    let mut cr = BufReader::new(client.try_clone().ok()?);
    let (line, h) = head(&mut cr)?;
    let mut p = line.splitn(3, ' ');
    let (method, target, ver) = (p.next()?.to_string(), p.next()?.to_string(), p.next().unwrap_or("HTTP/1.1").to_string());
    let u = url::Url::parse(&target).ok().filter(|u| u.scheme() == "http")?;
    let mut out = &client;
    let Ok(mut server) = std::net::TcpStream::connect((u.host_str()?, u.port_or_known_default()?)) else {
        let _ = out.write_all(b"HTTP/1.0 502 Bad Gateway\r\n\r\n");
        return None;
    };
    // ponytail: one request per connection (Connection: close both ways); keep-alive if pages get slow
    let mut req = format!("{method} {} {ver}\r\n", &u[url::Position::BeforePath..]);
    h.iter().filter(|(k, _)| !hop(k)).for_each(|(k, v)| req += &format!("{k}: {v}\r\n"));
    server.write_all(format!("{req}Connection: close\r\n\r\n").as_bytes()).ok()?;
    let body: u64 = get(&h, "content-length").parse().unwrap_or(0);
    std::io::copy(&mut (&mut cr).take(body), &mut server).ok()?;
    let mut sr = BufReader::new(server.try_clone().ok()?);
    let (status, rh) = head(&mut sr)?;
    let code = status.split(' ').nth(1).unwrap_or("");
    if method == "GET" && (code == "200" || code == "206") {
        let inc = Incoming {
            url: target.clone(),
            referrer: get(&h, "referer"),
            cookies: get(&h, "cookie"),
            filename: engine::disposition_name(&get(&rh, "content-disposition")).unwrap_or_default(),
            mime: get(&rh, "content-type").split(';').next().unwrap_or("").trim().to_string(),
            ..Default::default()
        };
        if core.captures(&get(&h, "user-agent")) && core.wants(&inc) {
            let _ = out.write_all(b"HTTP/1.0 204 Intercepted by NDM\r\n\r\n");
            return Some(inc);
        }
    }
    let mut resp = format!("{status}\r\n");
    rh.iter().filter(|(k, _)| !hop(k)).for_each(|(k, v)| resp += &format!("{k}: {v}\r\n"));
    out.write_all(format!("{resp}Connection: close\r\n\r\n").as_bytes()).ok()?;
    let _ = std::io::copy(&mut sr, &mut out);
    None
}

/// An extension request: (path, body, User-Agent) of `POST /add` or `POST /config`.
fn handle(s: &std::net::TcpStream) -> Option<(String, Vec<u8>, String)> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut h = HashMap::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).ok()? == 0 || l.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            h.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let origin_ok = h.get("origin").is_none_or(|o| o.starts_with("chrome-extension://") || o.starts_with("moz-extension://"));
    let path = line.strip_prefix("POST ")?.split(' ').next()?.to_string();
    if !["/add", "/config", "/cookies"].contains(&path.as_str()) || h.get("x-ndm").map(String::as_str) != Some("1") || !origin_ok {
        return None;
    }
    let len: usize = h.get("content-length").map_or(Some(0), |l| l.parse().ok())?;
    let mut body = vec![0; len.min(1 << 20)];
    r.read_exact(&mut body).ok()?;
    Some((path, body, h.get("user-agent").cloned().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changes Windows' real proxy setting for a moment: run by hand (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn integration_restores_system_proxy() {
        let core = Core::load();
        core.settings().saved_proxy = None;
        let before = crate::win::system_proxy();
        core.apply_integration(true);
        let (on, server, _) = crate::win::system_proxy();
        assert!(on && server.starts_with(&format!("http=127.0.0.1:{PROXY_PORT}")), "{server}");
        let p = core.settings().proxy_for("http://x.com/a");
        assert_eq!(p, if before.0 { core.settings().proxy_for("http://x.com/a") } else { String::new() }, "NDM's own downloads skip its proxy");
        core.shutdown();
        assert_eq!(crate::win::system_proxy(), before);
    }

    #[test]
    fn integration_proxy() {
        // a server with a page and a file; the proxy passes the page through and takes the file
        let srv = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = srv.local_addr().unwrap().port();
        thread::spawn(move || {
            for s in srv.incoming().flatten() {
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap() <= 2 {
                        break;
                    }
                }
                let (ty, body) = if line.contains("/a.zip") { ("application/zip", "zipdata") } else { ("text/html", "<html>hi</html>") };
                let _ = (&s).write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        let core = Core::load();
        let px = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = px.local_addr().unwrap();
        let ask = |path: &str| {
            let c = std::net::TcpStream::connect(addr).unwrap();
            (&c).write_all(format!("GET http://127.0.0.1:{port}{path} HTTP/1.1\r\nHost: x\r\nUser-Agent: curl/8\r\nProxy-Connection: keep-alive\r\n\r\n").as_bytes()).unwrap();
            let inc = proxy_conn(&core, px.accept().unwrap().0);
            let mut got = String::new();
            BufReader::new(&c).read_to_string(&mut got).unwrap();
            (inc, got)
        };
        let (inc, got) = ask("/page.html");
        assert!(inc.is_none() && got.starts_with("HTTP/1.1 200") && got.ends_with("<html>hi</html>"), "{got}");
        let (inc, got) = ask("/a.zip");
        assert!(got.starts_with("HTTP/1.0 204"), "{got}");
        assert_eq!(inc.unwrap().url, format!("http://127.0.0.1:{port}/a.zip"));
    }

    #[test]
    fn site_logins() {
        let mut s = Settings::default();
        s.logins.push(Login { site: "https://files.example.com/".into(), user: "u".into(), pass: "p w".into() });
        assert_eq!(s.with_login("http://files.example.com/a.zip"), "http://u:p%20w@files.example.com/a.zip");
        assert_eq!(s.with_login("http://cdn.files.example.com/a.zip"), "http://u:p%20w@cdn.files.example.com/a.zip");
        assert_eq!(s.with_login("http://other.com/a.zip"), "http://other.com/a.zip");
        assert_eq!(s.with_login("http://x:y@files.example.com/a.zip"), "http://x:y@files.example.com/a.zip");
    }

    #[test]
    fn categories_and_capture() {
        let mut s = Settings::default();
        assert_eq!(s.category_for("a.ZIP", ""), "Compressed");
        assert_eq!(s.category_for("movie.mkv", "http://x.com/movie.mkv"), "Video");
        assert_eq!(s.category_for("noext", ""), GENERAL);
        s.categories.insert(0, Category { name: "Work".into(), exts: "zip".into(), sites_on: true, sites: "*.corp.com intranet".into(), ..Default::default() });
        assert_eq!(s.category_for("a.zip", "https://files.corp.com/a.zip"), "Work");
        assert_eq!(s.category_for("a.zip", "https://other.com/a.zip"), "Compressed");
        let core = Core::load();
        let inc = |url: &str, mime: &str| Incoming { url: url.into(), mime: mime.into(), ..Default::default() };
        assert!(core.wants(&inc("https://x.com/a.zip", "")));
        assert!(!core.wants(&inc("https://x.com/a.html", "")));
        assert!(core.wants(&inc("https://x.com/get?id=1", "application/octet-stream")));
        assert!(!core.wants(&inc("https://x.com/get?id=1", "text/html")));
        assert!(!core.wants(&inc("blob:https://x.com/1", "")));
        core.settings().skip_urls = vec!["https://x.com/dl/*".into()];
        assert!(!core.wants(&inc("https://x.com/dl/a.zip", "")));
        assert!(core.wants(&Incoming { force: true, ..inc("https://x.com/dl/a.zip", "") }), "the context menu still downloads");
        let edge_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 Edg/130.0";
        let chrome_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
        assert_eq!((browser_kind(edge_ua), browser_kind(chrome_ua), browser_kind("Microsoft Edge")), ("edge", "chrome", "edge"));
        core.settings().browsers = vec![
            Browser { name: "Microsoft Edge".into(), exe: r"C:\x\msedge.exe".into(), on: false },
            Browser { name: "Google Chrome".into(), exe: r"C:\x\chrome.exe".into(), on: true },
        ];
        assert!(!core.captures(edge_ua) && core.captures(chrome_ua) && core.captures("curl/8"));
    }

    #[test]
    fn queues() {
        // a state file from before queues: its schedule becomes the main queue's
        let old = r#"{"settings":{"max_active":5,"sched":{"start_at":"03:00","on_startup":true}},"items":[{"id":1,"queued":true,"status":"Complete"}],"next_id":2}"#;
        let core = Core::from_saved(serde_json::from_str(old).unwrap(), std::env::temp_dir().join("ndm-test-unused.json"));
        {
            let s = core.settings();
            assert_eq!(s.queues.len(), 2);
            assert!(s.queues[1].sync && s.queues[1].builtin(), "NDM's Synchronization queue is added");
            assert_eq!((s.queues[0].id, s.queues[0].max_active, s.queues[0].sched.start_at.as_str()), (MAIN_QUEUE, 5, "03:00"));
        }
        assert!(core.queue_running(MAIN_QUEUE)); // "start on startup"
        core.tick_queue(); // only a finished file left: the queue finishes
        assert!(!core.queue_running(MAIN_QUEUE));
        assert_eq!(*core.queue_finished.lock().unwrap(), vec![MAIN_QUEUE]);

        let q = core.add_queue("");
        assert_eq!(core.settings().queue(q).unwrap().name, "Queue # 2");
        let mut i = core.new_info("http://x/a.bin");
        i.status = Status::Error("x".into());
        let id = i.id;
        core.add(i);
        core.set_queue(&[id], Some(q));
        assert!(core.find(id).unwrap().info().queued);
        core.delete_queue(MAIN_QUEUE); // not allowed
        core.delete_queue(1);
        core.delete_queue(q);
        assert_eq!(core.settings().queues.len(), 2);
        let i = core.find(id).unwrap().info().clone();
        assert!(!i.queued && i.queue == MAIN_QUEUE);

        // synchronization: a finished file stays queued; each pass checks it with the server and the
        // queue waits for the check, then starts again after "Start again every"
        let mut i = core.new_info("http://127.0.0.1:9/a.bin");
        (i.status, i.size) = (Status::Complete, Some(1));
        let id = i.id;
        core.add(i);
        core.set_queue(&[id], Some(1));
        assert!(core.find(id).unwrap().info().queued);
        {
            let mut s = core.settings();
            let sc = &mut s.queues[1].sched;
            (sc.again_on, sc.again_h, sc.again_m) = (true, 0, 1);
        }
        core.start_queue(1);
        core.tick_queue();
        assert!(core.queue_running(1) && core.checking.lock().unwrap().contains(&id));
        while core.checking.lock().unwrap().contains(&id) {
            thread::sleep(Duration::from_millis(20));
        }
        core.tick_queue();
        assert!(!core.queue_running(1));
        assert!(core.again.lock().unwrap().contains_key(&1));
    }

    #[test]
    fn old_state_keeps_behaviour() {
        // what 1.0.4 saved: none of 1.1's fields
        let s: Settings = serde_json::from_str(r#"{"proxy_mode":3,"proxy_host":"px","proxy_port":"3128","proxy_https":false,
            "adv":{"socks_on":true,"socks_host":"sx","socks_port":"1080","socks5":true}}"#).unwrap();
        assert_eq!(s.proxy_for("http://a.com/f"), "http://px:3128");
        assert_eq!(s.proxy_for("https://a.com/f"), "socks5h://sx:1080");
        // ftp not ticked goes by SOCKS, as the Advanced dialog's note says
        assert_eq!(s.proxy_for("ftp://a.com/f"), "socks5h://sx:1080");
        assert!(!s.proxy_ftp && !s.multi_net);
    }

    #[test]
    fn lanes_only_for_direct() {
        let mut s = Settings { multi_net: true, ..Default::default() };
        assert!(s.use_lanes("http://a.com/f") && !s.use_lanes("ftp://a.com/f"));
        (s.proxy_mode, s.proxy_host) = (3, "px".into());
        assert!(!s.use_lanes("http://a.com/f"));
        s.multi_net = false;
        (s.proxy_mode, s.proxy_host) = (0, String::new());
        assert!(!s.use_lanes("http://a.com/f"));
    }

    #[test]
    fn ftp_routes() {
        let mut s = Settings { proxy_mode: 3, proxy_host: "px".into(), proxy_port: "3128".into(), ..Default::default() };
        assert_eq!(s.proxy_for("ftp://a.com/f"), "");
        s.proxy_ftp = true;
        assert_eq!(s.proxy_for("ftp://a.com/f"), "http://px:3128");
        s.adv = AdvProxy { ftp_on: true, ftp_host: "fx".into(), ftp_port: "2121".into(), ..Default::default() };
        assert_eq!(s.proxy_for("ftp://a.com/f"), "ftp://fx:2121"); // the FTP proxy comes first
        (s.proxy_bypass_on, s.proxy_bypass) = (true, "a.com".into());
        assert_eq!(s.proxy_for("ftp://a.com/f"), "");
    }

    #[test]
    fn proxies() {
        let mut s = Settings { proxy_mode: 3, proxy_host: "px".into(), proxy_port: "3128".into(), proxy_user: "u@x".into(), proxy_pass: "p".into(), ..Default::default() };
        assert_eq!(s.proxy_for("http://a.com/f"), "http://u%40x:p@px:3128");
        s.proxy_https = false;
        assert_eq!(s.proxy_for("https://a.com/f"), "");
        s.adv = AdvProxy { socks_on: true, socks_host: "sx".into(), socks_port: "1080".into(), socks5: true, ..Default::default() };
        assert_eq!(s.proxy_for("https://a.com/f"), "socks5h://sx:1080"); // socks for the unticked protocol
        s.adv.local_dns = true;
        assert_eq!(s.proxy_for("https://a.com/f"), "socks5://sx:1080");
        s.adv.socks5 = false;
        assert_eq!(s.proxy_for("https://a.com/f"), "socks4://sx:1080");
        (s.adv.socks5, s.adv.local_dns) = (true, false);
        s.adv.https_on = true;
        s.adv.https_host = "hx:8443".into();
        assert_eq!(s.proxy_for("https://a.com/f"), "http://hx:8443");
        (s.proxy_bypass_on, s.proxy_bypass) = (true, "*.corp.com 10.".into());
        assert_eq!(s.proxy_for("http://files.corp.com/f"), "");
        assert_eq!(s.proxy_for("http://10.1.2.3/f"), "");
        s.proxy_mode = 0;
        assert_eq!(s.proxy_for("http://a.com/f"), "");

        // automatic configuration script, evaluated by WinHTTP (Windows) or QuickJS
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        s.pac_url = format!("http://{}/proxy.pac", l.local_addr().unwrap());
        thread::spawn(move || {
            for c in l.incoming().flatten() {
                use std::io::{Read, Write};
                let mut c = c;
                let _ = c.read(&mut [0; 4096]);
                let pac = r#"function FindProxyForURL(url, host) { return host == "d.com" ? "DIRECT" : "PROXY 10.1.2.3:8080"; }"#;
                let _ = c.write_all(format!("HTTP/1.1 200 OK
Content-Type: application/x-ns-proxy-autoconfig
Content-Length: {}
Connection: close

{pac}", pac.len()).as_bytes());
            }
        });
        s.proxy_mode = 2;
        assert_eq!(s.proxy_for("http://a.com/f"), "http://10.1.2.3:8080");
        assert_eq!(s.proxy_for("http://d.com/f"), "");
    }

    #[test]
    fn download_limits() {
        assert_eq!(quota_bytes(100), 100 * 1024 * 1024 / 100 * 98);
        let core = Core::from_saved(Saved::default(), std::env::temp_dir().join("ndm-test-unused.json"));
        {
            let mut s = core.settings();
            (s.quota_on, s.quota_mb, s.quota_hours) = (true, 1, 5);
        }
        core.tick_quota(); // window starts at 0 bytes
        core.limiter.received.store(quota_bytes(1) - 1, Relaxed);
        core.tick_quota();
        assert!(!core.quota_blocked());
        core.limiter.received.store(quota_bytes(1), Relaxed);
        core.tick_quota();
        assert!(core.quota_blocked());
        assert!(core.quota_msg.lock().unwrap().as_deref().is_some_and(|m| m.contains("you downloaded 0 MB")));
        // the window ends: downloads come back and counting starts over
        core.quota.lock().unwrap().start.as_mut().unwrap().0 -= chrono::Duration::hours(6);
        core.tick_quota();
        assert!(!core.quota_blocked());
        core.tick_quota();
        assert!(!core.quota_blocked());
        // switching the limit off lifts a stop
        core.limiter.received.store(10 * quota_bytes(1), Relaxed);
        core.tick_quota();
        assert!(core.quota_blocked());
        core.settings().quota_on = false;
        core.tick_quota();
        assert!(!core.quota_blocked());
    }
}
