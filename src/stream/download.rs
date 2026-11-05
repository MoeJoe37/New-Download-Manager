use crate::stream::{AtomicBool, AtomicU64, AtomicUsize, BufWriter, Duration, File, HashMap, HashSet, Instant, Job, Limiter, MAX_PARTS, Mutex, Part, Path, PathBuf, RETRIES, Read, Relaxed, Seg, Status, Write, engine, first_time, fs, mux_mp4, out_ext, plan_variant, refresh, remux, retime, text, thread, timed, transport, with_ext};

const PIECE: u64 = 4 << 20;

fn aes_cbc(key: &[u8], iv: [u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    if key.len() != 16 || !data.len().is_multiple_of(16) {
        return Err("bad AES-128 key or segment".into());
    }
    let mut out = aes_cbc_raw(key, iv, data)?;
    // PKCS#7 padding
    if let Some(&p) = out.last() {
        if (1..=16).contains(&p) && out.ends_with(&vec![p; p as usize]) {
            out.truncate(out.len() - p as usize);
        }
    }
    Ok(out)
}

/// AES-128-CBC, padding left in place: RustCrypto off Windows.
#[cfg(not(windows))]
fn aes_cbc_raw(key: &[u8], iv: [u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
    let mut out = data.to_vec();
    cbc::Decryptor::<aes::Aes128>::new_from_slices(key, &iv).map_err(|e| e.to_string())?.decrypt_padded_mut::<NoPadding>(&mut out).map_err(|_| "AES decryption failed")?;
    Ok(out)
}

/// AES-128-CBC, padding left in place: Windows' CNG.
#[cfg(windows)]
fn aes_cbc_raw(key: &[u8], mut iv: [u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Security::Cryptography::*;
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
    Ok(out)
}

struct Ctx<'a> {
    job: &'a Job,
    agent: &'a transport::Agent,
    global: &'a Limiter,
    referrer: String,
    cookies: String,
    origin: String,
    keys: Mutex<HashMap<String, Vec<u8>>>,
}

impl Ctx<'_> {
    fn transfer(&self, part: &Part, output: &mut dyn Write, got: &AtomicU64, tick: &dyn Fn(), max: u64, extra: bool) -> Result<u64, String> {
        let cookies = if crate::transport::same_origin(&self.origin, &part.url) { &self.cookies } else { "" };
        let mut request = transport::request(self.agent, &part.url, &self.referrer, cookies).prefer_http1(extra);
        if let Some((first, last)) = part.range {
            request = request.set("Range", &format!("bytes={first}-{last}"));
        }
        let response = transport::call(request)?;
        if part.range.is_some() && response.status() != 206 {
            return Err("server ignored the Range request".into());
        }
        engine::range::validate_http_response(&response, part.range.map(|(first, last)| (first, Some(last))), None)?;
        let expected = part.range.map(|(first, last)| last - first + 1)
            .or_else(|| response.header("Content-Length")?.parse().ok());
        if expected.is_some_and(|length| length > max) { return Err("Media fragment exceeds its size limit".into()); }
        let mut reader = response.into_reader();
        let mut buffer = vec![0; engine::CHUNK];
        let mut done = 0;
        let result = (|| {
            loop {
                if self.job.stopping() { return Err("stopped".into()); }
                let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 { break; }
                if done + n as u64 > max || expected.is_some_and(|length| done + n as u64 > length) {
                    return Err("Media fragment is longer than declared".into());
                }
                output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                done += n as u64;
                got.fetch_add(n as u64, Relaxed);
                tick();
                self.job.throttle(self.global, n);
            }
            if done == 0 || expected.is_some_and(|length| length != done) {
                return Err("Incomplete media fragment".into());
            }
            Ok(done)
        })();
        if result.is_err() { got.fetch_sub(done, Relaxed); }
        result
    }

    fn piece(&self, part: &Part, file: &Path, got: &AtomicU64, tick: &dyn Fn(), extra: bool) -> Result<(), String> {
        let tmp = file.with_extension("tmp");
        let mut committed_bytes = 0;
        let result = (|| {
            let mut output = File::create(&tmp).map_err(|e| e.to_string())?;
            if let Some((url, iv)) = &part.key {
                let key = {
                    let mut keys = self.keys.lock().unwrap();
                    if let Some(key) = keys.get(url) { key.clone() } else {
                        let mut key = Vec::with_capacity(16);
                        self.transfer(&Part { url: url.clone(), range: None, key: None, seq: 0, shift: 0.0 },
                            &mut key, &AtomicU64::new(0), &|| {}, 16, false)?;
                        keys.insert(url.clone(), key.clone());
                        key
                    }
                };
                let mut encrypted = Vec::new();
                committed_bytes = self.transfer(part, &mut encrypted, got, tick, 64 << 20, extra)?;
                let decrypted = aes_cbc(&key, *iv, &encrypted)?;
                output.write_all(&decrypted).map_err(|e| e.to_string())?;
                got.fetch_sub(committed_bytes - decrypted.len() as u64, Relaxed);
                committed_bytes = decrypted.len() as u64;
            } else {
                committed_bytes = self.transfer(part, &mut output, got, tick, u64::MAX, extra)?;
            }
            output.sync_data().map_err(|e| e.to_string())?;
            drop(output);
            fs::rename(&tmp, file).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            got.fetch_sub(committed_bytes, Relaxed);
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

pub fn run(job: &Job, agent: &transport::Agent, global: &Limiter) -> Result<bool, String> {
    let (url, referrer, cookies, output, variant) = {
        let i = job.info();
        (i.url.clone(), i.referrer.clone(), i.cookies.clone(), i.media_output.clone(), i.media_variant)
    };
    if !matches!(output.as_str(), "" | "mp4" | "ts" | "mkv" | "webm") {
        return Err("Unsupported media output container".into());
    }
    let scoped_cookies = |target: &str| if crate::transport::same_origin(&url, target) { cookies.as_str() } else { "" };
    let get = |u: &str| text(agent, u, &referrer, scoped_cookies(u));
    let mut tracks = plan_variant(&get, &url, variant)?;
    for t in tracks.iter_mut().filter(|t| t.whole) {
        // each period's single file: 4 MiB byte ranges, so it downloads over several connections and resumes
        t.parts = std::mem::take(&mut t.parts)
            .into_iter()
            .flat_map(|p| match (p.range, engine::probe::probe(agent, &p.url, &referrer, scoped_cookies(&p.url))) {
                (None, Ok(engine::probe::Probe { size: Some(len), ranges: true, .. })) => (0..len).step_by(PIECE as usize).map(|a| Part { range: Some((a, (a + PIECE).min(len) - 1)), ..p.clone() }).collect(),
                _ => vec![p],
            })
            .collect();
    }
    if job.stopping() {
        return Ok(false);
    }
    let live = tracks.iter().any(|t| t.live);
    let root = {
        let mut i = job.info();
        let ext = if output.is_empty() { out_ext(&tracks) } else { output.as_str() };
        i.name = if i.name.is_empty() { engine::names::unique_name(&i.folder, &engine::names::sanitize(&with_ext(&engine::names::url_name(&url), ext)), &|_| false) } else { with_ext(&i.name, ext) };
        (i.ranges, i.probed, i.stream, i.status) = (true, true, true, Status::Downloading);
        let dir = i.part();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        dir
    };
    // Indexes alone do not identify VOD fragments. Use a digest of the exact plan, including
    // ranges, keys and timing. Keep pre-upgrade pieces until a replacement output succeeds.
    use sha2::{Digest, Sha256};
    let fingerprint = if live {
        format!("live-{}", chrono::Utc::now().timestamp_micros())
    } else {
        format!("{:x}", Sha256::digest(format!("{tracks:?}")))
    };
    let dir = root.join(fingerprint);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
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
                let est = (g * *len as u64).checked_div(h).unwrap_or(g).max(g);
                let s = Seg { start: off, next: off + g, stop: (!live).then_some(off + est), info: note.into(), owned: false, ..Default::default() };
                off += if live { g } else { est };
                s
            })
            .collect();
        i.size = (!live).then_some(off);
    };
    show("Connecting...");
    let ctx = Ctx { job, agent, global, referrer: referrer.clone(), cookies: cookies.clone(), origin: url.clone(), keys: Default::default() };
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
    let active = AtomicUsize::new(0);
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
            let extra = active.fetch_add(1, Relaxed) != 0;
            let r = ctx.piece(&part, &f, &got[t], &tick, extra);
            active.fetch_sub(1, Relaxed);
            match r {
                Ok(()) => {
                    have[t].fetch_add(1, Relaxed);
                    break;
                }
                Err(_) if job.stopping() => return,
                Err(e) => {
                    fails += 1;
                    if fails > RETRIES || engine::range::permanent_error(&e) {
                        if live && e.starts_with("HTTP 404 ") {
                            break; // a live piece that's gone from the server: skip it
                        }
                        *failed.lock().unwrap() = Some(e);
                        return;
                    }
                    show(&format!("Error: {e}, retrying..."));
                    let until = Instant::now() + engine::range::retry_delay(fails);
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
        let key = |p: &Part| format!("{}#{:?}", p.url, p.range);
        let mut seen: Vec<HashSet<String>> = tracks.iter().map(|t| t.parts.iter().map(key).collect()).collect();
        while !job.stopping() && !all_ended() && failed.lock().unwrap().is_none() {
            let wait = tracks.iter().filter(|t| t.live).map(|t| t.target).fold(10.0, f64::min).clamp(1.0, 10.0);
            let until = Instant::now() + Duration::from_secs_f64(wait);
            while !job.stopping() && Instant::now() < until && failed.lock().unwrap().is_none() {
                thread::sleep(Duration::from_millis(100));
            }
            if job.stopping() || failed.lock().unwrap().is_some() { return; }
            for (t, tr) in tracks.iter().enumerate().filter(|(t, _)| !ended[*t].load(Relaxed)) {
                let Ok(now) = refresh(&get, tr, t) else { continue };
                let new: Vec<Part> = now.parts.into_iter().filter(|p| if tr.dash { seen[t].insert(key(p)) } else { p.seq > last[t] }).collect();
                if let Some(p) = new.last() {
                    last[t] = p.seq;
                }
                let mut lists = lists.lock().unwrap();
                if lists[t].len().saturating_add(new.len()) > MAX_PARTS {
                    *failed.lock().unwrap() = Some("Live recording reached the segment limit".into());
                    return;
                }
                lists[t].extend(new);
                if !now.live {
                    ended[t].store(true, Relaxed);
                }
            }
        }
    };
    let conns = job.info().conns.clamp(1, 16);
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
    let parts: Vec<Vec<Part>> = lists.into_inner().unwrap();
    let mut dest = job.info().path();
    let paths = |t: usize| (0..parts[t].len()).map(move |k| file(t, k)).filter(|p| p.is_file());
    let pieces = |t: usize| paths(t).map(|p| fs::read(p).map_err(|e| e.to_string()));
    if (0..n).any(|t| paths(t).next().is_none()) {
        return Err("A required media track has no complete fragments".into());
    }
    if !live && (0..n).any(|t| paths(t).count() != parts[t].len()) {
        return Err("A required media fragment is missing".into());
    }
    let ext = |t: usize| tracks[t].ext.as_str();
    let fmp4 = |t: usize| (tracks[t].init.is_some() || tracks[t].whole) && ext(t) != "webm";
    use remux::{Kind, Regroup};
    let tfile = |t: usize| move |k: usize| file(t, k);
    let (f0, f1) = (tfile(0), tfile(1.min(n - 1)));
    let ft: [&dyn Fn(usize) -> PathBuf; 2] = [&f0, &f1];
    let expected: u64 = (0..n).flat_map(paths).filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    let stage = crate::media::Stage::new(&dest)?;
    let staged = stage.path();
    let watch = [staged.to_path_buf()];
    let ffmpeg = (!job.stopping()).then(crate::media::toolchain).flatten();
    let result = with_post(job, "Joining the pieces...", &watch, expected, || -> Result<bool, String> {
        let base = (0..n).filter(|t| fmp4(*t)).filter_map(|t| first_time(timed(tracks[t].whole, &parts[t], ft[t]))).fold(f64::INFINITY, f64::min);
        let base = if base.is_finite() { base } else { 0.0 };
        let stop = || job.stopping() && !live;
        let cancel = |piece| if stop() { Err("stopped".into()) } else { piece };
        let target = dest.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        let muxed = if n == 2 && target == "mp4" && fmp4(0) && fmp4(1) {
            let mut v = Regroup::new(retime(timed(tracks[0].whole, &parts[0], ft[0]), base).map(cancel), Kind::Mp4);
            let mut a = Regroup::new(retime(timed(tracks[1].whole, &parts[1], ft[1]), base).map(cancel), Kind::Mp4);
            mux_mp4(&mut v, &mut a, staged)?
        } else if ext(0) == "ts" && target == "mp4" && ffmpeg.is_none() {
            let mut audio = pieces(1.min(n - 1)).map(cancel);
            remux::ts_to_mp4(&mut pieces(0).map(cancel), (n == 2).then_some(&mut audio as remux::Pieces), staged)?
        } else if n == 2 && target == "webm" && ext(0) == "webm" && ext(1) == "webm" {
            remux::webm_merge(&mut Regroup::new(pieces(0).map(cancel), Kind::Webm),
                &mut Regroup::new(pieces(1).map(cancel), Kind::Webm), staged)?
        } else { false };
        if muxed { return Ok(true); }

        let write_track = |track: usize, path: &Path| -> Result<(), String> {
            let mut writer = BufWriter::with_capacity(256 << 10, File::create(path).map_err(|e| e.to_string())?);
            if fmp4(track) {
                for bytes in retime(timed(tracks[track].whole, &parts[track], ft[track]), base).map(cancel) {
                    writer.write_all(&bytes?).map_err(|e| e.to_string())?;
                }
            } else {
                let mut buffer = vec![0; 256 << 10];
                for piece in paths(track) {
                    let mut reader = File::open(piece).map_err(|e| e.to_string())?;
                    loop {
                        if stop() { return Err("stopped".into()); }
                        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
                        if n == 0 { break; }
                        writer.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                    }
                }
            }
            writer.flush().map_err(|e| e.to_string())
        };
        if n == 1 && target == ext(0) {
            write_track(0, staged)?;
            return Ok(true);
        }
        if let Some(ffmpeg) = &ffmpeg {
            let video = dir.join(format!("track0.{}", ext(0)));
            let audio = (n == 2).then(|| dir.join(format!("track1.{}", ext(1))));
            write_track(0, &video)?;
            if let Some(audio) = &audio { write_track(1, audio)?; }
            match crate::media::mux(job, ffmpeg, &video, audio.as_deref(), staged, target, output == "mp4") {
                Ok(()) => return Ok(true),
                Err(err) if n > 1 || !output.is_empty() || stop() => return Err(err),
                Err(_) => {} // Automatic output may preserve a single unsupported TS source.
            }
        }
        if n == 1 && ext(0) == "ts" && output.is_empty() {
            write_track(0, staged)?;
            return Ok(false);
        }
        Err("Could not assemble the selected media tracks. Install FFmpeg and ffprobe, then resume.".into())
    });
    if job.stopping() && !live { return Ok(false); }
    if !result? {
        let folder = dest.parent().unwrap_or(Path::new(".")).to_string_lossy();
        let fallback = with_ext(dest.file_name().unwrap_or_default().to_string_lossy().as_ref(), "ts");
        dest = dest.with_file_name(engine::names::unique_name(&folder, &fallback, &|_| false));
    }
    stage.finish(&dest)?;
    let _ = fs::remove_dir_all(&root);
    let size = fs::metadata(&dest).map_err(|e| e.to_string())?.len();
    let mut i = job.info();
    i.name = dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    i.size = Some(size);
    i.segs = vec![Seg { start: 0, next: size, stop: Some(size), info: String::new(), owned: false, ..Default::default() }];
    Ok(true)
}

/// Runs `work` (joining pieces, video and audio...) while the download's bar shows `label` and how far the
/// files in `out` have grown towards `expected` bytes (0: unknown).
pub(crate) fn with_post<T>(job: &Job, label: &str, out: &[PathBuf], expected: u64, work: impl FnOnce() -> T) -> T {
    let stop = AtomicBool::new(false);
    job.set_post(label, if expected > 0 { 0.0 } else { -1.0 });
    let r = thread::scope(|sc| {
        sc.spawn(|| {
            while !stop.load(Relaxed) {
                if expected > 0 {
                    let have: u64 = out.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
                    let mut info = job.info();
                    if let Some((current, progress)) = &mut info.post {
                        if current == label {
                            *progress = (have as f64 / expected as f64).min(0.99) as f32;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(150));
            }
        });
        struct StopWatch<'a>(&'a AtomicBool);
        impl Drop for StopWatch<'_> {
            fn drop(&mut self) { self.0.store(true, Relaxed); }
        }
        let _stop_watch = StopWatch(&stop);
        work()
    });
    job.info().post = None;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_write_failure_rolls_back_progress() {
        use std::io::{BufRead, BufReader, Error, ErrorKind};
        use std::net::TcpListener;
        struct LimitedDisk(usize);
        impl Write for LimitedDisk {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.0 == 0 { return Err(Error::new(ErrorKind::StorageFull, "disk full")); }
                let amount = self.0.min(bytes.len());
                self.0 -= amount;
                Ok(amount)
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/segment.ts", server.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut reader = BufReader::new(&socket);
            while reader.read_line(&mut String::new()).unwrap() > 2 {}
            let data = vec![0x47; 2 * engine::CHUNK];
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()).unwrap();
            let _ = socket.write_all(&data);
        });
        let job = Job::new(engine::Info::default());
        let agent = transport::client::agent("", "");
        let limit = Limiter::default();
        let ctx = Ctx { job: &job, agent: &agent, global: &limit, referrer: String::new(), cookies: String::new(),
            origin: url.clone(), keys: Mutex::default() };
        let got = AtomicU64::new(0);
        let part = Part { url, range: None, key: None, seq: 0, shift: 0.0 };
        let result = ctx.transfer(&part, &mut LimitedDisk(engine::CHUNK), &got, &|| {}, u64::MAX, false);
        assert_eq!(result, Err("disk full".into()));
        assert_eq!(got.load(Relaxed), 0);
        handle.join().unwrap();
    }
}
