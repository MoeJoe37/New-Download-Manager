//! Videos on sites that play them in their own player (YouTube, Vimeo, TikTok...): the page's address is
//! handed to yt-dlp (https://github.com/yt-dlp/yt-dlp), which finds the streams and downloads them.
//! Downloads -> Set yt-dlp installs yt-dlp and Deno (the JavaScript runtime yt-dlp needs for YouTube) in
//! NDM's tools folder and keeps them up to date. The best video and audio are joined by ffmpeg when it is
//! installed, else by NDM itself (stream::fmp4::join_files).
pub(crate) mod install;
pub(crate) mod formats;
pub(crate) mod playlist;
#[cfg(test)]
mod tests;

use self::formats::{Stream, chosen, formats, named, picked, selection_error, video_info, video_name};
use self::install::tools_dir;
use self::playlist::browser_args;
use crate::engine::{names::sanitize, Info, Job, Limiter, probe::Probe, Seg, Status};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Sites whose pages hold a video for yt-dlp (the page itself, not a file on their servers); the browser
/// extension's "Download this video" menu item and panel use the same list (VIDEO_SITES).
const SITES: &[&str] = &[
    "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "dai.ly", "twitch.tv", "tiktok.com", "instagram.com",
    "facebook.com", "fb.watch", "x.com", "twitter.com", "reddit.com", "v.redd.it", "soundcloud.com", "bilibili.com",
    "rumble.com", "streamable.com", "ok.ru", "vk.com", "bandcamp.com", "mixcloud.com", "douyin.com", "kick.com",
    "threads.net", "threads.com", "pinterest.com", "tumblr.com", "linkedin.com", "9gag.com", "imgur.com", "weibo.com",
    "nicovideo.jp", "bilibili.tv", "snapchat.com", "likee.video", "coub.com", "odysee.com", "bitchute.com",
    "vkvideo.ru", "rutube.ru", "clips.twitch.tv",
];

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default()
}

fn on_site(host: &str, site: &str) -> bool {
    host == site || host.ends_with(&format!(".{site}"))
}

/// A page on one of `SITES` (not a plain file on them, which NDM downloads itself).
pub fn is_video_page(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let file = u.path().rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    // a file on the site (a picture, a document, a program, a stream's playlist) is NDM's own to download
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let known = crate::core::settings::CATEGORIES.iter().any(|(_, exts)| exts.split_whitespace().any(|x| x == ext));
    let plain = !ext.is_empty() && (known || ["jpg", "jpeg", "png", "gif", "gifv", "webp", "avif", "svg", "bmp", "m3u8", "mpd", "json", "txt", "csv"].contains(&ext));
    !plain && SITES.iter().any(|s| on_site(&host, s))
}

/// A download yt-dlp gets: a page marked so (the extension's download panel, a pasted page) or one on
/// `SITES`, unless it is a stream yt-dlp already found (`Info::plain`).
pub fn wants(i: &Info) -> bool {
    !i.plain && (i.ytdlp || is_video_page(&i.url))
}

/// A program next to NDM's exe, else NDM's own copy (Downloads -> Set yt-dlp), else the name to find on PATH.
pub(crate) fn beside_or_path(name: &str) -> PathBuf {
    let file = exe_file(name);
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&file))).filter(|p| p.is_file());
    beside.or_else(|| Some(tools_dir().join(&file)).filter(|p| p.is_file())).unwrap_or_else(|| PathBuf::from(file))
}

