//! Videos on sites that play them in their own player (YouTube, Vimeo, TikTok...): the page's address is
//! handed to yt-dlp (https://github.com/yt-dlp/yt-dlp), next to NDM or on PATH, which finds the streams and
//! downloads them; with ffmpeg installed the best video and audio are joined into one MP4.
use crate::engine::{sanitize, Job, Probe, Seg};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// A program next to NDM's exe, else the name to find on PATH.
fn beside_or_path(name: &str) -> PathBuf {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&file))).filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(file))
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
    Err(crate::i18n::tr_os("Downloading videos from this site needs yt-dlp: download it from github.com/yt-dlp/yt-dlp/releases and put it next to NDM or on PATH."))
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
        assert_eq!(last_error("[info] x\nERROR: [youtube] abc: Video unavailable\n"), "[youtube] abc: Video unavailable");
    }
}
