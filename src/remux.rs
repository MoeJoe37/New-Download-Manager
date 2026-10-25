//! Remuxing downloaded stream pieces into one file, without ffmpeg:
//! - MPEG-TS (H.264 / H.265 / MPEG-2 video with AAC, MP3, AC-3, E-AC-3 or Opus, what HLS sends) and packed
//!   audio -> one MP4, as NDM saves HLS videos;
//! - DASH WebM video + WebM audio -> one WebM.
//! Anything else (VC-1, DTS...) answers Ok(false) and the caller keeps the pieces as they are.
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

pub type Pieces<'a> = &'a mut dyn Iterator<Item = Result<Vec<u8>, String>>;

const UNSUPPORTED: &str = "\u{1}unsupported";

fn io(e: std::io::Error) -> String {
    e.to_string()
}

// ---------------------------------------------------------------- regrouping

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Mp4,
    Webm,
}

/// Downloaded pieces re-cut into whole container units, whatever the piece boundaries were (a single
/// file fetched in byte ranges): MP4 up to each `moov` / `mdat`, WebM up to each `Tracks` / `Cluster`.
pub struct Regroup<'a> {
    inner: Box<dyn Iterator<Item = Result<Vec<u8>, String>> + 'a>,
    kind: Kind,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl<'a> Regroup<'a> {
    pub fn new(inner: impl Iterator<Item = Result<Vec<u8>, String>> + 'a, kind: Kind) -> Regroup<'a> {
        Regroup { inner: Box::new(inner), kind, buf: Vec::new(), pos: 0, eof: false }
    }

    /// Ok(Some(end, last)) when a whole element starts at `pos`; None = more data needed.
    fn element(&self) -> Option<(usize, bool)> {
        let (d, at) = (&self.buf, self.pos);
        match self.kind {
            Kind::Mp4 => {
                let size = u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?) as u64;
                let t = d.get(at + 4..at + 8)?;
                let size = match size {
                    1 => u64::from_be_bytes(d.get(at + 8..at + 16)?.try_into().ok()?),
                    0 => return None, // to the end of the file
                    s => s,
                };
                let end = at.checked_add(size.max(8) as usize)?;
                (end <= d.len()).then_some((end, t == b"moov" || t == b"mdat"))
            }
            Kind::Webm => {
                let (id, il) = ebml_id(d, at)?;
                let (size, sl, unknown) = vint(d, at + il)?;
                if id == SEGMENT {
                    return Some((at + il + sl, false)); // step inside
                }
                if unknown {
                    if id != CLUSTER {
                        return None; // other open-ended elements run to the end of the data
                    }
                    // a live cluster without a size ends where an element that can't be inside it starts
                    let mut e = at + il + sl;
                    loop {
                        let (cid, cl) = ebml_id(d, e)?;
                        if ![0xe7, 0xa3, 0xa0, 0xa7, 0xab].contains(&cid) {
                            return Some((e, true));
                        }
                        let (size, l, _) = vint(d, e + cl)?;
                        e = (e + cl + l).checked_add(size as usize).filter(|end| *end <= d.len())?;
                    }
                }
                let end = (at + il + sl).checked_add(size as usize)?;
                (end <= d.len()).then_some((end, id == TRACKS || id == CLUSTER))
            }
        }
    }
}

impl Iterator for Regroup<'_> {
    type Item = Result<Vec<u8>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            while let Some((end, last)) = self.element() {
                self.pos = end;
                if last {
                    let unit: Vec<u8> = self.buf.drain(..end).collect();
                    self.pos = 0;
                    return Some(Ok(unit));
                }
            }
            if self.eof {
                self.pos = 0;
                return (!self.buf.is_empty()).then(|| Ok(std::mem::take(&mut self.buf)));
            }
            match self.inner.next() {
                Some(Ok(p)) => self.buf.extend_from_slice(&p),
                Some(Err(e)) => return Some(Err(e)),
                None => self.eof = true,
            }
        }
    }
}

// ---------------------------------------------------------------- MPEG-TS demuxing

/// 33-bit PTS/DTS made continuous across wraps.
#[derive(Default)]
struct Unwrap {
    last: Option<i64>,
    off: i64,
}

impl Unwrap {
    fn fix(&mut self, t: i64) -> i64 {
        let mut v = t + self.off;
        if let Some(l) = self.last {
            if v - l < -(1 << 32) {
                self.off += 1 << 33;
                v += 1 << 33;
            } else if v - l > 1 << 32 {
                self.off -= 1 << 33;
                v -= 1 << 33;
            }
        }
        self.last = Some(v);
        v
    }
}

/// An elementary-stream sample in the 90 kHz clock. Audio samples are single frames.
struct Sample {
    audio: bool,
    data: Vec<u8>,
    dts: i64,
    pts: i64,
    /// Audio: the MP4 sample entry and sample rate, and the frame's length in samples.
    entry: Option<(Vec<u8>, u32)>,
    dur: u32,
    /// Video: the MPEG-TS stream type (0x1b H.264, 0x24 H.265, 0x01 / 0x02 MPEG-1 / 2); 0 for audio.
    codec: u8,
}

#[derive(Default)]
struct Ts {
    pmt: Option<u16>,
    video: Option<(u16, u8)>,
    audio: Option<(u16, u8)>,
    pes: HashMap<u16, Vec<u8>>,
    carry: Vec<u8>,
    uv: (Unwrap, Unwrap),
    ua: Unwrap,
    /// Opus channel_config_code (from its extension descriptor).
    opus: u8,
}

/// Opus in MPEG-TS (ETSI TS 102 366 annex... carried as stream type 0x06 with an "Opus" registration
/// descriptor); a private code for it here.
const OPUS: u8 = 0xd0;

fn pts_at(b: &[u8]) -> i64 {
    ((b[0] as i64 >> 1) & 7) << 30 | (b[1] as i64) << 22 | (b[2] as i64 >> 1) << 15 | (b[3] as i64) << 7 | b[4] as i64 >> 1
}

enum Pes {
    Video { pts: i64, dts: i64, data: Vec<u8> },
    Audio { pts: Option<i64>, data: Vec<u8> },
}

impl Ts {
    fn feed(&mut self, piece: &[u8], out: &mut Vec<Pes>) {
        let mut d = std::mem::take(&mut self.carry);
        d.extend_from_slice(piece);
        let mut at = 0;
        while at + 188 <= d.len() {
            if d[at] != 0x47 {
                at += 1; // resync
                continue;
            }
            self.packet(&d[at..at + 188], out);
            at += 188;
        }
        self.carry = d[at..].to_vec();
    }

    fn packet(&mut self, p: &[u8], out: &mut Vec<Pes>) {
        let pusi = p[1] & 0x40 != 0;
        let pid = ((p[1] as u16 & 0x1f) << 8) | p[2] as u16;
        let afc = (p[3] >> 4) & 3;
        if afc & 1 == 0 {
            return;
        }
        let mut at = 4;
        if afc & 2 != 0 {
            at += 1 + p[4] as usize;
        }
        if at >= 188 {
            return;
        }
        let pay = &p[at..];
        if pid == 0 || Some(pid) == self.pmt {
            if !pusi {
                return; // ponytail: PAT/PMT assumed to fit one packet, as they do in HLS
            }
            let s = pay.get(1 + pay[0] as usize..).unwrap_or_default();
            if s.len() < 12 {
                return;
            }
            let len = (((s[1] as usize & 0x0f) << 8) | s[2] as usize).min(s.len() - 3);
            let end = 3 + len.saturating_sub(4); // before the CRC
            if pid == 0 {
                self.pmt = s[8..end].chunks(4).find(|e| e.len() == 4 && (e[0], e[1]) != (0, 0)).map(|e| ((e[2] as u16 & 0x1f) << 8) | e[3] as u16);
            } else {
                let mut k = 12 + (((s[10] as usize & 0x0f) << 8) | s[11] as usize);
                while k + 5 <= end {
                    let (mut ty, es) = (s[k], ((s[k + 1] as u16 & 0x1f) << 8) | s[k + 2] as u16);
                    let info = (((s[k + 3] as usize & 0x0f) << 8) | s[k + 4] as usize).min(end.saturating_sub(k + 5));
                    let descs = &s[k + 5..k + 5 + info];
                    if ty == 0x06 && descs.windows(6).any(|w| w == [0x05, 4, b'O', b'p', b'u', b's']) {
                        ty = OPUS;
                        self.opus = descs.windows(4).find(|w| w[0] == 0x7f && w[2] == 0x80).map_or(2, |w| w[3]);
                    }
                    if [0x01, 0x02, 0x10, 0x1b, 0x24, 0x42, 0xea].contains(&ty) && self.video.is_none() {
                        self.video = Some((es, ty));
                    } else if [0x03, 0x04, 0x0f, 0x11, 0x81, 0x87, OPUS].contains(&ty) && self.audio.is_none() {
                        self.audio = Some((es, ty));
                    }
                    k += 5 + (((s[k + 3] as usize & 0x0f) << 8) | s[k + 4] as usize);
                }
            }
            return;
        }
        if self.video.is_some_and(|v| v.0 == pid) || self.audio.is_some_and(|a| a.0 == pid) {
            if pusi {
                self.flush(pid, out);
            }
            if let Some(b) = self.pes.get_mut(&pid) {
                b.extend_from_slice(pay);
            } else if pusi {
                self.pes.insert(pid, pay.to_vec());
            }
        }
    }

