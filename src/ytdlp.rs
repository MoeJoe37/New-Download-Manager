//! Videos on sites that play them in their own player (YouTube, Vimeo, TikTok...): the page's address is
//! handed to yt-dlp (https://github.com/yt-dlp/yt-dlp), which finds the streams and downloads them.
//! Downloads -> Set yt-dlp installs yt-dlp and Deno (the JavaScript runtime yt-dlp needs for YouTube) in
//! NDM's tools folder and keeps them up to date. The best video and audio are joined by ffmpeg when it is
//! installed, else by NDM itself (stream::join_files).
use crate::engine::{sanitize, Info, Job, Limiter, Probe, Seg, Status};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Sites whose pages hold a video for yt-dlp (the page itself, not a file on their servers); the browser
/// extension's "Download this video" menu item and panel use the same list (VIDEO_SITES).
const SITES: [&str; 22] = [
    "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "dai.ly", "twitch.tv", "tiktok.com", "instagram.com",
    "facebook.com", "fb.watch", "x.com", "twitter.com", "reddit.com", "v.redd.it", "soundcloud.com", "bilibili.com",
    "rumble.com", "streamable.com", "ok.ru", "vk.com", "bandcamp.com", "mixcloud.com",
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
    let plain = [".mp4", ".webm", ".mp3", ".m4a", ".zip", ".jpg", ".png", ".m3u8", ".mpd"].iter().any(|e| file.ends_with(e));
    !plain && SITES.iter().any(|s| on_site(&host, s))
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

fn ffmpeg() -> Option<PathBuf> {
    let ff = beside_or_path("ffmpeg");
    runs(&ff).then_some(ff)
}

/// The arguments every run gets: one video, the page's referrer and cookies (sign-in, age checks), and
/// NDM's Deno for YouTube. YouTube gets no cookies: signed in, yt-dlp turns to clients that need tokens
/// it doesn't have and finds fewer formats.
fn base_args(url: &str, referrer: &str, cookies: &str) -> Vec<String> {
    let mut a = vec!["--no-playlist".into(), "--no-warnings".into()];
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

/// The last "ERROR: ..." yt-dlp printed, as the download's error.
fn last_error(err: &str) -> String {
    err.lines().rev().find_map(|l| l.strip_prefix("ERROR: ")).unwrap_or(err.lines().last().unwrap_or("yt-dlp failed")).trim().to_string()
}

/// With ffmpeg: the best video and audio, merged by yt-dlp into an MP4.
const MERGED: &str = "bv*+ba/b";
/// Without: the best video-only stream NDM can join with an audio stream (MP4, else WebM).
const VIDEO_ONLY: &str = "bv*[acodec=none][ext=mp4]/bv*[acodec=none][ext=webm]";
/// The single file with both, when a site has no separate streams.
const SINGLE: &str = "b";
/// Without ffmpeg: the best video-only stream and the audio NDM can join it with (MP4 and M4A, else WebM
/// and WebM), else the single file.
const PAIR: &str = "bv*[acodec=none][ext=mp4]+ba[vcodec=none][ext=m4a]/bv*[acodec=none][ext=webm]+ba[vcodec=none][ext=webm]/b";

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
        let url = text("url");
        let direct = matches!(text("protocol").as_str(), "http" | "https") && (url.starts_with("http://") || url.starts_with("https://"));
        Some(Stream { id: f.get("format_id")?.as_str()?.to_string(), url, ext: text("ext"), size, headers, direct })
    }
}

/// What the look-up picked: the video and audio streams, or the one stream that has both.
fn picked(json: &[u8]) -> Option<Vec<Stream>> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    match v.get("requested_formats").and_then(|r| r.as_array()).filter(|r| r.len() == 2) {
        Some(r) => r.iter().map(Stream::of).collect(),
        None => Some(vec![Stream::of(&v)?]),
    }
}

/// The video's file name (title and type) and size, if yt-dlp knows it.
pub fn probe(url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    let p = yt_dlp()?;
    let ff = ffmpeg();
    let fmt = if ff.is_some() { MERGED.to_string() } else { format!("{VIDEO_ONLY}/{SINGLE}") };
    let out = command(&p)
        .args(base_args(url, referrer, cookies))
        .args(["-f", &fmt, "--skip-download", "--print", "%(title)s", "--print", "%(ext)s"])
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let (Some(title), Some(ext)) = (lines.next(), lines.next()) else {
        return Err(last_error(&String::from_utf8_lossy(&out.stderr)));
    };
    let ext = if ff.is_some() { "mp4" } else { ext.trim() };
    let name = sanitize(&format!("{}.{ext}", title.trim()));
    // the size shows as the streams arrive (video and audio come separately)
    Ok(Probe { name, size: None, ranges: false, modified: String::new(), stream: false })
}

