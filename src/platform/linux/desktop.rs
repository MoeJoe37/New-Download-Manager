use super::*;

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

pub(super) fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

pub(super) fn home() -> PathBuf {
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
    let _ = std::fs::write(file, desktop_entry(&exe) + "X-GNOME-Autostart-enabled=true\n");
}

/// NDM's .desktop entry for `exe`. Exec quotes the path and escapes \ " ` $ inside it.
fn desktop_entry(exe: &Path) -> String {
    let mut path = String::new();
    for c in exe.display().to_string().chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            path.push('\\');
        }
        path.push(c);
    }
    format!("[Desktop Entry]\nType=Application\nName=New Download Manager\nGenericName=Download Manager\nExec=\"{path}\"\nIcon=ndm\nStartupWMClass=ndm\nCategories=Network;FileTransfer;\nTerminal=false\n")
}

/// Run from an AppImage: NDM's entry in the applications menu and its icon, rewritten only when they
/// changed (the AppImage moved, or a new icon).
pub fn install_desktop_entry() {
    let Some(img) = std::env::var_os("APPIMAGE").map(PathBuf::from) else { return };
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|d| d.is_absolute()).unwrap_or_else(|| home().join(".local/share"));
    let write = |file: PathBuf, bytes: &[u8]| {
        if std::fs::read(&file).ok().as_deref() != Some(bytes) {
            let _ = std::fs::create_dir_all(file.parent().unwrap());
            let _ = std::fs::write(&file, bytes);
        }
    };
    write(data.join("icons/hicolor/256x256/apps/ndm.png"), include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ndm.png")));
    write(data.join("applications/ndm.desktop"), desktop_entry(&img).as_bytes());
}

#[test]
fn desktop_entry_quotes() {
    let e = desktop_entry(Path::new("/home/u/My Apps/NDM \"1\".AppImage"));
    assert!(e.contains("Exec=\"/home/u/My Apps/NDM \\\"1\\\".AppImage\"\n") && e.contains("StartupWMClass=ndm\n"), "{e}");
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
    for (d, is_firefox) in host_dirs(&home(), &config_dir()) {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(format!("{HOST}.json")), if is_firefox { &firefox } else { &chrome });
    }
}

/// The native messaging host folders of the installed browsers (their profile folder exists), and
/// whether each is Firefox's. Snap's Firefox reads ~/.mozilla through the desktop portal.
fn host_dirs(home: &Path, cfg: &Path) -> Vec<(PathBuf, bool)> {
    let chromes = ["google-chrome", "google-chrome-beta", "google-chrome-unstable", "chromium", "microsoft-edge", "microsoft-edge-beta", "microsoft-edge-dev",
        "BraveSoftware/Brave-Browser", "vivaldi", "vivaldi-snapshot", "opera", "yandex-browser"];
    let mut d: Vec<(PathBuf, bool)> = chromes.iter().map(|b| cfg.join(b)).filter(|b| b.is_dir()).map(|b| (b.join("NativeMessagingHosts"), false)).collect();
    if home.join(".mozilla").is_dir() || home.join("snap/firefox").is_dir() {
        d.push((home.join(".mozilla/native-messaging-hosts"), true));
    }
    d
}

#[test]
fn host_dirs_cover_snap_firefox() {
    let home = std::env::temp_dir().join(format!("ndm-test-hosts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    for p in ["snap/firefox", ".config/google-chrome-beta"] {
        std::fs::create_dir_all(home.join(p)).unwrap();
    }
    let d = host_dirs(&home, &home.join(".config"));
    assert!(d.contains(&(home.join(".mozilla/native-messaging-hosts"), true)));
    assert!(d.contains(&(home.join(".config/google-chrome-beta/NativeMessagingHosts"), false)));
    assert_eq!(d.len(), 2);
    let _ = std::fs::remove_dir_all(&home);
}

/// The native messaging host (see `posix::native_host`).
pub fn native_host(title: &str) {
    crate::posix::native_host(title, me(), clean_env);
}
