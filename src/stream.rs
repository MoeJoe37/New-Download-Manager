//! Segmented video streams: HLS playlists (.m3u8) and DASH manifests (.mpd), as NDM's video panel
//! downloads them. The best quality is picked, the pieces are fetched over several connections into
//! `<file>.part\` (kept for resume), AES-128 pieces decrypted, and a separate audio track is mixed into
//! the video natively (fragmented MP4 here, MPEG-TS -> MP4 and WebM in `remux`; anything else through ffmpeg
//! when installed, else saved alongside). Live HLS playlists and live (dynamic) DASH manifests are followed
//! until they end or the user stops.
use crate::engine::{self, Job, Limiter, Probe, Seg, Status, RETRIES};
use crate::remux;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const PIECE: u64 = 4 << 20;
const PROTECTED: &str = "the stream is protected (DRM) and can't be downloaded";

pub fn is_manifest(url: &str) -> bool {
    let p = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    p.ends_with(".m3u8") || p.ends_with(".mpd")
}

/// HLS / DASH Content-Types, for playlists whose address doesn't end in .m3u8 / .mpd.
pub fn is_manifest_type(ct: &str) -> bool {
    let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
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
    url::Url::parse(base).and_then(|b| b.join(rel)).map(String::from).map_err(|e| e.to_string())
}

/// Video (or the only track) first, then a separate audio track.
fn plan(get: Get, url: &str) -> Result<Vec<Track>, String> {
    let (base, text) = get(url)?;
    if text.trim_start().starts_with('<') { dash(&base, &text, unix_now()) } else { hls(get, &base, &text) }
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
        dash(&base, &text, unix_now())?.into_iter().nth(t).ok_or_else(|| "the stream changed".to_string())
    } else {
        hls_media(&base, &text)
    }
}

// ---- HLS ----

/// `A=1,B="x,y"` attribute lists.
fn attrs(s: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut rest = s;
    while let Some((k, v)) = rest.split_once('=') {
        let (val, tail) = match v.strip_prefix('"') {
            Some(q) => {
                let (a, b) = q.split_once('"').unwrap_or((q, ""));
                (a, b.trim_start_matches(','))
            }
            None => v.split_once(',').unwrap_or((v, "")),
        };
        m.insert(k.trim().to_ascii_uppercase(), val.to_string());
        rest = tail;
    }
    m
}

fn hls(get: Get, base: &str, text: &str) -> Result<Vec<Track>, String> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("not an HLS playlist".into());
    }
    if !text.contains("#EXT-X-STREAM-INF") {
        return Ok(vec![hls_media(base, text)?]);
    }
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut best: Option<(u64, &str, String)> = None; // bandwidth, URI, audio group
    let mut audio = Vec::new(); // group, default, URI
    for (n, l) in lines.iter().enumerate() {
        if let Some(a) = l.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(a);
            let bw = a.get("BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0);
            let uri = lines[n + 1..].iter().find(|x| !x.is_empty() && !x.starts_with('#'));
            if let Some(uri) = uri.filter(|_| best.as_ref().is_none_or(|b| bw > b.0)) {
                best = Some((bw, uri, a.get("AUDIO").cloned().unwrap_or_default()));
            }
        } else if let Some(a) = l.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(a);
            if let (Some("AUDIO"), Some(uri)) = (a.get("TYPE").map(String::as_str), a.get("URI")) {
                audio.push((a.get("GROUP-ID").cloned().unwrap_or_default(), a.get("DEFAULT").is_some_and(|d| d == "YES"), uri.clone()));
            }
        }
    }
    let (_, uri, group) = best.ok_or("the playlist has no streams")?;
    let sub = |u: &str| -> Result<Track, String> {
        let (b, t) = get(&join(base, u)?)?;
        hls_media(&b, &t)
    };
    let mut tracks = vec![sub(uri)?];
    let mut a: Vec<_> = audio.iter().filter(|x| !group.is_empty() && x.0 == group).collect();
    a.sort_by_key(|x| !x.1);
    if let Some(x) = a.first() {
        tracks.push(sub(&x.2)?);
    }
    Ok(tracks)
}

/// `len[@offset]`; without an offset the range follows the previous one.
fn byterange(v: &str, prev_end: u64) -> (u64, u64) {
    let (len, off) = v.split_once('@').map_or((v, None), |(l, o)| (l, o.trim().parse().ok()));
    let off = off.unwrap_or(prev_end);
    (off, (off + len.trim().parse::<u64>().unwrap_or(0)).saturating_sub(1))
}

fn hls_media(base: &str, text: &str) -> Result<Track, String> {
    let mut t = Track { src: base.into(), ..Default::default() };
    let (mut seq, mut key, mut range, mut end, mut vod) = (0u64, None::<(String, Option<[u8; 16]>)>, None, 0, false);
    for l in text.lines().map(str::trim) {
        if let Some(v) = l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("#EXT-X-KEY:") {
            let a = attrs(v);
            key = match a.get("METHOD").map(String::as_str) {
                Some("AES-128") => {
                    let iv = a.get("IV").and_then(|iv| u128::from_str_radix(iv.trim_start_matches("0x").trim_start_matches("0X"), 16).ok());
                    Some((join(base, a.get("URI").ok_or("AES-128 key without a URI")?)?, iv.map(u128::to_be_bytes)))
                }
                Some("NONE") | None => None,
                _ => return Err(PROTECTED.into()),
            };
        } else if let Some(v) = l.strip_prefix("#EXT-X-MAP:") {
            let a = attrs(v);
            let url = join(base, a.get("URI").ok_or("EXT-X-MAP without a URI")?)?;
            t.init = Some(Part { url, range: a.get("BYTERANGE").map(|r| byterange(r, 0)), key: None, seq: 0 });
        } else if let Some(v) = l.strip_prefix("#EXT-X-TARGETDURATION:") {
            t.target = v.trim().parse().unwrap_or(0.0);
        } else if l == "#EXT-X-ENDLIST" || l == "#EXT-X-PLAYLIST-TYPE:VOD" {
            vod = true;
        } else if let Some(v) = l.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(byterange(v, end));
        } else if !l.is_empty() && !l.starts_with('#') {
            if let Some((_, b)) = range {
                end = b + 1;
            }
            let key = key.as_ref().map(|(u, iv)| (u.clone(), iv.unwrap_or((seq as u128).to_be_bytes())));
            t.parts.push(Part { url: join(base, l)?, range: range.take(), key, seq });
            seq += 1;
        }
    }
    t.live = !vod;
    let first = t.parts.first().ok_or("the playlist has no segments")?;
    let ext = engine::url_name(&first.url).rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    t.ext = if t.init.is_some() { "mp4".into() } else if ["aac", "mp3", "ac3"].contains(&ext.as_str()) { ext } else { "ts".into() };
    Ok(t)
}

