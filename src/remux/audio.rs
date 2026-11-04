use super::bits::Bits;
use super::mp4_boxes::{audio_entry, bx, mp4a};
use super::Sample;
use std::collections::VecDeque;

/// One audio frame at the start of `b`; `len` may run past the data (wait for more).
pub(super) struct Frame {
    pub(super) len: usize,
    /// Bytes of header that MP4 doesn't keep (ADTS); MP3 and AC-3 samples are whole frames.
    pub(super) head: usize,
    pub(super) entry: Vec<u8>,
    pub(super) rate: u32,
    pub(super) samples: u32,
    /// A dependent E-AC-3 substream: belongs to the frame before it.
    pub(super) dep: bool,
    /// E-AC-3: data rate (kb/s), fscod, bsid, acmod, lfeon; and a dependent substream's chan_loc.
    ec3: Option<[u32; 5]>,
    chan_loc: u32,
}

/// Big-endian bit fields packed into bytes.
pub(super) fn pack(fields: &[(u32, u32)]) -> Vec<u8> {
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

/// AAC (ADTS), MPEG-1/2 audio layers I-III, AC-3 and E-AC-3 frame headers.
pub(super) fn audio_frame(b: &[u8]) -> Option<Frame> {
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
pub(super) struct Audio {
    buf: Vec<u8>,
    base: Option<i64>,
    /// Samples so far and their rate.
    done: u64,
    rate: u32,
    /// The last independent E-AC-3 frame's parameters and rate.
    ec3: Option<([u32; 5], u32)>,
}

impl Audio {
    pub(super) fn push(&mut self, pts: Option<i64>, data: &[u8], out: &mut VecDeque<Sample>) {
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
    pub(super) fn push_opus(&mut self, pts: Option<i64>, d: &[u8], code: u8, out: &mut VecDeque<Sample>) {
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
pub(super) fn strip_id3(p: &[u8]) -> (Option<i64>, &[u8]) {
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
