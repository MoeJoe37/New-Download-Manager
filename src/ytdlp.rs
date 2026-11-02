//! Videos on sites that play them in their own player (YouTube, Vimeo, TikTok...): the page's address is
//! handed to yt-dlp (https://github.com/yt-dlp/yt-dlp), which finds the streams and downloads them.
//! Downloads -> Set yt-dlp installs yt-dlp and Deno (the JavaScript runtime yt-dlp needs for YouTube) in
//! NDM's tools folder and keeps them up to date. The best video and audio are joined by ffmpeg when it is
//! installed, else by NDM itself (stream::join_files).
use crate::engine::{sanitize, Info, Job, Limiter, Probe, Seg, Status};
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
    let known = crate::core::CATEGORIES.iter().any(|(_, exts)| exts.split_whitespace().any(|x| x == ext));
    let plain = !ext.is_empty() && (known || ["jpg", "jpeg", "png", "gif", "gifv", "webp", "avif", "svg", "bmp", "m3u8", "mpd", "json", "txt", "csv"].contains(&ext));
    !plain && SITES.iter().any(|s| on_site(&host, s))
}

/// A download yt-dlp gets: a page marked so (the extension's download panel, a pasted page) or one on
/// `SITES`, unless it is a stream yt-dlp already found (`Info::plain`).
pub fn wants(i: &Info) -> bool {
    !i.plain && (i.ytdlp || is_video_page(&i.url))
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c
}

/// Also stop a downloader/postprocessor yt-dlp started, so it cannot keep writing after Stop.
fn kill_child(child: &mut Child) {
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

fn ffmpeg() -> Option<PathBuf> {
    let ff = beside_or_path("ffmpeg");
    runs(&ff).then_some(ff)
}

/// The arguments every run gets: one video, the page's referrer and cookies (sign-in, age checks), and
/// NDM's Deno for YouTube. YouTube gets no cookies: signed in, yt-dlp turns to clients that need tokens
/// it doesn't have and finds fewer formats.
fn base_args(url: &str, referrer: &str, cookies: &str) -> Vec<String> {
    // This is one NDM job. For a playlist-only URL take its first entry rather than treating the
    // playlist JSON as a file. Ignore external configs that can change output names/progress or run hooks.
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
        return format!("{}\n{e}", crate::i18n::tr_os("There is no video at this address that yt-dlp knows: open the video's own page (click the video or its date) and download it from there."));
    }
    e
}

/// With ffmpeg: the best video and audio, merged by yt-dlp into an MP4.
const MERGED: &str = "bv*+ba/b/ba/bv";
/// The single file with both, when a site has no separate streams.
const SINGLE: &str = "b";
/// Without ffmpeg: the best video-only stream and the audio NDM can join it with (MP4 and M4A, else WebM
/// and WebM), else the single file.
const PAIR: &str = "bv*[acodec=none][ext=mp4]+ba[vcodec=none][ext=m4a]/bv*[acodec=none][ext=webm]+ba[vcodec=none][ext=webm]/b/ba/bv";

/// The formats asked for on `url`'s site, `ff`: with ffmpeg. TikTok and Douyin: the one file that has both
/// (H.264 first, which every player plays), not their video-only streams (some broken) with the separate
/// "music" track, which isn't always the video's own sound. YouTube without ffmpeg: its separate streams,
/// which NDM joins itself. Other sites without ffmpeg: the one file that has both when there is one, since
/// their separate streams may be whole MP4 files that only ffmpeg joins.
fn formats(url: &str, ff: bool) -> String {
    let host = host_of(url);
    let both = if ff { MERGED } else { PAIR };
    if ["tiktok.com", "douyin.com"].iter().any(|s| on_site(&host, s)) {
        format!("b[vcodec^=h264]/b[vcodec^=avc]/b/{both}")
    } else if ff || ["youtube.com", "youtu.be"].iter().any(|s| on_site(&host, s)) {
        both.to_string()
    } else {
        format!("{SINGLE}/{PAIR}")
    }
}

/// One stream yt-dlp's look-up (-J) picked: its format id, address, type and size (0: unknown), the
/// headers it is fetched with, and whether it is a plain file NDM can fetch itself (http / https; DASH and
/// HLS streams stay with yt-dlp).
struct Stream {
    id: String,
    url: String,
    ext: String,
    size: u64,
    headers: Vec<(String, String)>,
    direct: bool,
}

impl Stream {
    fn of(f: &serde_json::Value) -> Option<Stream> {
        let text = |k: &str| f.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let size = f.get("filesize").and_then(|s| s.as_f64()).or_else(|| f.get("filesize_approx").and_then(|s| s.as_f64())).unwrap_or(0.0) as u64;
        let headers = f.get("http_headers").and_then(|h| h.as_object()).map(|h| h.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default();
        // the stream's cookies (yt-dlp keeps them out of its headers: "name=value; Domain=...; Path=/; ...")
        let mut headers: Vec<(String, String)> = headers;
        let cookie = cookie_header(&text("cookies"));
        if !cookie.is_empty() {
            headers.retain(|h: &(String, String)| !h.0.eq_ignore_ascii_case("cookie"));
            headers.push(("Cookie".into(), cookie));
        }
        let url = text("url");
        // Leave fragments, live streams, and requests requiring TLS impersonation to yt-dlp, even
        // when their URL looks like a plain HTTP file.
        let direct = matches!(text("protocol").as_str(), "http" | "https")
            && (url.starts_with("http://") || url.starts_with("https://"))
            && f.get("fragments").is_none_or(|v| v.is_null())
            && !f.get("is_live").and_then(|v| v.as_bool()).unwrap_or(false)
            && f.get("impersonate").is_none_or(|v| v.is_null())
            && f.pointer("/downloader_options/impersonate").is_none_or(|v| v.is_null());
        Some(Stream { id: f.get("format_id")?.as_str()?.to_string(), url, ext: text("ext"), size, headers, direct })
    }
}

/// yt-dlp's cookie list for a stream ("a=1; Domain=.x.com; Path=/; Secure; Expires=...; b=2; ...") as a
/// Cookie header ("a=1; b=2").
fn cookie_header(list: &str) -> String {
    const ATTRS: [&str; 7] = ["domain", "path", "secure", "expires", "version", "httponly", "max-age"];
    list.split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| !ATTRS.contains(&p.split('=').next().unwrap_or("").trim().to_ascii_lowercase().as_str()))
        .filter(|p| p.contains('='))
        .collect::<Vec<_>>()
        .join("; ")
}

