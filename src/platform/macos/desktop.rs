use super::{Path, PathBuf, clean_env, home};

/// ~/Library/Application Support, where the browsers keep their settings.
fn support() -> PathBuf {
    home().join("Library/Application Support")
}

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