// ---- DASH ----

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn el<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.tag_name().name() == name)
}

fn kids<'a, 'i: 'a>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.children().filter(move |c| c.tag_name().name() == name)
}

fn num<T: std::str::FromStr>(n: Option<&str>, or: T) -> T {
    n.and_then(|v| v.trim().parse().ok()).unwrap_or(or)
}

/// ISO 8601 duration ("PT1H2M3.5S") in seconds.
fn iso_dur(s: &str) -> f64 {
    let (mut total, mut n, mut time) = (0.0, String::new(), false);
    for c in s.chars() {
        match c {
            'P' => {}
            'T' => time = true,
            '0'..='9' | '.' => n.push(c),
            _ => {
                let unit = match (c, time) {
                    ('Y', false) => 31_536_000.0,
                    ('M', false) => 2_592_000.0,
                    ('W', _) => 604_800.0,
                    ('D', _) => 86_400.0,
                    ('H', true) => 3600.0,
                    ('M', true) => 60.0,
                    ('S', true) => 1.0,
                    _ => 0.0,
                };
                total += n.parse::<f64>().unwrap_or(0.0) * unit;
                n.clear();
            }
        }
    }
    total
}

/// SegmentTemplate identifiers: `$RepresentationID$`, `$Bandwidth$`, `$Number%05d$`, `$Time$`, `$$`.
fn fill(t: &str, id: &str, bw: &str, number: u64, time: u64) -> String {
    let mut out = String::new();
    for (i, p) in t.split('$').enumerate() {
        if i % 2 == 0 {
            out += p;
            continue;
        }
        let (name, fmt) = p.split_once('%').unwrap_or((p, ""));
        let w: usize = num(Some(fmt.trim_end_matches('d')), 0);
        match name {
            "" => out.push('$'),
            "RepresentationID" => out += id,
            "Bandwidth" => out += bw,
            "Number" => out += &format!("{number:0w$}"),
            "Time" => out += &format!("{time:0w$}"),
            _ => out += &format!("${p}$"),
        }
    }
    out
}

fn base_of(b: &str, n: Node) -> String {
    el(n, "BaseURL").and_then(|x| join(b, x.text().unwrap_or("").trim()).ok()).unwrap_or_else(|| b.to_string())
}

/// Live timing of a dynamic manifest: seconds since the period became available, and how far back pieces stay.
#[derive(Clone, Copy)]
struct LiveAt {
    elapsed: f64,
    window: f64,
}

/// `now` = Unix time in seconds (a dynamic manifest's pieces depend on it).
fn dash(base: &str, text: &str, now: f64) -> Result<Vec<Track>, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let mpd = doc.root_element();
    // ponytail: the first Period only; multi-period streams (ad breaks) keep their first part
    let period = el(mpd, "Period").ok_or("the manifest has no Period")?;
    let secs = period.attribute("duration").or(mpd.attribute("mediaPresentationDuration")).map_or(0.0, iso_dur);
    let live = (mpd.attribute("type") == Some("dynamic")).then(|| {
        let ast = mpd.attribute("availabilityStartTime").and_then(|t| chrono::DateTime::parse_from_rfc3339(t.trim()).ok()).map_or(now, |t| t.timestamp_millis() as f64 / 1000.0);
        let start = period.attribute("start").map_or(0.0, iso_dur);
        LiveAt { elapsed: now - ast - start, window: mpd.attribute("timeShiftBufferDepth").map_or(30.0, iso_dur) }
    });
    let update = mpd.attribute("minimumUpdatePeriod").map_or(2.0, iso_dur);
    let b0 = base_of(&base_of(base, mpd), period);
    let mut tracks = Vec::new();
    for kind in ["video", "audio"] {
        let mut best: Option<(u64, Node, Node)> = None;
        for set in kids(period, "AdaptationSet") {
            for rep in kids(set, "Representation") {
                let mime = rep.attribute("mimeType").or(set.attribute("mimeType")).unwrap_or("");
                if !mime.starts_with(kind) && set.attribute("contentType") != Some(kind) {
                    continue;
                }
                let bw = num(rep.attribute("bandwidth"), 0);
                if best.is_none_or(|b| bw > b.0) {
                    best = Some((bw, set, rep));
                }
            }
        }
        if let Some((_, set, rep)) = best {
            if set.descendants().any(|n| n.tag_name().name() == "ContentProtection") {
                return Err(PROTECTED.into());
            }
            let mut t = dash_track(&b0, period, set, rep, secs, kind, live)?;
            (t.live, t.dash, t.target, t.src) = (live.is_some(), true, update, base.to_string());
            tracks.push(t);
        }
    }
    if tracks.is_empty() {
        return Err("the manifest has no video or audio".into());
    }
    Ok(tracks)
}

