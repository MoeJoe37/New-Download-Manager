use super::*;

/// Joins a video file and an audio file (a site's separate streams, fetched by yt-dlp without ffmpeg)
/// into `out`: fragmented MP4 into one MP4, WebM into one WebM. Ok(false): not a pair NDM can join
/// (nothing useful written).
pub fn join_files(video: &Path, audio: &Path, out: &Path) -> Result<bool, String> {
    fn chunks(p: &Path) -> Result<impl Iterator<Item = Result<Vec<u8>, String>>, String> {
        let mut f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(std::iter::from_fn(move || {
            let mut buf = vec![0u8; 1 << 20];
            match f.read(&mut buf) {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some(Ok(buf))
                }
                Err(e) => Some(Err(e.to_string())),
            }
        }))
    }
    use remux::{Kind, Regroup};
    let webm = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("webm"));
    if webm(video) != webm(audio) {
        return Ok(false);
    }
    if webm(video) {
        return remux::webm_merge(&mut Regroup::new(chunks(video)?, Kind::Webm), &mut Regroup::new(chunks(audio)?, Kind::Webm), out);
    }
    let timed = |p: &Path| chunks(p).map(|c| Regroup::new(c, Kind::Mp4).map(|u| (u, 0.0)));
    let mut base = f64::INFINITY;
    for p in [video, audio] {
        if let Some(t) = first_time(timed(p)?) {
            base = base.min(t);
        }
    }
    let base = if base.is_finite() { base } else { 0.0 };
    let mut v = Regroup::new(retime(timed(video)?, base), Kind::Mp4);
    let mut a = Regroup::new(retime(timed(audio)?, base), Kind::Mp4);
    mux_mp4(&mut v, &mut a, out)
}

/// A track's pieces on disk with their timeline shift; a whole file is re-cut into fragments first.
pub(super) fn timed<'a>(whole: bool, parts: &'a [Part], file: &'a dyn Fn(usize) -> PathBuf) -> Box<dyn Iterator<Item = (Result<Vec<u8>, String>, f64)> + 'a> {
    let on_disk = (0..parts.len()).filter(move |k| file(*k).exists());
    let read = move |k: usize| fs::read(file(k)).map_err(|e| e.to_string());
    if whole {
        // one file per period: each is re-cut on its own and moved by its own period's shift
        let mut runs: Vec<(&str, f64, Vec<usize>)> = vec![];
        for k in on_disk {
            match runs.last_mut() {
                Some(r) if r.0 == parts[k].url => r.2.push(k),
                _ => runs.push((&parts[k].url, parts[k].shift, vec![k])),
            }
        }
        Box::new(runs.into_iter().flat_map(move |(_, shift, ks)| remux::Regroup::new(ks.into_iter().map(read), remux::Kind::Mp4).map(move |u| (u, shift))))
    } else {
        Box::new(on_disk.map(move |k| (read(k), parts[k].shift)))
    }
}

pub(super) fn tfdt_of(d: &[u8], moof: Bx) -> Option<u64> {
    let b = find(d, moof, &[b"traf", b"tfdt"])?;
    let at = b.1 + 4;
    Some(if d[b.1] == 1 { u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?) } else { be32(d, at) as u64 })
}

/// When a track's first fragment (moved by its piece's shift) is presented, in seconds.
pub(super) fn first_time(it: impl Iterator<Item = (Result<Vec<u8>, String>, f64)>) -> Option<f64> {
    let mut scale = 0.0;
    for (p, shift) in it.take(64) {
        let p = p.ok()?;
        let top = boxes(&p, 0, p.len());
        if let Some(s) = top.iter().find(|b| &b.3 == b"moov").and_then(|m| traks(&p, *m).first().map(|t| t.2)) {
            scale = s;
        }
        if let Some(t) = top.iter().find(|b| &b.3 == b"moof").and_then(|m| tfdt_of(&p, *m)) {
            return (scale > 0.0).then(|| t as f64 / scale + shift);
        }
    }
    None
}

