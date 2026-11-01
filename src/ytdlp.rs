//! Videos on sites that play them in their own player (YouTube, Vimeo, TikTok...): the page's address is
//! handed to yt-dlp (https://github.com/yt-dlp/yt-dlp), next to NDM or on PATH, which finds the streams and
//! downloads them; with ffmpeg installed the best video and audio are joined into one MP4.
use crate::engine::{sanitize, Job, Probe, Seg};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Sites whose pages hold a video for yt-dlp (the page itself, not a file on their servers); the browser
/// extension's "Download this video" menu item lists the same sites (VIDEO_SITES).
const SITES: [&str; 22] = [
    "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "dai.ly", "twitch.tv", "tiktok.com", "instagram.com",
    "facebook.com", "fb.watch", "x.com", "twitter.com", "reddit.com", "v.redd.it", "soundcloud.com", "bilibili.com",
    "rumble.com", "streamable.com", "ok.ru", "vk.com", "bandcamp.com", "mixcloud.com",
];

/// A page on one of `SITES` (not a plain file on them, which NDM downloads itself).
pub fn is_video_page(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let file = u.path().rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    let plain = [".mp4", ".webm", ".mp3", ".m4a", ".zip", ".jpg", ".png", ".m3u8", ".mpd"].iter().any(|e| file.ends_with(e));
    !plain && SITES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// A program next to NDM's exe, else NDM's own copy (Downloads -> Set yt-dlp), else the name to find on PATH.
fn beside_or_path(name: &str) -> PathBuf {
    let file = exe_file(name);
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&file))).filter(|p| p.is_file());
    beside.or_else(|| Some(tools_dir().join(&file)).filter(|p| p.is_file())).unwrap_or_else(|| PathBuf::from(file))
}