/// What one yt-dlp run left: done, stopped by the user, or yt-dlp's error.
enum Got {
    Done,
    Stopped,
    Failed(String),
}

/// One yt-dlp run with format `fmt` into `out` (an output template); its progress goes on the job's bar
/// after `before` bytes of earlier runs. Returns the bytes this run fetched.
fn fetch_video(job: &Job, p: &Path, source: &[String], args: &[String], fmt: &str, out: &str, before: u64, expected: u64) -> Result<(Got, u64), String> {
    let mut child = command(p)
        .args(args)
        .args(["-f", fmt, "--newline", "--continue", "--progress-template", "download:ndm %(progress.downloaded_bytes)s %(progress.total_bytes,progress.total_bytes_estimate)s", "-o", out])
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
    // with ffmpeg, video then audio in one run: each part counts from 0, the bar adds them up
    let (mut base, mut last, mut part_total) = (0u64, 0u64, 0u64);
    let mut stopped = false;
    // after the download, yt-dlp's ffmpeg joins video and audio (its "[Merger]" line) into "<name>.temp.<ext>":
    // the bar follows that file growing towards the bytes fetched
    let merging: std::sync::Mutex<Option<(PathBuf, u64)>> = Default::default();
    let done_reading = std::sync::atomic::AtomicBool::new(false);
    let ok = std::thread::scope(|sc| {
        sc.spawn(|| {
            while !done_reading.load(Relaxed) {
                if let Some((out, expected)) = merging.lock().unwrap().clone() {
                    let have = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                    let label = job.info().post.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| "Processing...".into());
                    job.set_post(&label, (have as f64 / expected.max(1) as f64).min(0.99) as f32);
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        });
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix("ndm ") {
                let mut it = rest.split_whitespace().map(|v| v.parse::<f64>().ok().map(|v| v as u64));
                let (done, total) = (it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0));
                if done < last {
                    base += part_total.max(last);
                }
                (last, part_total) = (done, total);
                // the size known from the look-up (video and audio) until the streams say more
                let all = (before + base + total.max(done)).max(expected);
                // the bar never goes back: the total only grows (yt-dlp's estimates), done only advances
                let mut i = job.info();
                i.post = None;
                i.size = Some(all).filter(|s| *s > 0).max(i.size);
                let next = (before + base + done).max(i.segs.first().map_or(0, |s| s.next));
                i.segs = vec![Seg { start: 0, next, stop: i.size, info: "Receiving data...".into(), owned: false, ..Default::default() }];
                if i.status == Status::Connecting {
                    i.status = Status::Downloading;
                }
            } else if let Some((label, out, expected)) = post_step(&line, base + part_total.max(last)) {
                // after the download, ffmpeg (through yt-dlp) writes "<name>.temp.<ext>": the bar follows it
                job.set_post(label, 0.0);
                *merging.lock().unwrap() = out.map(|o| (o, expected));
            }
            if job.stopping() {
                let _ = child.kill();
                stopped = true;
                break;
            }
        }
        let ok = child.wait().is_ok_and(|s| s.success());
        done_reading.store(true, Relaxed);
        ok
    });
    job.info().post = None;
    let errors = errors.join().unwrap_or_default();
    let fetched = base + part_total.max(last);
    Ok((if stopped { Got::Stopped } else if ok { Got::Done } else { Got::Failed(last_error(&errors)) }, fetched))
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