fn exe_file(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

pub(crate) fn command(program: &Path) -> Command {
    let mut c = Command::new(program);
    crate::win::clean_env(&mut c);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c
}

/// Also stop a downloader/postprocessor yt-dlp started, so it cannot keep writing after Stop.
pub(crate) fn kill_child(child: &mut Child) {
    #[cfg(unix)]
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
    #[cfg(windows)]
    {
        let _ = command(Path::new("taskkill.exe")).args(["/PID", &child.id().to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
    let _ = child.wait();
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

pub(crate) fn ffmpeg() -> Option<PathBuf> {
    let ff = beside_or_path("ffmpeg");
    runs(&ff).then_some(ff)
}

/// The arguments every run gets: one video, the page's referrer and cookies (sign-in, age checks), and
/// NDM's Deno for YouTube. YouTube gets no cookies: signed in, yt-dlp turns to clients that need tokens
/// it doesn't have and finds fewer formats.
fn base_args(url: &str, referrer: &str, cookies: &str) -> Vec<String> {
    // This is one NDM job. For a playlist-only URL take its first entry rather than treating the
    // playlist JSON as a file.
    let mut a = vec!["--ignore-config".into(), "--no-playlist".into(), "--playlist-items".into(), "1".into(), "--no-warnings".into()];
    if !referrer.is_empty() {
        a.extend(["--referer".into(), referrer.into()]);
    }
    let host = host_of(url);
    if !cookies.is_empty() && !["youtube.com", "youtu.be"].iter().any(|s| on_site(&host, s)) {
        a.extend(["--add-header".into(), format!("Cookie: {cookies}")]);
    }
    let deno = beside_or_path("deno");
    if deno.is_absolute() {
        a.extend(["--js-runtimes".into(), format!("deno:{}", deno.display())]);
    }
    a
}

/// The last "ERROR: ..." yt-dlp printed, as the download's error; an address yt-dlp doesn't know as a video
/// (its "generic" extractor gave up: a feed, a profile, a search) says so first.
fn last_error(err: &str) -> String {
    let e = err.lines().rev().find_map(|l| l.strip_prefix("ERROR: ")).unwrap_or(err.lines().last().unwrap_or("yt-dlp failed")).trim().to_string();
    if e.starts_with("[generic]") || e.contains("Unsupported URL") {
        return format!("{}\n{}", crate::i18n::tr_os("There is no video at this address that yt-dlp knows: open the video's own page (click the video or its date) and download it from there."), crate::transport::redact(&e));
    }
    crate::transport::redact(&e)
}

/// The video title, for File Info.
pub fn probe(agent: &crate::transport::Agent, url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    if agent.invalid_proxy { return Err("Invalid proxy configuration".into()); }
    let p = yt_dlp()?;
    let ff = crate::media::toolchain();
    let fmt = formats(url, ff.is_some());
    let out = command(&p)
        .args(base_args(url, referrer, cookies))
        .args(["--proxy", &crate::transport::external_proxy(&agent.proxy), "--socket-timeout", "20"])
        .args(["-f", &fmt, "-J"])
        .arg("--")
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(last_error(&String::from_utf8_lossy(&out.stderr)));
    }
    let v = video_info(serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?)?;
    let name = video_name(&v);
    // the size shows as the streams arrive (video and audio come separately)
    Ok(Probe { name, size: None, ranges: false, modified: String::new(), etag: String::new(), stream: false, page: false, ytdlp: true })
}

/// Arguments for yt-dlp's own downloader. Its native HLS/DASH fetcher otherwise uses one fragment
/// at a time. Keep its protocol-specific downloaders for sites NDM cannot fetch directly.
fn download_args(conns: usize, limit: u64) -> Vec<String> {
    let mut args = vec![
        "--concurrent-fragments".into(), conns.clamp(1, 64).to_string(),
        "--retries".into(), "10".into(), "--fragment-retries".into(), "10".into(),
        "--abort-on-unavailable-fragments".into(),
    ];
    if limit > 0 {
        args.extend(["--limit-rate".into(), limit.to_string()]);
    }
    args
}

/// One selected stream, using the saved extraction result (signed URLs and extractor-specific data
/// included). Read stdout on its own thread: a silent or stalled downloader must still obey Stop.
fn fetch_video(job: &Job, p: &Path, source: &[String], args: &[String], fmt: &str, out: &str, expected: u64, global: &Limiter) -> Result<bool, String> {
    let (conns, limit) = {
        let i = job.info();
        let limits = [job.limiter.rate.load(Relaxed), global.rate.load(Relaxed)];
        (i.conns, limits.into_iter().filter(|l| *l > 0).min().unwrap_or(0))
    };
    let mut child = command(p)
        .args(args)
        .args(download_args(conns, limit))
        .args(["-f", fmt, "--newline", "--continue", "--progress", "--progress-template", "download:ndm %(progress.downloaded_bytes)s %(progress.total_bytes,progress.total_bytes_estimate)s", "-o", out])
        .args(source)
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
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() { break; }
        }
    });
    let mut merging: Option<(PathBuf, u64)> = None;
    let (mut reading, mut status, mut received) = (true, None, 0u64);
    let mut stopped = false;
    while reading || status.is_none() {
        if job.stopping() {
            kill_child(&mut child);
            stopped = true;
            break;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(rest) = line.strip_prefix("ndm ") {
                    let mut it = rest.split_whitespace().map(|v| v.parse::<f64>().ok().map(|v| v as u64));
                    let (done, total) = (it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0));
                    global.received.fetch_add(done.saturating_sub(received), Relaxed);
                    received = received.max(done);
                    let mut i = job.info();
                    i.post = None;
                    i.size = Some(total.max(done).max(expected)).filter(|s| *s > 0).max(i.size);
                    let next = done.max(i.segs.first().map_or(0, |s| s.next));
                    i.segs = vec![Seg { start: 0, next, stop: i.size, info: "Receiving data...".into(), owned: false, ..Default::default() }];
                    i.status = Status::Downloading;
                } else if let Some((label, out, expected)) = post_step(&line, received) {
                    job.set_post(label, 0.0);
                    merging = out.map(|o| (o, expected));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                reading = false;
                // stdout may close before a postprocessor exits.
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        if let Some((out, expected)) = &merging {
            let have = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
            let label = job.info().post.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| "Processing...".into());
            job.set_post(&label, (have as f64 / (*expected).max(1) as f64).min(0.99) as f32);
        }
        if status.is_none() {
            status = child.try_wait().map_err(|e| e.to_string())?;
        }
    }
    let _ = reader.join();
    let errors = errors.join().unwrap_or_default();
    job.info().post = None;
    if stopped { return Ok(false); }
    if !status.is_some_and(|s| s.success()) { return Err(last_error(&errors)); }
    Ok(true)
}

/// A yt-dlp line that starts a step after the download: its label, the file ffmpeg writes meanwhile
/// ("<name>.temp.<ext>") and the size that file grows to. `fetched`: the bytes downloaded (video and audio).
fn post_step(line: &str, fetched: u64) -> Option<(&'static str, Option<PathBuf>, u64)> {
    let tag = line.strip_prefix('[')?.split(']').next()?;
    let quoted = || line.split('"').nth(1).map(PathBuf::from);
    let temp = |p: PathBuf| {
        let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        p.with_extension(format!("temp.{ext}"))
    };
    match tag {
        "Merger" => Some(("Joining video and audio...", quoted().map(temp), fetched)),
        t if t.starts_with("Fixup") => {
            let file = quoted()?;
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(fetched);
            Some(("Processing...", Some(temp(file)), size))
        }
        "VideoRemuxer" | "VideoConvertor" | "ExtractAudio" => {
            let file = line.split("Destination: ").nth(1).map(|d| PathBuf::from(d.trim()));
            Some(("Processing...", file.map(temp), fetched))
        }
        _ => None,
    }
}

/// "%" starts a field in yt-dlp's output templates.
fn template(p: &Path) -> String {
    p.display().to_string().replace('%', "%%")
}

/// A yt-dlp run's output, unless the download is stopped meanwhile (None: yt-dlp ended at once).
pub(crate) fn output_unless_stopped(job: &Job, cmd: Command) -> Result<Option<std::process::Output>, String> {
    output_with_timeout(job, cmd, None)
}

fn output_with_timeout(job: &Job, mut cmd: Command, timeout: Option<std::time::Duration>) -> Result<Option<std::process::Output>, String> {
    let started = std::time::Instant::now();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let read = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = r.by_ref().take(32 << 20).read_to_end(&mut v);
            v
        })
    };
    let out = read(Box::new(child.stdout.take().unwrap()));
    let err = read(Box::new(child.stderr.take().unwrap()));
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        let expired = timeout.is_some_and(|timeout| started.elapsed() >= timeout);
        if job.stopping() || expired {
            kill_child(&mut child);
            let _ = out.join();
            let _ = err.join();
            if expired { return Err("Playlist lookup timed out.".into()); }
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    Ok(Some(std::process::Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() }))
}