fn dash_track(b0: &str, period: Node, set: Node, rep: Node, secs: f64, kind: &str, live: Option<LiveAt>) -> Result<Track, String> {
    let base = base_of(&base_of(b0, set), rep);
    let (id, bw) = (rep.attribute("id").unwrap_or(""), rep.attribute("bandwidth").unwrap_or("0"));
    let mime = rep.attribute("mimeType").or(set.attribute("mimeType")).unwrap_or("");
    let ext = if mime.contains("webm") { "webm" } else if kind == "audio" { "m4a" } else { "mp4" };
    let mut t = Track { ext: ext.into(), ..Default::default() };
    // seq: the piece's number (or $Time$ on a timeline), which tells a live manifest's new pieces apart
    let part = |u: &str, range: Option<&str>, seq: u64| -> Result<Part, String> {
        let range = range.and_then(|r| r.split_once('-')).and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
        Ok(Part { url: join(&base, u)?, range, key: None, seq })
    };
    let tmpl: Vec<Node> = [rep, set, period].into_iter().filter_map(|n| el(n, "SegmentTemplate")).collect();
    let ta = |k: &str| tmpl.iter().find_map(|t| t.attribute(k));
    if let Some(media) = ta("media") {
        let ts: u64 = num(ta("timescale"), 1u64).max(1);
        let start: u64 = num(ta("startNumber"), 1);
        if let Some(i) = ta("initialization") {
            t.init = Some(part(&fill(i, id, bw, 0, 0), None, 0)?);
        }
        let pto: f64 = num(ta("presentationTimeOffset"), 0.0);
        if let Some(tl) = tmpl.iter().find_map(|x| el(*x, "SegmentTimeline")) {
            let (mut time, mut n) = (0u64, start);
            // a live timeline lists the pieces on offer; r = -1 repeats up to the live edge or the period's end
            let end = pto + live.map_or(secs, |l| l.elapsed) * ts as f64;
            for s in kids(tl, "S") {
                let d: u64 = num(s.attribute("d"), 0);
                if d == 0 {
                    continue;
                }
                time = num(s.attribute("t"), time);
                let r: i64 = num(s.attribute("r"), 0);
                let left = (end - time as f64) / d as f64;
                let count = if r >= 0 { r as u64 + 1 } else if live.is_some() { left.floor().max(0.0) as u64 } else { left.ceil().max(1.0) as u64 };
                for _ in 0..count.min(1_000_000) {
                    t.parts.push(part(&fill(media, id, bw, n, time), None, time)?);
                    (time, n) = (time + d, n + 1);
                }
            }
        } else {
            let d: f64 = num(ta("duration"), 0.0);
            let ks: Vec<u64> = match live {
                // the pieces finished by now, as far back as the time-shift window reaches
                Some(l) if d > 0.0 => {
                    let last = (l.elapsed * ts as f64 / d).floor() as i64 - 1;
                    let first = (last + 1 - (l.window * ts as f64 / d).ceil() as i64).max(0);
                    (first.max(0) as u64..(last + 1).max(0) as u64).collect()
                }
                _ if d <= 0.0 || secs <= 0.0 => return Err("the manifest doesn't say how long the stream is".into()),
                _ => (0..(secs * ts as f64 / d).ceil() as u64).collect(),
            };
            for k in ks {
                t.parts.push(part(&fill(media, id, bw, start + k, (k as f64 * d) as u64), None, start + k)?);
            }
        }
    } else if let Some(sl) = [rep, set].into_iter().find_map(|n| el(n, "SegmentList")) {
        if let Some(i) = el(sl, "Initialization") {
            t.init = Some(part(i.attribute("sourceURL").unwrap_or(""), i.attribute("range"), 0)?);
        }
        let start: u64 = num(sl.attribute("startNumber"), 1);
        for (k, s) in kids(sl, "SegmentURL").enumerate() {
            t.parts.push(part(s.attribute("media").unwrap_or(""), s.attribute("mediaRange"), start + k as u64)?);
        }
    } else {
        t.parts.push(part("", None, 0)?); // the whole file at BaseURL (SegmentBase)
        t.whole = true;
    }
    Ok(t)
}

// ---- download ----

fn text(agent: &ureq::Agent, url: &str, referrer: &str, cookies: &str) -> Result<(String, String), String> {
    let r = engine::call(engine::request(agent, url, referrer, cookies))?;
    if let Some(t) = r.header("Date").and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok()) {
        CLOCK.store(t.timestamp_millis() - chrono::Utc::now().timestamp_millis(), Relaxed);
    }
    let base = r.get_url().to_string();
    Ok((base, r.into_string().map_err(|e| e.to_string())?))
}

/// `name` with the stream's extension in place of a playlist / container one.
fn with_ext(name: &str, ext: &str) -> String {
    let stem = match name.rsplit_once('.') {
        Some((s, e)) if ["m3u8", "mpd", "mp4", "ts", "webm", "m4a", "aac"].contains(&e.to_ascii_lowercase().as_str()) => s,
        _ => name,
    };
    format!("{stem}.{ext}")
}

/// The saved file's extension: MPEG-TS becomes MP4 (as NDM saves it), others keep the first track's.
fn out_ext(tracks: &[Track]) -> &str {
    if tracks[0].ext == "ts" { "mp4" } else { &tracks[0].ext }
}

pub fn probe(agent: &ureq::Agent, url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    let tracks = plan(&|u| text(agent, u, referrer, cookies), url)?;
    Ok(Probe { name: engine::sanitize(&with_ext(&engine::url_name(url), out_ext(&tracks))), size: None, ranges: true, modified: String::new(), stream: true })
}

fn aes_cbc(key: &[u8], mut iv: [u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Security::Cryptography::*;
    if key.len() != 16 || data.len() % 16 != 0 {
        return Err("bad AES-128 key or segment".into());
    }
    let mut out = vec![0u8; data.len()];
    unsafe {
        let mut h = std::ptr::null_mut();
        if BCryptGenerateSymmetricKey(BCRYPT_AES_CBC_ALG_HANDLE, &mut h, std::ptr::null_mut(), 0, key.as_ptr(), 16, 0) != 0 {
            return Err("AES is not available".into());
        }
        let mut n = 0u32;
        let st = BCryptDecrypt(h, data.as_ptr(), data.len() as u32, std::ptr::null(), iv.as_mut_ptr(), 16, out.as_mut_ptr(), out.len() as u32, &mut n, 0);
        BCryptDestroyKey(h);
        if st != 0 {
            return Err("AES decryption failed".into());
        }
    }
    // PKCS#7 padding
    if let Some(&p) = out.last() {
        if (1..=16).contains(&p) && out.ends_with(&vec![p; p as usize]) {
            out.truncate(out.len() - p as usize);
        }
    }
    Ok(out)
}

struct Ctx<'a> {
    job: &'a Job,
    agent: &'a ureq::Agent,
    global: &'a Limiter,
    referrer: String,
    cookies: String,
    keys: Mutex<HashMap<String, Vec<u8>>>,
}

impl Ctx<'_> {
    fn bytes(&self, p: &Part, got: &AtomicU64, tick: &dyn Fn()) -> Result<Vec<u8>, String> {
        let mut req = engine::request(self.agent, &p.url, &self.referrer, &self.cookies);
        if let Some((a, b)) = p.range {
            req = req.set("Range", &format!("bytes={a}-{b}"));
        }
        let resp = engine::call(req)?;
        let whole = p.range.filter(|_| resp.status() == 200);
        let mut r = resp.into_reader();
        let (mut out, mut buf) = (Vec::new(), vec![0; engine::CHUNK]);
        let res = loop {
            if self.job.stopping() {
                break Err("stopped".to_string());
            }
            match r.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    got.fetch_add(n as u64, Relaxed);
                    tick();
                    self.job.throttle(self.global, n);
                }
                Err(e) => break Err(e.to_string()),
            }
        };
        if let Err(e) = res {
            got.fetch_sub(out.len() as u64, Relaxed);
            return Err(e);
        }
        if let Some((a, b)) = whole {
            // the server sent the whole file instead of the range
            out = out.get(a as usize..=(b as usize).min(out.len().saturating_sub(1))).unwrap_or_default().to_vec();
        }
        Ok(out)
    }

    fn piece(&self, p: &Part, got: &AtomicU64, tick: &dyn Fn()) -> Result<Vec<u8>, String> {
        let data = self.bytes(p, got, tick)?;
        let Some((url, iv)) = &p.key else { return Ok(data) };
        let mut keys = self.keys.lock().unwrap();
        if !keys.contains_key(url) {
            let k = self.bytes(&Part { url: url.clone(), range: None, key: None, seq: 0 }, &AtomicU64::new(0), &|| {})?;
            keys.insert(url.clone(), k);
        }
        aes_cbc(&keys[url], *iv, &data)
    }
}

