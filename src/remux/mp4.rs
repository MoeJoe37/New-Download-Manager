use super::mp4_boxes::{avc1, bx, esds, full, visual};
use super::video::{hvc1, mpeg2_picture, mpeg2_seq, nals, sps_size};
use super::{io, Sample};
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

const MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

fn matrix() -> Vec<u8> {
    MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect()
}

#[derive(Default)]
struct TrackW {
    audio: bool,
    scale: u32,
    entry: Vec<u8>,
    size: (u16, u16),
    sizes: Vec<u32>,
    offs: Vec<u64>,
    dts: Vec<i64>,
    cts: Vec<i64>,
    sync: Vec<u32>,
    /// Audio frame lengths, in samples.
    adur: Vec<u32>,
    /// First presentation time, 90 kHz.
    start: i64,
}

impl TrackW {
    fn durations(&self) -> Vec<u32> {
        if self.audio {
            return self.adur.clone();
        }
        let mut d: Vec<u32> = self.dts.windows(2).map(|w| (w[1] - w[0]).clamp(1, u32::MAX as i64) as u32).collect();
        d.push(d.last().copied().unwrap_or(3000));
        d
    }

    fn trak(&self, id: u32, origin: i64) -> Vec<u8> {
        let durs = self.durations();
        let media: u64 = durs.iter().map(|d| *d as u64).sum();
        let ms = |t: u64, scale: u32| (t * 1000 / scale.max(1) as u64) as u32;
        let lead = ms((self.start - origin).max(0) as u64, 90000);
        let first_cts = self.cts.first().copied().unwrap_or(0).max(0) as u32;
        let mut edits = Vec::new();
        if lead > 0 {
            edits.push((lead, -1i32));
        }
        edits.push((ms(media - (first_cts as u64).min(media), self.scale), first_cts as i32));
        let total: u32 = edits.iter().map(|e| e.0).sum();
        let elst: Vec<u8> = edits.iter().flat_map(|(d, t)| [d.to_be_bytes(), t.to_be_bytes(), 0x10000u32.to_be_bytes()].concat()).collect();
        let edts = bx(b"edts", &[&full(b"elst", 0, &[&(edits.len() as u32).to_be_bytes(), &elst])]);
        let (w, h) = self.size;
        let tkhd = full(
            b"tkhd",
            3,
            &[&[0; 8], &id.to_be_bytes(), &[0; 4], &total.to_be_bytes(), &[0; 8], &[0; 4], &(if self.audio { 0x0100u16 } else { 0 }).to_be_bytes(), &[0; 2], &matrix(), &((w as u32) << 16).to_be_bytes(), &((h as u32) << 16).to_be_bytes()],
        );
        let mdhd = full(b"mdhd", 0, &[&[0; 8], &self.scale.to_be_bytes(), &(media.min(u32::MAX as u64) as u32).to_be_bytes(), &0x55c4u16.to_be_bytes(), &[0; 2]]);
        let (handler, name): (&[u8; 4], &[u8]) = if self.audio { (b"soun", b"SoundHandler\0") } else { (b"vide", b"VideoHandler\0") };
        let hdlr = full(b"hdlr", 0, &[&[0; 4], handler, &[0; 12], name]);
        let head = if self.audio { full(b"smhd", 0, &[&[0; 4]]) } else { full(b"vmhd", 1, &[&[0; 8]]) };
        let dinf = bx(b"dinf", &[&full(b"dref", 0, &[&1u32.to_be_bytes(), &full(b"url ", 1, &[])])]);
        let runs = |v: &mut dyn Iterator<Item = u32>| -> Vec<(u32, u32)> {
            let mut r: Vec<(u32, u32)> = Vec::new();
            for x in v {
                match r.last_mut() {
                    Some(l) if l.1 == x => l.0 += 1,
                    _ => r.push((1, x)),
                }
            }
            r
        };
        let table = |r: &[(u32, u32)]| -> Vec<u8> { [(r.len() as u32).to_be_bytes().to_vec(), r.iter().flat_map(|(c, v)| [c.to_be_bytes(), v.to_be_bytes()].concat()).collect()].concat() };
        let n = self.sizes.len() as u32;
        let mut stbl = vec![
            full(b"stsd", 0, &[&1u32.to_be_bytes(), &self.entry]),
            full(b"stts", 0, &[&table(&runs(&mut durs.iter().copied()))]),
        ];
        if self.cts.iter().any(|c| *c != 0) {
            stbl.push(full(b"ctts", 0, &[&table(&runs(&mut self.cts.iter().map(|c| (*c).max(0) as u32)))]));
        }
        if !self.audio {
            stbl.push(full(b"stss", 0, &[&(self.sync.len() as u32).to_be_bytes(), &self.sync.iter().flat_map(|s| s.to_be_bytes()).collect::<Vec<u8>>()]));
        }
        stbl.push(full(b"stsc", 0, &[&1u32.to_be_bytes(), &1u32.to_be_bytes(), &1u32.to_be_bytes(), &1u32.to_be_bytes()]));
        stbl.push(full(b"stsz", 0, &[&0u32.to_be_bytes(), &n.to_be_bytes(), &self.sizes.iter().flat_map(|s| s.to_be_bytes()).collect::<Vec<u8>>()]));
        stbl.push(full(b"co64", 0, &[&n.to_be_bytes(), &self.offs.iter().flat_map(|o| o.to_be_bytes()).collect::<Vec<u8>>()]));
        let minf = bx(b"minf", &[&head, &dinf, &bx(b"stbl", &[&stbl.concat()])]);
        bx(b"trak", &[&tkhd, &edts, &bx(b"mdia", &[&mdhd, &hdlr, &minf])])
    }
}