/// The extractor can return signed URLs and scoped cookies. Store them under NDM's user
/// configuration with a private, uniquely created file rather than in the download destination.
fn save_extraction(info: &serde_json::Value, store: &Path) -> Result<crate::media::Stage, String> {
    use std::io::Write;
    let stage = crate::media::Stage::new(&store.with_file_name("yt-dlp.json"))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(stage.path()).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, info).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    Ok(stage)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MergeReceipt {
    source: String,
    tracks: [(String, u64); 2],
}

fn receipt_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    dest.with_file_name(format!(".{name}.ndm-merge.json"))
}

fn source_identity(job: &Job) -> String {
    use sha2::{Digest, Sha256};
    let info = job.info();
    let mut source = format!("{}:{}", info.id, info.url);
    if !info.yt.quality.is_empty() {
        source.push_str(&format!("\nquality:{}", info.yt.quality));
    }
    if !info.yt.browser.is_empty() {
        source.push_str(&format!("\nbrowser:{}", info.yt.browser));
    }
    format!("{:x}", Sha256::digest(source))
}

fn restore_tracks(job: &Job, dest: &Path) -> Option<(PathBuf, PathBuf)> {
    let receipt = receipt_path(dest);
    if std::fs::metadata(&receipt).ok()?.len() > 8192 { return None; }
    let receipt: MergeReceipt = serde_json::from_slice(&std::fs::read(receipt).ok()?).ok()?;
    if receipt.source != source_identity(job) { return None; }
    let stem = dest.file_stem()?.to_str()?;
    let resolve = |(name, size): &(String, u64), tag| {
        if Path::new(name).file_name()?.to_str()? != name || !name.starts_with(&format!("{stem}.{tag}.")) {
            return None;
        }
        let path = dest.with_file_name(name);
        (path.is_file() && *size > 0 && std::fs::metadata(&path).ok()?.len() == *size).then_some(path)
    };
    Some((resolve(&receipt.tracks[0], "ndm-video")?, resolve(&receipt.tracks[1], "ndm-audio")?))
}

