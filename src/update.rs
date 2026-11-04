use crate::file_ops::copy_dir;
use crate::{i18n, win};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

/// Where new versions are published: a GitHub "owner/repo" whose latest release carries the new program
/// (Help -> Check for updates, About -> Check for Update, and the automatic check every 2 weeks).
/// Empty = no update source; the check says so.
pub(crate) const UPDATE_REPO: &str = "MoeJoe37/New-Download-Manager";

/// First argument of the copy started by "Update now".
pub(crate) const UPDATED: &str = "/updated";

/// The message when the release information can't be read.
const UPDATE_INVALID: &str = "Check for updates failed, because update information is invalid.\r\nIf you use wi-fi that requires logging in in the browser, please log in before checking for updates";

/// A newer release: its version, notes, this platform's program to download ("" = none attached) and
/// the release's web page.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) notes: String,
    pub(crate) exe: String,
    pub(crate) page: String,
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

/// The release file that replaces this program, for this system and processor: the setup (or a loose
/// ndm.exe) on Windows; on Linux the AppImage, the .deb, the .rpm or the .tar.gz, whichever this copy came as;
/// on macOS the disk image. ARM builds' names never match what NDM 1.x looks for (it would take them for
/// its own): NDM-ARM64-Setup-*.exe, ndm-arm64.exe, *aarch64*, *_arm64.deb.
fn update_asset(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "macos") {
        return n.ends_with(".dmg") && n.contains("macos");
    }
    if !cfg!(windows) {
        let (deb, rpm, arch) = if arm { ("_arm64.deb", ".aarch64.rpm", "aarch64") } else { ("_amd64.deb", ".x86_64.rpm", "x86_64") };
        match linux_install() {
            LinuxInstall::AppImage => n.ends_with(".appimage") && n.contains(arch),
            LinuxInstall::Deb => n.ends_with(deb),
            LinuxInstall::Rpm => n.ends_with(rpm),
            LinuxInstall::Folder => n.starts_with("ndm-") && n.ends_with(&format!("-{arch}.tar.gz")),
        }
    } else if installed() {
        n.starts_with(if arm { "ndm-arm64-setup" } else { "ndm-setup" }) && n.ends_with(".exe")
    } else {
        n == if arm { "ndm-arm64.exe" } else { "ndm.exe" }
    }
}

#[test]
fn update_assets() {
    let names = ["NDM-Setup-2.0.0.exe", "NDM-ARM64-Setup-2.0.0.exe", "ndm.exe", "ndm-arm64.exe", "NDM-x86_64.AppImage", "NDM-aarch64.AppImage", "ndm_2.0.0_amd64.deb", "ndm_2.0.0_arm64.deb",
        "ndm-2.0.0-1.x86_64.rpm", "ndm-2.0.0-1.aarch64.rpm", "ndm-2.0.0-x86_64.tar.gz", "ndm-2.0.0-aarch64.tar.gz", "NDM-2.0.0-macOS.dmg", "NDM-2.0.0-windows-x64.zip"];
    // exactly one file is this build's (whatever way it was installed)
    assert_eq!(names.iter().filter(|n| update_asset(n)).count(), 1, "{:?}", names.iter().filter(|n| update_asset(n)).collect::<Vec<_>>());
    // what NDM 1.x looks for on x64 Windows (setup or loose exe) and x86_64 Linux never matches an ARM file
    let old = |n: &str| {
        let n = n.to_ascii_lowercase();
        (n.starts_with("ndm-setup") && n.ends_with(".exe")) || n == "ndm.exe" || (n.ends_with(".appimage") && n.contains("x86_64")) || n.ends_with("_amd64.deb") || n.ends_with(".x86_64.rpm")
            || (n.starts_with("ndm-") && n.ends_with("-x86_64.tar.gz"))
    };
    assert!(names.iter().filter(|n| old(n)).all(|n| !n.to_ascii_lowercase().contains("arm") && !n.contains("aarch64")));
}

/// The latest release on GitHub, if newer than this build (None = this is the latest).
pub(crate) fn check_update(agent: &ureq::Agent) -> Result<Option<Release>, String> {
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
pub(crate) fn install_update(agent: &ureq::Agent, url: &str, got: &std::sync::atomic::AtomicU64, total: &std::sync::atomic::AtomicU64) -> Result<(), String> {
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
    #[cfg(target_os = "macos")]
    return mac_update(&exe, &data);
    #[allow(unreachable_code)]
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

/// macOS: the disk image's NDM.app takes the place of the one this copy runs from (the old one is kept as
/// "<name>.old" until the next start), then the new one starts.
#[cfg(target_os = "macos")]
fn mac_update(exe: &Path, dmg: &[u8]) -> Result<(), String> {
    // a disk image ends with its "koly" trailer
    if dmg.len() < 4096 || !dmg[dmg.len() - 512..].starts_with(b"koly") {
        return Err(String::new());
    }
    let bundle = exe.ancestors().find(|a| a.extension().is_some_and(|e| e == "app")).ok_or("NDM isn't running from its app: download the new version from the release page")?;
    let file = std::env::temp_dir().join("NDM-update.dmg");
    let mnt = std::env::temp_dir().join(format!("ndm-update-{}", std::process::id()));
    std::fs::write(&file, dmg).map_err(|e| e.to_string())?;
    let attached = std::process::Command::new("hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mnt).arg(&file).status().is_ok_and(|s| s.success());
    if !attached {
        return Err("the update file is damaged".into());
    }
    let result = (|| -> Result<(), String> {
        let app = std::fs::read_dir(&mnt).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "app")).ok_or("the update file is damaged")?;
        let old = bundle.with_extension("old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(bundle, &old).map_err(|e| format!("{e}: {}", bundle.display()))?;
        // ditto keeps the app's links, permissions and signature
        if !std::process::Command::new("ditto").arg(&app).arg(bundle).status().is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(bundle);
            let _ = std::fs::rename(&old, bundle); // put the old one back
            return Err("the new version couldn't be copied".into());
        }
        Ok(())
    })();
    let _ = std::process::Command::new("hdiutil").arg("detach").arg(&mnt).arg("-quiet").status();
    let _ = std::fs::remove_file(&file);
    result?;
    std::process::Command::new("open").arg("-n").arg(bundle).args(["--args", UPDATED]).spawn().map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
