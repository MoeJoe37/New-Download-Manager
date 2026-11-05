//! Segmented video streams: HLS playlists (.m3u8) and DASH manifests (.mpd), as NDM's video panel
//! downloads them. The best quality is picked, the pieces are fetched over several connections into
//! `<file>.part\` (kept for resume), AES-128 pieces decrypted, and a separate audio track is mixed into
//! the video natively (fragmented MP4 here, MPEG-TS -> MP4 and WebM in `remux`; anything else through ffmpeg
//! when installed, else saved alongside). Live HLS playlists and live (dynamic) DASH manifests are followed
//! until they end or the user stops.
mod dash;
pub(crate) mod download;
pub(crate) mod fmp4;
mod hls;
#[cfg(test)]
mod tests;

use self::dash::{dash, dash_variant, kids, num};
use self::fmp4::{first_time, mux_mp4, retime, timed};
use self::hls::{attrs, hls, hls_media};
use crate::engine::{self, probe::Probe, Job, Limiter, Seg, Status, RETRIES};
use crate::remux;
use crate::transport;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const MAX_PARTS: usize = 100_000;
const PROTECTED: &str = "the stream is protected (DRM) and can't be downloaded";

pub fn is_manifest(url: &str) -> bool {
    let p = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    p.ends_with(".m3u8") || p.ends_with(".mpd")
}

/// HLS / DASH Content-Types, for playlists whose address doesn't end in .m3u8 / .mpd.
pub fn is_manifest_type(ct: &str) -> bool {
    let ct = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ct.ends_with("mpegurl") || ct == "application/dash+xml"
}

#[derive(Clone, Debug, PartialEq)]
struct Part {
    url: String,
    /// Inclusive byte range.
    range: Option<(u64, u64)>,
    /// AES-128 key URL and IV.
    key: Option<(String, [u8; 16])>,
    /// HLS media sequence number (new pieces of a live playlist).
    seq: u64,
    /// Seconds to move this piece's fragments by (DASH: its period's start less its presentationTimeOffset).
    shift: f64,
}

#[derive(Default, Debug)]
struct Track {
    /// fMP4 initialization section (HLS EXT-X-MAP, DASH Initialization).
    init: Option<Part>,
    parts: Vec<Part>,
    ext: String,
    /// A live HLS playlist (no EXT-X-ENDLIST) or DASH manifest (type="dynamic"): `src` is fetched again
    /// every `target` seconds.
    live: bool,
    dash: bool,
    /// One whole file (DASH SegmentBase): fetched in byte ranges.
    whole: bool,
    target: f64,
    src: String,
}

type Get<'a> = &'a dyn Fn(&str) -> Result<(String, String), String>;

fn join(base: &str, rel: &str) -> Result<String, String> {
    url::Url::parse(base)
        .and_then(|b| b.join(rel))
        .map(String::from)
        .map_err(|e| e.to_string())
}

/// Video (or the only track) first, then a separate audio track.
fn plan(get: Get, url: &str) -> Result<Vec<Track>, String> {
    let (base, text) = get(url)?;
    if text.trim_start().starts_with('<') {
        dash(&base, &text, unix_now())
    } else {
        hls(get, &base, &text, None)
    }
}

fn plan_variant(get: Get, url: &str, variant: Option<usize>) -> Result<Vec<Track>, String> {
    if variant.is_none() {
        return plan(get, url);
    }
    let (base, manifest) = get(url)?;
    if manifest.trim_start().starts_with('<') {
        dash_variant(&base, &manifest, unix_now(), variant)
    } else {
        hls(get, &base, &manifest, variant)
    }
}

/// How far this PC's clock is behind the servers' (ms), from the last manifest's Date header. Date is
/// rounded down to the second, so live DASH pieces are asked for no earlier than the server offers them.
static CLOCK: AtomicI64 = AtomicI64::new(0);

