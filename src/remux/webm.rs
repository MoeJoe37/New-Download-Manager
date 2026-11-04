use super::{io, Pieces};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

pub(super) const EBML: u32 = 0x1a45_dfa3;
pub(super) const SEGMENT: u32 = 0x1853_8067;
pub(super) const INFO: u32 = 0x1549_a966;
pub(super) const TRACKS: u32 = 0x1654_ae6b;
pub(super) const CLUSTER: u32 = 0x1f43_b675;

/// (value without the length marker, length, all ones = unknown size)
pub(super) fn vint(d: &[u8], at: usize) -> Option<(u64, usize, bool)> {
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

pub(super) fn ebml_id(d: &[u8], at: usize) -> Option<(u32, usize)> {
    let len = d.get(at)?.leading_zeros() as usize + 1;
    (len <= 4 && at + len <= d.len()).then(|| (d[at..at + len].iter().fold(0u32, |v, b| v << 8 | *b as u32), len))
}

/// Elements in d[from..to]: (id, start, data, end). Unknown-size elements run to the end, except
/// clusters (live WebM), which end at the first element that can't be inside a cluster.
pub(super) fn elements(d: &[u8], from: usize, to: usize, descend: bool) -> Vec<(u32, usize, usize, usize)> {
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

pub(super) fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0, |v, b| v << 8 | *b as u64)
}

pub(super) fn child(d: &[u8], e: (u32, usize, usize, usize), id: u32) -> Option<(u32, usize, usize, usize)> {
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