pub fn run(job: &Job, agent: &ureq::Agent, global: &Limiter) -> Result<bool, String> {
    let (url, referrer, cookies) = {
        let i = job.info();
        (i.url.clone(), i.referrer.clone(), i.cookies.clone())
    };
    let get = |u: &str| text(agent, u, &referrer, &cookies);
    let mut tracks = plan(&get, &url)?;
    for t in tracks.iter_mut().filter(|t| t.whole && t.parts.len() == 1) {
        // a single file: 4 MiB byte ranges, so it downloads over several connections and resumes
        if let Ok(engine::Probe { size: Some(len), ranges: true, .. }) = engine::probe(agent, &t.parts[0].url, &referrer, &cookies) {
            let p = t.parts.remove(0);
            t.parts = (0..len).step_by(PIECE as usize).map(|a| Part { range: Some((a, (a + PIECE).min(len) - 1)), ..p.clone() }).collect();
        }
    }
    if job.stopping() {
        return Ok(false);
    }
    let live = tracks.iter().any(|t| t.live);
    let dir = {
        let mut i = job.info();
        let ext = out_ext(&tracks);
        i.name = if i.name.is_empty() { engine::unique_name(&i.folder, &engine::sanitize(&with_ext(&engine::url_name(&url), ext)), &|_| false) } else { with_ext(&i.name, ext) };
        (i.ranges, i.probed, i.stream, i.status) = (true, true, true, Status::Downloading);
        let dir = i.part();
        let _ = fs::remove_file(&dir); // a .part file left from before
        if live {
            let _ = fs::remove_dir_all(&dir); // a live window moves on: its old pieces don't line up
        }
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        dir
    };
    let file = |t: usize, k: usize| dir.join(format!("{t}_{k:06}"));
    let lists: Mutex<Vec<Vec<Part>>> = Mutex::new(tracks.iter().map(|t| t.init.iter().chain(&t.parts).cloned().collect()).collect());
    let n = tracks.len();
    let cursor = Mutex::new(vec![0usize; n]);
    let ended: Vec<AtomicBool> = tracks.iter().map(|t| AtomicBool::new(!t.live)).collect();
    let got: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(0)).collect();
    let have: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    for (t, l) in lists.lock().unwrap().iter().enumerate() {
        for k in 0..l.len() {
            if let Ok(m) = fs::metadata(file(t, k)) {
                got[t].fetch_add(m.len(), Relaxed);
                have[t].fetch_add(1, Relaxed);
            }
        }
    }
    // Progress: one segment per track, its size estimated from the pieces so far (unknown while live).
    let show = |note: &str| {
        let lens: Vec<usize> = lists.lock().unwrap().iter().map(Vec::len).collect();
        let mut i = job.info();
        let mut off = 0;
        i.segs = lens
            .iter()
            .enumerate()
            .map(|(t, len)| {
                let (g, h) = (got[t].load(Relaxed), have[t].load(Relaxed) as u64);
                let est = if h > 0 { g * *len as u64 / h } else { g }.max(g);
                let s = Seg { start: off, next: off + g, stop: (!live).then_some(off + est), info: note.into(), owned: false };
                off += if live { g } else { est };
                s
            })
            .collect();
        i.size = (!live).then_some(off);
    };
    show("Connecting...");
    let ctx = Ctx { job, agent, global, referrer: referrer.clone(), cookies: cookies.clone(), keys: Default::default() };
    let failed = Mutex::new(None::<String>);
    let tick_at = Mutex::new(Instant::now());
    let tick = || {
        let mut at = tick_at.lock().unwrap();
        if at.elapsed() >= Duration::from_millis(200) {
            *at = Instant::now();
            show("Receiving data...");
        }
    };
    let all_ended = || ended.iter().all(|e| e.load(Relaxed));
    // the next piece nobody has taken, from the track that is furthest behind
    let claim = || -> Option<(usize, usize, Part)> {
        let l = lists.lock().unwrap();
        let mut c = cursor.lock().unwrap();
        let t = (0..n).filter(|t| c[*t] < l[*t].len()).min_by_key(|t| c[*t])?;
        c[t] += 1;
        Some((t, c[t] - 1, l[t][c[t] - 1].clone()))
    };
    let work = || loop {
        if job.stopping() || failed.lock().unwrap().is_some() {
            return;
        }
        let Some((t, k, part)) = claim() else {
            if all_ended() {
                return;
            }
            thread::sleep(Duration::from_millis(300)); // a live playlist: wait for its next pieces
            continue;
        };
        let f = file(t, k);
        if f.exists() {
            continue;
        }
        let mut fails = 0;
        loop {
            if job.stopping() || failed.lock().unwrap().is_some() {
                return;
            }
            let r = ctx.piece(&part, &got[t], &tick).and_then(|d| {
                let tmp = f.with_extension("tmp");
                fs::write(&tmp, d).and_then(|_| fs::rename(&tmp, &f)).map_err(|e| e.to_string())
            });
            match r {
                Ok(()) => {
                    have[t].fetch_add(1, Relaxed);
                    break;
                }
                Err(_) if job.stopping() => return,
                Err(e) => {
                    fails += 1;
                    if fails > RETRIES {
                        if live {
                            break; // a live piece that's gone from the server: skip it
                        }
                        *failed.lock().unwrap() = Some(e);
                        return;
                    }
                    show(&format!("Error: {e}, retrying..."));
                    let until = Instant::now() + Duration::from_secs(2u64.pow(fails).min(30));
                    while !job.stopping() && Instant::now() < until {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    };
    // Live playlists: fetch them again every target duration and queue the new pieces.
    let poll = || {
        let mut last: Vec<u64> = tracks.iter().map(|t| t.parts.last().map_or(0, |p| p.seq)).collect();
        while !job.stopping() && !all_ended() {
            let wait = tracks.iter().filter(|t| t.live).map(|t| t.target).fold(10.0, f64::min).clamp(1.0, 10.0);
            let until = Instant::now() + Duration::from_secs_f64(wait);
            while !job.stopping() && Instant::now() < until {
                thread::sleep(Duration::from_millis(100));
            }
            for (t, tr) in tracks.iter().enumerate().filter(|(t, _)| !ended[*t].load(Relaxed)) {
                let Ok(now) = refresh(&get, tr, t) else { continue };
                let new: Vec<Part> = now.parts.into_iter().filter(|p| p.seq > last[t]).collect();
                if let Some(p) = new.last() {
                    last[t] = p.seq;
                }
                lists.lock().unwrap()[t].extend(new);
                if !now.live {
                    ended[t].store(true, Relaxed);
                }
            }
        }
    };
    let conns = job.info().conns.max(1);
    thread::scope(|sc| {
        if live {
            sc.spawn(poll);
        }
        for _ in 0..conns {
            sc.spawn(work);
        }
    });
    show("");
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }
    if job.stopping() && !live {
        return Ok(false); // paused; a stopped live recording is saved as it is
    }
    let lens: Vec<usize> = lists.into_inner().unwrap().iter().map(Vec::len).collect();
    let mut dest = job.info().path();
    let pieces = |t: usize| (0..lens[t]).map(move |k| file(t, k)).filter(|f| f.exists()).map(|f| fs::read(f).map_err(|e| e.to_string()));
    let ext = |t: usize| tracks[t].ext.as_str();
    let fmp4 = |t: usize| (tracks[t].init.is_some() || tracks[t].whole) && ext(t) != "webm";
    use remux::{Kind, Regroup};
    let muxed = if n == 2 && fmp4(0) && fmp4(1) {
        mux_mp4(&mut Regroup::new(&mut pieces(0), Kind::Mp4), &mut Regroup::new(&mut pieces(1), Kind::Mp4), &dest)?
    } else if ext(0) == "ts" {
        let mut a = pieces(1.min(n - 1));
        remux::ts_to_mp4(&mut pieces(0), (n == 2).then_some(&mut a as remux::Pieces), &dest)?
    } else if n == 2 && ext(0) == "webm" && ext(1) == "webm" {
        remux::webm_merge(&mut Regroup::new(&mut pieces(0), Kind::Webm), &mut Regroup::new(&mut pieces(1), Kind::Webm), &dest)?
    } else {
        false
    };
    if !muxed {
        if ext(0) == "ts" {
            // couldn't remux (MPEG-2 video, Opus...): keep it as MPEG-TS
            let mut i = job.info();
            i.name = with_ext(&i.name, "ts");
            dest = i.path();
        }
        let joined: Vec<PathBuf> = (0..n)
            .map(|t| {
                let out = dir.join(format!("track{t}.{}", ext(t)));
                let mut w = BufWriter::new(File::create(&out).map_err(|e| e.to_string())?);
                for p in pieces(t) {
                    w.write_all(&p?).map_err(|e| e.to_string())?;
                }
                w.flush().map_err(|e| e.to_string())?;
                Ok(out)
            })
            .collect::<Result<_, String>>()?;
        if joined.len() == 1 || !ffmpeg(&joined[0], &joined[1], &dest) {
            move_file(&joined[0], &dest)?;
            if let Some(a) = joined.get(1) {
                // ponytail: formats without a native muxer here; ffmpeg on PATH makes them one file
                let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
                move_file(a, &dest.with_file_name(format!("{stem}.audio.{}", ext(1))))?;
            }
        }
    }
    let _ = fs::remove_dir_all(&dir);
    let size = fs::metadata(&dest).map_err(|e| e.to_string())?.len();
    let mut i = job.info();
    i.size = Some(size);
    i.segs = vec![Seg { start: 0, next: size, stop: Some(size), info: String::new(), owned: false }];
    Ok(true)
}

fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if fs::rename(from, to).is_err() {
        fs::copy(from, to).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(from);
    }
    Ok(())
}

/// Mix video and audio with ffmpeg if it is installed (on PATH).
fn ffmpeg(v: &Path, a: &Path, out: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(v)
        .arg("-i")
        .arg(a)
        .args(["-map", "0:v:0?", "-map", "1:a:0", "-c", "copy"])
        .arg(out)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .is_ok_and(|s| s.success())
}

// ---- fragmented MP4 muxing ----

/// Top-level boxes in d[from..to]: (start, payload, end, type).
pub(crate) type Bx = (usize, usize, usize, [u8; 4]);

pub(crate) fn boxes(d: &[u8], from: usize, to: usize) -> Vec<Bx> {
    let (mut v, mut at) = (Vec::new(), from);
    while at + 8 <= to {
        let size = be32(d, at) as usize;
        let t: [u8; 4] = d[at + 4..at + 8].try_into().unwrap();
        let (pay, end) = match size {
            1 if at + 16 <= to => (at + 16, at.saturating_add(u64::from_be_bytes(d[at + 8..at + 16].try_into().unwrap()) as usize)),
            0 => (at + 8, to),
            s => (at + 8, at + s),
        };
        if end < pay || end > to {
            break;
        }
        v.push((at, pay, end, t));
        at = end;
    }
    v
}

pub(crate) fn find(d: &[u8], b: Bx, path: &[&[u8; 4]]) -> Option<Bx> {
    path.iter().try_fold(b, |b, p| boxes(d, b.1, b.2).into_iter().find(|c| &c.3 == *p))
}

fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap())
}