/// What the look-up picked: the video and audio streams, or the one stream that has both.
fn picked(v: &serde_json::Value) -> Option<Vec<Stream>> {
    match v.get("requested_formats").and_then(|r| r.as_array()).filter(|r| r.len() == 2) {
        Some(r) => r.iter().map(Stream::of).collect(),
        None => Some(vec![Stream::of(&v)?]),
    }
}

/// Multi-video posts and playlist-only extractors wrap the selected video in `entries`.
fn video_info(mut v: serde_json::Value) -> Result<serde_json::Value, String> {
    for _ in 0..16 {
        if let Some(entries) = v.get_mut("entries").and_then(|e| e.as_array_mut()) {
            v = entries.iter_mut().find(|e| !e.is_null()).map(serde_json::Value::take).ok_or("yt-dlp found no video in this playlist")?;
        } else {
            return Ok(v);
        }
    }
    Err("yt-dlp returned too many nested playlists".into())
}

fn video_name(v: &serde_json::Value) -> String {
    let title = v.get("title").and_then(|v| v.as_str()).unwrap_or("video");
    let ext = v.get("ext").and_then(|v| v.as_str()).filter(|e| !e.is_empty()).unwrap_or("mp4");
    sanitize(&format!("{title}.{ext}"))
}

/// The video's file name (title and type) and size, if yt-dlp knows it.
pub fn probe(url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    let p = yt_dlp()?;
    let ff = ffmpeg();
    let fmt = formats(url, ff.is_some());
    let out = command(&p)
        .args(base_args(url, referrer, cookies))
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
    Ok(Probe { name, size: None, ranges: false, modified: String::new(), stream: false, page: false, ytdlp: true })
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
        let limits = [i.limit_kb.saturating_mul(1024), global.rate.load(Relaxed)];
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
fn output_unless_stopped(job: &Job, mut cmd: Command) -> Result<Option<std::process::Output>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let read = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = r.read_to_end(&mut v);
            v
        })
    };
    let out = read(Box::new(child.stdout.take().unwrap()));
    let err = read(Box::new(child.stderr.take().unwrap()));
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if job.stopping() {
            kill_child(&mut child);
            let _ = out.join();
            let _ = err.join();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    Ok(Some(std::process::Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() }))
}

struct SavedInfo(PathBuf);
impl SavedInfo {
    fn new(path: PathBuf, info: &serde_json::Value) -> Result<Self, String> {
        use std::io::Write;
        let saved = Self(path);
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&saved.0).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(info).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Ok(saved)
    }
}
impl Drop for SavedInfo {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

/// Downloads the video into the job's file. yt-dlp looks the page up and picks the streams (the best
/// video and its audio, or the one file that has both); plain files among them are fetched by NDM itself,
/// each over the download's connections in segments and both at once, others by yt-dlp. The video and
/// audio are then joined (NDM's own joiner, else ffmpeg) as a processing step of its own.
pub fn run(job: &Job, agent: &ureq::Agent, global: &Limiter) -> Result<bool, String> {
    let p = yt_dlp()?;
    let (url, referrer, cookies) = {
        let i = job.info();
        (i.url.clone(), i.referrer.clone(), i.cookies.clone())
    };
    let args = base_args(&url, &referrer, &cookies);
    let ff = ffmpeg();
    let fmt = formats(&url, ff.is_some());
    let mut look = command(&p);
    look.args(&args).args(["-J", "-f", &fmt, "--"]).arg(&url);
    let Some(looked) = output_unless_stopped(job, look)? else { return Ok(false) };
    if !looked.status.success() {
        return Err(last_error(&String::from_utf8_lossy(&looked.stderr)));
    }
    let v = video_info(serde_json::from_slice(&looked.stdout).map_err(|e| e.to_string())?)?;
    let streams = picked(&v).ok_or("yt-dlp gave no formats")?;
    // A single extraction also names jobs added without the File Info window (CLI, queues).
    let dest = {
        let mut i = job.info();
        if i.name.is_empty() { i.name = video_name(&v); }
        std::fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
        i.path()
    };
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let outs: Vec<PathBuf> = if streams.len() == 2 {
        ["ndm-video", "ndm-audio"].iter().zip(&streams).map(|(tag, s)| dir.join(format!("{stem}.{tag}.{}", s.ext))).collect()
    } else {
        vec![dest.clone()]
    };
    // Preserve extractor-specific fields and cookies for protocol-specific downloaders. Remove this
    // private file on success, failure, Stop, or a Rust error while starting a subprocess.
    let json = SavedInfo::new(dir.join(format!("{stem}.ndm-info.json")), &v)?;
    let source = ["--load-info-json".to_string(), json.0.display().to_string()];
    let fallback = |part: &Job, s: &Stream, out: &Path| {
        fetch_video(part, &p, &source, &args, &s.id, &template(out), s.size, global)
    };
    let got = fetch_streams(job, agent, global, &streams, &outs, &fallback)?;
    if !got {
        return Ok(false);
    }
    if let [video, audio] = &outs[..] {
        join(job, video, audio, &dest, ff.as_deref())?;
    }
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
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
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let expected = size(video) + size(audio);
    let watch = [dest.to_path_buf()];
    let label = "Joining video and audio...";
    let mut joined = crate::stream::with_post(job, label, &watch, expected, || crate::stream::join_files(video, audio, dest)).unwrap_or(false);
    if !joined {
        if let Some(ff) = ff {
            let _ = std::fs::remove_file(dest);
            joined = crate::stream::with_post(job, label, &watch, expected, || {
                command(ff).args(["-y", "-v", "error", "-i"]).arg(video).arg("-i").arg(audio).args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy"]).arg(dest).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
            });
        }
    }
    if joined {
        let _ = std::fs::remove_file(video);
        let _ = std::fs::remove_file(audio);
        return Ok(());
    }
    let _ = std::fs::remove_file(dest);
    Err(crate::i18n::tr_os("Could not join video and audio. Install ffmpeg next to NDM or on PATH, then resume the download. The separate streams have been kept."))
}

/// Fetch each stream concurrently with its appropriate backend. A failed native fetch switches only
/// that stream to yt-dlp. The parent's bar combines both streams, including mixed native/yt-dlp jobs.
fn fetch_streams(job: &Job, agent: &ureq::Agent, global: &Limiter, streams: &[Stream], outs: &[PathBuf],
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
            show_parts(job, &parts, &expected);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        show_parts(job, &parts, &expected);
        running.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a download thread stopped".into()))).collect()
    });
    if let Some(e) = results.iter().find_map(|r| r.as_ref().err()) { return Err(e.clone()); }
    Ok(!job.stopping() && results.iter().all(|r| matches!(r, Ok(true))))
}