fn record_tracks(job: &Job, dest: &Path, video: &Path, audio: &Path) -> Result<(), String> {
    let track = |path: &Path| -> Result<(String, u64), String> {
        let name = path.file_name().ok_or("Media track has no filename")?.to_string_lossy().into_owned();
        Ok((name, std::fs::metadata(path).map_err(|e| e.to_string())?.len()))
    };
    let receipt = MergeReceipt { source: source_identity(job), tracks: [track(video)?, track(audio)?] };
    let path = receipt_path(dest);
    let stage = crate::media::Stage::new(&path)?;
    let bytes = serde_json::to_vec(&receipt).map_err(|e| e.to_string())?;
    std::fs::write(stage.path(), bytes).map_err(|e| e.to_string())?;
    stage.finish(&path)
}

/// Downloads the video into the job's file. yt-dlp looks the page up and picks the streams (the best
/// video and its audio, or the one file that has both); plain files among them are fetched by NDM itself,
/// each over the download's connections in segments and both at once, others by yt-dlp. The video and
/// audio are then joined (NDM's own joiner, else ffmpeg) as a processing step of its own.
pub fn run(job: &Job, agent: &crate::transport::Agent, global: &Limiter) -> Result<bool, String> {
    let existing = { let i = job.info(); (!i.name.is_empty()).then(|| i.path()) };
    if let Some(dest) = existing {
        if let Some((video, audio)) = restore_tracks(job, &dest) {
            let merged = join(job, &video, &audio, &dest, crate::media::toolchain().as_deref());
            if job.stopping() { return Ok(false); }
            merged?;
            let _ = std::fs::remove_file(receipt_path(&dest));
            return finish_job(job, &dest);
        }
    }
    let p = yt_dlp()?;
    let (url, referrer, cookies, yt) = {
        let i = job.info();
        (i.url.clone(), i.referrer.clone(), i.cookies.clone(), i.yt.clone())
    };
    if agent.invalid_proxy { return Err("Invalid proxy configuration".into()); }
    let (referrer, cookies) = if yt.playlist_entry { ("", "") } else { (referrer.as_str(), cookies.as_str()) };
    let mut args = base_args(&url, referrer, cookies);
    browser_args(&mut args, &yt.browser)?;
    let proxy = crate::transport::external_proxy(&agent.proxy);
    let route = ["--proxy", proxy.as_str(), "--socket-timeout", "20"];
    args.extend(route.map(str::to_string));
    let ff = crate::media::toolchain();
    let encoder = ff.as_deref();
    if yt.quality == "mp3" && encoder.is_none() {
        return Err(crate::i18n::tr_os("Saving the sound as MP3 needs ffmpeg: install it with Downloads -> Set yt-dlp, then resume the download."));
    }
    let fmt = chosen(&url, ff.is_some(), &yt.quality);
    let mut look = command(&p);
    look.args(&args).args(["-J", "-f", &fmt, "--"]).arg(&url);
    let Some(looked) = output_unless_stopped(job, look)? else { return Ok(false) };
    if !looked.status.success() {
        return Err(selection_error(&String::from_utf8_lossy(&looked.stderr), &yt.quality));
    }
    let v = video_info(serde_json::from_slice(&looked.stdout).map_err(|e| e.to_string())?)?;
    let mut streams = picked(&v).ok_or("yt-dlp gave no formats")?;
    if !yt.browser.is_empty() {
        // yt-dlp must retain the browser cookie jar's domain and path restrictions.
        streams.iter_mut().for_each(|stream| stream.direct = false);
    }
    // A single extraction also names jobs added without the File Info window (CLI, queues).
    // Keep native M4A audio and convert when the selection needs a different output.
    let sound = streams.first().filter(|_| streams.len() == 1 && matches!(yt.quality.as_str(), "mp3" | "m4a")).map(|s| s.ext.clone());
    let convert = sound.as_ref().is_some_and(|ext| yt.quality == "mp3" || ext != "m4a"
        || v.get("vcodec").and_then(|codec| codec.as_str()) != Some("none"));
    if convert && encoder.is_none() {
        return Err("This audio format requires FFmpeg. Install it with Downloads -> Set yt-dlp and resume.".into());
    }
    let dest = {
        let mut i = job.info();
        if i.name.is_empty() {
            i.name = named(&video_name(&v), &yt.quality);
            if yt.playlist_entry {
                // Distinct jobs can resolve to the same title, even when started concurrently.
                let name = Path::new(&i.name);
                let stem = name.file_stem().unwrap_or_default().to_string_lossy();
                let ext = name.extension().unwrap_or_default().to_string_lossy();
                let name = format!("{stem} [NDM {}].{ext}", i.id);
                i.name = crate::engine::names::unique_name(&i.folder, &name, &|_| false);
            }
        }
        if yt.quality.parse::<u32>().is_ok() {
            // The probe used the default quality, whose container may differ from this selection.
            if let Some(ext) = v.get("ext").and_then(|ext| ext.as_str())
                .filter(|ext| matches!(*ext, "mp4" | "webm" | "mkv" | "ts" | "mov" | "flv"))
                .filter(|ext| Path::new(&i.name).extension().is_none_or(|old| !old.eq_ignore_ascii_case(ext)))
            {
                let name = Path::new(&i.name).with_extension(ext);
                i.name = crate::engine::names::unique_name(&i.folder, &name.to_string_lossy(), &|_| false);
            }
        }
        if sound.is_some() && Path::new(&i.name).extension().is_none_or(|ext| !ext.eq_ignore_ascii_case(&yt.quality)) {
            return Err(crate::i18n::trf_os("The selected audio format requires a .%s filename.", &[&yt.quality]));
        }
        std::fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
        i.path()
    };
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let outs: Vec<PathBuf> = if streams.len() == 2 {
        ["ndm-video", "ndm-audio"].iter().zip(&streams).map(|(tag, s)| dir.join(format!("{stem}.{tag}.{}", s.ext))).collect()
    } else if convert {
        vec![dir.join(format!("{stem}.ndm-audio.{}", streams[0].ext))]
    } else {
        vec![dest.clone()]
    };
    // Preserve extractor-specific fields and cookies for protocol-specific downloaders. Remove this
    // private file on success, failure, Stop, or a Rust error while starting a subprocess.
    let json = save_extraction(&v, &crate::core::state::store_path())?;
    let source = ["--load-info-json".to_string(), json.path().display().to_string()];
    let fallback = |part: &Job, s: &Stream, out: &Path| {
        fetch_video(part, &p, &source, &args, &s.id, &template(out), s.size, global)
    };
    let got = fetch_streams(job, agent, global, &streams, &outs, &fallback)?;
    if !got {
        return Ok(false);
    }
    if yt.subs || yt.thumb {
        extras(job, &p, &route, &source, &dest, &yt);
        if job.stopping() { return Ok(false); }
    }
    if let [video, audio] = &outs[..] {
        record_tracks(job, &dest, video, audio)?;
        let merged = join(job, video, audio, &dest, ff.as_deref());
        if job.stopping() { return Ok(false); }
        merged?;
        let _ = std::fs::remove_file(receipt_path(&dest));
    }
    if let (true, Some(encoder)) = (convert, encoder) {
        let converted = convert_audio(job, encoder, &outs[0], &dest, &yt.quality);
        if job.stopping() { return Ok(false); }
        converted?;
    }
    finish_job(job, &dest)
}