fn mkbox(t: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [&((body.len() + 8) as u32).to_be_bytes()[..], t, body].concat()
}

/// A full box's field after version/flags: `v0` bytes in for version 0, `v1` for version 1.
fn field(d: &[u8], b: Bx, v0: usize, v1: usize) -> usize {
    b.1 + 4 + if d[b.1] == 1 { v1 } else { v0 }
}

struct Src<I> {
    pieces: I,
    data: Vec<u8>,
    frags: VecDeque<(f64, usize, usize)>,
    /// timescale per (new) track id; old -> new track ids (audio only)
    scale: HashMap<u32, f64>,
    ids: HashMap<u32, u32>,
}

impl<I: Iterator<Item = Result<Vec<u8>, String>>> Src<I> {
    /// moof + mdat pairs of the current piece, with their decode times in seconds.
    fn scan(&mut self) {
        let d = &self.data;
        let top = boxes(d, 0, d.len());
        for w in top.windows(2) {
            if &w[0].3 != b"moof" || &w[1].3 != b"mdat" {
                continue;
            }
            let time = find(d, w[0], &[b"traf"]).and_then(|traf| {
                let id = be32(d, find(d, traf, &[b"tfhd"])?.1 + 4);
                let tfdt = find(d, traf, &[b"tfdt"])?;
                let at = tfdt.1 + 4;
                let t = if d[tfdt.1] == 1 { u64::from_be_bytes(d[at..at + 8].try_into().unwrap()) } else { be32(d, at) as u64 };
                Some(t as f64 / self.scale.get(self.ids.get(&id).unwrap_or(&id))?)
            });
            let time = time.or(self.frags.back().map(|f| f.0)).unwrap_or(0.0);
            self.frags.push_back((time, w[0].0, w[1].2));
        }
    }

