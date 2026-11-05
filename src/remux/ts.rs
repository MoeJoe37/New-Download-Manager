use super::audio::{audio_frame, strip_id3, Audio};
use super::mp4::Mp4;
use super::{Pieces, Sample};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;

const UNSUPPORTED: &str = "\u{1}unsupported";

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
    ((b[0] as i64 >> 1) & 7) << 30
        | (b[1] as i64) << 22
        | (b[2] as i64 >> 1) << 15
        | (b[3] as i64) << 7
        | b[4] as i64 >> 1
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
                self.pmt = s[8..end]
                    .chunks(4)
                    .find(|e| e.len() == 4 && (e[0], e[1]) != (0, 0))
                    .map(|e| ((e[2] as u16 & 0x1f) << 8) | e[3] as u16);
            } else {
                let mut k = 12 + (((s[10] as usize & 0x0f) << 8) | s[11] as usize);
                while k + 5 <= end {
                    let (mut ty, es) = (s[k], ((s[k + 1] as u16 & 0x1f) << 8) | s[k + 2] as u16);
                    let info = (((s[k + 3] as usize & 0x0f) << 8) | s[k + 4] as usize)
                        .min(end.saturating_sub(k + 5));
                    let descs = &s[k + 5..k + 5 + info];
                    if ty == 0x06
                        && descs
                            .windows(6)
                            .any(|w| w == [0x05, 4, b'O', b'p', b'u', b's'])
                    {
                        ty = OPUS;
                        self.opus = descs
                            .windows(4)
                            .find(|w| w[0] == 0x7f && w[2] == 0x80)
                            .map_or(2, |w| w[3]);
                    }
                    if [0x01, 0x02, 0x10, 0x1b, 0x24, 0x42, 0xea].contains(&ty)
                        && self.video.is_none()
                    {
                        self.video = Some((es, ty));
                    } else if [0x03, 0x04, 0x0f, 0x11, 0x81, 0x87, OPUS].contains(&ty)
                        && self.audio.is_none()
                    {
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
        let Some(b) = self.pes.remove(&pid) else {
            return;
        };
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
            out.push(Pes::Video {
                pts: p,
                dts: d,
                data,
            });
        } else {
            out.push(Pes::Audio {
                pts: pts.map(|p| self.ua.fix(p)),
                data,
            });
        }
    }

    fn finish(&mut self, out: &mut Vec<Pes>) {
        let pids: Vec<u16> = self.pes.keys().copied().collect();
        for pid in pids {
            self.flush(pid, out);
        }
    }
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
                        let bad =
                            |s: Option<(u16, u8)>, ok: &[u8]| s.is_some_and(|s| !ok.contains(&s.1));
                        // H.264, H.265, MPEG-1/2 video; AAC, MPEG-1/2 audio, AC-3, E-AC-3, Opus
                        if (self.video && bad(self.ts.video, &[0x1b, 0x24, 0x01, 0x02]))
                            || (self.audio
                                && bad(self.ts.audio, &[0x0f, 0x03, 0x04, 0x81, 0x87, OPUS]))
                        {
                            return Err(UNSUPPORTED.into());
                        }
                    } else if self.video {
                        return Err(UNSUPPORTED.into()); // not MPEG-TS
                    } else {
                        let (ts, rest) = strip_id3(&p);
                        if rest.len() >= 8 && audio_frame(rest).is_none() {
                            return Err(UNSUPPORTED.into()); // not AAC, MP3 or AC-3
                        }
                        self.adts
                            .push(ts.map(|t| self.ts.ua.fix(t)), rest, &mut self.q);
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
                        self.q.push_back(Sample {
                            audio: false,
                            data,
                            dts,
                            pts,
                            entry: None,
                            dur: 0,
                            codec,
                        })
                    }
                    Pes::Audio { pts, data }
                        if self.audio && self.ts.audio.is_some_and(|a| a.1 == OPUS) =>
                    {
                        self.adts.push_opus(pts, &data, self.ts.opus, &mut self.q)
                    }
                    Pes::Audio { pts, data } if self.audio => {
                        self.adts.push(pts, &data, &mut self.q)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// HLS MPEG-TS video (H.264 / H.265 / MPEG-2, with its own audio or a separate rendition: TS or packed audio) -> MP4.
pub fn ts_to_mp4<'a>(
    video: Pieces<'a>,
    audio: Option<Pieces<'a>>,
    out: &Path,
) -> Result<bool, String> {
    let r = (|| {
        let mut w = Mp4::create(out)?;
        let sep = audio.is_some();
        let src = |pieces, video, audio| Src {
            pieces,
            ts: Ts::default(),
            adts: Audio::default(),
            q: VecDeque::new(),
            video,
            audio,
            done: false,
        };
        let mut srcs = vec![src(video, true, !sep)];
        if let Some(a) = audio {
            srcs.push(src(a, false, true));
        }
        loop {
            for s in &mut srcs {
                s.fill()?;
            }
            // interleave by time
            let Some(k) = (0..srcs.len())
                .filter(|k| !srcs[*k].q.is_empty())
                .min_by_key(|k| srcs[*k].q[0].dts)
            else {
                break;
            };
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