fn unix_now() -> f64 {
    (chrono::Utc::now().timestamp_millis() + CLOCK.load(Relaxed)) as f64 / 1000.0
}

/// A live track's playlist / manifest again (the same track: video first, then audio).
fn refresh(get: Get, tr: &Track, t: usize) -> Result<Track, String> {
    let (base, text) = get(&tr.src)?;
    if tr.dash {
        dash(&base, &text, unix_now())?
            .into_iter()
            .nth(t)
            .ok_or_else(|| "the stream changed".to_string())
    } else {
        hls_media(&base, &text)
    }
}

// ---- HLS ----

// ---- DASH ----

// ---- download ----

fn text(
    agent: &transport::Agent,
    url: &str,
    referrer: &str,
    cookies: &str,
) -> Result<(String, String), String> {
    let r = transport::call(transport::request(agent, url, referrer, cookies))?;
    if let Some(t) = r
        .header("Date")
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok())
    {
        CLOCK.store(
            t.timestamp_millis() - chrono::Utc::now().timestamp_millis(),
            Relaxed,
        );
    }
    let base = r.get_url().to_string();
    let text = r.into_string().map_err(|e| e.to_string())?;
    Ok((
        base,
        text.strip_prefix('\u{feff}')
            .map(str::to_string)
            .unwrap_or(text),
    )) // a byte-order mark
}

/// `name` with the stream's extension in place of a playlist / container one.
#[derive(serde::Serialize)]
pub(crate) struct FormatChoice {
    variant: Option<usize>,
    resolution: String,
    bandwidth: u64,
    codecs: String,
    audio: Option<bool>,
    outputs: Vec<&'static str>,
    transcode: bool,
}

pub(crate) fn transport_formats(
    agent: &transport::Agent,
    url: &str,
    referrer: &str,
    cookies: &str,
) -> Result<Vec<FormatChoice>, String> {
    if crate::media::toolchain().is_none() {
        return Ok(vec![]);
    }
    let (transcode, audio) = crate::media::probe_transport(agent, url, referrer, cookies)?;
    Ok(vec![FormatChoice {
        variant: None,
        resolution: String::new(),
        bandwidth: 0,
        codecs: String::new(),
        audio: Some(audio),
        outputs: vec!["mp4", "mkv"],
        transcode,
    }])
}