/// fMP4 pieces with every fragment's decode time moved by its piece's shift less `base` (seconds).
/// Initialization pieces after the first are left out.
// ponytail: the first period's codec settings serve every period; ad breaks with other codecs need a re-encode
pub(super) fn retime<'a>(it: impl Iterator<Item = (Result<Vec<u8>, String>, f64)> + 'a, base: f64) -> impl Iterator<Item = Result<Vec<u8>, String>> + 'a {
    let (mut scale, mut first) = (0.0, true);
    it.map(move |(p, shift)| {
        let p = p?;
        let top = boxes(&p, 0, p.len());
        let mut out = Vec::with_capacity(p.len() + 16);
        for b in &top {
            let raw = &p[b.0..b.2];
            match &b.3 {
                b"moov" => {
                    if let Some(t) = traks(&p, *b).first() {
                        scale = t.2;
                    }
                    if first {
                        out.extend_from_slice(raw);
                    }
                }
                b"ftyp" if !first => {}
                b"moof" => {
                    let delta = ((shift - base) * scale).round() as i64;
                    out.extend(if delta == 0 { raw.to_vec() } else { moof_shift(raw, delta) });
                }
                _ => out.extend_from_slice(raw),
            }
        }
        out.extend_from_slice(&p[top.last().map_or(0, |b| b.2)..]);
        if top.iter().any(|b| &b.3 == b"moov") {
            first = false;
        }
        Ok(out)
    })
}

/// A moof with its tfdt values moved by `delta` ticks. One that outgrows 32 bits becomes version 1; the
/// truns' data offsets (counted from the moof's start) follow any change in the moof's size.
fn moof_shift(m: &[u8], delta: i64) -> Vec<u8> {
    let moof = boxes(m, 0, m.len())[0];
    let mut body = Vec::new();
    for c in boxes(m, moof.1, moof.2) {
        if &c.3 != b"traf" {
            body.extend_from_slice(&m[c.0..c.2]);
            continue;
        }
        let mut traf = Vec::new();
        for x in boxes(m, c.1, c.2) {
            if &x.3 != b"tfdt" {
                traf.extend_from_slice(&m[x.0..x.2]);
                continue;
            }
            let (v1, at) = (m[x.1] == 1, x.1 + 4);
            let old = if v1 { u64::from_be_bytes(m[at..at + 8].try_into().unwrap()) } else { be32(m, at) as u64 };
            let new = (old as i64).saturating_add(delta).max(0) as u64;
            traf.extend(if !v1 && new <= u32::MAX as u64 { mkbox(b"tfdt", &[&m[x.1..x.1 + 4], &(new as u32).to_be_bytes()[..]].concat()) } else { mkbox(b"tfdt", &[&[1, 0, 0, 0][..], &new.to_be_bytes()].concat()) });
        }
        body.extend(mkbox(b"traf", &traf));
    }
    let mut out = mkbox(b"moof", &body);
    let grew = out.len() as i64 - m.len() as i64;
    if grew != 0 {
        let o = boxes(&out, 0, out.len())[0];
        let runs: Vec<Bx> = boxes(&out, o.1, o.2).into_iter().filter(|b| &b.3 == b"traf").flat_map(|t| boxes(&out, t.1, t.2)).filter(|b| &b.3 == b"trun").collect();
        for r in runs {
            if be32(&out, r.1) & 1 != 0 {
                let at = r.1 + 8;
                let v = i32::from_be_bytes(out[at..at + 4].try_into().unwrap()) as i64 + grew;
                out[at..at + 4].copy_from_slice(&(v as i32).to_be_bytes());
            }
        }
    }
    out
}

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

pub(super) fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap())
}

pub(super) fn mkbox(t: &[u8; 4], body: &[u8]) -> Vec<u8> {
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

pub(super) fn traks(d: &[u8], moov: Bx) -> Vec<(Bx, u32, f64)> {
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
pub(super) fn mux_mp4<'a>(v: &'a mut (dyn Iterator<Item = Result<Vec<u8>, String>> + 'a), a: &'a mut (dyn Iterator<Item = Result<Vec<u8>, String>> + 'a), out: &Path) -> Result<bool, String> {
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