    fn flush(&mut self, pid: u16, out: &mut Vec<Pes>) {
        let Some(b) = self.pes.remove(&pid) else { return };
        if b.len() < 9 || b[..3] != [0, 0, 1] {
            return;
        }
        let flags = b[7] >> 6;
        let start = 9 + b[8] as usize;
        if start > b.len() {
            return;
        }
        let pts = (flags & 2 != 0 && b.len() >= 14).then(|| pts_at(&b[9..14]));
        let dts = (flags == 3 && b.len() >= 19).then(|| pts_at(&b[14..19]));
        let data = b[start..].to_vec();
        if self.video.is_some_and(|v| v.0 == pid) {
            let Some(p) = pts else { return };
            let d = self.uv.0.fix(dts.unwrap_or(p));
            let p = self.uv.1.fix(p).max(d);
            out.push(Pes::Video { pts: p, dts: d, data });
        } else {
            out.push(Pes::Audio { pts: pts.map(|p| self.ua.fix(p)), data });
        }
    }

    fn finish(&mut self, out: &mut Vec<Pes>) {
        let pids: Vec<u16> = self.pes.keys().copied().collect();
        for pid in pids {
            self.flush(pid, out);
        }
    }
}

/// One audio frame at the start of `b`; `len` may run past the data (wait for more).
struct Frame {
    len: usize,
    /// Bytes of header that MP4 doesn't keep (ADTS); MP3 and AC-3 samples are whole frames.
    head: usize,
    entry: Vec<u8>,
    rate: u32,
    samples: u32,
    /// A dependent E-AC-3 substream: belongs to the frame before it.
    dep: bool,
    /// E-AC-3: data rate (kb/s), fscod, bsid, acmod, lfeon; and a dependent substream's chan_loc.
    ec3: Option<[u32; 5]>,
    chan_loc: u32,
}

/// Big-endian bit fields packed into bytes.
fn pack(fields: &[(u32, u32)]) -> Vec<u8> {
    let (mut out, mut acc, mut n) = (Vec::new(), 0u64, 0u32);
    for &(v, bits) in fields {
        acc = acc << bits | (v as u64 & ((1u64 << bits) - 1));
        n += bits;
        while n >= 8 {
            out.push((acc >> (n - 8)) as u8);
            n -= 8;
        }
    }
    if n > 0 {
        out.push((acc << (8 - n)) as u8);
    }
    out
}

fn audio_entry(t: &[u8; 4], ch: u16, rate: u32, child: &[u8]) -> Vec<u8> {
    bx(t, &[&[0; 6], &1u16.to_be_bytes(), &[0; 8], &ch.to_be_bytes(), &16u16.to_be_bytes(), &[0; 4], &(rate.min(65535) << 16).to_be_bytes(), child])
}