/// The downloaded sound as an MP3 (or an M4A) in `dest`, by ffmpeg; the source is removed once it's done.
fn convert_audio(job: &Job, ff: &Path, src: &Path, dest: &Path, quality: &str) -> Result<(), String> {
    let stage = crate::media::Stage::new(dest)?;
    let mut cmd = command(ff);
    cmd.args(["-y", "-nostdin", "-v", "error", "-xerror", "-protocol_whitelist", "file", "-i"]).arg(src).args(["-map", "0:a:0", "-map_metadata", "0"]);
    if quality == "mp3" {
        cmd.args(["-codec:a", "libmp3lame", "-q:a", "2", "-f", "mp3"]);
    } else {
        cmd.args(["-codec:a", "aac", "-b:a", "192k", "-f", "mp4"]);
    }
    cmd.arg(stage.path());
    let watch = [stage.path().to_path_buf()];
    let out = crate::stream::download::with_post(job, "Converting media...", &watch, 0, || output_unless_stopped(job, cmd))?;
    let Some(out) = out else { return Err("stopped".into()) };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("ffmpeg: {}", err.lines().last().unwrap_or("conversion failed")));
    }
    crate::media::validate_audio(job, stage.path(), quality)?;
    if job.stopping() { return Err("stopped".into()); }
    stage.finish(dest)?;
    let _ = std::fs::remove_file(src);
    Ok(())
}