/// Report only formats identified in a real manifest. TS/MP4 choices require evidence
/// from the selected playlist or adaptation set; unknown codecs keep automatic output.
pub(crate) fn formats(
    agent: &transport::Agent,
    url: &str,
    referrer: &str,
    cookies: &str,
) -> Result<Vec<FormatChoice>, String> {
    let get = |target: &str| {
        text(
            agent,
            target,
            referrer,
            if crate::transport::same_origin(url, target) {
                cookies
            } else {
                ""
            },
        )
    };
    let (base, manifest) = get(url)?;
    let mkv = crate::media::toolchain().is_some();
    if manifest.trim_start().starts_with('<') {
        let doc = roxmltree::Document::parse(&manifest).map_err(|e| e.to_string())?;
        let root = doc.root_element();
        let periods: Vec<_> = kids(root, "Period").collect();
        if periods.len() != 1 || root.attribute("type") == Some("dynamic") {
            return Ok(vec![]);
        }
        let mut choices = Vec::new();
        let period = periods[0];
        let audio = kids(period, "AdaptationSet").any(|set| {
            set.attribute("contentType") == Some("audio")
                || set
                    .attribute("mimeType")
                    .is_some_and(|mime| mime.starts_with("audio/"))
        });
        let mut index = 0;
        for set in kids(period, "AdaptationSet") {
            let protected = set
                .descendants()
                .any(|node| node.tag_name().name() == "ContentProtection");
            for rep in kids(set, "Representation") {
                let mime = rep
                    .attribute("mimeType")
                    .or(set.attribute("mimeType"))
                    .unwrap_or("");
                if !mime.starts_with("video") && set.attribute("contentType") != Some("video") {
                    continue;
                }
                let variant = index;
                index += 1;
                if protected || choices.len() == 16 {
                    continue;
                }
                let width = rep.attribute("width").or(set.attribute("width"));
                let height = rep.attribute("height").or(set.attribute("height"));
                let mut outputs = if mime == "video/mp4" {
                    vec!["mp4"]
                } else if mime == "video/webm" {
                    vec!["webm"]
                } else {
                    vec![]
                };
                if mkv && !outputs.is_empty() {
                    outputs.push("mkv");
                }
                choices.push(FormatChoice {
                    variant: Some(variant),
                    resolution: width
                        .zip(height)
                        .map_or_else(String::new, |(w, h)| format!("{w}×{h}")),
                    bandwidth: num(rep.attribute("bandwidth"), 0),
                    codecs: rep
                        .attribute("codecs")
                        .or(set.attribute("codecs"))
                        .unwrap_or("")
                        .into(),
                    audio: audio.then_some(true),
                    outputs,
                    transcode: false,
                });
            }
        }
        return Ok(choices);
    }
    let specs: Vec<_> = manifest
        .lines()
        .filter_map(|line| line.trim().strip_prefix("#EXT-X-STREAM-INF:"))
        .take(8)
        .map(attrs)
        .collect();
    let mut choices = Vec::new();
    for index in 0..specs.len().max(1) {
        let spec = specs.get(index);
        let variant = spec.map(|_| index);
        let tracks = match hls(&get, &base, &manifest, variant) {
            Ok(tracks) => tracks,
            Err(err) if spec.is_none() => return Err(err),
            Err(_) => continue,
        };
        let value = |name| {
            spec.and_then(|a| a.get(name))
                .map(String::as_str)
                .unwrap_or("")
        };
        let codecs = value("CODECS");
        let mp4_safe = !codecs.is_empty()
            && codecs.split(',').all(|codec| {
                let codec = codec.trim().split('.').next().unwrap_or("");
                matches!(
                    codec,
                    "avc1" | "avc3" | "hvc1" | "hev1" | "mp4a" | "ac-3" | "ec-3"
                )
            });
        let transport = tracks[0].ext == "ts"
            && tracks[0].parts.first().is_some_and(|p| {
                engine::names::url_name(&p.url)
                    .to_ascii_lowercase()
                    .ends_with(".ts")
            });
        let mut outputs = Vec::new();
        if tracks[0].ext == "mp4" || transport && mp4_safe {
            outputs.push("mp4");
        }
        if transport && tracks.len() == 1 {
            outputs.push("ts");
        }
        if mkv && !outputs.is_empty() {
            outputs.push("mkv");
        }
        let audio = tracks.len() == 2
            || codecs.split(',').any(|c| {
                let c = c.trim();
                c.starts_with("mp4a") || c.starts_with("ac-3") || c.starts_with("ec-3")
            });
        choices.push(FormatChoice {
            variant,
            resolution: value("RESOLUTION").replace('x', "×"),
            bandwidth: value("BANDWIDTH").parse().unwrap_or(0),
            codecs: codecs.into(),
            audio: audio.then_some(true),
            outputs,
            transcode: false,
        });
    }
    Ok(choices)
}

fn with_ext(name: &str, ext: &str) -> String {
    let stem = match name.rsplit_once('.') {
        Some((s, e))
            if [
                "m3u8", "mpd", "mp4", "ts", "mkv", "webm", "m4a", "aac", "mov", "avi", "mp3", "ogg",
            ]
            .contains(&e.to_ascii_lowercase().as_str()) =>
        {
            s
        }
        _ => name,
    };
    format!("{stem}.{ext}")
}

/// The saved file's extension: MPEG-TS becomes MP4, others keep the first track's.
fn out_ext(tracks: &[Track]) -> &str {
    if tracks[0].ext == "ts" {
        "mp4"
    } else {
        &tracks[0].ext
    }
}