pub(super) struct Mp4 {
    f: BufWriter<File>,
    pos: u64,
    mdat: u64,
    v: Option<TrackW>,
    a: Option<TrackW>,
    vps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    shift: i64,
}

impl Mp4 {
    pub(super) fn create(out: &Path) -> Result<Mp4, String> {
        let mut f = BufWriter::new(File::create(out).map_err(io)?);
        let ftyp = bx(b"ftyp", &[b"isom", &0x200u32.to_be_bytes(), b"isom", b"iso2", b"avc1", b"mp41"]);
        f.write_all(&ftyp).map_err(io)?;
        let mdat = ftyp.len() as u64;
        f.write_all(&[0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 0]).map_err(io)?;
        Ok(Mp4 { f, pos: mdat + 16, mdat, v: None, a: None, vps: None, sps: None, pps: None, shift: 0 })
    }

    fn put(&mut self, data: &[u8]) -> Result<(u64, u32), String> {
        self.f.write_all(data).map_err(io)?;
        let at = self.pos;
        self.pos += data.len() as u64;
        Ok((at, data.len() as u32))
    }

    pub(super) fn add(&mut self, s: Sample) -> Result<(), String> {
        if s.audio {
            if self.a.is_none() {
                let Some((entry, rate)) = &s.entry else { return Ok(()) };
                self.a = Some(TrackW { audio: true, scale: *rate, entry: entry.clone(), start: s.pts, ..Default::default() });
            }
            let (at, n) = self.put(&s.data)?;
            let t = self.a.as_mut().unwrap();
            t.offs.push(at);
            t.sizes.push(n);
            t.adur.push(s.dur);
            return Ok(());
        }
        if s.codec == 0x01 || s.codec == 0x02 {
            // MPEG-1/2 video: pictures stored as they are, the sequence header as the decoder config
            let key = mpeg2_picture(&s.data) == Some(1);
            if self.v.is_none() {
                let Some((dsi, size)) = mpeg2_seq(&s.data).filter(|_| key) else { return Ok(()) };
                let oti = if s.codec == 0x01 { 0x6a } else { 0x61 };
                self.v = Some(TrackW { scale: 90000, entry: visual(b"mp4v", size, &esds(oti, 0x11, &dsi)), size, start: s.pts, ..Default::default() });
            }
            return self.frame(&s.data, s.dts, s.pts, key);
        }
        let hevc = s.codec == 0x24;
        let (mut out, mut key) = (Vec::with_capacity(s.data.len() + 16), false);
        for n in nals(&s.data) {
            if hevc && n.len() < 2 {
                continue;
            }
            let ty = if hevc { (n[0] >> 1) & 0x3f } else { n[0] & 0x1f };
            match (hevc, ty) {
                (false, 7) | (true, 33) => self.sps = self.sps.take().or_else(|| Some(n.to_vec())),
                (false, 8) | (true, 34) => self.pps = self.pps.take().or_else(|| Some(n.to_vec())),
                (true, 32) => self.vps = self.vps.take().or_else(|| Some(n.to_vec())),
                (false, 9) | (true, 35) => {} // access unit delimiters
                (hevc, t) => {
                    key |= if hevc { (16..=21).contains(&t) } else { t == 5 }; // IDR / IRAP pictures
                    out.extend_from_slice(&(n.len() as u32).to_be_bytes());
                    out.extend_from_slice(n);
                }
            }
        }
        if out.is_empty() {
            return Ok(());
        }
        if self.v.is_none() {
            if !key {
                return Ok(()); // start at a key frame
            }
            let (entry, size) = match (hevc, &self.vps, &self.sps, &self.pps) {
                (true, Some(v), Some(sp), Some(pp)) if sp.len() > 16 => hvc1(v, sp, pp),
                (false, _, Some(sp), Some(pp)) if sp.len() >= 4 => {
                    let size = sps_size(sp);
                    (avc1(sp, pp, size), size)
                }
                _ => return Ok(()),
            };
            self.v = Some(TrackW { scale: 90000, entry, size, start: s.pts, ..Default::default() });
        }
        self.frame(&out, s.dts, s.pts, key)
    }

