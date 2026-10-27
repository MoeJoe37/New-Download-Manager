//! Linux stand-ins for `win.rs`: the same functions, done the freedesktop way where there is one
//! (xdg-open, ~/.config/autostart, the browsers' NativeMessagingHosts folders, fc-match) and no-ops where
//! Linux has nothing alike (dial-up, WinINet cache, shell icons, the drop target).
// ponytail: no tray icon on Linux (closing the window quits) and no clipboard watching; add them with a
// GTK thread for tray-icon and an X11/Wayland clipboard owner check.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Rgba = (usize, usize, Vec<u8>);

pub fn url_cache(_url: &str) -> Option<(PathBuf, String)> {
    None
}
pub fn shell_icon(_ext: &str, _folder: bool) -> Option<Rgba> {
    None
}
pub fn type_name(_ext: &str) -> String {
    String::new()
}
pub fn exe_icon(_path: &Path) -> Option<Rgba> {
    None
}

// The window is shown and hidden by egui itself on Linux; there is no HWND (it stays 0).
pub static PUMP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn pump(_hwnd: isize) {}
pub fn wake(_hwnd: isize) {}
pub fn hide(_hwnd: isize) {}
pub fn visible(_hwnd: isize) -> bool {
    true
}
pub fn own_dialogs(_main: isize, _dark: bool, _retitle: bool, _light: &[String]) {}
pub fn dark_title(_hwnd: isize, _dark: bool) {}

pub fn clip_seq() -> u32 {
    0
}

/// A sound file through PulseAudio / PipeWire, else ALSA.
pub fn play_sound(path: &str) {
    if Path::new(path).is_file() && Command::new("paplay").arg(path).spawn().is_err() {
        let _ = Command::new("aplay").arg("-q").arg(path).spawn();
    }
}

pub fn decimal_sep() -> &'static str {
    "."
}

/// GNOME / GTK's dark preference.
pub fn system_dark() -> bool {
    static DARK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DARK.get_or_init(|| {
        let get = |key: &str| Command::new("gsettings").args(["get", "org.gnome.desktop.interface", key]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
        get("color-scheme").contains("dark") || get("gtk-theme").contains("dark")
    })
}

/// The proxy from the environment (http_proxy / https_proxy / no_proxy) as Windows' Internet Settings
/// would give it: (on, "http=host:port;https=host:port", bypass list).
pub fn system_proxy() -> (bool, String, String) {
    let var = |k: &str| std::env::var(k).or_else(|_| std::env::var(k.to_uppercase())).unwrap_or_default();
    let host = |v: String| v.split("://").last().unwrap_or("").trim_end_matches('/').to_string();
    let (http, https) = (host(var("http_proxy")), host(var("https_proxy")));
    let server = [("http", http), ("https", https)].into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";");
    (!server.is_empty(), server, var("no_proxy").replace(',', ";"))
}

pub fn ras_entries() -> Vec<String> {
    vec![]
}
pub fn ras_connected() -> bool {
    true
}
pub fn ras_save(_entry: &str, _user: &str, _pass: &str, _save: bool) -> Result<(), String> {
    Err("Dial-up connections are not supported on Linux".into())
}
pub fn ras_dial(_entry: &str) -> Result<(), String> {
    Err("Dial-up connections are not supported on Linux".into())
}
pub fn ras_hangup_all() {}
pub fn set_system_proxy(_on: bool, _server: &str, _bypass: &str) {}
pub fn pac_proxy(_pac: &str, _url: &str) -> Option<String> {
    None
}

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

pub fn exit_splash(_title: &str, _text: &str) {}

pub fn choose_font(_owner: isize, _face: &str, _px: f32) -> Option<(String, f32)> {
    None
}

/// The file of an installed font face, from fontconfig.
pub fn font_file(face: &str) -> Option<PathBuf> {
    let o = Command::new("fc-match").args(["-f", "%{file}", face]).output().ok()?;
    let f = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    f.is_file().then_some(f)
}

pub fn take_drops() -> Vec<String> {
    vec![]
}
pub fn accept_drops(_hwnd: isize, _wake: impl Fn() + Send + Sync + 'static) {}
pub fn drop_target(_show: bool, _px: &Rgba) {}
pub fn drop_target_visible() -> bool {
    false
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