#[cfg(test)]
fn fetch_direct(job: &Job, agent: &ureq::Agent, global: &Limiter, streams: &[Stream], outs: &[PathBuf]) -> Result<bool, String> {
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

/// The newest releases' files: GitHub sends ".../releases/latest/download/<file>" on to the newest one.
const YTDLP_LATEST: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const DENO_LATEST: &str = "https://github.com/denoland/deno/releases/latest/download";

/// The folder of NDM's own yt-dlp and Deno, beside its settings (added to PATH by `install`).
pub fn tools_dir() -> PathBuf {
    crate::core::store_path().parent().map(|d| d.join("tools")).unwrap_or_else(|| PathBuf::from("tools"))
}

/// yt-dlp's release file for this system: its stand-alone builds, which need no Python.
fn ytdlp_asset() -> &'static str {
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

/// Deno's release file for this system.
fn deno_asset() -> String {
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    let os = if cfg!(windows) { "pc-windows-msvc" } else if cfg!(target_os = "macos") { "apple-darwin" } else { "unknown-linux-gnu" };
    format!("deno-{arch}-{os}.zip")
}

/// The SHA-256 a sums file lists for `file` ("hex  name" lines; Deno's .sha256sum has one line).
fn listed_sum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hex, name) = l.split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name.rsplit(['/', '\\']).next() == Some(file) && hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
    })
}

/// The one SHA-256 in a file that has only one: "hex  name" (Linux, macOS) or PowerShell's
/// "Algorithm : SHA256 / Hash : HEX / Path : ..." (Deno's Windows releases).
fn only_sum(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit()).find(|t| t.len() == 64).map(str::to_ascii_lowercase)
}

fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent.get(url).call().map_err(|e| format!("{url}: {e}"))?.into_string().map_err(|e| e.to_string())
}

/// Downloads `url` to `out`, checked against SHA-256 `want` (a damaged file is removed). `got` / `total`
/// add up over every file of the install.
fn get_checked(agent: &ureq::Agent, url: &str, want: &str, out: &Path, got: &AtomicU64, total: &AtomicU64) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let r = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    total.fetch_add(r.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0), Relaxed);
    let mut file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let (mut body, mut hash, mut buf) = (r.into_reader(), Sha256::new(), vec![0u8; 1 << 16]);
    loop {
        let n = body.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got.fetch_add(n as u64, Relaxed);
    }
    drop(file);
    let sum = hash.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    if sum != want {
        let _ = std::fs::remove_file(out);
        let name = url.rsplit('/').next().unwrap_or(url);
        return Err(format!("{name}: the downloaded file is damaged (SHA-256 {sum}, expected {want})"));
    }
    Ok(())
}