/// The subtitles and the thumbnail next to the video ("name.en.vtt", "name.webp"), from the look-up's
/// saved result. Missing ones don't fail the download.
fn extras(job: &Job, p: &Path, route: &[&str], source: &[String], dest: &Path, yt: &crate::engine::YtOpts) {
    let stem = dest.with_extension("");
    let mut cmd = command(p);
    // The saved extraction supplies per-resource context. Do not add the browser's raw Cookie
    // header to requests for subtitles and thumbnails, which can use different origins.
    cmd.args(route).args(["--ignore-config", "--no-playlist", "--skip-download", "--no-overwrites"]);
    let mut cookies = Vec::new();
    if browser_args(&mut cookies, &yt.browser).is_err() { return; }
    cmd.args(cookies);
    if yt.subs {
        cmd.args(["--write-subs", "--sub-langs", "all,-live_chat"]);
    }
    if yt.thumb {
        cmd.arg("--write-thumbnail");
    }
    cmd.args(["-o", &format!("{}.%(ext)s", template(&stem))]).args(source);
    let watch: [PathBuf; 0] = [];
    match crate::stream::download::with_post(job, "Saving subtitles and thumbnail...", &watch, 0, || output_unless_stopped(job, cmd)) {
        Ok(Some(o)) if !o.status.success() => eprintln!("NDM: subtitles / thumbnail: {}", last_error(&String::from_utf8_lossy(&o.stderr))),
        Err(e) => eprintln!("NDM: subtitles / thumbnail: {}", crate::transport::redact(&e)),
        _ => {}
    }
}

