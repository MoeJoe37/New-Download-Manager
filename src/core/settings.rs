use crate::core::{AdvProxy, BTreeMap, Deserialize, Path, Queue, Sched, Serialize};

/// NDM's default categories (and their file types); the user edits them in "NDM categories" (template 204).
pub const CATEGORIES: [(&str, &str); 5] = [
    ("Compressed", "zip rar r0 r1 arj gz sit sitx sea ace bz2 7z tar tgz xz zst iso"),
    ("Documents", "doc docx pdf ppt pps pptx xls xlsx odt ods rtf txt epub"),
    ("Music", "mp3 wav wma mpa ram ra aac aif m4a flac ogg opus"),
    ("Programs", "exe msi msix apk dmg deb rpm appimage"),
    ("Video", "avi mpg mpe mpeg asf wmv mov qt rm mp4 flv m4v webm ogv mkv ts 3gp"),
];

pub const GENERAL: &str = "General";

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

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub base_dir: String,
    pub conns: usize,
    /// Exact hosts that opt out of HTTP/2 for direct HTTPS downloads.
    pub http1_hosts: String,
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
    /// Speed Limiter settings: on by itself only between these times ("HH:MM", may run past midnight),
    /// off outside them (a manual change holds until the next of the two times).
    pub limit_sched: bool,
    pub limit_from: String,
    pub limit_to: String,
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
    /// The note "keeps downloading while minimized" was shown (closing without a tray icon).
    pub close_note_shown: bool,
    pub tip_next: usize,
    /// View -> "Classic style" (3D toolbar, +/- tree boxes) instead of "Modern style".
    pub classic: bool,
    /// View -> Toolbar -> "NDM Style": NDM's own modern look (rounded controls, accent colour, flat
    /// sidebar, status bar); `classic` is kept for when it is turned off again.
    pub ndm_style: bool,
    /// View -> "Custom scale...": the toolbar, the categories pane and the main window's text, 1.0 = 100%.
    pub scale_toolbar: f32,
    pub scale_tree: f32,
    pub scale_text: f32,
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
    /// Downloads -> Set yt-dlp: when NDM last looked for a newer yt-dlp (`ytdlp::install::keep_updated`).
    pub ytdlp_checked: i64,
    pub prog_sl_tab: bool,
    pub prog_c_tab: bool,
    pub prog_tab_btns: bool,
    /// Download progress dialog shows the connections.
    pub prog_details: bool,
    /// Toolbar set: "Neon" (built in) or the name of a folder in `<config>/toolbars` (see README, Custom toolbars).
    pub tb_skin: String,
    /// Toolbar buttons in order, -1 = separator.
    pub tb_layout: Vec<i8>,
    /// View -> Category icons: "" for NDM's own, or the name of a folder in `<config>/category-icons` (README).
    pub cat_icons: String,
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
            http1_hosts: String::new(),
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
            limit_sched: false,
            limit_from: "08:00".into(),
            limit_to: "23:00".into(),
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
            close_note_shown: false,
            tip_next: 0,
            classic: false,
            ndm_style: false,
            scale_toolbar: 1.0,
            scale_tree: 1.0,
            scale_text: 1.0,
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
            ytdlp_checked: 0,
            prog_sl_tab: true,
            prog_c_tab: true,
            prog_tab_btns: true,
            prog_details: true,
            tb_skin: "Neon".into(),
            cat_icons: String::new(),
            tb_layout: (0..12).collect(),
            search: String::new(),
            search_flags: 4,
        }
    }
}