/// Puts the new program `part` in place of `dest`, runnable. Windows can't overwrite a program that runs
/// (a video downloading), but can rename it out of the way.
fn put_in_place(part: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(part, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let old = dest.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(part, dest).is_err() {
        std::fs::rename(dest, &old).map_err(|e| e.to_string())?;
        std::fs::rename(part, dest).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The one file `name` (at any depth) from a zip archive's bytes: stored or deflated, zip64 offsets too.
fn unzip_one(zip: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let u16le = |at: usize| zip.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32le = |at: usize| zip.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64);
    let u64le = |at: usize| zip.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
    let bad = || "not a zip archive".to_string();
    let eocd = (0..zip.len().saturating_sub(21)).rev().take(70_000).find(|&i| zip[i..i + 4] == [0x50, 0x4b, 5, 6]).ok_or_else(bad)?;
    let (count, mut at) = (u16le(eocd + 10).ok_or_else(bad)?, u32le(eocd + 16).ok_or_else(bad)? as usize);
    // zip64: the central directory's place in the zip64 end record
    if at == 0xFFFF_FFFF && eocd >= 20 && zip[eocd - 20..eocd - 16] == [0x50, 0x4b, 6, 7] {
        let rec = u64le(eocd - 12).ok_or_else(bad)? as usize;
        at = u64le(rec + 48).ok_or_else(bad)? as usize;
    }
    for _ in 0..count.max(1) {
        if zip.get(at..at + 4) != Some(&[0x50, 0x4b, 1, 2]) {
            break;
        }
        let (method, nlen, xlen, clen) = (u16le(at + 10).ok_or_else(bad)?, u16le(at + 28).ok_or_else(bad)?, u16le(at + 30).ok_or_else(bad)?, u16le(at + 32).ok_or_else(bad)?);
        let (mut csize, mut usize_, mut local) = (u32le(at + 20).ok_or_else(bad)?, u32le(at + 24).ok_or_else(bad)?, u32le(at + 42).ok_or_else(bad)?);
        let entry = String::from_utf8_lossy(zip.get(at + 46..at + 46 + nlen).ok_or_else(bad)?).to_string();
        // zip64 sizes and offset, in that order, for the fields that are 0xFFFFFFFF
        let mut x = at + 46 + nlen;
        while x + 4 <= at + 46 + nlen + xlen {
            let (id, len) = (u16le(x).ok_or_else(bad)?, u16le(x + 2).ok_or_else(bad)?);
            if id == 1 {
                let mut v = x + 4;
                for f in [&mut usize_, &mut csize, &mut local] {
                    if *f == 0xFFFF_FFFF {
                        *f = u64le(v).ok_or_else(bad)?;
                        v += 8;
                    }
                }
            }
            x += 4 + len;
        }
        at += 46 + nlen + xlen + clen;
        if entry.rsplit('/').next() != Some(name) {
            continue;
        }
        let local = local as usize;
        let data = local + 30 + u16le(local + 26).ok_or_else(bad)? + u16le(local + 28).ok_or_else(bad)?;
        let raw = zip.get(data..data + csize as usize).ok_or_else(bad)?;
        return match method {
            0 => Ok(raw.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(usize_ as usize);
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut out).map_err(|e| e.to_string())?;
                Ok(out)
            }
            m => Err(format!("{name}: zip method {m} isn't supported")),
        };
    }
    Err(format!("{name} is not in the archive"))
}

/// The SHA-256 of the release file NDM's copy came from (written next to it).
fn installed_sum(dir: &Path, prog: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{prog}.sha256"))).unwrap_or_default().trim().to_string()
}

/// yt-dlp into `dir` from `base` (GitHub's newest release; a local stand-in in the tests), when the
/// release's file differs from the one NDM's copy came from or the copy doesn't run.
fn fetch_ytdlp(agent: &ureq::Agent, base: &str, dir: &Path, got: &AtomicU64, total: &AtomicU64) -> Result<(), String> {
    let file = ytdlp_asset();
    let want = listed_sum(&get_text(agent, &format!("{base}/SHA2-256SUMS"))?, file).ok_or_else(|| format!("{file} is not in yt-dlp's release"))?;
    let dest = dir.join(exe_file("yt-dlp"));
    if installed_sum(dir, "yt-dlp") == want && runs(&dest) {
        return Ok(());
    }
    let part = dir.join(format!("{}.part", exe_file("yt-dlp")));
    get_checked(agent, &format!("{base}/{file}"), &want, &part, got, total)?;
    put_in_place(&part, &dest)?;
    let _ = std::fs::write(dir.join("yt-dlp.sha256"), &want);
    Ok(())
}

/// Deno into `dir` from `base`, the same way (its release is a zip with the one program).
fn fetch_deno(agent: &ureq::Agent, base: &str, dir: &Path, got: &AtomicU64, total: &AtomicU64) -> Result<(), String> {
    let zip = deno_asset();
    let want = only_sum(&get_text(agent, &format!("{base}/{zip}.sha256sum"))?).ok_or_else(|| format!("{zip}: no SHA-256 in its release"))?;
    let dest = dir.join(exe_file("deno"));
    if installed_sum(dir, "deno") == want && runs(&dest) {
        return Ok(());
    }
    let archive = dir.join(format!("{zip}.part"));
    get_checked(agent, &format!("{base}/{zip}"), &want, &archive, got, total)?;
    let bytes = std::fs::read(&archive).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&archive);
    let program = unzip_one(&bytes?, &exe_file("deno"))?;
    let part = dir.join(format!("{}.part", exe_file("deno")));
    std::fs::write(&part, program).map_err(|e| e.to_string())?;
    put_in_place(&part, &dest)?;
    let _ = std::fs::write(dir.join("deno.sha256"), &want);
    Ok(())
}

/// Downloads (or brings up to date) yt-dlp and Deno in NDM's tools folder, each checked against the SHA-256
/// its release lists, and makes sure the folder is on PATH. `got` / `total`: progress for the window.
/// Returns yt-dlp's version.
pub fn install(agent: &ureq::Agent, got: &AtomicU64, total: &AtomicU64) -> Result<String, String> {
    let dir = tools_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fetch_ytdlp(agent, YTDLP_LATEST, &dir, got, total)?;
    fetch_deno(agent, DENO_LATEST, &dir, got, total)?;
    add_to_path(&dir);
    Ok(version(&dir.join(exe_file("yt-dlp"))))
}

fn version(p: &Path) -> String {
    command(p).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|v| !v.is_empty()).unwrap_or_default()
}

