use crate::engine::{
    call, fs, ftp_open, is_ftp, is_page, permanent_error, probe_of, request, retry_delay, sanitize,
    sniff, thread, unique_name, url_name, valid_layout, validate_http_response, Agent, Arc,
    AtomicBool, AtomicU32, Body, Duration, File, Info, Instant, Job, Limiter, Mutex, MutexGuard,
    OpenOptions, Path, PathBuf, Probe, Read, Relaxed, Request, Response, Seek, SeekFrom, Seg,
    Status, Write, CHANGED, CHUNK, MIN_SEG, RETRIES,
};
/// A connection's network left the download (`fetch`).
const LANE_GONE: &str = "network left the download";

/// A network whose connections all gave up is tried again after this long (it may come back).
const LANE_COOLDOWN: Duration = Duration::from_secs(30);

/// How often a network may double its connections: 1, 2, 4, 8, 16 within a second.
const RAMP_EVERY: Duration = Duration::from_millis(250);

/// A split hands over at least this much...
const MIN_STEAL: u64 = 64 * 1024;

/// ...and only if it saves more than twice what a new request costs before its first byte (seconds).
const STEAL_STARTUP: f64 = 1.0;

/// Bytes before a resume point that are fetched again and compared with the file.
pub(super) const VERIFY: u64 = 64;

/// How long a new connection waits for the server's answer while the download's other connections
/// receive, before it gives up (the server allows fewer connections).
const NO_ANSWER: Duration = Duration::from_secs(6);

const NO_ANSWER_ERR: &str = "no answer from the server";

/// `old` ("a=1; b=2") with each `name=value` of `set` replacing the cookie of that name or added.
pub(super) fn merge_cookies(old: &str, set: &[String]) -> String {
    let name = |c: &str| c.split('=').next().unwrap_or("").trim().to_string();
    let mut all: Vec<String> = old
        .split(';')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from)
        .collect();
    for c in set {
        all.retain(|o| name(o) != name(c));
        all.push(c.clone());
    }
    all.join("; ")
}

fn with_headers(mut request: Request, headers: &[(String, String)]) -> Request {
    for (key, value) in headers {
        if !matches!(
            key.to_ascii_lowercase().as_str(),
            "host"
                | "connection"
                | "proxy-authorization"
                | "content-length"
                | "transfer-encoding"
                | "te"
                | "upgrade"
                | "accept-encoding"
        ) {
            request = request.set(key, value);
        }
    }
    request
}

impl Job {
    pub fn new(info: Info) -> Arc<Job> {
        let limiter = Limiter::default();
        limiter
            .rate
            .store(info.limit_kb.saturating_mul(1024), Relaxed);
        let done = info.done();
        Arc::new(Job {
            info: Mutex::new(info),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            just_completed: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            limiter,
            global_bucket: Limiter::default(),
            speed: Mutex::new((Instant::now(), done, 0.0)),
            set_date: AtomicBool::new(false),
            ignore_date: AtomicBool::new(false),
            fails: AtomicU32::new(0),
            failed_at: Mutex::new(None),
            lanes: Mutex::new(vec![]),
            net_bytes: Mutex::default(),
            multi_ok: AtomicBool::new(false),
            caps: Mutex::default(),
        })
    }