/// AAC (ADTS), MPEG-1/2 audio layers I-III, AC-3 and E-AC-3 frame headers.
fn audio_frame(b: &[u8]) -> Option<Frame> {
    if b.len() < 8 {
        return None;
    }
    if b[0] == 0xff && b[1] & 0xf6 == 0xf0 {
        // ADTS
        let len = ((b[3] as usize & 3) << 11) | (b[4] as usize) << 3 | b[5] as usize >> 5;
        let head = if b[1] & 1 == 1 { 7 } else { 9 };
        if len < head {
            return None;
        }
        let (profile, sf, ch) = (b[2] >> 6, (b[2] >> 2) & 0x0f, ((b[2] & 1) << 2) | b[3] >> 6);
        const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
        let rate = *RATES.get(sf as usize)?;
        let asc = [((profile + 1) << 3) | sf >> 1, (sf & 1) << 7 | ch << 3];
        return Some(Frame { len, head, entry: mp4a(0x40, &asc, rate, ch as u16), rate, samples: 1024, dep: false, ec3: None, chan_loc: 0 });
    }
    if b[0] == 0xff && b[1] & 0xe0 == 0xe0 {
        // MPEG audio: version 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5; layer 3 = I, 2 = II, 1 = III
        let (ver, layer) = ((b[1] >> 3) & 3, (b[1] >> 1) & 3);
        let (bri, sri, pad) = ((b[2] >> 4) as usize, (b[2] >> 2) & 3, ((b[2] >> 1) & 1) as u32);
        if ver == 1 || layer == 0 || bri == 0 || bri == 15 || sri == 3 {
            return None;
        }
        let rate = [44100u32, 48000, 32000][sri as usize] >> [2, 0, 1, 0][ver as usize];
        const V1: [[u32; 15]; 3] = [
            [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
            [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
            [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        ];
        const V2: [[u32; 15]; 2] = [[0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160], [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256]];
        let br = 1000 * if ver == 3 { V1[layer as usize - 1][bri] } else { V2[(layer == 3) as usize][bri] };
        let (samples, len) = match (layer, ver == 3) {
            (3, _) => (384, (12 * br / rate + pad) * 4),
            (2, _) | (1, true) => (1152, 144 * br / rate + pad),
            _ => (576, 72 * br / rate + pad),
        };
        let ch = if b[3] >> 6 == 3 { 1 } else { 2 };
        let oti = if ver == 3 { 0x6b } else { 0x69 };
        return Some(Frame { len: len as usize, head: 0, entry: mp4a(oti, &[], rate, ch), rate, samples, dep: false, ec3: None, chan_loc: 0 });
    }
    if b[0] == 0x0b && b[1] == 0x77 {
        let bsid = b[5] >> 3;
        if bsid <= 10 {
            // AC-3 (ATSC A/52 5.3)
            let (fscod, frmsizecod) = ((b[4] >> 6) as usize, (b[4] & 0x3f) as usize);
            if fscod == 3 || frmsizecod > 37 {
                return None;
            }
            const BR: [u32; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];
            let br = BR[frmsizecod >> 1];
            let words = match fscod {
                0 => 2 * br,
                1 => br * 96000 / 44100 + (frmsizecod & 1) as u32,
                _ => 3 * br,
            };
            let bsmod = b[5] & 7;
            let mut r = Bits { d: b[6..].to_vec(), at: 0 };
            let acmod = r.bits(3);
            if acmod & 1 != 0 && acmod != 1 {
                r.bits(2);
            }
            if acmod & 4 != 0 {
                r.bits(2);
            }
            if acmod == 2 {
                r.bits(2);
            }
            let lfe = r.bit();
            let rate = [48000, 44100, 32000][fscod];
            let ch = [2, 1, 2, 3, 3, 4, 4, 5][acmod as usize] + lfe as u16;
            let dac3 = bx(b"dac3", &[&pack(&[(fscod as u32, 2), (bsid as u32, 5), (bsmod as u32, 3), (acmod, 3), (lfe, 1), ((frmsizecod >> 1) as u32, 5), (0, 5)])]);
            return Some(Frame { len: words as usize * 2, head: 0, entry: audio_entry(b"ac-3", ch, rate, &dac3), rate, samples: 1536, dep: false, ec3: None, chan_loc: 0 });
        }
        if bsid <= 16 {
            // E-AC-3 (ATSC A/52 E.1.2)
            let mut r = Bits { d: b[2..].to_vec(), at: 0 };
            let (strmtyp, _sub, frmsiz, fscod) = (r.bits(2), r.bits(3), r.bits(11), r.bits(2));
            let (rate, blocks) = if fscod == 3 {
                let f2 = r.bits(2);
                if f2 == 3 {
                    return None;
                }
                ([24000, 22050, 16000][f2 as usize], 6)
            } else {
                ([48000, 44100, 32000][fscod as usize], [1, 2, 3, 6][r.bits(2) as usize])
            };
            let (acmod, lfe) = (r.bits(3), r.bit());
            r.bits(5); // bsid
            r.bits(5); // dialnorm
            if r.bit() == 1 {
                r.bits(8);
            }
            if acmod == 0 {
                r.bits(5);
                if r.bit() == 1 {
                    r.bits(8);
                }
            }
            // a dependent substream's channel map (bit 0 = L ... 15 = LFE): its chan_loc is bits 5..13
            let chan_loc = if strmtyp == 1 && r.bit() == 1 { (r.bits(16) >> 2) & 0x1ff } else { 0 };
            let len = (frmsiz as usize + 1) * 2;
            let samples = blocks * 256;
            let kbps = (len as u64 * 8 * rate as u64 / samples as u64 / 1000) as u32;
            let p = [kbps, fscod.min(2), bsid as u32, acmod, lfe];
            let entry = ec3_entry(p, ec3_channels(p, 0), rate, None);
            return Some(Frame { len, head: 0, entry, rate, samples, dep: strmtyp == 1, ec3: Some(p), chan_loc });
        }
    }
    None
}

/// Channels of an E-AC-3 independent substream plus those a dependent one adds (chan_loc, ETSI TS 102 366 F.6).
fn ec3_channels(p: [u32; 5], chan_loc: u32) -> u16 {
    const ADD: [u16; 9] = [2, 2, 1, 1, 2, 2, 2, 1, 1]; // Lc/Rc, Lrs/Rrs, Cs, Ts, Lsd/Rsd, Lw/Rw, Lvh/Rvh, Cvh, LFE2
    let extra: u16 = (0..9).filter(|i| chan_loc >> (8 - i) & 1 == 1).map(|i| ADD[i as usize]).sum();
    [2, 1, 2, 3, 3, 4, 4, 5][p[3] as usize] + p[4] as u16 + extra
}

/// `ec-3` sample entry: one independent substream, and a dependent one when `dep` (its chan_loc) is given.
fn ec3_entry(p: [u32; 5], ch: u16, rate: u32, dep: Option<u32>) -> Vec<u8> {
    let [kbps, fscod, bsid, acmod, lfe] = p;
    let mut f = vec![(kbps, 13), (0, 3), (fscod, 2), (bsid, 5), (0, 1), (0, 1), (0, 3), (acmod, 3), (lfe, 1), (0, 3), (dep.is_some() as u32, 4)];
    f.push(dep.map_or((0, 1), |c| (c, 9)));
    audio_entry(b"ec-3", ch, rate, &bx(b"dec3", &[&pack(&f)]))
}

fn opus_samples(p: &[u8]) -> u32 {
    let Some(&toc) = p.first() else { return 0 };
    let c = (toc >> 3) as usize;
    let frame = match c {
        0..=11 => [480, 960, 1920, 2880][c % 4],
        12..=15 => [480, 960][c % 2],
        _ => [120, 240, 480, 960][c % 4],
    };
    frame * match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => p.get(1).map_or(1, |b| (b & 0x3f) as u32),
    }
}

/// `Opus` sample entry with its dOps box, for an MPEG-TS channel_config_code (1-8 channels, Vorbis order).
fn opus_entry(code: u8) -> Vec<u8> {
    let ch = if (1..=8).contains(&code) { code } else { 2 };
    let mut d = vec![0, ch, 0, 0];
    d.extend(48000u32.to_be_bytes());
    d.extend([0, 0]);
    if ch > 2 {
        const STREAMS: [(u8, u8); 6] = [(2, 1), (2, 2), (3, 2), (4, 2), (5, 2), (5, 3)];
        const MAP: [&[u8]; 6] = [&[0, 2, 1], &[0, 1, 2, 3], &[0, 4, 1, 2, 3], &[0, 4, 1, 2, 3, 5], &[0, 4, 1, 2, 3, 5, 6], &[0, 6, 1, 2, 3, 4, 5, 7]];
        let (st, coupled) = STREAMS[ch as usize - 3];
        d.extend([1, st, coupled]);
        d.extend_from_slice(MAP[ch as usize - 3]);
    } else {
        d.push(0);
    }
    audio_entry(b"Opus", ch as u16, 48000, &bx(b"dOps", &[&d]))
}

/// Audio frames timed from the first timestamp seen, frame by frame.
#[derive(Default)]
struct Audio {
    buf: Vec<u8>,
    base: Option<i64>,
    /// Samples so far and their rate.
    done: u64,
    rate: u32,
    /// The last independent E-AC-3 frame's parameters and rate.
    ec3: Option<([u32; 5], u32)>,
}

impl Audio {
    fn push(&mut self, pts: Option<i64>, data: &[u8], out: &mut VecDeque<Sample>) {
        if self.base.is_none() {
            self.base = pts;
        }
        self.buf.extend_from_slice(data);
        let mut at = 0;
        while at + 8 <= self.buf.len() {
            let Some(f) = audio_frame(&self.buf[at..]) else {
                at += 1;
                continue;
            };
            if at + f.len > self.buf.len() {
                break;
            }
            let body = self.buf[at + f.head..at + f.len].to_vec();
            at += f.len;
            if f.dep {
                if let Some(s) = out.back_mut().filter(|s| s.audio) {
                    s.data.extend(body);
                    // the stream has a dependent substream (7.1): describe it in the sample entry
                    if let (Some((p, rate)), Some(e), Some(d)) = (self.ec3, s.entry.as_mut(), f.ec3) {
                        let p = [p[0] + d[0], p[1], p[2], p[3], p[4]];
                        e.0 = ec3_entry(p, ec3_channels(p, f.chan_loc), rate, Some(f.chan_loc));
                    }
                }
                continue;
            }
            self.ec3 = f.ec3.map(|p| (p, f.rate));
            if self.rate != f.rate {
                // rate change (or the first frame): restart the count from the time reached so far
                self.base = Some(self.base.unwrap_or(0) + (self.done * 90000 / self.rate.max(1) as u64) as i64 * (self.rate != 0) as i64);
                (self.done, self.rate) = (0, f.rate);
            }
            let t = self.base.unwrap_or(0) + (self.done * 90000 / f.rate as u64) as i64;
            out.push_back(Sample { audio: true, data: body, dts: t, pts: t, entry: Some((f.entry, f.rate)), dur: f.samples, codec: 0 });
            self.done += f.samples as u64;
        }
        self.buf.drain(..at);
    }

    /// Opus access units, each behind its control header (0x7FE0 prefix, size as 255-runs, trims, extension).
    fn push_opus(&mut self, pts: Option<i64>, d: &[u8], code: u8, out: &mut VecDeque<Sample>) {
        if self.base.is_none() {
            self.base = pts;
        }
        let mut at = 0;
        while at + 2 <= d.len() && d[at] == 0x7f && d[at + 1] & 0xe0 == 0xe0 {
            let flags = d[at + 1];
            at += 2;
            let mut size = 0;
            while let Some(&b) = d.get(at) {
                at += 1;
                size += b as usize;
                if b != 255 {
                    break;
                }
            }
            at += 2 * ((flags & 0x10 != 0) as usize + (flags & 0x08 != 0) as usize);
            if flags & 0x04 != 0 {
                at += 1 + *d.get(at).unwrap_or(&0) as usize;
            }
            let Some(pkt) = d.get(at..at + size) else { break };
            at += size;
            let n = opus_samples(pkt);
            let t = self.base.unwrap_or(0) + (self.done * 90000 / 48000) as i64;
            out.push_back(Sample { audio: true, data: pkt.to_vec(), dts: t, pts: t, entry: Some((opus_entry(code), 48000)), dur: n, codec: 0 });
            self.done += n as u64;
        }
    }
}

/// Packed audio (HLS .aac / .mp3 / .ac3 segments): an ID3 tag, whose PRIV frame may carry the 90 kHz
/// timestamp, then the frames.
fn strip_id3(p: &[u8]) -> (Option<i64>, &[u8]) {
    if p.len() < 10 || &p[..3] != b"ID3" {
        return (None, p);
    }
    let size = p[6..10].iter().fold(0usize, |n, b| n << 7 | (*b as usize & 0x7f));
    let end = (10 + size + if p[5] & 0x10 != 0 { 10 } else { 0 }).min(p.len());
    const OWNER: &[u8] = b"com.apple.streaming.transportStreamTimestamp\0";
    let ts = p[10..end].windows(OWNER.len()).position(|w| w == OWNER).and_then(|k| {
        let v = p.get(10 + k + OWNER.len()..10 + k + OWNER.len() + 8)?;
        Some((u64::from_be_bytes(v.try_into().ok()?) & ((1 << 33) - 1)) as i64)
    });
    (ts, &p[end..])
}

struct Src<'a> {
    pieces: Pieces<'a>,
    ts: Ts,
    adts: Audio,
    q: VecDeque<Sample>,
    video: bool,
    audio: bool,
    done: bool,
}

impl Src<'_> {
    fn fill(&mut self) -> Result<(), String> {
        while self.q.is_empty() && !self.done {
            let mut pes = Vec::new();
            match self.pieces.next() {
                Some(p) => {
                    let p = p?;
                    if p.first() == Some(&0x47) {
                        self.ts.feed(&p, &mut pes);
                        let bad = |s: Option<(u16, u8)>, ok: &[u8]| s.is_some_and(|s| !ok.contains(&s.1));
                        // H.264, H.265, MPEG-1/2 video; AAC, MPEG-1/2 audio, AC-3, E-AC-3, Opus
                        if (self.video && bad(self.ts.video, &[0x1b, 0x24, 0x01, 0x02])) || (self.audio && bad(self.ts.audio, &[0x0f, 0x03, 0x04, 0x81, 0x87, OPUS])) {
                            return Err(UNSUPPORTED.into());
                        }
                    } else if self.video {
                        return Err(UNSUPPORTED.into()); // not MPEG-TS
                    } else {
                        let (ts, rest) = strip_id3(&p);
                        if rest.len() >= 8 && audio_frame(rest).is_none() {
                            return Err(UNSUPPORTED.into()); // not AAC, MP3 or AC-3
                        }
                        self.adts.push(ts.map(|t| self.ts.ua.fix(t)), rest, &mut self.q);
                    }
                }
                None => {
                    self.ts.finish(&mut pes);
                    self.done = true;
                }
            }
            for e in pes {
                match e {
                    Pes::Video { pts, dts, data } if self.video => {
                        let codec = self.ts.video.map_or(0x1b, |v| v.1);
                        self.q.push_back(Sample { audio: false, data, dts, pts, entry: None, dur: 0, codec })
                    }
                    Pes::Audio { pts, data } if self.audio && self.ts.audio.is_some_and(|a| a.1 == OPUS) => self.adts.push_opus(pts, &data, self.ts.opus, &mut self.q),
                    Pes::Audio { pts, data } if self.audio => self.adts.push(pts, &data, &mut self.q),
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- H.264

/// NAL units of an Annex B access unit.
fn nals(d: &[u8]) -> Vec<&[u8]> {
    let (mut v, mut i, mut start) = (Vec::new(), 0, None);
    let trim = |n: &[u8]| -> usize { n.len() - n.iter().rev().take_while(|b| **b == 0).count() };
    while i + 3 <= d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            if let Some(s) = start {
                let n = &d[s..i];
                v.push(&n[..trim(n)]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        let n = &d[s..];
        v.push(&n[..trim(n)]);
    }
    v.retain(|n| !n.is_empty());
    v
}

struct Bits {
    d: Vec<u8>,
    at: usize,
}

impl Bits {
    /// RBSP of a NAL (emulation-prevention bytes removed), after the header byte.
    fn new(nal: &[u8]) -> Bits {
        let mut d = Vec::with_capacity(nal.len());
        for (k, b) in nal.iter().enumerate().skip(1) {
            if *b == 3 && k >= 3 && nal[k - 1] == 0 && nal[k - 2] == 0 && d.len() >= 2 && d[d.len() - 1] == 0 && d[d.len() - 2] == 0 {
                continue;
            }
            d.push(*b);
        }
        Bits { d, at: 0 }
    }
    fn bit(&mut self) -> u32 {
        let b = self.d.get(self.at / 8).map_or(0, |b| (b >> (7 - self.at % 8)) & 1);
        self.at += 1;
        b as u32
    }
    fn bits(&mut self, n: u32) -> u32 {
        (0..n).fold(0, |v, _| v << 1 | self.bit())
    }
    fn ue(&mut self) -> u32 {
        let mut z = 0;
        while self.bit() == 0 && z < 32 {
            z += 1;
        }
        ((1u64 << z) - 1 + self.bits(z) as u64) as u32
    }
    fn se(&mut self) -> i32 {
        let v = self.ue() as i64;
        (if v % 2 == 1 { (v + 1) / 2 } else { -(v / 2) }) as i32
    }
}

/// Width and height from an H.264 SPS (ITU-T H.264 7.3.2.1.1).
fn sps_size(sps: &[u8]) -> (u16, u16) {
    let mut b = Bits::new(sps);
    let profile = b.bits(8);
    b.bits(16);
    b.ue();
    let mut chroma = 1;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        chroma = b.ue();
        if chroma == 3 {
            b.bit();
        }
        b.ue();
        b.ue();
        b.bit();
        if b.bit() == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if b.bit() == 1 {
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..if i < 6 { 16 } else { 64 } {
                        if next != 0 {
                            next = (last + b.se() + 256) % 256;
                        }
                        last = if next == 0 { last } else { next };
                    }
                }
            }
        }
    }
    b.ue();
    match b.ue() {
        0 => {
            b.ue();
        }
        1 => {
            b.bit();
            b.se();
            b.se();
            for _ in 0..b.ue() {
                b.se();
            }
        }
        _ => {}
    }
    b.ue();
    b.bit();
    let w = b.ue() + 1;
    let h = b.ue() + 1;
    let frame_mbs_only = b.bit();
    if frame_mbs_only == 0 {
        b.bit();
    }
    b.bit();
    let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
    if b.bit() == 1 {
        (cl, cr, ct, cb) = (b.ue(), b.ue(), b.ue(), b.ue());
    }
    let (sx, sy) = match chroma {
        0 => (1, 2 - frame_mbs_only),
        1 => (2, 2 * (2 - frame_mbs_only)),
        2 => (2, 2 - frame_mbs_only),
        _ => (1, 2 - frame_mbs_only),
    };
    let width = (w * 16).saturating_sub((cl + cr) * sx);
    let height = ((2 - frame_mbs_only) * h * 16).saturating_sub((ct + cb) * sy);
    (width.min(u16::MAX as u32) as u16, height.min(u16::MAX as u32) as u16)
}

// ---------------------------------------------------------------- H.265

/// What an H.265 SPS says (ITU-T H.265 7.3.2.2): size, the 12-byte general profile/tier/level, chroma format,
/// bit depths, sub-layers and temporal-id nesting.
struct HevcSps {
    size: (u16, u16),
    ptl: Vec<u8>,
    chroma: u32,
    depth: (u32, u32),
    layers: u32,
    nested: u32,
}

fn hevc_sps(sps: &[u8]) -> HevcSps {
    let mut b = Bits::new(&sps[1..]); // after the 2-byte NAL header
    b.bits(4);
    let msl = b.bits(3);
    let nested = b.bit();
    let ptl: Vec<u8> = (0..12).map(|_| b.bits(8) as u8).collect();
    let flags: Vec<(u32, u32)> = (0..msl).map(|_| (b.bit(), b.bit())).collect();
    if msl > 0 {
        for _ in msl..8 {
            b.bits(2);
        }
    }
    for (profile, level) in flags {
        if profile == 1 {
            b.bits(32);
            b.bits(32);
            b.bits(24);
        }
        if level == 1 {
            b.bits(8);
        }
    }
    b.ue();
    let chroma = b.ue();
    if chroma == 3 {
        b.bit();
    }
    let (mut w, mut h) = (b.ue(), b.ue());
    if b.bit() == 1 {
        let (l, r, t, bo) = (b.ue(), b.ue(), b.ue(), b.ue());
        let sw = if chroma == 1 || chroma == 2 { 2 } else { 1 };
        let sh = if chroma == 1 { 2 } else { 1 };
        w = w.saturating_sub(sw * (l + r));
        h = h.saturating_sub(sh * (t + bo));
    }
    let depth = (b.ue(), b.ue());
    HevcSps { size: (w.min(65535) as u16, h.min(65535) as u16), ptl, chroma, depth, layers: msl + 1, nested }
}

fn visual(t: &[u8; 4], (w, h): (u16, u16), cfg: &[u8]) -> Vec<u8> {
    bx(t, &[&[0; 6], &1u16.to_be_bytes(), &[0; 16], &w.to_be_bytes(), &h.to_be_bytes(), &0x0048_0000u32.to_be_bytes(), &0x0048_0000u32.to_be_bytes(), &[0; 4], &1u16.to_be_bytes(), &[0; 32], &0x18u16.to_be_bytes(), &0xffffu16.to_be_bytes(), cfg])
}

/// `hvc1` sample entry with its HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 8.3.3).
fn hvc1(vps: &[u8], sps: &[u8], pps: &[u8]) -> (Vec<u8>, (u16, u16)) {
    let s = hevc_sps(sps);
    let mut c = vec![1];
    c.extend_from_slice(&s.ptl);
    c.extend([0xf0, 0x00, 0xfc, 0xfc | s.chroma as u8, 0xf8 | s.depth.0 as u8, 0xf8 | s.depth.1 as u8, 0, 0]);
    c.push(((s.layers & 7) << 3 | (s.nested & 1) << 2 | 3) as u8);
    c.push(3);
    for (ty, nal) in [(32u8, vps), (33, sps), (34, pps)] {
        c.push(0x80 | ty);
        c.extend(1u16.to_be_bytes());
        c.extend((nal.len() as u16).to_be_bytes());
        c.extend_from_slice(nal);
    }
    (visual(b"hvc1", s.size, &bx(b"hvcC", &[&c])), s.size)
}

// ---------------------------------------------------------------- MPEG-1/2 video

fn start_codes(d: &[u8]) -> impl Iterator<Item = usize> + '_ {
    (0..d.len().saturating_sub(3)).filter(move |&i| d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1)
}

/// The sequence header with its extensions (the decoder config) and the picture size.
fn mpeg2_seq(d: &[u8]) -> Option<(Vec<u8>, (u16, u16))> {
    let s = start_codes(d).find(|&i| d.get(i + 3) == Some(&0xb3))?;
    let (a, b, c) = (*d.get(s + 4)? as u16, *d.get(s + 5)? as u16, *d.get(s + 6)? as u16);
    let end = start_codes(d).filter(|&i| i > s).find(|&i| d.get(i + 3) != Some(&0xb5)).unwrap_or(d.len());
    Some((d[s..end].to_vec(), (a << 4 | b >> 4, (b & 0x0f) << 8 | c)))
}

/// picture_coding_type of the first picture: 1 = I, 2 = P, 3 = B.
fn mpeg2_picture(d: &[u8]) -> Option<u8> {
    let i = start_codes(d).find(|&i| d.get(i + 3) == Some(&0x00))?;
    d.get(i + 5).map(|b| (b >> 3) & 7)
}

// ---------------------------------------------------------------- MP4 writing

fn bx(t: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let body = parts.concat();
    [&((body.len() + 8) as u32).to_be_bytes()[..], t, &body].concat()
}

fn full(t: &[u8; 4], flags: u32, parts: &[&[u8]]) -> Vec<u8> {
    bx(t, &[&flags.to_be_bytes(), &parts.concat()])
}

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

fn avc1(sps: &[u8], pps: &[u8], size: (u16, u16)) -> Vec<u8> {
    let avcc = bx(b"avcC", &[&[1, sps[1], sps[2], sps[3], 0xff, 0xe1], &(sps.len() as u16).to_be_bytes(), sps, &[1], &(pps.len() as u16).to_be_bytes(), pps]);
    visual(b"avc1", size, &avcc)
}

/// `esds` box: object type `oti`, stream type byte `st` (0x15 audio, 0x11 visual), decoder-specific info.
fn esds(oti: u8, st: u8, dsi: &[u8]) -> Vec<u8> {
    let desc = |tag: u8, body: &[u8]| {
        let n = body.len() as u32;
        [&[tag, 0x80 | (n >> 21) as u8 & 0x7f, 0x80 | (n >> 14) as u8 & 0x7f, 0x80 | (n >> 7) as u8 & 0x7f, n as u8 & 0x7f][..], body].concat()
    };
    let dsi = if dsi.is_empty() { Vec::new() } else { desc(0x05, dsi) };
    let dcd = desc(0x04, &[&[oti, st, 0, 0, 0][..], &[0; 8], &dsi].concat());
    full(b"esds", 0, &[&desc(0x03, &[&[0, 0, 0][..], &dcd, &desc(0x06, &[2])].concat())])
}

/// `mp4a`: object type 0x40 = AAC (with its AudioSpecificConfig), 0x6b / 0x69 = MPEG-1 / 2 audio.
fn mp4a(oti: u8, asc: &[u8], rate: u32, ch: u16) -> Vec<u8> {
    audio_entry(b"mp4a", ch, rate, &esds(oti, 0x15, asc))
}

struct Mp4 {
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
    fn create(out: &Path) -> Result<Mp4, String> {
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

    fn add(&mut self, s: Sample) -> Result<(), String> {
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

    fn finish(mut self) -> Result<bool, String> {
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

/// HLS MPEG-TS video (H.264 / H.265 / MPEG-2, with its own audio or a separate rendition: TS or packed audio) -> MP4.
pub fn ts_to_mp4<'a>(video: Pieces<'a>, audio: Option<Pieces<'a>>, out: &Path) -> Result<bool, String> {
    let r = (|| {
        let mut w = Mp4::create(out)?;
        let sep = audio.is_some();
        let src = |pieces, video, audio| Src { pieces, ts: Ts::default(), adts: Audio::default(), q: VecDeque::new(), video, audio, done: false };
        let mut srcs = vec![src(video, true, !sep)];
        if let Some(a) = audio {
            srcs.push(src(a, false, true));
        }
        loop {
            for s in &mut srcs {
                s.fill()?;
            }
            // interleave by time
            let Some(k) = (0..srcs.len()).filter(|k| !srcs[*k].q.is_empty()).min_by_key(|k| srcs[*k].q[0].dts) else { break };
            let s = srcs[k].q.pop_front().unwrap();
            w.add(s)?;
        }
        w.finish()
    })();
    match r {
        Ok(true) => Ok(true),
        Err(e) if e != UNSUPPORTED => {
            let _ = fs::remove_file(out);
            Err(e)
        }
        _ => {
            let _ = fs::remove_file(out);
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------- WebM

const EBML: u32 = 0x1a45_dfa3;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_a966;
const TRACKS: u32 = 0x1654_ae6b;
const CLUSTER: u32 = 0x1f43_b675;

/// (value without the length marker, length, all ones = unknown size)
fn vint(d: &[u8], at: usize) -> Option<(u64, usize, bool)> {
    let b = *d.get(at)?;
    let len = b.leading_zeros() as usize + 1;
    if len > 8 || at + len > d.len() {
        return None;
    }
    let mask = (0xffu16 >> len) as u8;
    let (mut v, mut ones) = ((b & mask) as u64, b & mask == mask);
    for k in 1..len {
        v = v << 8 | d[at + k] as u64;
        ones &= d[at + k] == 0xff;
    }
    Some((v, len, ones))
}

fn ebml_id(d: &[u8], at: usize) -> Option<(u32, usize)> {
    let len = d.get(at)?.leading_zeros() as usize + 1;
    (len <= 4 && at + len <= d.len()).then(|| (d[at..at + len].iter().fold(0u32, |v, b| v << 8 | *b as u32), len))
}

/// Elements in d[from..to]: (id, start, data, end). Unknown-size elements run to the end, except
/// clusters (live WebM), which end at the first element that can't be inside a cluster.
fn elements(d: &[u8], from: usize, to: usize, descend: bool) -> Vec<(u32, usize, usize, usize)> {
    let (mut v, mut at) = (Vec::new(), from);
    while at < to {
        let Some((id, il)) = ebml_id(d, at) else { break };
        let Some((size, sl, unknown)) = vint(d, at + il) else { break };
        let data = at + il + sl;
        if descend && id == SEGMENT {
            at = data;
            continue;
        }
        let end = if !unknown {
            data.saturating_add(size as usize).min(to)
        } else if id == CLUSTER {
            let mut e = data;
            while let Some((_, cl)) = ebml_id(d, e).filter(|c| [0xe7, 0xa3, 0xa0, 0xa7, 0xab].contains(&c.0)) {
                let Some((s, l, _)) = vint(d, e + cl) else { break };
                e = (e + cl + l).saturating_add(s as usize).min(to);
            }
            e
        } else {
            to
        };
        v.push((id, at, data, end));
        at = end;
    }
    v
}

fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0, |v, b| v << 8 | *b as u64)
}

fn child(d: &[u8], e: (u32, usize, usize, usize), id: u32) -> Option<(u32, usize, usize, usize)> {
    elements(d, e.2, e.3, false).into_iter().find(|c| c.0 == id)
}

fn element(id: u32, body: &[u8]) -> Vec<u8> {
    let idb: Vec<u8> = id.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
    let mut size = (body.len() as u64).to_be_bytes();
    size[0] = 0x01; // 8-byte size
    [&idb[..], &size, body].concat()
}

#[derive(Default)]
struct WebmSrc {
    ebml: Option<Vec<u8>>,
    info: Option<Vec<u8>>,
    tracks: Option<Vec<u8>>,
    clusters: VecDeque<(u64, Vec<u8>)>,
}

impl WebmSrc {
    fn feed(&mut self, d: &[u8], renumber: Option<u8>) {
        for e in elements(d, 0, d.len(), true) {
            let bytes = || d[e.1..e.3].to_vec();
            match e.0 {
                EBML => {
                    let _ = self.ebml.get_or_insert_with(bytes);
                }
                INFO => {
                    let _ = self.info.get_or_insert_with(bytes);
                }
                TRACKS => {
                    let _ = self.tracks.get_or_insert_with(bytes);
                }
                CLUSTER => {
                    let tc = child(d, e, 0xe7).map_or(0, |c| uint(&d[c.2..c.3]));
                    let mut body = d[e.2..e.3].to_vec();
                    if let Some(n) = renumber {
                        for c in elements(d, e.2, e.3, false) {
                            let block = match c.0 {
                                0xa3 => Some(c.2),
                                0xa0 => child(d, c, 0xa1).map(|b| b.2),
                                _ => None,
                            };
                            if let Some(at) = block.filter(|at| d.get(*at).is_some_and(|b| b & 0x80 != 0)) {
                                body[at - e.2] = 0x80 | n; // 1-byte track number
                            }
                        }
                    }
                    self.clusters.push_back((tc, element(CLUSTER, &body)));
                }
                _ => {}
            }
        }
    }
}

fn timecode_scale(info: &[u8]) -> u64 {
    elements(info, 0, info.len(), false).first().and_then(|i| child(info, *i, 0x2a_d7b1)).map_or(1_000_000, |c| uint(&info[c.2..c.3]))
}

/// DASH WebM video + WebM audio -> one WebM: the video's header, both tracks (audio renumbered), the
/// clusters interleaved by time. Cues and seek heads are left out (players seek without them).
pub fn webm_merge(v: Pieces, a: Pieces, out: &Path) -> Result<bool, String> {
    let (mut vs, mut au) = (WebmSrc::default(), WebmSrc::default());
    let vp = v;
    let Some(first) = vp.next() else { return Ok(false) };
    vs.feed(&first?, None);
    let (Some(vt), Some(vi), Some(eb)) = (vs.tracks.clone(), vs.info.clone(), vs.ebml.clone()) else { return Ok(false) };
    let ve = elements(&vt, 0, vt.len(), false)[0];
    let entries = elements(&vt, ve.2, ve.3, false);
    let num = entries.iter().filter(|e| e.0 == 0xae).filter_map(|e| child(&vt, *e, 0xd7).map(|n| uint(&vt[n.2..n.3]))).max().unwrap_or(1);
    let n = (num + 1) as u8;
    if n >= 127 {
        return Ok(false);
    }
    let ap = a;
    let Some(afirst) = ap.next() else { return Ok(false) };
    au.feed(&afirst?, Some(n));
    let (Some(at), Some(ai)) = (au.tracks.clone(), au.info.clone()) else { return Ok(false) };
    if timecode_scale(&vi) != timecode_scale(&ai) {
        return Ok(false);
    }
    let ae = elements(&at, 0, at.len(), false)[0];
    let mut audio_entries = Vec::new();
    for e in elements(&at, ae.2, ae.3, false).into_iter().filter(|e| e.0 == 0xae) {
        let mut body = Vec::new();
        for c in elements(&at, e.2, e.3, false) {
            match c.0 {
                0xd7 => body.extend([0xd7, 0x81, n]),
                0x73c5 => body.extend([&[0x73, 0xc5, 0x88][..], &(uint(&at[c.2..c.3]) ^ 0x5a5a_5a5a_5a5a_5a5a).to_be_bytes()].concat()),
                _ => body.extend_from_slice(&at[c.1..c.3]),
            }
        }
        audio_entries.extend(element(0xae, &body));
    }
    let tracks = element(TRACKS, &[&vt[ve.2..ve.3], &audio_entries[..]].concat());
    let mut f = BufWriter::new(File::create(out).map_err(io)?);
    let r = (|| {
        f.write_all(&eb).map_err(io)?;
        f.write_all(&[0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).map_err(io)?; // Segment, unknown size
        f.write_all(&vi).map_err(io)?;
        f.write_all(&tracks).map_err(io)?;
        loop {
            while vs.clusters.is_empty() {
                match vp.next() {
                    Some(p) => vs.feed(&p?, None),
                    None => break,
                }
            }
            while au.clusters.is_empty() {
                match ap.next() {
                    Some(p) => au.feed(&p?, Some(n)),
                    None => break,
                }
            }
            let c = match (vs.clusters.front(), au.clusters.front()) {
                (None, None) => break,
                (Some(x), Some(y)) if x.0 <= y.0 => vs.clusters.pop_front(),
                (Some(_), None) => vs.clusters.pop_front(),
                _ => au.clusters.pop_front(),
            };
            f.write_all(&c.unwrap().1).map_err(io)?;
        }
        f.flush().map_err(io)
    })();
    if let Err(e) = r {
        drop(f);
        let _ = fs::remove_file(out);
        return Err(e);
    }
    Ok(true)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::stream::{boxes, find};

    /// A 640x480 baseline SPS, written bit by bit.
    pub fn sps_640x480() -> Vec<u8> {
        let mut bits = String::new();
        let ue = |v: u32, s: &mut String| {
            let x = format!("{:b}", v + 1);
            s.push_str(&"0".repeat(x.len() - 1));
            s.push_str(&x);
        };
        bits += &format!("{:08b}{:08b}{:08b}", 66, 0xc0, 30);
        for v in [0, 0, 0, 0, 1] {
            ue(v, &mut bits); // sps id, log2_max_frame_num-4, poc type 0, log2_max_poc_lsb-4, max refs
        }
        bits += "0";
        ue(39, &mut bits);
        ue(29, &mut bits);
        bits += "1100"; // frame_mbs_only, direct_8x8, no cropping, no VUI
        bits += "1";
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let mut v = vec![0x67];
        v.extend((0..bits.len()).step_by(8).map(|i| u8::from_str_radix(&bits[i..i + 8], 2).unwrap()));
        v
    }

    fn ts_packets(pid: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, c) in payload.chunks(184).enumerate() {
            let mut p = vec![0x47, (if k == 0 { 0x40 } else { 0 }) | (pid >> 8) as u8, pid as u8];
            if c.len() == 184 {
                p.push(0x10);
            } else {
                // pad with an adaptation field
                let fill = 184 - c.len();
                p.push(0x30);
                p.push(fill as u8 - 1);
                if fill > 1 {
                    p.push(0);
                    p.extend(vec![0xff; fill - 2]);
                }
            }
            p.extend_from_slice(c);
            out.extend(p);
        }
        out
    }

    fn pts5(t: i64, marker: u8) -> [u8; 5] {
        [marker << 4 | ((t >> 29) as u8 & 0x0e) | 1, (t >> 22) as u8, (t >> 14) as u8 | 1, (t >> 7) as u8, (t << 1) as u8 | 1]
    }

    fn pes(stream: u8, pts: i64, dts: Option<i64>, data: &[u8]) -> Vec<u8> {
        let mut h = vec![0, 0, 1, stream, 0, 0, 0x80];
        match dts {
            Some(d) => {
                h.extend([0xc0, 10]);
                h.extend(pts5(pts, 3));
                h.extend(pts5(d, 1));
            }
            None => {
                h.extend([0x80, 5]);
                h.extend(pts5(pts, 2));
            }
        }
        h.extend_from_slice(data);
        h
    }

    fn adts(n: usize) -> Vec<u8> {
        let len = 7 + n;
        let mut f = vec![0xff, 0xf1, (1 << 6) | (4 << 2), (2 << 6) | ((len >> 11) as u8 & 3), (len >> 3) as u8, ((len & 7) as u8) << 5 | 0x1f, 0xfc];
        f.extend(vec![0x21; n]);
        f
    }

    /// PAT, PMT (video `vtype` on PID 0x100, audio `atype` on 0x101).
    fn tables(vtype: u8, atype: u8) -> Vec<u8> {
        let mut pat = vec![0, 0, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0x00];
        pat.extend([0; 4]);
        let mut pmt = vec![0, 2, 0xb0, 23, 0, 1, 0xc1, 0, 0, 0xe1, 0x00, 0xf0, 0];
        pmt.extend([vtype, 0xe1, 0x00, 0xf0, 0, atype, 0xe1, 0x01, 0xf0, 0]);
        pmt.extend([0; 4]);
        [ts_packets(0, &pat), ts_packets(0x1000, &pmt)].concat()
    }

    /// Emulation-prevention bytes, as an encoder writes them.
    fn epb(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for b in rbsp {
            if out.len() >= 2 && out[out.len() - 1] == 0 && out[out.len() - 2] == 0 && *b <= 3 {
                out.push(3);
            }
            out.push(*b);
        }
        out
    }

    fn bitstring(bits: &str) -> Vec<u8> {
        let mut bits = bits.to_string() + "1";
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        (0..bits.len()).step_by(8).map(|i| u8::from_str_radix(&bits[i..i + 8], 2).unwrap()).collect()
    }

    fn ue(v: u32) -> String {
        let x = format!("{:b}", v + 1);
        "0".repeat(x.len() - 1) + &x
    }

    /// A Main-profile 1280x720 H.265 SPS (4:2:0, 8 bit), with a conformance window cropping 8 lines.
    fn hevc_sps_720() -> Vec<u8> {
        let mut b = String::from("0000") + "000" + "1"; // vps id, max_sub_layers_minus1 0, nesting
        b += "00000001"; // profile space, tier, Main
        b += &format!("{:032b}", 0x6000_0000u32);
        b += &(format!("{:08b}", 0x90) + &"0".repeat(40)); // progressive + frame-only constraint flags
        b += &format!("{:08b}", 93); // level 3.1
        b += &ue(0);
        b += &ue(1);
        b += &ue(1280);
        b += &ue(736);
        b += "1";
        b += &(ue(0) + &ue(0) + &ue(0) + &ue(8)); // 8 chroma rows = 16 luma lines off the bottom
        b += &(ue(0) + &ue(0));
        [&[0x42u8, 0x01][..], &epb(&bitstring(&b))].concat()
    }

    pub fn segment(vtype: u8, first: bool, t0: i64) -> Vec<u8> {
        let mut s = tables(vtype, 0x0f);
        let sc = [0u8, 0, 0, 1];
        for k in 0..3i64 {
            let mut au = [&sc[..], &[0x09, 0xf0]].concat();
            if k == 0 && first {
                au.extend([&sc[..], &sps_640x480(), &sc, &[0x68, 0xce, 0x38, 0x80]].concat());
            }
            au.extend([&sc[..], &[if k == 0 { 0x65 } else { 0x41 }, 0x88, 0x84, k as u8 + 1, 0x00]].concat());
            // B-frame-like reordering: pts 1 frame after dts
            s.extend(ts_packets(0x100, &pes(0xe0, t0 + (k + 1) * 3000, Some(t0 + k * 3000), &au)));
        }
        s.extend(ts_packets(0x101, &pes(0xc0, t0, None, &[adts(300), adts(5)].concat())));
        s
    }

    fn ac3(n: usize) -> Vec<u8> {
        // 48 kHz, 64 kb/s (256 bytes), bsid 8, stereo
        let mut f = vec![0x0b, 0x77, 0, 0, 0x08, 0x40, 0x40];
        f.resize(256, n as u8);
        f
    }

    fn mp3(n: u8) -> Vec<u8> {
        // MPEG-1 layer III, 128 kb/s, 44.1 kHz: 417 bytes
        let mut f = vec![0xff, 0xfb, 0x90, 0x64];
        f.resize(417, n);
        f
    }

    fn stsz(d: &[u8], trak: (usize, usize, usize, [u8; 4])) -> Vec<u32> {
        let b = find(d, trak, &[b"mdia", b"minf", b"stbl", b"stsz"]).unwrap();
        let n = u32::from_be_bytes(d[b.1 + 8..b.1 + 12].try_into().unwrap()) as usize;
        (0..n).map(|k| u32::from_be_bytes(d[b.1 + 12 + 4 * k..b.1 + 16 + 4 * k].try_into().unwrap())).collect()
    }

    #[test]
    fn ts_remuxes_to_mp4() {
        assert_eq!(sps_size(&sps_640x480()), (640, 480));
        let out = std::env::temp_dir().join(format!("ndm-ts-{}.mp4", std::process::id()));
        // two segments, the clock wrapping past 2^33 in between
        let t0 = (1i64 << 33) - 4000;
        let segs = vec![segment(0x1b, true, t0), segment(0x1b, false, (t0 + 9000) & ((1 << 33) - 1))];
        assert!(ts_to_mp4(&mut segs.into_iter().map(Ok), None, &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let top = boxes(&d, 0, d.len());
        assert_eq!(top.iter().map(|b| b.3).collect::<Vec<_>>(), [*b"ftyp", *b"mdat", *b"moov"]);
        let moov = top[2];
        let traks: Vec<_> = boxes(&d, moov.1, moov.2).into_iter().filter(|b| &b.3 == b"trak").collect();
        assert_eq!(traks.len(), 2);
        assert_eq!(stsz(&d, traks[0]), [8; 6], "6 frames, 4-byte length + 4-byte slice, AUD/SPS/PPS dropped");
        assert_eq!(stsz(&d, traks[1]), [300, 5, 300, 5], "ADTS headers stripped");
        let stts = find(&d, traks[0], &[b"mdia", b"minf", b"stbl", b"stts"]).unwrap();
        assert_eq!(&d[stts.1 + 4..stts.1 + 16], [0, 0, 0, 1, 0, 0, 0, 6, 0, 0, 0x0b, 0xb8], "3000 ticks each, across the wrap");
        let stss = find(&d, traks[0], &[b"mdia", b"minf", b"stbl", b"stss"]).unwrap();
        assert_eq!(&d[stss.1 + 4..stss.1 + 16], [0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 4]);
        let tkhd = find(&d, traks[0], &[b"tkhd"]).unwrap();
        assert_eq!(&d[tkhd.2 - 8..tkhd.2], [2, 0x80, 0, 0, 1, 0xe0, 0, 0], "640x480");
        let stsd = find(&d, traks[0], &[b"mdia", b"minf", b"stbl", b"stsd"]).unwrap();
        assert_eq!(&d[stsd.1 + 12..stsd.1 + 16], b"avc1", "after version/flags and the entry count");
        let esds = find(&d, traks[1], &[b"mdia", b"minf", b"stbl", b"stsd"]).unwrap();
        assert!(d[esds.1..esds.2].windows(2).any(|w| w == [0x12, 0x10]), "AAC LC 44.1 kHz stereo AudioSpecificConfig");
        let co = find(&d, traks[1], &[b"mdia", b"minf", b"stbl", b"co64"]).unwrap();
        let off = u64::from_be_bytes(d[co.1 + 8..co.1 + 16].try_into().unwrap()) as usize;
        assert_eq!(d[off], 0x21, "offsets point at the AAC data");
        // VC-1 isn't remuxed
        assert!(!ts_to_mp4(&mut vec![segment(0xea, true, 0)].into_iter().map(Ok), None, &out).unwrap());
        assert!(!out.exists());
    }

    fn entry_of(d: &[u8], trak: (usize, usize, usize, [u8; 4])) -> (usize, usize) {
        let stsd = find(d, trak, &[b"mdia", b"minf", b"stbl", b"stsd"]).unwrap();
        (stsd.1 + 8, stsd.2)
    }

    #[test]
    fn hevc_and_ac3_remux() {
        let sps = hevc_sps_720();
        let h = hevc_sps(&sps);
        assert_eq!(h.size, (1280, 720));
        assert_eq!((h.chroma, h.depth, h.layers, h.nested), (1, (0, 0), 1, 1));
        assert_eq!(h.ptl, [0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 93]);
        let sc = [0u8, 0, 0, 1];
        let mut s = tables(0x24, 0x81);
        for k in 0..2i64 {
            let mut au = [&sc[..], &[0x46, 0x01, 0x50]].concat(); // AUD
            if k == 0 {
                au.extend([&sc[..], &[0x40, 0x01, 0x0c, 0x01, 0xff, 0xff], &sc, &sps, &sc, &[0x44, 0x01, 0xc1, 0x72]].concat());
            }
            au.extend([&sc[..], &[if k == 0 { 0x26 } else { 0x02 }, 0x01, 0xaf, k as u8 + 1]].concat()); // IDR_W_RADL, TRAIL_R
            s.extend(ts_packets(0x100, &pes(0xe0, k * 3000, Some(k * 3000), &au)));
        }
        s.extend(ts_packets(0x101, &pes(0xbd, 0, None, &[ac3(1), ac3(2)].concat())));
        let out = std::env::temp_dir().join(format!("ndm-hevc-{}.mp4", std::process::id()));
        assert!(ts_to_mp4(&mut vec![s].into_iter().map(Ok), None, &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let top = boxes(&d, 0, d.len());
        let traks: Vec<_> = boxes(&d, top[2].1, top[2].2).into_iter().filter(|b| &b.3 == b"trak").collect();
        let (v0, v1) = entry_of(&d, traks[0]);
        assert_eq!(&d[v0 + 4..v0 + 8], b"hvc1");
        let hv = v0 + d[v0..v1].windows(4).position(|w| w == b"hvcC").unwrap() + 4;
        assert_eq!(&d[hv..hv + 13], [1, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 93]);
        assert_eq!((d[hv + 16], d[hv + 17]), (0xfd, 0xf8), "4:2:0, 8 bit");
        assert_eq!(d[hv + 21], 1 << 3 | 1 << 2 | 3, "1 temporal layer, nested, 4-byte lengths");
        assert_eq!(d[hv + 22], 3, "VPS, SPS, PPS arrays");
        assert_eq!(&d[v0 + 32..v0 + 36], [5, 0, 2, 0xd0], "1280x720 in the sample entry");
        assert_eq!(stsz(&d, traks[0]), [8, 8], "AUD / VPS / SPS / PPS kept out of the samples");
        let (a0, a1) = entry_of(&d, traks[1]);
        assert_eq!(&d[a0 + 4..a0 + 8], b"ac-3");
        assert!(d[a0..a1].windows(7).any(|w| w == [b'd', b'a', b'c', b'3', 0x10, 0x10, 0x80]));
        assert_eq!(stsz(&d, traks[1]), [256, 256]);
        let stts = find(&d, traks[1], &[b"mdia", b"minf", b"stbl", b"stts"]).unwrap();
        assert_eq!(&d[stts.1 + 8..stts.1 + 16], [0, 0, 0, 2, 0, 0, 6, 0], "1536 samples a frame");
    }

    #[test]
    fn mpeg2_and_opus_remux() {
        let sc = [0u8, 0, 0, 1];
        let seq = [&[0u8, 0, 1, 0xb3, 0x2d, 0x02, 0x40, 0x33, 0xff, 0xff, 0xe0, 0x18][..], &[0, 0, 1, 0xb5, 0x14, 0x8a, 0, 1, 0, 0]].concat();
        let pic = |pct: u8, n: u8| [&sc[1..], &[0x00, 0x00, pct << 3, 0xff, 0xf8], &sc[1..], &[0x01, 0x12, n]].concat();
        let mut s = tables(0x02, 0x0f);
        let first = [&seq[..], &[0, 0, 1, 0xb8, 0, 8, 0, 0], &pic(1, 1)].concat();
        s.extend(ts_packets(0x100, &pes(0xe0, 3000, Some(0), &first)));
        s.extend(ts_packets(0x100, &pes(0xe0, 9000, Some(3000), &pic(2, 2))));
        let out = std::env::temp_dir().join(format!("ndm-m2v-{}.mp4", std::process::id()));
        assert!(ts_to_mp4(&mut vec![s].into_iter().map(Ok), None, &out).unwrap());
        let d = fs::read(&out).unwrap();
        let top = boxes(&d, 0, d.len());
        let traks: Vec<_> = boxes(&d, top[2].1, top[2].2).into_iter().filter(|b| &b.3 == b"trak").collect();
        let (v0, v1) = entry_of(&d, traks[0]);
        assert_eq!(&d[v0 + 4..v0 + 8], b"mp4v");
        assert_eq!(&d[v0 + 32..v0 + 36], [2, 0xd0, 2, 0x40], "720x576");
        assert!(d[v0..v1].windows(2).any(|w| w == [0x61, 0x11]), "MPEG-2 Main visual object type");
        let dsi = v0 + d[v0..v1].windows(4).position(|w| w == [0, 0, 1, 0xb3]).unwrap();
        assert_eq!(&d[dsi..dsi + seq.len()], &seq[..], "sequence header + extension as the decoder config");
        assert_eq!(stsz(&d, traks[0]), [(first.len()) as u32, pic(2, 2).len() as u32]);
        let stss = find(&d, traks[0], &[b"mdia", b"minf", b"stbl", b"stss"]).unwrap();
        assert_eq!(&d[stss.1 + 4..stss.1 + 12], [0, 0, 0, 1, 0, 0, 0, 1], "only the I picture is a sync sample");

        // Opus: stream type 0x06 with an "Opus" registration and a stereo channel_config_code
        let mut pat = vec![0, 0, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0x00];
        pat.extend([0; 4]);
        let mut pmt = vec![0, 2, 0xb0, 28, 0, 1, 0xc1, 0, 0, 0xe1, 0x01, 0xf0, 0];
        pmt.extend([0x06, 0xe1, 0x01, 0xf0, 10, 0x05, 4, b'O', b'p', b'u', b's', 0x7f, 2, 0x80, 0x02]);
        pmt.extend([0; 4]);
        let au = |n: u8| vec![0x7f, 0xe0, 4, 0xf8, n, n, n]; // CELT 20 ms: 960 samples
        let mut s = [ts_packets(0, &pat), ts_packets(0x1000, &pmt)].concat();
        s.extend(ts_packets(0x101, &pes(0xbd, 0, None, &[au(1), au(2)].concat())));
        assert!(ts_to_mp4(&mut vec![s].into_iter().map(Ok), None, &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let top = boxes(&d, 0, d.len());
        let traks: Vec<_> = boxes(&d, top[2].1, top[2].2).into_iter().filter(|b| &b.3 == b"trak").collect();
        let (a0, a1) = entry_of(&d, traks[0]);
        assert_eq!(&d[a0 + 4..a0 + 8], b"Opus");
        let dops = a0 + d[a0..a1].windows(4).position(|w| w == b"dOps").unwrap() + 4;
        assert_eq!(&d[dops..dops + 11], [0, 2, 0, 0, 0, 0, 0xbb, 0x80, 0, 0, 0]);
        assert_eq!(stsz(&d, traks[0]), [4, 4]);
        let stts = find(&d, traks[0], &[b"mdia", b"minf", b"stbl", b"stts"]).unwrap();
        assert_eq!(&d[stts.1 + 8..stts.1 + 16], [0, 0, 0, 2, 0, 0, 3, 0xc0], "960 samples each");
    }

    #[test]
    fn eac3_dependent_substream() {
        // 5.1 independent substream (768 bytes) + a dependent one adding Lrs/Rrs (7.1, 256 bytes)
        let mut ind = pack(&[(0x0b77, 16), (0, 2), (0, 3), (383, 11), (0, 2), (3, 2), (7, 3), (1, 1), (16, 5), (0, 5), (0, 1)]);
        ind.resize(768, 0);
        let mut dep = pack(&[(0x0b77, 16), (1, 2), (0, 3), (127, 11), (0, 2), (3, 2), (2, 3), (0, 1), (16, 5), (0, 5), (0, 1), (1, 1), (0x0200, 16)]);
        dep.resize(256, 0);
        let mut a = Audio::default();
        let mut q = VecDeque::new();
        a.push(Some(0), &[ind, dep].concat(), &mut q);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].data.len(), 1024, "both substreams in one sample");
        let e = &q[0].entry.as_ref().unwrap().0;
        assert_eq!(&e[24..26], [0, 8], "7.1");
        let want = pack(&[(192 + 64, 13), (0, 3), (0, 2), (16, 5), (0, 1), (0, 1), (0, 3), (7, 3), (1, 1), (0, 3), (1, 4), (0x080, 9)]);
        assert!(e.windows(want.len()).any(|w| w == want), "dec3 with one dependent substream at Lrs/Rrs");
    }

    #[test]
    fn regroup_live_webm_clusters() {
        let open = |tc: u8, data: &[u8]| [&[0x1f, 0x43, 0xb6, 0x75, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..], &ebml(0xe7, &[tc]), &ebml(0xa3, &[&[0x81, 0, 0, 0x80][..], data].concat())].concat();
        let head = webm(1, &[], 1).concat();
        let w = [head.clone(), open(0, b"aa"), open(20, b"bb")].concat();
        let cut: Vec<Vec<u8>> = w.chunks(3).map(<[u8]>::to_vec).collect();
        let units: Vec<Vec<u8>> = Regroup::new(cut.into_iter().map(Ok), Kind::Webm).map(Result::unwrap).collect();
        assert_eq!(units.len(), 3, "header + tracks, then each open cluster on its own");
        assert_eq!(units[1], open(0, b"aa"));
        assert_eq!(units.concat(), w);
    }

    #[test]
    fn audio_frames() {
        let f = audio_frame(&mp3(0)).unwrap();
        assert_eq!((f.len, f.rate, f.samples, f.head), (417, 44100, 1152, 0));
        let f = audio_frame(&ac3(0)).unwrap();
        assert_eq!((f.len, f.rate, f.samples), (256, 48000, 1536));
        // E-AC-3: independent substream, 768 bytes, 48 kHz, 6 blocks, 5.1, bsid 16
        let e = pack(&[(0x0b77, 16), (0, 2), (0, 3), (383, 11), (0, 2), (3, 2), (7, 3), (1, 1), (16, 5)]);
        let mut e = e;
        e.resize(768, 0);
        let f = audio_frame(&e).unwrap();
        assert_eq!((f.len, f.rate, f.samples, f.dep), (768, 48000, 1536, false));
        assert!(f.entry.windows(4).any(|w| w == b"dec3"));
        assert_eq!(&f.entry[24..26], [0, 6], "5.1: six channels");
    }

    #[test]
    fn separate_packed_mp3() {
        let out = std::env::temp_dir().join(format!("ndm-ts-mp3-{}.mp4", std::process::id()));
        let audio = [b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec(), mp3(1), mp3(2), mp3(3)].concat();
        assert!(ts_to_mp4(&mut vec![segment(0x1b, true, 0)].into_iter().map(Ok), Some(&mut vec![audio].into_iter().map(Ok)), &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let top = boxes(&d, 0, d.len());
        let traks: Vec<_> = boxes(&d, top[2].1, top[2].2).into_iter().filter(|b| &b.3 == b"trak").collect();
        assert_eq!(stsz(&d, traks[1]), [417; 3], "whole MP3 frames, headers kept");
        let (a0, a1) = entry_of(&d, traks[1]);
        assert!(d[a0..a1].windows(2).any(|w| w == [0x6b, 0x15]), "esds object type MPEG-1 audio");
    }

    #[test]
    fn separate_packed_audio() {
        let out = std::env::temp_dir().join(format!("ndm-ts-a-{}.mp4", std::process::id()));
        let mut id3 = b"ID3\x04\x00\x00\x00\x00\x00\x3f".to_vec();
        id3.extend(b"PRIV\x00\x00\x00\x35\x00\x00com.apple.streaming.transportStreamTimestamp\0");
        id3.extend(9000u64.to_be_bytes());
        let aac = [&id3[..], &adts(10), &adts(11)].concat();
        let v = vec![segment(0x1b, true, 0)];
        assert!(ts_to_mp4(&mut v.into_iter().map(Ok), Some(&mut vec![aac].into_iter().map(Ok)), &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let top = boxes(&d, 0, d.len());
        let traks: Vec<_> = boxes(&d, top[2].1, top[2].2).into_iter().filter(|b| &b.3 == b"trak").collect();
        assert_eq!(stsz(&d, traks[1]), [10, 11], "the separate audio, not the TS's own");
        // audio starts at 9000 ticks, the video at its first pts 3000: 66 ms of empty edit first
        let elst = find(&d, traks[1], &[b"edts", b"elst"]).unwrap();
        assert_eq!(&d[elst.1 + 4..elst.1 + 16], [0, 0, 0, 2, 0, 0, 0, 66, 0xff, 0xff, 0xff, 0xff]);
    }

    fn ebml(id: u32, body: &[u8]) -> Vec<u8> {
        let idb: Vec<u8> = id.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        [&idb[..], &[0x80 | body.len() as u8], body].concat()
    }

    fn webm(track_uid: u8, clusters: &[(u8, &[u8])], number: u8) -> Vec<Vec<u8>> {
        let head = [
            ebml(EBML, &ebml(0x4282, b"webm")),
            [&[0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..], &ebml(INFO, &ebml(0x2ad7b1, &[0x0f, 0x42, 0x40])), &ebml(TRACKS, &ebml(0xae, &[ebml(0xd7, &[number]), ebml(0x73c5, &[track_uid])].concat()))].concat(),
        ]
        .concat();
        let mut v = vec![head];
        for (tc, data) in clusters {
            v.push(ebml(CLUSTER, &[ebml(0xe7, &[*tc]), ebml(0xa3, &[&[0x80 | number, 0, 0, 0x80][..], data].concat())].concat()));
        }
        v
    }

    #[test]
    fn regroup_cuts_units() {
        let mp4 = [bx(b"ftyp", &[b"isom"]), bx(b"moov", &[&[1; 20]]), bx(b"sidx", &[&[0; 4]]), bx(b"moof", &[&[2; 9]]), bx(b"mdat", &[&[3; 30]])].concat();
        let cut: Vec<Vec<u8>> = mp4.chunks(7).map(<[u8]>::to_vec).collect();
        let units: Vec<usize> = Regroup::new(&mut cut.into_iter().map(Ok), Kind::Mp4).map(|u| u.unwrap().len()).collect();
        assert_eq!(units, [12 + 28, 12 + 17 + 38]);
        let w = webm(1, &[(0, b"v0"), (40, b"v1")], 1).concat();
        let cut: Vec<Vec<u8>> = w.chunks(5).map(<[u8]>::to_vec).collect();
        let units: Vec<Vec<u8>> = Regroup::new(&mut cut.into_iter().map(Ok), Kind::Webm).map(Result::unwrap).collect();
        assert_eq!(units.len(), 3, "header + tracks, two clusters");
        assert_eq!(units.concat(), w);
    }

    #[test]
    fn webm_interleaves_and_renumbers() {
        let out = std::env::temp_dir().join(format!("ndm-webm-{}.webm", std::process::id()));
        let v = webm(1, &[(0, b"v0"), (40, b"v1")], 1);
        let a = webm(1, &[(0, b"a0"), (20, b"a1"), (60, b"a2")], 1);
        assert!(webm_merge(&mut v.into_iter().map(Ok), &mut a.into_iter().map(Ok), &out).unwrap());
        let d = fs::read(&out).unwrap();
        let _ = fs::remove_file(&out);
        let all = elements(&d, 0, d.len(), true);
        let tracks = all.iter().find(|e| e.0 == TRACKS).unwrap();
        let nums: Vec<u64> = elements(&d, tracks.2, tracks.3, false).iter().map(|e| uint(&d[child(&d, *e, 0xd7).unwrap().2..child(&d, *e, 0xd7).unwrap().3])).collect();
        assert_eq!(nums, [1, 2]);
        let blocks: Vec<(u8, Vec<u8>)> = all
            .iter()
            .filter(|e| e.0 == CLUSTER)
            .map(|e| {
                let b = child(&d, *e, 0xa3).unwrap();
                (d[b.2], d[b.2 + 4..b.3].to_vec())
            })
            .collect();
        let want: Vec<(u8, Vec<u8>)> = [(0x81, "v0"), (0x82, "a0"), (0x82, "a1"), (0x81, "v1"), (0x82, "a2")].iter().map(|(n, s)| (*n, s.as_bytes().to_vec())).collect();
        assert_eq!(blocks, want);
    }
}