/// Downloads the video into the job's file. yt-dlp looks the page up and picks the streams (the best
/// video and its audio, or the one file that has both); plain files among them are fetched by NDM itself,
/// each over the download's connections in segments and both at once, others by yt-dlp. The video and
/// audio are then joined (NDM's own joiner, else ffmpeg) as a processing step of its own.
pub fn run(job: &Job, agent: &ureq::Agent, global: &Limiter) -> Result<bool, String> {
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
    let args = base_args(&url, &referrer, &cookies);
    let ff = ffmpeg();
    // with ffmpeg any video and audio can be joined; without, a pair NDM joins itself (MP4 + M4A, WebM + WebM)
    let fmt = if ff.is_some() { MERGED } else { PAIR };
    let looked = command(&p).args(&args).args(["-J", "-f", fmt]).arg(&url).output().map_err(|e| e.to_string())?;
    if !looked.status.success() {
        return Err(last_error(&String::from_utf8_lossy(&looked.stderr)));
    }
    let streams = picked(&looked.stdout).ok_or("yt-dlp gave no formats")?;
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let outs: Vec<PathBuf> = if streams.len() == 2 {
        ["ndm-video", "ndm-audio"].iter().zip(&streams).map(|(tag, s)| dir.join(format!("{stem}.{tag}.{}", s.ext))).collect()
    } else {
        vec![dest.clone()]
    };
    let got = if streams.iter().all(|s| s.direct) {
        fetch_direct(job, agent, global, &streams, &outs)?
    } else {
        // DASH, HLS...: yt-dlp downloads them from the look-up, one after the other
        let json = dir.join(format!("{stem}.ndm-info.json"));
        std::fs::write(&json, &looked.stdout).map_err(|e| e.to_string())?;
        let source = ["--load-info-json".to_string(), json.display().to_string()];
        let expected = if streams.iter().all(|s| s.size > 0) { streams.iter().map(|s| s.size).sum() } else { 0 };
        let mut got = Got::Done;
        let mut before = 0;
        for (s, out) in streams.iter().zip(&outs) {
            let (g, fetched) = fetch_video(job, &p, &source, &args, &s.id, &template(out), before, expected)?;
            before += fetched;
            if !matches!(g, Got::Done) {
                got = g;
                break;
            }
        }
        let _ = std::fs::remove_file(&json);
        match got {
            Got::Done => true,
            Got::Stopped => false,
            Got::Failed(e) => return Err(e),
        }
    };
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
/// joiner (MP4, WebM), else ffmpeg; failing both, the video is the file and its sound lies beside it.
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
    std::fs::rename(video, dest).map_err(|e| e.to_string())?;
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = audio.extension().unwrap_or_default().to_string_lossy().to_string();
    let _ = std::fs::rename(audio, dest.with_file_name(format!("{stem}.audio.{ext}")));
    Ok(())
}

/// The streams fetched by NDM's own engine, all at once, each a download of its own with the parent's
/// connections, segments, networks and speed limit; the parent's bar shows their segments one after the
/// other, against the size the look-up gave (or the servers', once they answer). False: stopped.
fn fetch_direct(job: &Job, agent: &ureq::Agent, global: &Limiter, streams: &[Stream], outs: &[PathBuf]) -> Result<bool, String> {
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
                ..Default::default()
            });
            *part.lanes.lock().unwrap() = lanes.clone();
            part
        })
        .collect();
    let expected: Vec<u64> = streams.iter().map(|s| s.size).collect();
    // the total from the look-up before anything arrives
    show_parts(job, &parts, &expected);
    let results: Vec<Result<bool, String>> = std::thread::scope(|sc| {
        let running: Vec<_> = parts.iter().map(|part| sc.spawn(move || part.run_part(agent, global))).collect();
        while running.iter().any(|h| !h.is_finished()) {
            if job.stopping() {
                parts.iter().for_each(|p| p.stop());
            }
            show_parts(job, &parts, &expected);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        show_parts(job, &parts, &expected);
        running.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a download thread stopped".into()))).collect()
    });
    for r in results {
        match r {
            Err(e) => return Err(e),
            Ok(false) => return Ok(false),
            Ok(true) => {}
        }
    }
    Ok(!job.stopping())
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
    fn video_pages() {
        assert!(is_video_page("https://www.youtube.com/watch?v=abc"));
        assert!(is_video_page("https://youtu.be/abc"));
        assert!(is_video_page("https://vimeo.com/12345"));
        assert!(!is_video_page("https://notyoutube.com/watch"));
        assert!(!is_video_page("https://video.twimg.com/a/b.mp4"));
        assert!(!is_video_page("https://cdn.vimeo.com/file.mp4"), "a file is downloaded by NDM itself");
        assert!(!is_video_page("ftp://youtube.com/x"));
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
    #[cfg(unix)]
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
        std::fs::copy(&bin, tools.join("yt-dlp")).unwrap();
        // NDM's tools folder under this home, and no ffmpeg on PATH
        std::env::set_var("XDG_CONFIG_HOME", &home);
        std::env::set_var("APPDATA", &home);
        std::env::set_var("PATH", "/nonexistent");
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
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line.split_whitespace().nth(1).unwrap_or("").trim_start_matches('/').split('?').next().unwrap_or("").to_string();
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
            }
        });
        assert!(ffmpeg().is_none(), "ffmpeg hidden");
        let dir = home.join("Video");
        let job = Job::new(crate::engine::Info { url: format!("{base}clip.mpd"), folder: dir.display().to_string(), name: "clip.mp4".into(), ..Default::default() });
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
}