    /// A video sample, its times continued across discontinuities.
    fn frame(&mut self, out: &[u8], dts: i64, pts: i64, key: bool) -> Result<(), String> {
        let (at, n) = self.put(out)?;
        let t = self.v.as_mut().unwrap();
        let (mut d, mut p) = (dts + self.shift, pts + self.shift);
        if let Some(&l) = t.dts.last() {
            if d <= l {
                // a discontinuity (or a stream that restarted its clock): continue right after the last frame
                let step = t.dts.len().checked_sub(2).map_or(3000, |k| l - t.dts[k]).max(1);
                let shift = l + step - d;
                (self.shift, d, p) = (self.shift + shift, d + shift, p + shift);
            }
        }
        if key {
            t.sync.push(t.sizes.len() as u32 + 1);
        }
        t.offs.push(at);
        t.sizes.push(n);
        t.dts.push(d);
        t.cts.push(p - d);
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<bool, String> {
        let tracks: Vec<&TrackW> = [self.v.as_ref(), self.a.as_ref()].into_iter().flatten().filter(|t| !t.sizes.is_empty()).collect();
        if tracks.is_empty() {
            return Ok(false);
        }
        let origin = tracks.iter().map(|t| t.start).min().unwrap_or(0);
        let traks: Vec<u8> = tracks.iter().enumerate().flat_map(|(k, t)| t.trak(k as u32 + 1, origin)).collect();
        let dur = tracks.iter().map(|t| {
            let media: u64 = t.durations().iter().map(|d| *d as u64).sum();
            (t.start - origin).max(0) as u64 / 90 + media * 1000 / t.scale as u64
        });
        let dur = dur.max().unwrap_or(0).min(u32::MAX as u64) as u32;
        let mvhd = full(
            b"mvhd",
            0,
            &[&[0; 8], &1000u32.to_be_bytes(), &dur.to_be_bytes(), &0x10000u32.to_be_bytes(), &0x0100u16.to_be_bytes(), &[0; 10], &matrix(), &[0; 24], &(tracks.len() as u32 + 1).to_be_bytes()],
        );
        let moov = bx(b"moov", &[&mvhd, &traks]);
        let size = self.pos - self.mdat;
        self.f.seek(SeekFrom::Start(self.mdat + 8)).map_err(io)?;
        self.f.write_all(&size.to_be_bytes()).map_err(io)?;
        self.f.seek(SeekFrom::End(0)).map_err(io)?;
        self.f.write_all(&moov).map_err(io)?;
        self.f.flush().map_err(io)?;
        Ok(true)
    }
}