/// Puts NDM's tools folder on the user's PATH, so yt-dlp works in a terminal too: the user's Path in the
/// registry on Windows, links in ~/.local/bin on Linux, a line in ~/.zprofile on macOS.
fn add_to_path(dir: &Path) {
    #[cfg(windows)]
    crate::win::add_user_path(dir);
    #[cfg(target_os = "linux")]
    if let Some(home) = std::env::var_os("HOME") {
        let bin = Path::new(&home).join(".local/bin");
        let _ = std::fs::create_dir_all(&bin);
        for prog in ["yt-dlp", "deno"] {
            let link = bin.join(prog);
            // a program of the user's own stays
            if std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(&link);
            }
            let _ = std::os::unix::fs::symlink(dir.join(prog), &link);
        }
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

/// Keeps NDM's copies up to date: once a day, when there is one (video sites change often and an old
/// yt-dlp stops working on them). Only the small checksum lists are fetched unless a release changed.
pub fn keep_updated(core: std::sync::Arc<crate::core::Core>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        loop {
            let now = chrono::Local::now().timestamp();
            let due = now - core.settings().ytdlp_checked > 86400;
            if due && tools_dir().join(exe_file("yt-dlp")).is_file() {
                core.settings().ytdlp_checked = now;
                let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
                if let Err(e) = install(&core.agent(YTDLP_LATEST), &got, &total) {
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
    fn extractor_results_are_not_limited_to_known_sites() {
        use serde_json::json;
        let page = "https://media.example.org/podcast/episode";
        assert!(!is_video_page(page));
        assert!(wants(&Info { url: page.into(), ytdlp: true, ..Default::default() }));
        let info = video_info(json!({"_type": "playlist", "entries": [null,
            {"title": "An episode", "ext": "m4a", "format_id": "audio", "protocol": "https", "url": "https://cdn.example.org/signed?token=x"}
        ]})).unwrap();
        assert_eq!(video_name(&info), "An episode.m4a");
        let streams = picked(&info).unwrap();
        assert_eq!(streams.len(), 1);
        assert!(streams[0].direct);
        assert_eq!(streams[0].id, "audio");
        assert!(video_info(json!({"entries": []})).is_err());
        for extra in [json!({"fragments": []}), json!({"is_live": true}), json!({"downloader_options": {"impersonate": "chrome"}})] {
            let mut f = info.clone();
            f.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            assert!(!Stream::of(&f).unwrap().direct, "{extra}");
        }
        let args = base_args(page, page, "session=abc");
        assert!(args.iter().any(|a| a == "--ignore-config"));
        assert!(args.iter().any(|a| a == "Cookie: session=abc"));
    }

    #[test]
    fn fallback_streams_download_together_and_stop_on_error() {
        use std::sync::atomic::AtomicUsize;
        let job = Job::new(Info { conns: 4, ..Default::default() });
        let make = |id: &str| Stream { id: id.into(), url: String::new(), ext: "mp4".into(), size: 16, headers: vec![], direct: false };
        let mut streams = [make("video"), make("audio")];
        // One native stream fails before receiving bytes and switches to the fallback while its
        // companion is already downloading there. No network is needed for an invalid URL.
        streams[0].direct = true;
        streams[0].url = "invalid-url".into();
        let outs = [PathBuf::from("video"), PathBuf::from("audio")];
        let active = AtomicUsize::new(0);
        let fallback = |part: &Job, _: &Stream, _: &Path| {
            assert_eq!(part.info().conns, 4);
            active.fetch_add(1, Relaxed);
            let start = std::time::Instant::now();
            while active.load(Relaxed) < 2 && start.elapsed().as_secs() < 3 { std::thread::yield_now(); }
            assert_eq!(active.load(Relaxed), 2, "both streams start concurrently");
            part.info().segs = vec![Seg { next: 16, stop: Some(16), ..Default::default() }];
            Ok(true)
        };
        let agent = crate::engine::agent("", "ndm-test");
        assert_eq!(fetch_streams(&job, &agent, &Limiter::default(), &streams, &outs, &fallback), Ok(true));
        assert_eq!(job.info().done(), 32);
        let failed = |part: &Job, stream: &Stream, _: &Path| {
            if stream.id == "audio" { return Err("audio failed".into()); }
            let start = std::time::Instant::now();
            while !part.stopping() && start.elapsed().as_secs() < 3 { std::thread::sleep(std::time::Duration::from_millis(10)); }
            assert!(part.stopping(), "failed companion stops the other stream");
            Ok(false)
        };
        assert_eq!(fetch_streams(&job, &agent, &Limiter::default(), &streams, &outs, &failed), Err("audio failed".into()));
    }

    #[test]
    fn failed_merge_keeps_sources_and_reports_error() {
        let dir = std::env::temp_dir().join(format!("ndm-merge-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (video, audio, dest) = (dir.join("v.mp4"), dir.join("a.m4a"), dir.join("out.mp4"));
        std::fs::write(&video, b"not an MP4").unwrap();
        std::fs::write(&audio, b"not an M4A").unwrap();
        assert!(join(&Job::new(Info::default()), &video, &audio, &dest, None).is_err());
        assert!(video.exists() && audio.exists() && !dest.exists());
        let saved = SavedInfo::new(dir.join("private.json"), &serde_json::json!({"cookies": "secret"})).unwrap();
        let path = saved.0.clone();
        assert!(path.exists());
        drop(saved);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn silent_downloader_stops() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ndm-silent-ytdlp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("yt-dlp");
        std::fs::write(&bin, b"#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let job = Job::new(Info::default());
        let start = std::time::Instant::now();
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                job.stop();
            });
            fetch_video(&job, &bin, &[], &[], "v", "unused", 0, &Limiter::default())
        });
        assert_eq!(result, Ok(false));
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn video_pages() {
        assert!(is_video_page("https://www.youtube.com/watch?v=abc"));
        assert!(is_video_page("https://youtu.be/abc"));
        assert!(is_video_page("https://vimeo.com/12345"));
        assert!(!is_video_page("https://notyoutube.com/watch"));
        assert!(!is_video_page("https://video.twimg.com/a/b.mp4"));
        assert!(!is_video_page("https://cdn.vimeo.com/file.mp4"), "a file is downloaded by NDM itself");
        assert!(!is_video_page("ftp://youtube.com/x"));
        assert!(is_video_page("https://www.tiktok.com/@a/video/123"));
        assert!(is_video_page("https://www.threads.net/@a/post/xyz"));
        assert!(!is_video_page("https://i.imgur.com/abc.gif"), "a picture on a video site is a file");
        assert!(!is_video_page("https://www.linkedin.com/dms/report.pdf"));
        assert_eq!(last_error("[info] x\nERROR: [youtube] abc: Video unavailable\n"), "[youtube] abc: Video unavailable");
        let sums = "aa  yt-dlp\n0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa  yt-dlp_macos\n";
        assert_eq!(listed_sum(sums, "yt-dlp_macos").as_deref(), Some("0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa"));
        assert_eq!(listed_sum(sums, "yt-dlp"), None);
        let sum = "e890975bc2549807bbcc0b584ffafda72cc7c93349dd99c80483dd784f6c4609";
        assert_eq!(only_sum(&format!("{sum}  deno-x86_64-unknown-linux-gnu.zip\n")).as_deref(), Some(sum));
        let ps = format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\nPath      : D:\\a\\deno\\deno-x86_64-pc-windows-msvc.zip\r\n", sum.to_uppercase());
        assert_eq!(only_sum(&ps).as_deref(), Some(sum), "PowerShell's Get-FileHash, as Deno's Windows releases have it");
        // YouTube gets no cookies, other sites do
        assert!(!base_args("https://www.youtube.com/watch?v=a", "", "SID=1").iter().any(|a| a.starts_with("Cookie")));
        assert!(base_args("https://www.instagram.com/p/a", "", "s=1").iter().any(|a| a == "Cookie: s=1"));
        assert_eq!(template(Path::new("/d/100% fun.mp4")), "/d/100%% fun.mp4");
        assert_eq!(cookie_header("tt_chain_token=abc; Domain=.tiktok.com; Path=/; Secure; Expires=1790000000; ttwid=1%7Cx; Domain=.tiktok.com; Path=/"), "tt_chain_token=abc; ttwid=1%7Cx");
        assert_eq!(cookie_header(""), "");
        assert!(last_error("ERROR: [generic] Unable to download webpage: HTTP Error 404\n").contains("[generic]"));
        assert!(last_error("ERROR: Unsupported URL: https://www.tiktok.com/foryou\n").starts_with("There is no video"));
    }

    /// A zip as Deno's release has it: one program, deflated (or stored), maybe in a folder.
    pub(super) fn zip_of(name: &str, data: &[u8], deflate: bool) -> Vec<u8> {
        use std::io::Write;
        let body = if deflate {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let method: u16 = if deflate { 8 } else { 0 };
        let n = name.len() as u16;
        let mut z = Vec::new();
        z.extend([0x50, 0x4b, 3, 4, 20, 0, 0, 0]);
        z.extend(method.to_le_bytes());
        z.extend([0u8; 8]); // time, date, crc
        z.extend((body.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend(n.to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(name.as_bytes());
        z.extend(&body);
        let cd = z.len() as u32;
        z.extend([0x50, 0x4b, 1, 2, 20, 0, 20, 0, 0, 0]);
        z.extend(method.to_le_bytes());
        z.extend([0u8; 8]);
        z.extend((body.len() as u32).to_le_bytes());
        z.extend((data.len() as u32).to_le_bytes());
        z.extend(n.to_le_bytes());
        z.extend([0u8; 12]); // extra, comment, disk, attributes
        z.extend(0u32.to_le_bytes()); // local header offset
        z.extend(name.as_bytes());
        let cdlen = z.len() as u32 - cd;
        z.extend([0x50, 0x4b, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend(cdlen.to_le_bytes());
        z.extend(cd.to_le_bytes());
        z.extend([0, 0]);
        z
    }

    #[test]
    fn unzips_deno() {
        let prog = b"#!/bin/sh\necho deno 9.9.9\n".repeat(50);
        assert_eq!(unzip_one(&zip_of("deno", &prog, true), "deno").unwrap(), prog);
        assert_eq!(unzip_one(&zip_of("bin/deno.exe", &prog, false), "deno.exe").unwrap(), prog);
        assert!(unzip_one(&zip_of("other", &prog, true), "deno").is_err());
        assert!(unzip_one(b"not a zip at all, just some bytes here", "deno").is_err());
    }

    /// Set yt-dlp against a local stand-in for GitHub's newest-release downloads: files checked against
    /// the listed SHA-256, a damaged one refused, up-to-date copies left alone.
    #[cfg(unix)]
    #[test]
    fn installs_and_checks() {
        use sha2::{Digest, Sha256};
        use std::io::{BufRead, BufReader, Write};
        let hex = |d: &[u8]| Sha256::digest(d).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let ytdlp = b"#!/bin/sh\necho 2099.01.02\n".to_vec();
        let deno_zip = zip_of("deno", b"#!/bin/sh\necho deno 9.9.9\n", true);
        let sums = std::sync::Arc::new(std::sync::Mutex::new(format!("{}  {}\n", hex(&ytdlp), ytdlp_asset())));
        let served = std::sync::Arc::new(AtomicU64::new(0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/latest/download", listener.local_addr().unwrap());
        let (s2, n2) = (sums.clone(), served.clone());
        let (yt, dz, dsum) = (ytdlp.clone(), deno_zip.clone(), format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\n", hex(&deno_zip).to_uppercase()));
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut c = c;
                let body: Option<Vec<u8>> = if path.ends_with("/SHA2-256SUMS") {
                    Some(s2.lock().unwrap().clone().into_bytes())
                } else if path.ends_with(&format!("/{}", ytdlp_asset())) {
                    n2.fetch_add(1, Relaxed);
                    Some(yt.clone())
                } else if path.ends_with(&format!("/{}.sha256sum", deno_asset())) {
                    Some(dsum.clone().into_bytes())
                } else if path.ends_with(&format!("/{}", deno_asset())) {
                    n2.fetch_add(1, Relaxed);
                    Some(dz.clone())
                } else {
                    None
                };
                let (head, body) = match body {
                    Some(b) => ("200 OK", b),
                    None => ("404 Not Found", vec![]),
                };
                let _ = write!(c, "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = c.write_all(&body);
            }
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-ytdlp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let agent = ureq::AgentBuilder::new().build();
        let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
        // a damaged download is refused and leaves nothing behind
        *sums.lock().unwrap() = format!("{}  {}\n", "0".repeat(64), ytdlp_asset());
        assert!(fetch_ytdlp(&agent, &base, &dir, &got, &total).unwrap_err().contains("damaged"));
        assert!(!dir.join("yt-dlp").exists() && !dir.join("yt-dlp.part").exists());
        *sums.lock().unwrap() = format!("{}  {}\n", hex(&ytdlp), ytdlp_asset());
        fetch_ytdlp(&agent, &base, &dir, &got, &total).unwrap();
        fetch_deno(&agent, &base, &dir, &got, &total).unwrap();
        assert_eq!(std::fs::read(dir.join("yt-dlp")).unwrap(), ytdlp);
        assert!(runs(&dir.join("yt-dlp")) && runs(&dir.join("deno")), "installed programs run");
        assert_eq!(version(&dir.join("yt-dlp")), "2099.01.02");
        assert_eq!(total.load(Relaxed), (2 * ytdlp.len() + deno_zip.len()) as u64, "the damaged try, then both");
        assert!(!dir.read_dir().unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".part")));
        // up to date: neither is downloaded again
        fetch_ytdlp(&agent, &base, &dir, &got, &total).unwrap();
        fetch_deno(&agent, &base, &dir, &got, &total).unwrap();
        assert_eq!(served.load(Relaxed), 3, "the damaged try, yt-dlp and Deno, nothing more");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The real yt-dlp (NDM_TEST_YTDLP=<its path>) on a site with separate video and audio streams, as
    /// YouTube has them, without ffmpeg: video, then audio, joined by NDM into one MP4.
    #[test]
    #[ignore = "needs the real yt-dlp: NDM_TEST_YTDLP=/path/to/yt-dlp cargo test real_ytdlp -- --ignored"]
    fn real_ytdlp_separate_streams() {
        use crate::stream::tests::{frag, init};
        use std::io::{BufRead, BufReader, Write};
        let bin = std::env::var("NDM_TEST_YTDLP").expect("NDM_TEST_YTDLP");
        let home = std::env::temp_dir().join(format!("ndm-real-ytdlp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let tools = home.join("NewDownloadManager/tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::copy(&bin, tools.join(exe_file("yt-dlp"))).unwrap();
        // Also find the isolated executable through PATH: macOS stores NDM's configuration under
        // Application Support rather than XDG_CONFIG_HOME/APPDATA. No ffmpeg is on this PATH.
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::env::set_var("APPDATA", &home);
        std::env::set_var("PATH", &tools);
        let seg = |kind: &str| format!(r#"<SegmentList timescale="1000" duration="2000"><Initialization sourceURL="{kind}init.mp4"/><SegmentURL media="{kind}1.m4s"/><SegmentURL media="{kind}2.m4s"/></SegmentList>"#);
        let mpd = format!(
            r#"<?xml version="1.0"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT4S" minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-live:2011"><Period>
<AdaptationSet mimeType="video/mp4" contentType="video"><Representation id="v1" codecs="avc1.4d401f" width="640" height="360" bandwidth="500000">{}</Representation></AdaptationSet>
<AdaptationSet mimeType="audio/mp4" contentType="audio" lang="en"><Representation id="a1" codecs="mp4a.40.2" audioSamplingRate="48000" bandwidth="128000">{}</Representation></AdaptationSet>
</Period></MPD>"#,
            seg("v"),
            seg("a")
        );
        let files: Vec<(String, Vec<u8>, &str)> = vec![
            ("clip.mpd".into(), mpd.into_bytes(), "application/dash+xml"),
            ("clip.m3u8".into(), b"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"vinit.mp4\"\n#EXTINF:2,\nv1.m4s\n#EXTINF:2,\nv2.m4s\n#EXT-X-ENDLIST\n".to_vec(), "application/vnd.apple.mpegurl"),
            ("vinit.mp4".into(), init(1, 1000), "video/mp4"),
            ("v1.m4s".into(), frag(1, 0, b'v'), "video/mp4"),
            ("v2.m4s".into(), frag(1, 2000, b'w'), "video/mp4"),
            ("ainit.mp4".into(), init(1, 48000), "audio/mp4"),
            ("a1.m4s".into(), frag(1, 0, b'a'), "audio/mp4"),
            ("a2.m4s".into(), frag(1, 96000, b'b'), "audio/mp4"),
            ("movie.mp4".into(), [init(1, 1000), frag(1, 0, b'm')].concat(), "video/mp4"),
        ];
        let movie = [init(1, 1000), frag(1, 0, b'm')].concat();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        let files = std::sync::Arc::new(files);
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = maximum.clone();
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                let (files, active, maximum) = (files.clone(), active.clone(), maximum.clone());
                std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line.split_whitespace().nth(1).unwrap_or("").trim_start_matches('/').split('?').next().unwrap_or("").to_string();
                let fragment = path.ends_with(".m4s");
                if fragment {
                    let live = active.fetch_add(1, Relaxed) + 1;
                    maximum.fetch_max(live, Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                let mut c = c;
                match files.iter().find(|f| f.0 == path) {
                    Some((_, body, ct)) => {
                        let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        let _ = c.write_all(body);
                    }
                    None => {
                        let _ = write!(c, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
                if fragment { active.fetch_sub(1, Relaxed); }
                });
            }
        });
        assert!(ffmpeg().is_none(), "ffmpeg hidden");
        let dir = home.join("Video");
        let job = Job::new(crate::engine::Info { url: format!("{base}clip.mpd"), folder: dir.display().to_string(), name: "clip.mp4".into(), conns: 4, ..Default::default() });
        assert_eq!(run(&job, &crate::engine::agent("", "ndm-test"), &Limiter::default()), Ok(true));
        let d = std::fs::read(dir.join("clip.mp4")).unwrap();
        let top = crate::stream::boxes(&d, 0, d.len());
        let moov = top.iter().find(|b| &b.3 == b"moov").copied().expect("one MP4");
        assert_eq!(crate::stream::find(&d, moov, &[b"trak"]).is_some(), true);
        let traks = crate::stream::boxes(&d, moov.1, moov.2).iter().filter(|b| &b.3 == b"trak").count();
        assert_eq!(traks, 2, "video and audio in one file");
        let order: Vec<u8> = top.iter().filter(|b| &b.3 == b"mdat").map(|b| d[b.1]).collect();
        assert_eq!(order, *b"vawb");
        let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left, ["clip.mp4"], "the separate streams are cleaned up");
        assert_eq!(job.info().size, Some(d.len() as u64));
        assert!(peak.load(Relaxed) > 2, "DASH fragments and companion streams fetched concurrently");
        // HLS uses the same concurrent fragment path, preserving the assembled fragment order.
        peak.store(0, Relaxed);
        let job = Job::new(Info { url: format!("{base}clip.m3u8"), folder: dir.display().to_string(), name: "hls.mp4".into(), conns: 4, ..Default::default() });
        assert_eq!(run(&job, &crate::engine::agent("", "ndm-test"), &Limiter::default()), Ok(true));
        assert_eq!(std::fs::read(dir.join("hls.mp4")).unwrap(), [init(1, 1000), frag(1, 0, b'v'), frag(1, 2000, b'w')].concat());
        assert!(peak.load(Relaxed) > 1, "HLS fragments fetched concurrently");
        // a site with one file that has both: no separate streams, so that file
        let job = Job::new(crate::engine::Info { url: format!("{base}movie.mp4"), folder: dir.display().to_string(), name: "movie.mp4".into(), ..Default::default() });
        assert_eq!(run(&job, &crate::engine::agent("", "ndm-test"), &Limiter::default()), Ok(true));
        assert_eq!(std::fs::read(dir.join("movie.mp4")).unwrap(), movie);
        // and yt-dlp's own error when there is nothing
        let job = Job::new(crate::engine::Info { url: format!("{base}gone.mp4"), folder: dir.display().to_string(), name: "gone.mp4".into(), ..Default::default() });
        let e = run(&job, &crate::engine::agent("", "ndm-test"), &Limiter::default()).unwrap_err();
        assert!(e.contains("404"), "{e}");
        let _ = std::fs::remove_dir_all(&home);
    }
    /// Deno's real release zip (NDM_TEST_DENO_ZIP=<path>, NDM_TEST_DENO_SHA256=<the program's SHA-256>).
    #[test]
    #[ignore = "needs Deno's release zip"]
    fn real_deno_zip() {
        use sha2::{Digest, Sha256};
        let zip = std::fs::read(std::env::var("NDM_TEST_DENO_ZIP").unwrap()).unwrap();
        let name = std::env::var("NDM_TEST_DENO_NAME").unwrap_or("deno".into());
        let prog = unzip_one(&zip, &name).unwrap();
        let sum = Sha256::digest(&prog).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(sum, std::env::var("NDM_TEST_DENO_SHA256").unwrap());
    }
    /// The video and audio streams fetched by NDM's engine at once, each over several connections; the
    /// parent's bar only moves forward against a total that stays put.
    #[test]
    fn ndm_fetches_both_streams() {
        use std::sync::Arc;
        let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 251) as u8).collect();
        let base = crate::engine::tests::serve(Arc::new(data.clone()));
        let dir = std::env::temp_dir().join(format!("ndm-direct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let job = Job::new(Info { conns: 4, folder: dir.display().to_string(), name: "x.mp4".into(), ..Default::default() });
        let stream = |id: &str, path: &str, ext: &str| Stream { id: id.into(), url: format!("{base}{path}"), ext: ext.into(), size: data.len() as u64, headers: vec![("User-Agent".into(), "ndm-test/1".into())], direct: true };
        let streams = [stream("137", "rslow-v.mp4", "mp4"), stream("140", "r-a.m4a", "m4a")];
        let outs = [dir.join("x.ndm-video.mp4"), dir.join("x.ndm-audio.m4a")];
        let agent = crate::engine::agent("", "ndm-test");
        let done = std::sync::atomic::AtomicBool::new(false);
        let (ok, samples) = std::thread::scope(|sc| {
            let watch = sc.spawn(|| {
                let mut v = Vec::new();
                while !done.load(Relaxed) {
                    let i = job.info();
                    v.push((i.done(), i.size, i.segs.iter().filter(|s| s.owned).count()));
                    drop(i);
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
                v
            });
            let ok = fetch_direct(&job, &agent, &Limiter::default(), &streams, &outs);
            done.store(true, Relaxed);
            (ok, watch.join().unwrap())
        });
        assert_eq!(ok, Ok(true));
        for out in &outs {
            assert_eq!(std::fs::read(out).unwrap(), data, "{out:?}");
        }
        assert!(samples.windows(2).all(|w| w[1].0 >= w[0].0), "the bar never goes back");
        let total = 2 * data.len() as u64;
        assert!(samples.iter().all(|s| s.1 == Some(total)), "the total stays put: {:?}", samples.iter().map(|s| s.1).collect::<Vec<_>>());
        assert!(samples.iter().map(|s| s.2).max().unwrap() > 2, "several connections at once");
        assert_eq!(job.info().done(), total);
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A page through the real yt-dlp (NDM_TEST_YTDLP), as the app downloads it: NDM_TEST_PAGE, into a
    /// temporary folder; ffmpeg on PATH unless NDM_TEST_NO_FFMPEG.
    #[cfg(unix)]
    #[test]
    #[ignore = "needs the real yt-dlp and a page: NDM_TEST_YTDLP=... NDM_TEST_PAGE=... cargo test real_ytdlp_page -- --ignored"]
    fn real_ytdlp_page() {
        let bin = std::env::var("NDM_TEST_YTDLP").expect("NDM_TEST_YTDLP");
        let home = std::env::temp_dir().join(format!("ndm-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let tools = home.join("NewDownloadManager/tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::copy(&bin, tools.join("yt-dlp")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &home);
        if std::env::var("NDM_TEST_NO_FFMPEG").is_ok() {
            std::env::set_var("PATH", "/nonexistent");
        }
        let dir = home.join("Video");
        let job = Job::new(crate::engine::Info { url: std::env::var("NDM_TEST_PAGE").unwrap(), folder: dir.display().to_string(), conns: 4, ..Default::default() });
        let t = std::time::Instant::now();
        let r = run(&job, &crate::engine::agent("", "ndm-test"), &Limiter::default());
        eprintln!("{r:?} in {:?}: {:?}", t.elapsed(), std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| (e.file_name(), e.metadata().map(|m| m.len()).unwrap_or(0))).collect::<Vec<_>>()));
        assert_eq!(r, Ok(true));
    }
    #[test]
    fn formats_per_site() {
        let tiktok = formats("https://www.tiktok.com/@a/video/1", false);
        assert!(tiktok.starts_with("b[vcodec^=h264]/"), "{tiktok}");
        assert!(formats("https://m.douyin.com/video/1", true).starts_with("b[vcodec^=h264]/"));
        assert_eq!(formats("https://www.youtube.com/watch?v=x", false), PAIR);
        assert_eq!(formats("https://www.youtube.com/watch?v=x", true), MERGED);
        assert_eq!(formats("https://vimeo.com/1", true), MERGED);
        assert_eq!(formats("https://vimeo.com/1", false), format!("b/{PAIR}"));
    }
    /// TikTok's streams are on tiktok.com with no file type in the address: one yt-dlp found is fetched
    /// as it is, not looked up again (which went on forever, the download stuck at "Connecting...").
    #[test]
    fn found_streams_stay_plain() {
        let cdn = "https://v16-webapp-prime.tiktok.com/video/tos/useast2a/tos-useast2a-pve-0068/oQ/?a=1988&mime_type=video_mp4&tk=tt_chain_token";
        assert!(is_video_page(cdn), "on the site, no file type");
        assert!(wants(&Info { url: cdn.into(), ..Default::default() }));
        assert!(!wants(&Info { url: cdn.into(), plain: true, ..Default::default() }));
        assert!(!wants(&Info { url: cdn.into(), plain: true, ytdlp: true, ..Default::default() }));
    }
    /// Stop ends a yt-dlp look-up at once instead of waiting for it.
    #[cfg(unix)]
    #[test]
    fn lookup_stops() {
        let job = Job::new(Info::default());
        job.stop();
        let mut c = Command::new("sleep");
        c.arg("30");
        let t = std::time::Instant::now();
        assert!(output_unless_stopped(&job, c).unwrap().is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        let mut c = Command::new("echo");
        c.arg("hi");
        let out = output_unless_stopped(&Job::new(Info::default()), c).unwrap().unwrap();
        assert_eq!(out.stdout, b"hi\n");
    }
}