    fn fill(&mut self) -> Result<bool, String> {
        while self.frags.is_empty() {
            match self.pieces.next() {
                Some(p) => {
                    self.data = p?;
                    self.scan();
                }
                None => return Ok(false),
            }
        }
        Ok(true)
    }

    /// The next fragment, its track ids renumbered.
    fn take(&mut self) -> Vec<u8> {
        let (_, a, b) = self.frags.pop_front().unwrap();
        let mut f = self.data[a..b].to_vec();
        if let Some(moof) = boxes(&f, 0, f.len()).first().copied() {
            for traf in boxes(&f, moof.1, moof.2).into_iter().filter(|x| &x.3 == b"traf") {
                if let Some(tfhd) = find(&f, traf, &[b"tfhd"]) {
                    let at = tfhd.1 + 4;
                    if let Some(new) = self.ids.get(&be32(&f, at)) {
                        f[at..at + 4].copy_from_slice(&new.to_be_bytes());
                    }
                }
            }
        }
        f
    }
}

fn traks(d: &[u8], moov: Bx) -> Vec<(Bx, u32, f64)> {
    boxes(d, moov.1, moov.2)
        .into_iter()
        .filter(|b| &b.3 == b"trak")
        .filter_map(|t| {
            let tkhd = find(d, t, &[b"tkhd"])?;
            let mdhd = find(d, t, &[b"mdia", b"mdhd"])?;
            Some((t, be32(d, field(d, tkhd, 8, 16)), be32(d, field(d, mdhd, 8, 16)) as f64))
        })
        .collect()
}