pub fn probe(
    agent: &transport::Agent,
    url: &str,
    referrer: &str,
    cookies: &str,
) -> Result<Probe, String> {
    let tracks = plan(&|u| text(agent, u, referrer, cookies), url)?;
    Ok(Probe {
        name: engine::names::sanitize(&with_ext(&engine::names::url_name(url), out_ext(&tracks))),
        size: None,
        ranges: true,
        modified: String::new(),
        etag: String::new(),
        stream: true,
        page: false,
        ytdlp: false,
    })
}

pub fn is_media_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    [
        "mms://", "mmsh://", "mmst://", "rtmp://", "rtmps://", "rtmpe://", "rtmpt://", "rtsp://",
    ]
    .iter()
    .any(|p| u.starts_with(p))
}

/// Records an mms / rtmp / rtsp stream with ffmpeg (on PATH), copying the streams as they come; the
/// file grows as `<file>` and Stop ends the recording (kept, like a stopped live stream).
// ponytail: ffmpeg speaks these protocols; a native MMS/RTMP client if ffmpeg can't be assumed
pub fn run_media(job: &Job, agent: &transport::Agent) -> Result<bool, String> {
    use std::io::BufRead;
    use std::process::Stdio;
    if agent.invalid_proxy || !agent.proxy.is_empty() {
        return Err("This recording protocol cannot use the configured proxy".into());
    }
    let (url, dest) = {
        let mut i = job.info();
        if Path::new(&i.name).extension().is_none() {
            let ext = if i.url.to_ascii_lowercase().starts_with("rtmp") {
                "flv"
            } else if i.url.to_ascii_lowercase().starts_with("mms") {
                "wmv"
            } else {
                "mp4"
            };
            i.name = format!(
                "{}.{ext}",
                if i.name.is_empty() { "stream" } else { &i.name }
            );
        }
        (i.url.clone(), i.path())
    };
    let stage = crate::media::Stage::new(&dest)?;
    let ffmpeg = crate::ytdlp::beside_or_path("ffmpeg");
    let mut child = crate::ytdlp::command(&ffmpeg)
        .args(["-y", "-v", "error", "-rw_timeout", "30000000", "-progress", "pipe:1", "-nostats", "-i"])
        .arg(&url).args(["-c", "copy"]).arg(stage.path())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn()
        .map_err(|_| crate::i18n::trf_os("\"%s\" data transmission protocol is not supported by NDM at this time. Or it might be a spelling error, therefore please check the spelling, and try again.", &[&url.split("://").next().unwrap_or("")]))?;
    let stdout = child.stdout.take().unwrap();
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if let Some(bytes) = line
                    .strip_prefix("total_size=")
                    .and_then(|size| size.parse().ok())
                {
                    let mut info = job.info();
                    info.status = Status::Downloading;
                    info.segs = vec![Seg {
                        next: bytes,
                        info: "Receiving data...".into(),
                        ..Default::default()
                    }];
                }
            }
        });
        let mut stop_at = None;
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Ok::<bool, String>(status.success());
            }
            if job.stopping() && stop_at.is_none() {
                // Let FFmpeg write the trailer for a stopped live recording.
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(b"q\n");
                }
                stop_at = Some(Instant::now());
            }
            if stop_at.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
                crate::ytdlp::kill_child(&mut child);
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(100));
        }
    })?;
    if !result {
        return if job.stopping() {
            Ok(false)
        } else {
            Err(crate::i18n::tr_os("Cannot download this file."))
        };
    }
    stage.finish(&dest)?;
    let size = fs::metadata(&dest).map_err(|e| e.to_string())?.len();
    let mut i = job.info();
    i.size = Some(size);
    i.segs = vec![Seg {
        next: size,
        stop: Some(size),
        ..Default::default()
    }];
    Ok(true)
}

// ---- fragmented MP4 muxing ----