fn finish_job(job: &Job, dest: &Path) -> Result<bool, String> {
    let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err("yt-dlp finished without a file".into());
    }
    let mut i = job.info();
    i.size = Some(size);
    i.segs = vec![Seg { start: 0, next: size, stop: Some(size), info: String::new(), owned: false, ..Default::default() }];
    Ok(true)
}

/// The video and audio streams joined into `dest` after the download, with the bar showing it: NDM's own
/// joiner (MP4, WebM), else ffmpeg; failing both, keep the sources and report the failure.
fn join(job: &Job, video: &Path, audio: &Path, dest: &Path, ff: Option<&Path>) -> Result<(), String> {
    let stage = crate::media::Stage::new(dest)?;
    let ext = dest.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    let can_use_native = matches!(ext.as_str(), "mp4" | "webm")
        && (ext == "webm") == video.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("webm"));
    if let Some(ff) = ff {
        crate::media::mux(job, ff, video, Some(audio), stage.path(), &ext, false)?;
    } else if can_use_native {
        let expected = [video, audio].iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let watch = [stage.path().to_path_buf()];
        let joined = crate::stream::download::with_post(job, "Joining video and audio...", &watch, expected, || {
            crate::stream::fmp4::join_files(video, audio, stage.path())
        })?;
        if !joined {
            return Err(crate::i18n::tr_os("Could not join video and audio. Install ffmpeg next to NDM or on PATH, then resume the download. The separate streams have been kept."));
        }
    } else {
        return Err(crate::i18n::tr_os("Could not join video and audio. Install ffmpeg next to NDM or on PATH, then resume the download. The separate streams have been kept."));
    }
    if job.stopping() { return Err("stopped".into()); }
    stage.finish(dest)?;
    let _ = std::fs::remove_file(video);
    let _ = std::fs::remove_file(audio);
    Ok(())
}