    pub fn info(&self) -> MutexGuard<'_, Info> {
        self.info.lock().unwrap()
    }

    pub fn running(&self) -> bool {
        self.running.load(Relaxed)
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Relaxed)
    }

    /// Bytes/s, re-sampled at most once a second.
    pub fn speed(&self) -> f64 {
        let done = self.info().done();
        let mut s = self.speed.lock().unwrap();
        let dt = s.0.elapsed().as_secs_f64();
        if dt >= 1.0 {
            *s = (Instant::now(), done, done.saturating_sub(s.1) as f64 / dt);
        }
        if self.running() {
            s.2
        } else {
            0.0
        }
    }

    /// Shows what NDM does with the data after the download (see `Info::post`); `frac` below 0 = unknown.
    pub fn set_post(&self, label: &str, frac: f32) {
        let mut i = self.info();
        if i.post.as_ref().is_none_or(|p| p.0 != label) {
            i.post_since = Some(Instant::now());
        }
        i.post = Some((label.to_string(), frac.min(1.0)));
    }

    /// The finished file copied from the temporary directory to its folder on another drive, then the
    /// temporary copy removed; the bar shows the copy.
    pub(super) fn copy_out(&self, from: &Path, to: &Path) -> Result<(), String> {
        let total = fs::metadata(from).map(|m| m.len()).unwrap_or(0).max(1);
        let tmp = PathBuf::from(format!("{}.ndm-copy", to.display()));
        let copy = || -> std::io::Result<()> {
            let (mut src, mut dst) = (File::open(from)?, File::create(&tmp)?);
            let mut buf = vec![0u8; 4 << 20];
            let mut done = 0u64;
            loop {
                if self.stopping() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "stopped",
                    ));
                }
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n])?;
                done += n as u64;
                self.set_post(
                    "Moving the file to its folder...",
                    done as f32 / total as f32,
                );
            }
            dst.sync_all()?;
            drop(dst);
            fs::rename(&tmp, to)
        };
        let r = copy();
        self.info().post = None;
        if let Err(e) = r {
            let _ = fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        let _ = fs::remove_file(from);
        Ok(())
    }

    /// Runs this download here and now (not on a thread of its own, no status kept): a part of another
    /// download, such as the video or audio stream of a video page (`ytdlp::run`).
    pub(crate) fn run_part(&self, agent: &Agent, global: &Limiter) -> Result<bool, String> {
        self.run(agent, global)
    }

    pub fn start(self: &Arc<Self>, agent: Agent, global: Arc<Limiter>) {
        if self.info().status == Status::Complete || self.running.swap(true, Relaxed) {
            return;
        }
        let changed = matches!(&self.info().status, Status::Error(error) if error == CHANGED);
        if changed {
            // Queue retries skip identity mismatches. A subsequent user start explicitly
            // requests a new transfer, after the changed-resource error was shown.
            self.reset();
        }
        self.stop.store(false, Relaxed);
        self.net_bytes.lock().unwrap().clear();
        self.info().status = Status::Connecting;
        let job = self.clone();
        thread::spawn(move || {
            let r = job.run(&agent, &global);
            let mut i = job.info();
            i.post = None;
            i.status = match r {
                Ok(true) => {
                    i.queued = false;
                    job.just_completed.store(true, Relaxed);
                    Status::Complete
                }
                Ok(false) => Status::Stopped,
                Err(e) => Status::Error(e),
            };
            job.ended
                .store(!matches!(i.status, Status::Stopped), Relaxed);
            match i.status {
                Status::Error(_) => job.fails.fetch_add(1, Relaxed),
                _ => job.fails.swap(0, Relaxed),
            };
            *job.failed_at.lock().unwrap() =
                matches!(i.status, Status::Error(_)).then(Instant::now);
            for s in &mut i.segs {
                s.info.clear();
                s.owned = false;
            }
            job.running.store(false, Relaxed);
        });
    }

    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
    }

    /// Forget all progress so the next start downloads from scratch.
    pub fn reset(&self) {
        let mut i = self.info();
        let _ = fs::remove_file(i.part());
        let _ = fs::remove_dir_all(i.part()); // a stream's pieces
        i.segs.clear();
        i.etag.clear();
        i.modified.clear();
        i.probed = false;
        i.status = Status::Stopped;
    }

    pub(super) fn run(&self, agent: &Agent, global: &Limiter) -> Result<bool, String> {
        let (url, referrer, cookies, probed, headers) = {
            let mut i = self.info();
            i.last_try = chrono::Local::now().timestamp();
            (
                i.url.clone(),
                i.referrer.clone(),
                i.cookies.clone(),
                i.probed,
                i.headers.clone(),
            )
        };
        if self.info().fed {
            return Err(crate::i18n::tr_os("This file came from its web page through the browser: download it again from the page."));
        }
        if crate::stream::is_media_url(&url) {
            return crate::stream::run_media(self, agent);
        }
        if crate::ytdlp::wants(&self.info()) {
            return crate::ytdlp::run(self, agent, global);
        }
        if crate::stream::is_manifest(&url) || self.info().stream {
            return crate::stream::download::run(self, agent, global);
        }
        // streams above stay on one network; here every network in `lanes` runs its own connections
        let lanes = self.lanes();
        let mut first_net = lanes.first().map(|l| l.0.clone()).unwrap_or_default();
        let mut first = None;
        // "Look up files in IE cache before downloading": a cached copy is offered to the
        // server as If-Modified-Since; on 304 Not Modified (0x7573) the cached file is copied into place.
        let cached = if self.info().ie_cache && !probed && !is_ftp(&url) {
            crate::win::url_cache(&url)
        } else {
            None
        };
        if !probed {
            // Like NDM (0x589960) the first request is the download itself, `bytes=0-`; no separate probe.
            let (p, r): (Probe, Body) = if is_ftp(&url) {
                let (size, ranges, r) = ftp_open(&url, 0)?;
                (
                    Probe {
                        name: sanitize(&url_name(&url)),
                        size,
                        ranges,
                        modified: String::new(),
                        etag: String::new(),
                        stream: false,
                        page: false,
                        ytdlp: false,
                    },
                    r,
                )
            } else {
                let first_req = |agent: &Agent| {
                    let mut req = with_headers(request(agent, &url, &referrer, &cookies), &headers)
                        .set("Range", "bytes=0-");
                    if let Some((_, since)) = &cached {
                        req = req.set("If-Modified-Since", since);
                    }
                    call(req)
                };
                // a first network that doesn't answer leaves the download to the default one
                let r = match lanes.first() {
                    Some((_, a)) => first_req(a).or_else(|_| {
                        first_net.clear();
                        first_req(agent)
                    })?,
                    None => first_req(agent)?,
                };
                if let (304, Some((file, _))) = (r.status(), &cached) {
                    drop(r);
                    let mut i = self.info();
                    if i.name.is_empty() {
                        i.name = unique_name(&i.folder, &sanitize(&url_name(&url)), &|_| false);
                    }
                    fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
                    let n = fs::copy(file, i.path()).map_err(|e| e.to_string())?;
                    (i.size, i.probed) = (Some(n), true);
                    i.segs = vec![Seg {
                        next: n,
                        stop: Some(n),
                        ..Default::default()
                    }];
                    return Ok(true);
                }
                validate_http_response(&r, Some((0, None)), None)?;
                if r.header("Content-Type")
                    .is_some_and(crate::stream::is_manifest_type)
                {
                    drop(r);
                    self.info().stream = true;
                    return crate::stream::download::run(self, agent, global);
                }
                // Queued, silent CLI and "start downloading immediately" jobs can reach here without
                // the File Info probe. Let yt-dlp try HTML from any site, while explicit HTML downloads
                // (and the Site Grabber's pages) keep their ordinary file behavior.
                let video_page = {
                    let i = self.info();
                    let html = |name: &str| {
                        matches!(crate::core::ext(name).as_str(), "html" | "htm" | "xhtml")
                    };
                    !i.plain
                        && !i.page
                        && !html(&i.name)
                        && !html(&url_name(&i.url))
                        && is_page(r.header("Content-Type"))
                };
                if video_page {
                    drop(r);
                    self.info().ytdlp = true;
                    return crate::ytdlp::run(self, agent, global);
                }
                // the server's cookies go along on the other connections
                let set: Vec<String> = r
                    .all("Set-Cookie")
                    .iter()
                    .filter_map(|c| c.split(';').next())
                    .map(|c| c.trim().to_string())
                    .collect();
                if !set.is_empty() && r.get_url() == url {
                    let mut i = self.info();
                    i.cookies = merge_cookies(&i.cookies, &set);
                }
                self.info().page = is_page(r.header("Content-Type"));
                (probe_of(&r), r.into_reader())
            };
            let mut i = self.info();
            if i.name.is_empty() {
                i.name = unique_name(&i.folder, &p.name, &|_| false);
            }
            if !i.segs.is_empty()
                && i.done() > 0
                && (i.size != p.size
                    || i.ranges != p.ranges
                    || (!i.etag.is_empty() && !p.etag.is_empty() && i.etag != p.etag)
                    || (!i.modified.is_empty()
                        && !p.modified.is_empty()
                        && i.modified != p.modified
                        && !self.ignore_date.load(Relaxed)))
            {
                return Err(CHANGED.into());
            }
            (i.size, i.ranges, i.probed) = (p.size, p.ranges, true);
            if i.segs.is_empty() {
                i.modified = p.modified;
                i.etag = p.etag;
                first = Some(r); // becomes the first connection
            }
        }
        if self.stopping() {
            return Ok(false);
        }
        let part = {
            let mut i = self.info();
            fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
            if !i.temp.is_empty() {
                fs::create_dir_all(&i.temp).map_err(|e| e.to_string())?;
            }
            let part = i.part();
            if !i.segs.is_empty() && !valid_layout(&i.segs, i.size) {
                return Err(
                    "Saved download ranges are invalid. Restart the download to recover.".into(),
                );
            }
            if i.ranges
                && i.done() > 0
                && part.exists()
                && fs::metadata(&part).map_err(|e| e.to_string())?.len()
                    < i.segs.iter().map(|s| s.next).max().unwrap_or(0)
            {
                return Err("The partial download is shorter than its saved progress".into());
            }
            // Servers without Range support can't resume, so they always start over.
            if i.segs.is_empty() || !i.ranges || !part.exists() {
                // One segment for the whole file; more connections split it as they open.
                i.segs = vec![Seg {
                    stop: i.size,
                    ..Default::default()
                }];
                let f = File::create(&part).map_err(|e| e.to_string())?;
                // NTFS fills the gap with zeros before a write far into a preallocated file, which stalls
                // the connections that start in the middle of a big file; a sparse file skips that
                #[cfg(windows)]
                crate::win::sparse(&f);
                if let Some(s) = i.size {
                    f.set_len(s).map_err(|e| e.to_string())?;
                }
            }
            i.status = Status::Downloading;
            part
        };
        let part = part.as_path();
        let max = self.info().conns.max(1);
        let mut errors: Vec<String> = Vec::new();
        // Each network ("" = the default one) runs up to `max` connections of its own, as one network
        // alone would: it starts with one, and doubles at most every RAMP_EVERY while every one of them is
        // receiving and the speed limits leave room. The networks are looked at on every round, so one
        // that joins gets connections and one that leaves loses them (see `worker`). A network whose
        // last connection gave up waits LANE_COOLDOWN before it is tried again.
        thread::scope(|sc| {
            let mut hs: Vec<(thread::ScopedJoinHandle<Option<String>>, String)> = Vec::new();
            let mut grown: std::collections::HashMap<String, Instant> = Default::default();
            let mut gave_up: std::collections::HashMap<String, Instant> = Default::default();
            let mut started = false;
            self.caps.lock().unwrap().clear();
            loop {
                let (done, live): (Vec<_>, Vec<_>) =
                    hs.into_iter().partition(|(h, _)| h.is_finished());
                hs = live;
                for (h, net) in done {
                    if let Some(e) = h.join().unwrap_or(Some("worker panicked".into())) {
                        gave_up.insert(net, Instant::now());
                        errors.push(e);
                    }
                }
                if self.stopping() {
                    if hs.is_empty() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let lanes = self.lanes_or(agent);
                for (net, lane_agent) in &lanes {
                    let n = hs.iter().filter(|(_, l)| l == net).count();
                    // fewer once the server turned connections away (`worker`)
                    let max = self
                        .caps
                        .lock()
                        .unwrap()
                        .get(net)
                        .copied()
                        .unwrap_or(max)
                        .min(max);
                    let want = if n == 0 {
                        // its first connection, unless it gave up a moment ago
                        usize::from(
                            gave_up
                                .get(net)
                                .is_none_or(|t| t.elapsed() >= LANE_COOLDOWN),
                        )
                    } else if n < max
                        && grown.get(net).is_none_or(|t| t.elapsed() >= RAMP_EVERY)
                        && self.ramp_ok(global, net)
                    {
                        (n * 2).min(max) - n
                    } else {
                        0
                    };
                    for _ in 0..want {
                        let Some(k) = self.claim(net) else { break };
                        // the first request's body becomes the first connection on its network
                        let pre = if *net == first_net {
                            first.take()
                        } else {
                            None
                        };
                        let (a, l) = (lane_agent.clone(), net.clone());
                        hs.push((
                            sc.spawn(move || self.worker(k, part, (a, l), global, pre, n > 0)),
                            net.clone(),
                        ));
                        grown.insert(net.clone(), Instant::now());
                        started = true;
                    }
                }
                if first.is_some() && !lanes.iter().any(|(n, _)| *n == first_net) {
                    first = None; // its network left before it was taken: the others start from scratch
                }
                if hs.is_empty() && (started || first.is_none()) {
                    // nothing left to claim, or every network has given up for now
                    let waiting = lanes.iter().any(|(net, _)| {
                        gave_up
                            .get(net)
                            .is_some_and(|t| t.elapsed() < LANE_COOLDOWN)
                    });
                    if !waiting
                        || self.info().segs.iter().all(Seg::done)
                        || lanes.iter().all(|(net, _)| gave_up.contains_key(net))
                    {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        if !errors.iter().any(|e| e == CHANGED) && self.info().segs.iter().all(Seg::done) {
            // the finished file in place: a rename, or a copy (with its progress, the lock free) when the
            // temporary directory is on another drive
            let (size, len, dest) = {
                let i = self.info();
                (
                    i.size,
                    i.segs.iter().map(|s| s.next).max().unwrap_or(0),
                    i.path(),
                )
            };
            if size.is_none() {
                OpenOptions::new()
                    .write(true)
                    .open(part)
                    .and_then(|f| f.set_len(len))
                    .map_err(|e| e.to_string())?;
            }
            let output = self.info().media_output.clone();
            if matches!(output.as_str(), "mp4" | "mkv") {
                if sniff(part) != Some("ts") {
                    return Err("The downloaded source is no longer a transport stream".into());
                }
                let ffmpeg = crate::media::toolchain()
                    .ok_or("Install FFmpeg and ffprobe to convert this transport stream")?;
                let mut converted = dest.with_extension(&output);
                if converted != dest && converted.exists() {
                    let folder = converted
                        .parent()
                        .unwrap_or(Path::new("."))
                        .to_string_lossy();
                    let name = unique_name(
                        &folder,
                        &converted.file_name().unwrap_or_default().to_string_lossy(),
                        &|_| false,
                    );
                    converted = converted.with_file_name(name);
                }
                let stage = crate::media::Stage::new(&converted)?;
                let result = crate::media::mux(
                    self,
                    &ffmpeg,
                    part,
                    None,
                    stage.path(),
                    &output,
                    output == "mp4",
                );
                if self.stopping() {
                    return Ok(false);
                }
                result?;
                stage.finish(&converted)?;
                let size = fs::metadata(&converted)
                    .map_err(|err| err.to_string())?
                    .len();
                let _ = fs::remove_file(part);
                let mut info = self.info();
                info.name = converted
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                info.size = Some(size);
                info.segs = vec![Seg {
                    next: size,
                    stop: Some(size),
                    ..Default::default()
                }];
                return Ok(true);
            }
            if fs::rename(part, &dest).is_err() {
                let copied = self.copy_out(part, &dest);
                if self.stopping() && copied.is_err() {
                    return Ok(false);
                }
                copied?;
            }
        }
        let mut i = self.info();
        if errors.iter().any(|e| e == CHANGED) {
            // Preserve existing data and identity. An explicit restart can discard them.
            return Err(CHANGED.into());
        }
        if i.segs.iter().all(Seg::done) && !part.exists() {
            if i.size.is_none() {
                i.size = Some(i.done());
            }
            // "Set file creation date as provided by the server"
            let date = chrono::DateTime::parse_from_rfc2822(&i.modified)
                .ok()
                .filter(|_| self.set_date.load(Relaxed));
            if let (Some(t), Ok(f)) = (date, File::options().write(true).open(i.path())) {
                let t = std::time::SystemTime::from(t);
                #[cfg(windows)]
                let _ = {
                    use std::os::windows::fs::FileTimesExt;
                    f.set_times(fs::FileTimes::new().set_modified(t).set_created(t))
                };
                #[cfg(not(windows))]
                let _ = f.set_times(fs::FileTimes::new().set_modified(t)); // no creation time to set
            }
            Ok(true)
        } else if self.stopping() || errors.is_empty() {
            Ok(false)
        } else {
            Err(errors.last().unwrap().clone())
        }
    }

    fn worker(
        &self,
        mut k: usize,
        part: &Path,
        (agent, net): (Agent, String),
        global: &Limiter,
        mut pre: Option<Body>,
        extra: bool,
    ) -> Option<String> {
        let mut fails = 0;
        loop {
            // a network that is gone (unplugged, unticked) hands its part back to the others
            if self.stopping() || !self.lane_present(&net) {
                self.info().segs[k].owned = false;
                return None;
            }
            let before = {
                let mut i = self.info();
                i.segs[k].net = net.clone();
                i.segs[k].next
            };
            match self.fetch(k, part, &agent, global, pre.take(), extra) {
                Ok(()) if self.stopping() => return None,
                Err(e) if e == CHANGED => {
                    self.stop();
                    return Some(e);
                }
                Err(e) if permanent_error(&e) => {
                    self.info().segs[k].owned = false;
                    return Some(e);
                }
                Err(e) if e == LANE_GONE => {
                    self.info().segs[k].owned = false;
                    return None;
                }
                Ok(()) => {
                    let n = self.claim(&net)?;
                    (k, fails) = (n, 0)
                }
                Err(e)
                    if e == NO_ANSWER_ERR
                        || ((e.starts_with("HTTP 503")
                            || e.starts_with("HTTP 429")
                            || self.info().segs[k].next == before)
                            && self.others_receiving(k, &net) > 0) =>
                {
                    // the server allows fewer connections than this (it says busy, or a new connection
                    // gets nothing: turned away, or left hanging until the read times out while the others
                    // receive): this one ends and hands its part back, and the network keeps to the
                    // connections that work (`caps`)
                    let working = self.others_receiving(k, &net).max(1);
                    let mut caps = self.caps.lock().unwrap();
                    let cap = caps.get(&net).map_or(working, |c| working.min(*c));
                    caps.insert(net.clone(), cap);
                    drop(caps);
                    let mut i = self.info();
                    let s = &mut i.segs[k];
                    (s.owned, s.rate, s.info) = (false, 0.0, String::new());
                    return None;
                }
                Err(e) => {
                    fails = if self.info().segs[k].next > before {
                        1
                    } else {
                        fails + 1
                    };
                    {
                        // released before the wait, so the other connections can take it; another
                        // network gets the first go at it
                        let mut i = self.info();
                        let s = &mut i.segs[k];
                        s.info = format!("Error: {e}, retrying...");
                        s.owned = false;
                        s.rate = 0.0;
                        if !net.is_empty() {
                            s.failed_on = net.clone();
                        }
                    }
                    if fails > RETRIES {
                        return Some(e);
                    }
                    let until = Instant::now() + retry_delay(fails);
                    while !self.stopping() && Instant::now() < until && self.lane_present(&net) {
                        thread::sleep(Duration::from_millis(100));
                    }
                    if !self.lane_present(&net) {
                        return None;
                    }
                    {
                        let n = self.claim(&net)?;
                        k = n
                    }
                }
            }
        }
    }

    fn fetch(
        &self,
        k: usize,
        part: &Path,
        agent: &Agent,
        global: &Limiter,
        pre: Option<Body>,
        extra: bool,
    ) -> Result<(), String> {
        let (url, referrer, cookies, ranges, start, stop, back, headers) = {
            let mut i = self.info();
            let ranges = i.ranges;
            let s = &mut i.segs[k];
            if !ranges {
                (s.start, s.next) = (0, 0);
            }
            if s.done() {
                return Ok(());
            }
            s.info = "Connecting...".into();
            let (start, stop) = (s.next, s.stop);
            let back = if ranges {
                (s.next - s.start).min(VERIFY)
            } else {
                0
            };
            (
                i.url.clone(),
                i.referrer.clone(),
                i.cookies.clone(),
                ranges,
                start,
                stop,
                back,
                i.headers.clone(),
            )
        };
        let from = start - back;
        let mut r = match pre.filter(|_| start == 0) {
            Some(r) => r,
            None if is_ftp(&url) => ftp_open(&url, from)?.2,
            None => {
                let mut req = with_headers(request(agent, &url, &referrer, &cookies), &headers)
                    .prefer_http1(extra && ranges);
                if ranges {
                    let range = stop.map_or(format!("bytes={from}-"), |e| {
                        format!("bytes={from}-{}", e - 1)
                    });
                    req = req.set("Range", &range);
                    let etag = self.info().etag.clone();
                    if etag.starts_with('"') {
                        req = req.set("If-Range", &etag);
                    }
                }
                let Some(resp) = self.answer(req)? else {
                    return Ok(());
                };
                let mut partial = ranges;
                if ranges && resp.status() != 206 {
                    let mut info = self.info();
                    if resp.status() == 200
                        && start == 0
                        && info.done() == 0
                        && info.segs.len() == 1
                    {
                        info.ranges = false;
                        partial = false;
                    } else {
                        return Err("server ignored the Range request".into());
                    }
                }
                validate_http_response(
                    &resp,
                    partial.then_some((from, stop.map(|end| end - 1))),
                    self.info().size,
                )?;
                // a page where the file was (an expired link, a login or error page): not written into the file
                if is_page(resp.header("Content-Type")) && !self.info().page {
                    return Err("the server sends a web page instead of the file".into());
                }
                let lm = resp.header("Last-Modified").unwrap_or_default();
                let etag = resp.header("ETag").unwrap_or_default();
                let mut info = self.info();
                if (!lm.is_empty()
                    && !info.modified.is_empty()
                    && lm != info.modified
                    && !self.ignore_date.load(Relaxed))
                    || (!etag.is_empty() && !info.etag.is_empty() && etag != info.etag)
                {
                    return Err(CHANGED.into());
                }
                if info.modified.is_empty() {
                    info.modified = lm.into();
                }
                if info.etag.is_empty() {
                    info.etag = etag.into();
                }
                resp.into_reader()
            }
        };
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(part)
            .map_err(|e| e.to_string())?;
        if back > 0 {
            // NDM compares the re-fetched bytes with the file (0x565ef0, "Check w_e_of_cn failed!").
            let (mut old, mut new) = (vec![0; back as usize], vec![0; back as usize]);
            f.seek(SeekFrom::Start(from))
                .and_then(|_| f.read_exact(&mut old))
                .map_err(|e| e.to_string())?;
            r.read_exact(&mut new).map_err(|e| e.to_string())?;
            if old != new {
                return Err(CHANGED.into());
            }
        }
        f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let mut buf = vec![0; CHUNK];
        let net = {
            let mut i = self.info();
            i.segs[k].info = "Receiving data...".into();
            i.segs[k].net.clone()
        };
        // the connection's speed, measured over half-second windows
        let (mut window, mut got) = (Instant::now(), 0u64);
        loop {
            if self.stopping() {
                return Ok(());
            }
            let want = {
                let i = self.info();
                let s = &i.segs[k];
                if s.stop.is_some() {
                    CHUNK.min(s.left() as usize)
                } else {
                    CHUNK
                }
            };
            if want == 0 {
                return Ok(());
            }
            let n = r.read(&mut buf[..want]).map_err(|e| e.to_string())?;
            if n == 0 {
                let mut i = self.info();
                let s = &mut i.segs[k];
                if s.stop.is_some() {
                    return Err("connection closed early".into());
                }
                s.stop = Some(s.next);
                return Ok(());
            }
            f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            if !net.is_empty() && !self.lane_present(&net) {
                self.info().segs[k].next += n as u64;
                return Err(LANE_GONE.into()); // unticked or gone: the part goes back to the others
            }
            got += n as u64;
            {
                let mut i = self.info();
                let s = &mut i.segs[k];
                s.next += n as u64;
                let dt = window.elapsed().as_secs_f64();
                if dt >= 0.5 {
                    let r = got as f64 / dt;
                    s.rate = if s.rate > 0.0 { (s.rate + r) / 2.0 } else { r };
                    (window, got) = (Instant::now(), 0);
                }
            }
            *self
                .net_bytes
                .lock()
                .unwrap()
                .entry(net.clone())
                .or_default() += n as u64;
            self.throttle(global, n);
        }
    }

    /// Count `n` received bytes and wait as the speed limits ask (app-wide per-file rate, this file's).
    pub(crate) fn throttle(&self, global: &Limiter, n: usize) {
        global.received.fetch_add(n as u64, Relaxed);
        self.global_bucket
            .take_at(global.rate.load(Relaxed), n, &self.stop);
        self.limiter.take(n, &self.stop);
    }

    /// How many other connections on `net` than part `k`'s are receiving data (a finished part keeps its
    /// owner's last state, so it doesn't count).
    /// A connection's request, waited for while the download runs: a server that leaves a new connection
    /// unanswered for NO_ANSWER while the others receive (it allows fewer connections), or twice that once
    /// it has sent part of the file, ends it instead of after the read timeout. None: the download stopped.
    fn answer(&self, req: Request) -> Result<Option<Response>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(call(req));
        });
        let since = Instant::now();
        let before = self.info().done();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => return r.map(Some),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the request failed".into())
                }
                Err(_) if self.stopping() => return Ok(None),
                Err(_) => {
                    // the other connections received meanwhile; or, alone, a server that has been sending (it
                    // may still count the connection just closed) gets twice as long
                    let done = self.info().done();
                    let waited = since.elapsed();
                    if (waited >= NO_ANSWER && done > before)
                        || (waited >= 2 * NO_ANSWER && done > 0)
                    {
                        return Err(NO_ANSWER_ERR.into());
                    }
                }
            }
        }
    }

    fn others_receiving(&self, k: usize, net: &str) -> usize {
        self.info()
            .segs
            .iter()
            .enumerate()
            .filter(|(j, s)| {
                *j != k && s.owned && !s.done() && s.net == net && s.info.starts_with("Receiving")
            })
            .count()
    }

    /// Every open connection on `net` is receiving data and the speed limits leave room.
    fn ramp_ok(&self, global: &Limiter, net: &str) -> bool {
        let busy = self
            .info()
            .segs
            .iter()
            .filter(|s| s.owned && s.net == net)
            .all(|s| s.info.starts_with("Receiving"));
        let speed = self.speed();
        let full = |l: &Limiter| {
            let r = l.rate.load(Relaxed);
            r > 0 && speed >= r as f64 * 0.85
        };
        busy && !full(global) && !full(&self.limiter)
    }

    /// The networks the core set for this download (`lanes`).
    fn lanes(&self) -> Vec<(String, Agent)> {
        self.lanes.lock().unwrap().clone()
    }

    /// `lanes`, or the default network alone when there are none.
    fn lanes_or(&self, agent: &Agent) -> Vec<(String, Agent)> {
        let l = self.lanes();
        if l.is_empty() {
            vec![(String::new(), agent.clone())]
        } else {
            l
        }
    }

    /// Is `net` still one of this download's networks? The default one ("") is while there are no others.
    fn lane_present(&self, net: &str) -> bool {
        let l = self.lanes.lock().unwrap();
        if net.is_empty() {
            l.is_empty()
        } else {
            l.iter().any(|(n, _)| n == net)
        }
    }

    /// Next segment for a connection on `net`: an unfinished one nobody owns (resume, or handed back),
    /// else a split. A part that failed on `net` is left to another network while there is one.
    fn claim(&self, net: &str) -> Option<usize> {
        let others = !net.is_empty() && self.lanes.lock().unwrap().iter().any(|(n, _)| n != net);
        let mut i = self.info();
        let free = |s: &Seg| !s.owned && !s.done();
        let k = i
            .segs
            .iter()
            .position(|s| free(s) && !(others && s.failed_on == net))
            .or_else(|| (!others).then(|| i.segs.iter().position(free)).flatten());
        if let Some(k) = k {
            i.segs[k].owned = true;
            return Some(k);
        }
        drop(i);
        self.steal(net)
    }

    /// Dynamic segmentation: split the segment that will take longest and hand back its end. Split in
    /// proportion to the two connections' speeds, so both finish together: a slow network no longer
    /// holds up the end of the download, as a part that would take it much longer goes mostly to the
    /// faster one, down to the last MIN_STEAL. Without speeds yet, the biggest segment is halved
    /// (at least MIN_SEG each).
    /// The split is always >= CHUNK past `next`, so the owner's in-flight chunk never crosses it.
    pub(super) fn steal(&self, net: &str) -> Option<usize> {
        let mut i = self.info();
        if !i.ranges {
            return None;
        }
        // this network's speed per connection: its fastest segment now, else none known
        let mine = i
            .segs
            .iter()
            .filter(|s| s.net == net && s.rate > 0.0)
            .map(|s| s.rate)
            .fold(0.0, f64::max);
        let eta = |s: &Seg| {
            if s.rate > 0.0 {
                s.left() as f64 / s.rate
            } else {
                0.0
            }
        };
        let timed = mine > 0.0
            && i.segs
                .iter()
                .filter(|s| s.owned && s.left() > 0)
                .all(|s| s.rate > 0.0);
        let (k, best) = if timed {
            i.segs
                .iter()
                .enumerate()
                .filter(|(_, s)| s.left() > 0)
                .max_by(|a, b| eta(a.1).total_cmp(&eta(b.1)))?
        } else {
            i.segs.iter().enumerate().max_by_key(|(_, s)| s.left())?
        };
        let (next, left, stop) = (best.next, best.left(), best.stop);
        let mid = if timed && best.rate > 0.0 {
            // where both would finish together; worth a new connection only if it saves real time
            let keep = (left as f64 * best.rate / (best.rate + mine)) as u64;
            let mid = next + keep.max(CHUNK as u64);
            let taken = (next + left).saturating_sub(mid);
            // a big part splits as always; a small one at the end only when the new connection would
            // clearly finish it first (it saves more than twice what a new request costs)
            let alone = left as f64 / best.rate;
            if taken < MIN_STEAL
                || (left < 2 * MIN_SEG && alone < 2.0 * (STEAL_STARTUP + taken as f64 / mine))
            {
                return None;
            }
            mid
        } else {
            if left < 2 * MIN_SEG {
                return None;
            }
            next + left / 2
        };
        i.segs[k].stop = Some(mid);
        i.segs.push(Seg {
            start: mid,
            next: mid,
            stop,
            owned: true,
            ..Default::default()
        });
        Some(i.segs.len() - 1)
    }
}