fn exe_file(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

fn command(program: &Path) -> Command {
    let mut c = Command::new(program);
    crate::win::clean_env(&mut c);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.stdin(Stdio::null());
    c
}

fn runs(program: &Path) -> bool {
    command(program).arg(if program.to_string_lossy().contains("ffmpeg") { "-version" } else { "--version" }).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// yt-dlp, or the message that says how to get it.
fn yt_dlp() -> Result<PathBuf, String> {
    let p = beside_or_path("yt-dlp");
    if runs(&p) {
        return Ok(p);
    }
    Err(crate::i18n::tr_os("Downloading videos from this site needs yt-dlp: choose Downloads → Set yt-dlp, or put yt-dlp next to NDM or on PATH."))
}

/// The format to ask for: best video and audio joined (needs ffmpeg), else the best single file.
fn format_args() -> (Vec<String>, bool) {
    let ff = beside_or_path("ffmpeg");
    if runs(&ff) {
        let mut a = vec!["-f".into(), "bv*+ba/b".into(), "--merge-output-format".into(), "mp4".into()];
        if ff.is_absolute() {
            a.extend(["--ffmpeg-location".into(), ff.display().to_string()]);
        }
        (a, true)
    } else {
        (vec!["-f".into(), "b".into()], false)
    }
}

/// The page's cookies and referrer for yt-dlp (sign-in, age checks).
fn request_args(referrer: &str, cookies: &str) -> Vec<String> {
    let mut a = vec!["--no-playlist".into(), "--no-warnings".into()];
    if !referrer.is_empty() {
        a.extend(["--referer".into(), referrer.into()]);
    }
    if !cookies.is_empty() {
        a.extend(["--add-header".into(), format!("Cookie: {cookies}")]);
    }
    a
}

/// The last "ERROR: ..." yt-dlp printed, as the download's error.
fn last_error(err: &str) -> String {
    err.lines().rev().find_map(|l| l.strip_prefix("ERROR: ")).unwrap_or(err.lines().last().unwrap_or("yt-dlp failed")).trim().to_string()
}

/// The video's file name (title and type) and size, if yt-dlp knows it.
pub fn probe(url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    let p = yt_dlp()?;
    let (fmt, merged) = format_args();
    let out = command(&p)
        .args(request_args(referrer, cookies))
        .args(&fmt)
        .args(["--skip-download", "--print", "%(title)s", "--print", "%(ext)s", "--print", "%(filesize,filesize_approx)s"])
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let (Some(title), Some(ext)) = (lines.next(), lines.next()) else {
        return Err(last_error(&String::from_utf8_lossy(&out.stderr)));
    };
    let size = lines.next().and_then(|s| s.trim().parse::<f64>().ok()).map(|s| s as u64).filter(|s| *s > 0);
    let name = sanitize(&format!("{}.{}", title.trim(), if merged { "mp4" } else { ext.trim() }));
    Ok(Probe { name, size: size.filter(|_| !merged), ranges: false, modified: String::new(), stream: false })
}

/// Downloads the video into the job's file; progress from yt-dlp's own lines. Stop ends yt-dlp (its .part
/// files stay, and the next start goes on from them).
pub fn run(job: &Job) -> Result<bool, String> {
    let p = yt_dlp()?;
    let (url, referrer, cookies, named) = {
        let i = job.info();
        (i.url.clone(), i.referrer.clone(), i.cookies.clone(), !i.name.is_empty())
    };
    // added without the File Info window (command line, queue): the video's title names it
    let found = if named { None } else { probe(&url, &referrer, &cookies).ok().map(|p| p.name) };
    let dest = {
        let mut i = job.info();
        if !named {
            i.name = found.unwrap_or_else(|| "video.mp4".into());
        }
        std::fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
        i.path()
    };
    let (fmt, _) = format_args();
    let mut child = command(&p)
        .args(request_args(&referrer, &cookies))
        .args(&fmt)
        .args(["--newline", "--continue", "--progress-template", "download:ndm %(progress.downloaded_bytes)s %(progress.total_bytes,progress.total_bytes_estimate)s", "-o"])
        .arg(&dest)
        .arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut err = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    // video, then audio: each part counts from 0, the bar adds them up
    let (mut base, mut last, mut part_total) = (0u64, 0u64, 0u64);
    let mut stopped = false;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(rest) = line.strip_prefix("ndm ") {
            let mut it = rest.split_whitespace().map(|v| v.parse::<f64>().ok().map(|v| v as u64));
            let (done, total) = (it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0));
            if done < last {
                base += part_total.max(last);
            }
            (last, part_total) = (done, total);
            let all = base + total.max(done);
            let mut i = job.info();
            i.size = Some(all).filter(|s| *s > 0);
            i.segs = vec![Seg { start: 0, next: base + done, stop: i.size, info: "Receiving data...".into(), owned: false, ..Default::default() }];
        }
        if job.stopping() {
            let _ = child.kill();
            stopped = true;
            break;
        }
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    let errors = errors.join().unwrap_or_default();
    if stopped {
        return Ok(false);
    }
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    if !ok || size == 0 {
        return Err(last_error(&errors));
    }
    let mut i = job.info();
    i.size = Some(size);
    i.segs = vec![Seg { start: 0, next: size, stop: Some(size), info: String::new(), owned: false, ..Default::default() }];
    Ok(true)
}

// ---------- Downloads -> Set yt-dlp ----------

const RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases";

/// The folder of NDM's own yt-dlp, beside its settings (added to PATH by `install`).
pub fn tools_dir() -> PathBuf {
    crate::core::store_path().parent().map(|d| d.join("tools")).unwrap_or_else(|| PathBuf::from("tools"))
}

/// NDM's own copy of yt-dlp.
pub fn managed() -> PathBuf {
    tools_dir().join(exe_file("yt-dlp"))
}

/// The release file for this system: yt-dlp's stand-alone builds, which need no Python.
fn asset() -> &'static str {
    if cfg!(windows) {
        if cfg!(target_arch = "aarch64") {
            "yt-dlp_arm64.exe"
        } else if cfg!(target_arch = "x86") {
            "yt-dlp_x86.exe"
        } else {
            "yt-dlp.exe"
        }
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

/// The newest release's tag: GitHub sends ".../releases/latest" on to ".../releases/tag/<version>".
fn latest(agent: &ureq::Agent, releases: &str) -> Result<String, String> {
    let r = agent.get(&format!("{releases}/latest")).call().map_err(|e| e.to_string())?;
    tag_of(r.get_url()).ok_or_else(|| format!("no release found ({})", r.get_url()))
}

fn tag_of(url: &str) -> Option<String> {
    let tag = url.split("/releases/tag/").nth(1)?.split(['?', '#', '/']).next()?;
    (!tag.is_empty() && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))).then(|| tag.to_string())
}