/// Fetch each stream concurrently with its appropriate backend. A failed native fetch switches only
/// that stream to yt-dlp. The parent's bar combines both streams, including mixed native/yt-dlp jobs.
fn fetch_streams(job: &Job, agent: &crate::transport::Agent, global: &Limiter, streams: &[Stream], outs: &[PathBuf],
    fallback: &(dyn Fn(&Job, &Stream, &Path) -> Result<bool, String> + Sync)) -> Result<bool, String> {
    let (conns, referrer, limit) = {
        let i = job.info();
        (i.conns.max(1), i.referrer.clone(), i.limit_kb)
    };
    let lanes = job.lanes.lock().unwrap().clone();
    let parts: Vec<std::sync::Arc<Job>> = streams
        .iter()
        .zip(outs)
        .map(|(s, out)| {
            let header = |name: &str| s.headers.iter().find(|h| h.0.eq_ignore_ascii_case(name)).map(|h| h.1.clone());
            let headers = s.headers.iter().filter(|h| !["referer", "cookie"].contains(&h.0.to_ascii_lowercase().as_str())).cloned().collect();
            let part = Job::new(Info {
                url: s.url.clone(),
                folder: out.parent().map(|d| d.display().to_string()).unwrap_or_default(),
                name: out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                referrer: header("referer").unwrap_or_else(|| referrer.clone()),
                cookies: header("cookie").unwrap_or_default(),
                headers,
                conns,
                limit_kb: limit,
                plain: true,
                ..Default::default()
            });
            part.limiter.rate.store(stream_limit(job, global, streams.len()), Relaxed);
            *part.lanes.lock().unwrap() = lanes.clone();
            part
        })
        .collect();
    let expected: Vec<u64> = streams.iter().map(|s| s.size).collect();
    // the total from the look-up before anything arrives
    show_parts(job, &parts, &expected);
    let failed = std::sync::atomic::AtomicBool::new(false);
    let results: Vec<Result<bool, String>> = std::thread::scope(|sc| {
        let running: Vec<_> = parts.iter().zip(streams).zip(outs).map(|((part, stream), out)| {
            let failed = &failed;
            sc.spawn(move || {
                let result = (|| {
                    if stream.direct {
                        match part.run_part(agent, global) {
                            Ok(got) => return Ok(got),
                            Err(_) if part.stopping() => return Ok(false),
                            Err(e) => {
                                // Keep the other stream downloading; only the rejected stream switches backend.
                                eprintln!("NDM could not fetch format {} ({e}); trying yt-dlp", stream.id);
                                let _ = std::fs::remove_file(out);
                                let _ = std::fs::remove_file(part.info().part());
                                let mut i = part.info();
                                (i.size, i.segs) = (None, Vec::new());
                            }
                        }
                    }
                    fallback(part, stream, out)
                })();
                if result.is_err() { failed.store(true, Relaxed); }
                result
        })}).collect();
        while running.iter().any(|h| !h.is_finished()) {
            if job.stopping() || failed.load(Relaxed) {
                parts.iter().for_each(|p| p.stop());
            }
            let live = running.iter().filter(|h| !h.is_finished()).count();
            let limit = stream_limit(job, global, live);
            parts.iter().for_each(|p| p.limiter.rate.store(limit, Relaxed));
            show_parts(job, &parts, &expected);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        show_parts(job, &parts, &expected);
        running.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a download thread stopped".into()))).collect()
    });
    if let Some(e) = results.iter().find_map(|r| r.as_ref().err()) { return Err(e.clone()); }
    Ok(!job.stopping() && results.iter().all(|r| matches!(r, Ok(true))))
}

/// Video and audio belong to one download's bandwidth budget, even though they have separate engines.
fn stream_limit(job: &Job, global: &Limiter, live: usize) -> u64 {
    let rate = [job.limiter.rate.load(Relaxed), global.rate.load(Relaxed)].into_iter().filter(|r| *r > 0).min().unwrap_or(0);
    if rate == 0 { 0 } else { (rate / live.max(1) as u64).max(1) }
}

#[cfg(test)]
fn fetch_direct(job: &Job, agent: &crate::transport::Agent, global: &Limiter, streams: &[Stream], outs: &[PathBuf]) -> Result<bool, String> {
    fetch_streams(job, agent, global, streams, outs, &|_, _, _| Err("unexpected fallback".into()))
}

/// The parent's bar and segment list from its streams' downloads: their segments one after the other, the
/// total from each stream's size (the server's once known, else the look-up's). The total only grows when
/// a stream's real size turns out larger; done never goes back.
fn show_parts(job: &Job, parts: &[std::sync::Arc<Job>], expected: &[u64]) {
    let (mut segs, mut offset, mut known) = (Vec::new(), 0u64, true);
    for (part, guess) in parts.iter().zip(expected) {
        let i = part.info();
        for s in &i.segs {
            let mut s = s.clone();
            s.start += offset;
            s.next += offset;
            s.stop = s.stop.map(|e| e + offset);
            segs.push(s);
        }
        let size = i.size.unwrap_or(*guess);
        known &= size > 0;
        offset += size;
    }
    let probed = parts.iter().all(|p| p.info().probed);
    let ranges = parts.iter().all(|p| p.info().ranges);
    let mut i = job.info();
    if known {
        i.size = Some(offset);
    }
    (i.probed, i.ranges) = (probed, ranges);
    if !segs.is_empty() {
        i.segs = segs;
        if i.status == Status::Connecting && i.done() > 0 {
            i.status = Status::Downloading;
        }
    }
}

// ---------- Downloads -> Set yt-dlp ----------
