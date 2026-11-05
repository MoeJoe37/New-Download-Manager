use super::{Path, running, wide};

/// A REG_SZ value ("" = the key's default value); empty if missing.
fn reg_text(root: windows_sys::Win32::System::Registry::HKEY, key: &str, name: &str) -> String {
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_SZ};
    let (key, val, mut buf, mut len) = (wide(key), wide(name), vec![0u16; 2048], 4096u32);
    let r = unsafe { RegGetValueW(root, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    if r != 0 { String::new() } else { String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]) }
}

/// Browsers registered with Windows (Clients\StartMenuInternet, per user and per machine): (name, exe).
/// NDM fills its General page list the same way and adds the ones it can integrate with.
pub fn installed_browsers() -> Vec<(String, String)> {
    use windows_sys::Win32::System::Registry::*;
    let mut out: Vec<(String, String)> = Vec::new();
    let (native, wow) = (r"SOFTWARE\Clients\StartMenuInternet", r"SOFTWARE\WOW6432Node\Clients\StartMenuInternet");
    for (root, base) in [(HKEY_CURRENT_USER, native), (HKEY_LOCAL_MACHINE, native), (HKEY_LOCAL_MACHINE, wow)] {
        let mut hk: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(root, wide(base).as_ptr(), 0, KEY_READ, &mut hk) } != 0 {
            continue;
        }
        for i in 0.. {
            let (mut name, mut len) = (vec![0u16; 256], 256u32);
            let r = unsafe { RegEnumKeyExW(hk, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if r != 0 {
                break;
            }
            let sub = format!(r"{base}\{}", String::from_utf16_lossy(&name[..len as usize]));
            let title = reg_text(root, &sub, "");
            let cmd = reg_text(root, &format!(r"{sub}\shell\open\command"), "");
            let exe = cmd.trim().trim_start_matches('"').split('"').next().unwrap_or("").trim().to_string();
            if !title.is_empty() && Path::new(&exe).is_file() && !out.iter().any(|b| b.1.eq_ignore_ascii_case(&exe)) {
                out.push((title, exe));
            }
        }
        unsafe { RegCloseKey(hk) };
    }
    out
}

pub fn set_autostart(on: bool) {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let args: Vec<&str> = if on { vec!["add", key, "/v", "NewDownloadManager", "/t", "REG_SZ", "/d", &exe, "/f"] } else { vec!["delete", key, "/v", "NewDownloadManager", "/f"] };
    let _ = std::process::Command::new("reg").args(args).output();
}

/// The browser extension's native messaging host name and its (fixed, from the manifest's "key") id.
const HOST: &str = "com.ndm.host";

const EXT_ID: &str = "fjkomnlijmdmohmlbfgkepgdieleinoa";

/// Registers this exe as the extension's native messaging host (Chromium browsers and Firefox), so the
/// extension can start the app when a download comes and it isn't running. Run at every start: the exe may move.
pub fn register_host(dir: &Path) {
    use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::fs::create_dir_all(dir);
    let host = |file: &str, allow: serde_json::Value| {
        let mut m = serde_json::json!({ "name": HOST, "description": "New Download Manager", "path": exe, "type": "stdio" });
        m.as_object_mut().unwrap().extend(allow.as_object().unwrap().clone());
        let p = dir.join(file);
        std::fs::write(&p, m.to_string()).ok().map(|_| p.display().to_string())
    };
    let chrome = host("host-chrome.json", serde_json::json!({ "allowed_origins": [format!("chrome-extension://{EXT_ID}/")] }));
    let firefox = host("host-firefox.json", serde_json::json!({ "allowed_extensions": ["ndm@newdownloadmanager"] }));
    let keys = [(r"Google\Chrome", &chrome), (r"Microsoft\Edge", &chrome), ("Chromium", &chrome), (r"BraveSoftware\Brave-Browser", &chrome), ("Mozilla", &firefox)];
    for (browser, file) in keys {
        let Some(file) = file else { continue };
        let (key, data) = (wide(&format!(r"Software\{browser}\NativeMessagingHosts\{HOST}")), wide(file));
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), std::ptr::null(), REG_SZ, data.as_ptr().cast(), data.len() as u32 * 2) };
    }
}

/// Is this start the browser launching the native messaging host? Chromium passes the caller's origin,
/// Firefox the host manifest's path.
pub fn is_host_launch(args: &[String]) -> bool {
    args.first().is_some_and(|a| a.starts_with("chrome-extension://") || a.to_ascii_lowercase().ends_with(".json"))
}

/// The native messaging host: reads the extension's message (32-bit length, then JSON), starts the app
/// unless it's running, answers `{"ok":true}` and exits.
pub fn native_host(title: &str) {
    use std::io::{Read, Write};
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut len = [0u8; 4];
    if std::io::stdin().read_exact(&mut len).is_ok() {
        let _ = std::io::stdin().take(u32::from_le_bytes(len) as u64).read_to_end(&mut Vec::new());
    }
    if !running(title, false) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
        // the app mustn't keep the browser's pipes open
        for h in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
            unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) };
        }
        if let Ok(exe) = std::env::current_exe() {
            const DETACHED_PROCESS: u32 = 0x8;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            // "/tray": in the notification area, like NDM started by its browser extension
            let spawn = |flags| Command::new(&exe).arg("/tray").creation_flags(flags).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            // out of the browser's job object when allowed, so closing the browser doesn't close the app
            if spawn(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB).is_err() {
                let _ = spawn(DETACHED_PROCESS);
            }
        }
    }
    let reply = br#"{"ok":true}"#;
    let mut out = std::io::stdout();
    let _ = out.write_all(&(reply.len() as u32).to_le_bytes()).and_then(|_| out.write_all(reply)).and_then(|_| out.flush());
}