/// The SHA-256 a release lists for `file` in its SHA2-256SUMS ("hex  name" lines).
fn listed_sum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hex, name) = l.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file && hex.len() == 64).then(|| hex.to_ascii_lowercase())
    })
}

/// Downloads the newest yt-dlp into NDM's tools folder when NDM's copy is older (or missing), checked
/// against the release's SHA-256, puts it in place of the old one and makes sure the folder is on PATH.
/// `got` / `total`: progress for the window. Returns the version now installed.
pub fn install(agent: &ureq::Agent, got: &AtomicU64, total: &AtomicU64) -> Result<String, String> {
    let dir = tools_dir();
    let tag = fetch(agent, RELEASES, &dir, got, total)?;
    add_to_path(&dir);
    Ok(tag)
}

/// `install`'s download into `dir` from `releases` (GitHub's address, a local one in the tests).
fn fetch(agent: &ureq::Agent, releases: &str, dir: &Path, got: &AtomicU64, total: &AtomicU64) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let tag = latest(agent, releases)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = dir.join(exe_file("yt-dlp"));
    let current = std::fs::read_to_string(dir.join("yt-dlp.version")).unwrap_or_default();
    if current.trim() != tag || !runs(&dest) {
        let file = asset();
        let sums = agent.get(&format!("{releases}/download/{tag}/SHA2-256SUMS")).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())?;
        let want = listed_sum(&sums, file).ok_or_else(|| format!("{file} is not in release {tag}"))?;
        let r = agent.get(&format!("{releases}/download/{tag}/{file}")).call().map_err(|e| e.to_string())?;
        total.store(r.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0), Relaxed);
        let part = dir.join(format!("{}.part", exe_file("yt-dlp")));
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let (mut body, mut hash, mut buf) = (r.into_reader(), Sha256::new(), vec![0u8; 1 << 16]);
        loop {
            let n = body.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            got.fetch_add(n as u64, Relaxed);
        }
        drop(out);
        let sum = hash.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
        if sum != want {
            let _ = std::fs::remove_file(&part);
            return Err(format!("{file}: the downloaded file is damaged (SHA-256 {sum}, expected {want})"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        // Windows can't overwrite a program that runs (a video downloading), but can rename it
        let old = dir.join(format!("{}.old", exe_file("yt-dlp")));
        let _ = std::fs::remove_file(&old);
        if dest.exists() && std::fs::rename(&part, &dest).is_err() {
            std::fs::rename(&dest, &old).map_err(|e| e.to_string())?;
        }
        if !dest.exists() || part.exists() {
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::write(dir.join("yt-dlp.version"), &tag);
    }
    Ok(tag)
}

/// Puts NDM's tools folder on the user's PATH, so yt-dlp works in a terminal too: the user's Path in the
/// registry on Windows, a link in ~/.local/bin on Linux, a line in ~/.zprofile on macOS.
fn add_to_path(dir: &Path) {
    #[cfg(windows)]
    crate::win::add_user_path(dir);
    #[cfg(target_os = "linux")]
    if let Some(home) = std::env::var_os("HOME") {
        let bin = Path::new(&home).join(".local/bin");
        let link = bin.join("yt-dlp");
        let _ = std::fs::create_dir_all(&bin);
        // a yt-dlp of the user's own stays
        if std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
            let _ = std::fs::remove_file(&link);
        }
        let _ = std::os::unix::fs::symlink(dir.join("yt-dlp"), &link);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        let profile = Path::new(&home).join(".zprofile");
        let text = std::fs::read_to_string(&profile).unwrap_or_default();
        let line = format!("export PATH=\"$PATH:{}\"", dir.display());
        if !text.lines().any(|l| l.trim() == line) {
            let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
            let _ = std::fs::write(&profile, format!("{text}{sep}# New Download Manager's yt-dlp\n{line}\n"));
        }
    }
}

/// Keeps NDM's copy of yt-dlp up to date: once a day, when there is one (video sites change often and
/// an old yt-dlp stops working on them).
pub fn keep_updated(core: std::sync::Arc<crate::core::Core>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        loop {
            let now = chrono::Local::now().timestamp();
            let due = now - core.settings().ytdlp_checked > 86400;
            if due && managed().is_file() {
                core.settings().ytdlp_checked = now;
                let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
                if let Err(e) = install(&core.agent(RELEASES), &got, &total) {
                    eprintln!("yt-dlp update: {e}");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_pages() {
        assert!(is_video_page("https://www.youtube.com/watch?v=abc"));
        assert!(is_video_page("https://youtu.be/abc"));
        assert!(is_video_page("https://vimeo.com/12345"));
        assert!(!is_video_page("https://notyoutube.com/watch"));
        assert!(!is_video_page("https://video.twimg.com/a/b.mp4"));
        assert!(!is_video_page("https://cdn.vimeo.com/file.mp4"), "a file is downloaded by NDM itself");
        assert!(!is_video_page("ftp://youtube.com/x"));
        assert_eq!(tag_of("https://github.com/yt-dlp/yt-dlp/releases/tag/2025.10.22"), Some("2025.10.22".into()));
        assert_eq!(tag_of("https://github.com/yt-dlp/yt-dlp/releases"), None);
        let sums = "aa  yt-dlp\n0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa  yt-dlp_macos\n";
        assert_eq!(listed_sum(sums, "yt-dlp_macos").as_deref(), Some("0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa"));
        assert_eq!(listed_sum(sums, "yt-dlp"), None);
        assert_eq!(last_error("[info] x\nERROR: [youtube] abc: Video unavailable\n"), "[youtube] abc: Video unavailable");
    }

    /// Set yt-dlp against a local stand-in for GitHub: the newest tag from the redirect, the file checked
    /// against SHA2-256SUMS, a damaged one refused, an up-to-date copy left alone.
    #[cfg(unix)]
    #[test]
    fn installs_and_checks() {
        use sha2::{Digest, Sha256};
        use std::io::{BufRead, BufReader, Write};
        let script = b"#!/bin/sh\necho 2099.01.02\n".to_vec();
        let good = Sha256::digest(&script).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let sums = std::sync::Arc::new(std::sync::Mutex::new(format!("{good}  {}\n", asset())));
        let served = std::sync::Arc::new(AtomicU64::new(0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/yt-dlp/yt-dlp/releases", listener.local_addr().unwrap());
        let (s2, n2, b2, body) = (sums.clone(), served.clone(), base.clone(), script.clone());
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() <= 2 {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut c = c;
                let reply = |c: &mut std::net::TcpStream, head: &str, body: &[u8]| {
                    let _ = write!(c, "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = c.write_all(body);
                };
                if path.ends_with("/releases/latest") {
                    let _ = write!(c, "HTTP/1.1 302 Found\r\nLocation: {b2}/tag/2099.01.02\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                } else if path.ends_with("/tag/2099.01.02") {
                    reply(&mut c, "200 OK", b"<html></html>");
                } else if path.ends_with("/SHA2-256SUMS") {
                    reply(&mut c, "200 OK", s2.lock().unwrap().as_bytes());
                } else if path.ends_with(&format!("/download/2099.01.02/{}", asset())) {
                    n2.fetch_add(1, Relaxed);
                    reply(&mut c, "200 OK", &body);
                } else {
                    reply(&mut c, "404 Not Found", b"");
                }
            }
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-ytdlp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let agent = ureq::AgentBuilder::new().build();
        let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
        // a damaged download is refused and leaves nothing behind
        *sums.lock().unwrap() = format!("{}  {}\n", "0".repeat(64), asset());
        assert!(fetch(&agent, &base, &dir, &got, &total).unwrap_err().contains("damaged"));
        assert!(!dir.join("yt-dlp").exists());
        *sums.lock().unwrap() = format!("{good}  {}\n", asset());
        assert_eq!(fetch(&agent, &base, &dir, &got, &total).unwrap(), "2099.01.02");
        assert_eq!(std::fs::read(dir.join("yt-dlp")).unwrap(), script);
        assert!(runs(&dir.join("yt-dlp")), "installed yt-dlp can run");
        assert_eq!(total.load(Relaxed), script.len() as u64);
        // up to date: not downloaded again
        assert_eq!(fetch(&agent, &base, &dir, &got, &total).unwrap(), "2099.01.02");
        assert_eq!(served.load(Relaxed), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