/// Mix a fragmented-MP4 audio track into fragmented-MP4 video: one moov with both tracks (audio
/// renumbered), then the moof+mdat fragments interleaved by decode time. Ok(false) = not fragmented
/// MP4 (nothing written).
fn mux_mp4<'a>(v: &'a mut (dyn Iterator<Item = Result<Vec<u8>, String>> + 'a), a: &'a mut (dyn Iterator<Item = Result<Vec<u8>, String>> + 'a), out: &Path) -> Result<bool, String> {
    let mut vs = Src { pieces: v, data: Vec::new(), frags: VecDeque::new(), scale: HashMap::new(), ids: HashMap::new() };
    let mut au = Src { pieces: a, data: Vec::new(), frags: VecDeque::new(), scale: HashMap::new(), ids: HashMap::new() };
    for s in [&mut vs, &mut au] {
        s.data = match s.pieces.next() {
            Some(p) => p?,
            None => return Ok(false),
        };
    }
    let moov_of = |d: &[u8]| boxes(d, 0, d.len()).into_iter().find(|b| &b.3 == b"moov");
    let (Some(vm), Some(am)) = (moov_of(&vs.data), moov_of(&au.data)) else { return Ok(false) };
    let (vd, ad) = (&vs.data, &au.data);
    let vt = traks(vd, vm);
    let mut next = vt.iter().map(|t| t.1).max().unwrap_or(0) + 1;
    vs.scale = vt.iter().map(|t| (t.1, t.2)).collect();
    let mut audio = Vec::new();
    for (t, id, scale) in traks(ad, am) {
        let mut b = ad[t.0..t.2].to_vec();
        let tkhd = find(ad, t, &[b"tkhd"]).unwrap();
        let at = field(ad, tkhd, 8, 16) - t.0;
        b[at..at + 4].copy_from_slice(&next.to_be_bytes());
        audio.push(b);
        au.ids.insert(id, next);
        au.scale.insert(next, scale);
        next += 1;
    }
    let trex: Vec<u8> = find(ad, am, &[b"mvex"])
        .map(|mvex| {
            boxes(ad, mvex.1, mvex.2)
                .into_iter()
                .filter(|b| &b.3 == b"trex")
                .flat_map(|b| {
                    let mut x = ad[b.0..b.2].to_vec();
                    let id = be32(&x, 12);
                    x[12..16].copy_from_slice(&au.ids.get(&id).unwrap_or(&id).to_be_bytes());
                    x
                })
                .collect()
        })
        .unwrap_or_default();
    let mut body = Vec::new();
    let mut placed = false;
    for c in boxes(vd, vm.1, vm.2) {
        let mut b = vd[c.0..c.2].to_vec();
        match &c.3 {
            b"mvhd" => {
                let n = b.len();
                b[n - 4..].copy_from_slice(&next.to_be_bytes()); // next_track_ID
            }
            b"mvex" => {
                body.extend(audio.concat());
                placed = true;
                b = mkbox(b"mvex", &[&vd[c.1..c.2], &trex[..]].concat());
            }
            _ => {}
        }
        body.extend(b);
    }
    if !placed {
        body.extend(audio.concat());
        if !trex.is_empty() {
            body.extend(mkbox(b"mvex", &trex));
        }
    }
    let ftyp = boxes(vd, 0, vd.len()).into_iter().find(|b| &b.3 == b"ftyp").map(|b| vd[b.0..b.2].to_vec()).unwrap_or_default();
    vs.scan();
    au.scan();
    let mut w = BufWriter::new(File::create(out).map_err(|e| e.to_string())?);
    let e = |e: std::io::Error| e.to_string();
    w.write_all(&ftyp).map_err(e)?;
    w.write_all(&mkbox(b"moov", &body)).map_err(e)?;
    loop {
        let (hv, ha) = (vs.fill()?, au.fill()?);
        let f = match (hv, ha) {
            (false, false) => break,
            (true, true) if vs.frags[0].0 <= au.frags[0].0 => vs.take(),
            (true, false) => vs.take(),
            _ => au.take(),
        };
        w.write_all(&f).map_err(e)?;
    }
    w.flush().map_err(e)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::Arc;

    fn fake<'a>(files: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Result<(String, String), String> + 'a {
        move |u: &str| files.iter().find(|f| f.0 == u).map(|f| (u.to_string(), f.1.to_string())).ok_or(format!("404 {u}"))
    }

    #[test]
    fn hls_master_audio_keys_ranges() {
        let files = [
            ("http://h/m.m3u8", "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",DEFAULT=YES,URI=\"au/en.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS=\"avc1,mp4a\",AUDIO=\"a\"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000,AUDIO=\"a\",RESOLUTION=1920x1080\nhi/v.m3u8\n"),
            ("http://h/hi/v.m3u8", "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nseg1.m4s\n#EXT-X-KEY:METHOD=AES-128,URI=\"/k\"\n#EXT-X-BYTERANGE:100@0\n#EXTINF:4,\nall.m4s\n#EXT-X-BYTERANGE:50\nall.m4s\n#EXT-X-ENDLIST\n"),
            ("http://h/au/en.m3u8", "#EXTM3U\n#EXTINF:4,\na1.aac\n"),
        ];
        let t = plan(&fake(&files), "http://h/m.m3u8").unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].ext.as_str(), t[1].ext.as_str()), ("mp4", "aac"));
        assert_eq!(t[0].init.as_ref().unwrap().url, "http://h/hi/init.mp4");
        let p = &t[0].parts;
        assert_eq!((p[0].url.as_str(), p[0].key.is_none()), ("http://h/hi/seg1.m4s", true));
        assert_eq!(p[1].range, Some((0, 99)));
        assert_eq!(p[2].range, Some((100, 149)));
        assert_eq!(p[2].key, Some(("http://h/k".into(), 9u128.to_be_bytes())), "IV = media sequence number");
        assert_eq!(t[1].parts[0].url, "http://h/au/a1.aac");
        let drm = [("http://h/d.m3u8", "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\nx.ts\n")];
        assert_eq!(plan(&fake(&drm), "http://h/d.m3u8").unwrap_err(), PROTECTED);
    }

    #[test]
    fn dash_live_window() {
        let mpd = r#"<MPD type="dynamic" availabilityStartTime="2026-01-01T00:00:00Z" timeShiftBufferDepth="PT6S" minimumUpdatePeriod="PT2S">
 <Period start="PT0S"><AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
  <SegmentTemplate timescale="1000" duration="2000" startNumber="10" initialization="i.mp4" media="$Number$.m4s"/>
 </Representation></AdaptationSet></Period></MPD>"#;
        // 21 s in: pieces 0..=9 are finished; the 6 s window holds the last three
        let t = dash("http://h/l.mpd", mpd, 1_767_225_600.0 + 21.0).unwrap();
        assert_eq!(t[0].parts.iter().map(|p| p.seq).collect::<Vec<_>>(), [17, 18, 19]);
        assert_eq!(t[0].parts[2].url, "http://h/19.m4s");
        assert!(t[0].live && t[0].dash && t[0].target == 2.0 && t[0].src == "http://h/l.mpd");
        let t = dash("http://h/l.mpd", mpd, 1_767_225_600.0 + 1.0).unwrap();
        assert!(t[0].parts.is_empty(), "nothing finished yet");
    }

    /// A live DASH timeline: the second manifest adds a piece, the third is static and adds the last one.
    #[test]
    fn dash_live_follows_until_static() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        let fetches = Arc::new(AtomicUsize::new(0));
        let f2 = fetches.clone();
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let fetches = f2.clone();
                thread::spawn(move || {
                    let mut r = BufReader::new(&s);
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                    let path = line.split(' ').nth(1).unwrap_or("").trim_start_matches('/').to_string();
                    let body = if path == "live.mpd" {
                        let n = fetches.fetch_add(1, Relaxed);
                        let (ty, t0) = match n {
                            0 => ("dynamic", 0),
                            1 => ("dynamic", 1000),
                            _ => ("static", 2000),
                        };
                        format!(r#"<MPD type="{ty}" minimumUpdatePeriod="PT1S"><Period><AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1"><SegmentTemplate timescale="1000" initialization="init.mp4" media="seg$Time$.m4s"><SegmentTimeline><S t="{t0}" d="1000" r="2"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#)
                    } else {
                        path
                    };
                    let _ = (&s).write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                });
            }
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-dash-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(engine::Info { url: format!("{base}live.mpd"), folder: dir.display().to_string(), conns: 2, ..Default::default() });
        j.start(engine::agent("", ""), Arc::new(Limiter::default()));
        while j.running() {
            thread::sleep(Duration::from_millis(20));
        }
        let i = j.info().clone();
        assert_eq!(i.status, Status::Complete);
        assert_eq!(i.name, "live.mp4");
        assert_eq!(fs::read_to_string(i.path()).unwrap(), "init.mp4seg0.m4sseg1000.m4sseg2000.m4sseg3000.m4sseg4000.m4s");
        assert_eq!(fetches.load(Relaxed), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dash_templates() {
        let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT0M9.5S">
 <BaseURL>media/</BaseURL>
 <Period>
  <AdaptationSet mimeType="video/mp4">
   <SegmentTemplate timescale="1000" initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/$Number%03d$.m4s" startNumber="5">
    <SegmentTimeline><S t="0" d="4000" r="1"/><S d="1500"/></SegmentTimeline>
   </SegmentTemplate>
   <Representation id="v1" bandwidth="100"/>
   <Representation id="v2" bandwidth="900"/>
  </AdaptationSet>
  <AdaptationSet contentType="audio" mimeType="audio/mp4">
   <Representation id="a" bandwidth="64">
    <SegmentTemplate duration="2" initialization="a-init.mp4" media="a-$Time$.m4s"/>
   </Representation>
  </AdaptationSet>
 </Period>
</MPD>"#;
        let t = plan(&fake(&[("http://h/x/s.mpd", mpd)]), "http://h/x/s.mpd").unwrap();
        assert_eq!(t[0].init.as_ref().unwrap().url, "http://h/x/media/v2/init.mp4");
        let v: Vec<&str> = t[0].parts.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(v, ["http://h/x/media/v2/005.m4s", "http://h/x/media/v2/006.m4s", "http://h/x/media/v2/007.m4s"]);
        assert_eq!(t[1].parts.len(), 5, "ceil(9.5 s / 2 s)");
        assert_eq!(t[1].parts[1].url, "http://h/x/media/a-2.m4s");
        assert_eq!((t[0].ext.as_str(), t[1].ext.as_str()), ("mp4", "m4a"));
        assert_eq!(with_ext("My video.m3u8", "ts"), "My video.ts");
        assert_eq!(with_ext("Ep. 5", "mp4"), "Ep. 5.mp4");
    }

    fn mk(t: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
        mkbox(t, &parts.concat())
    }
    fn init(id: u32, scale: u32) -> Vec<u8> {
        let tkhd = mk(b"tkhd", &[&[0; 12], &id.to_be_bytes(), &[0; 4]]);
        let mdhd = mk(b"mdhd", &[&[0; 12], &scale.to_be_bytes(), &[0; 4]]);
        let trak = mk(b"trak", &[&tkhd, &mk(b"mdia", &[&mdhd])]);
        let mvhd = mk(b"mvhd", &[&[0; 96], &(id + 1).to_be_bytes()]);
        let trex = mk(b"trex", &[&[0; 4], &id.to_be_bytes(), &[0; 12]]);
        [mk(b"ftyp", &[b"iso6"]), mk(b"moov", &[&mvhd, &trak, &mk(b"mvex", &[&trex])])].concat()
    }
    fn frag(id: u32, t: u32, payload: u8) -> Vec<u8> {
        let traf = mk(b"traf", &[&mk(b"tfhd", &[&[0; 4], &id.to_be_bytes()]), &mk(b"tfdt", &[&[0; 4], &t.to_be_bytes()])]);
        [mk(b"styp", &[b"msdh"]), mk(b"moof", &[&traf]), mk(b"mdat", &[&[payload]])].concat()
    }

    #[test]
    fn mux_interleaves_fragments() {
        let v = [init(1, 1000), frag(1, 0, b'v'), [frag(1, 2000, b'w'), frag(1, 4000, b'x')].concat()];
        let a = [init(1, 48000), frag(1, 0, b'a'), frag(1, 96000, b'b')];
        let out = std::env::temp_dir().join(format!("ndm-mux-{}.mp4", std::process::id()));
        assert!(mux_mp4(&mut v.into_iter().map(Ok), &mut a.into_iter().map(Ok), &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let top = boxes(&d, 0, d.len());
        let moov = top.iter().find(|b| &b.3 == b"moov").copied().unwrap();
        let ids: Vec<u32> = traks(&d, moov).iter().map(|t| t.1).collect();
        assert_eq!(ids, [1, 2]);
        let mvex = find(&d, moov, &[b"mvex"]).unwrap();
        assert_eq!(boxes(&d, mvex.1, mvex.2).len(), 2, "two trex");
        let mvhd = find(&d, moov, &[b"mvhd"]).unwrap();
        assert_eq!(be32(&d, mvhd.2 - 4), 3);
        let order: Vec<u8> = top.iter().filter(|b| &b.3 == b"mdat").map(|b| d[b.1]).collect();
        assert_eq!(order, *b"vawbx");
        let tfhd_ids: Vec<u32> = top.iter().filter(|b| &b.3 == b"moof").map(|b| be32(&d, find(&d, *b, &[b"traf", b"tfhd"]).unwrap().1 + 4)).collect();
        assert_eq!(tfhd_ids, [1, 2, 1, 2, 1]);
        assert!(!top.iter().any(|b| &b.3 == b"styp"));
    }

    fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        let files = Arc::new(files);
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let files = files.clone();
                thread::spawn(move || {
                    let mut r = BufReader::new(&s);
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                    let path = line.split(' ').nth(1).unwrap_or("").trim_start_matches('/').to_string();
                    let mut w = &s;
                    match files.iter().find(|f| f.0 == path) {
                        Some((_, body)) => {
                            let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                            let _ = w.write_all(body);
                        }
                        None => {
                            let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                });
            }
        });
        base
    }

    /// End to end through the engine: an HLS playlist whose last piece is AES-128 encrypted
    /// (NIST SP 800-38A F.2.1 vector).
    #[test]
    fn hls_download_decrypts_and_joins() {
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let plain = hex("6bc1bee22e409f96e93d7e117393172a");
        let list = "#EXTM3U\n#EXTINF:2,\ns1.ts\n#EXTINF:2,\ns2.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:2,\ns3.ts\n#EXT-X-ENDLIST\n";
        let base = serve(vec![
            ("v.m3u8", list.into()),
            ("s1.ts", vec![1; 70_000]),
            ("s2.ts", vec![2; 5]),
            ("s3.ts", hex("7649abac8119b246cee98e9b12e9197d")),
            ("k.bin", key),
        ]);
        let dir = std::env::temp_dir().join(format!("ndm-test-hls-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(engine::Info { url: format!("{base}v.m3u8"), folder: dir.display().to_string(), conns: 2, ..Default::default() });
        j.start(engine::agent("", ""), Arc::new(Limiter::default()));
        while j.running() {
            thread::sleep(Duration::from_millis(20));
        }
        let i = j.info().clone();
        assert_eq!(i.status, Status::Complete);
        assert_eq!(i.name, "v.ts");
        assert_eq!(fs::read(i.path()).unwrap(), [vec![1; 70_000], vec![2; 5], plain].concat());
        assert!(!i.part().exists(), "pieces removed");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A live playlist (no ENDLIST) at an address without .m3u8, known only by its Content-Type: the
    /// first fetch lists two pieces, the next adds a third and ends the stream.
    #[test]
    fn live_playlist_by_content_type() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        let fetches = Arc::new(AtomicUsize::new(0));
        let f2 = fetches.clone();
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let fetches = f2.clone();
                thread::spawn(move || {
                    let mut r = BufReader::new(&s);
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                    let path = line.split(' ').nth(1).unwrap_or("").to_string();
                    let (ct, body) = if path == "/play" {
                        let n = fetches.fetch_add(1, Relaxed);
                        let mut b = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:5\n#EXTINF:1,\ns5.ts\n#EXTINF:1,\ns6.ts\n".to_string();
                        if n >= 4 {
                            // fetches: probe, the engine's first request, the plan, one live poll
                            b += "#EXTINF:1,\ns7.ts\n#EXT-X-ENDLIST\n";
                        }
                        ("application/vnd.apple.mpegurl", b.into_bytes())
                    } else {
                        ("video/mp2t", path.trim_start_matches('/').repeat(3).into_bytes())
                    };
                    let mut w = &s;
                    let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                    let _ = w.write_all(&body);
                });
            }
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let p = engine::probe(&engine::agent("", ""), &format!("{base}play"), "", "").unwrap();
        assert!(p.stream && p.name == "play.mp4", "{}", p.name);
        let j = Job::new(engine::Info { url: format!("{base}play"), folder: dir.display().to_string(), conns: 2, ..Default::default() });
        j.start(engine::agent("", ""), Arc::new(Limiter::default()));
        while j.running() {
            thread::sleep(Duration::from_millis(20));
        }
        let i = j.info().clone();
        assert_eq!(i.status, Status::Complete);
        assert!(i.stream);
        assert_eq!(i.name, "play.ts", "not MPEG-TS data, so kept as it is");
        assert_eq!(fs::read_to_string(i.path()).unwrap(), "s5.tss5.tss5.tss6.tss6.tss6.tss7.tss7.tss7.ts");
        assert!(fetches.load(Relaxed) >= 5, "the live playlist was fetched again");
        let _ = fs::remove_dir_all(&dir);
    }
}


