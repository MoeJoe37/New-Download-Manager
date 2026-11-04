use super::bits::Bits;
use super::mp4_boxes::{bx, visual};

/// NAL units of an Annex B access unit.
pub(super) fn nals(d: &[u8]) -> Vec<&[u8]> {
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

/// Width and height from an H.264 SPS (ITU-T H.264 7.3.2.1.1).
pub(super) fn sps_size(sps: &[u8]) -> (u16, u16) {
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
pub(super) struct HevcSps {
    pub(super) size: (u16, u16),
    pub(super) ptl: Vec<u8>,
    pub(super) chroma: u32,
    pub(super) depth: (u32, u32),
    pub(super) layers: u32,
    pub(super) nested: u32,
}

pub(super) fn hevc_sps(sps: &[u8]) -> HevcSps {
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

/// `hvc1` sample entry with its HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 8.3.3).
pub(super) fn hvc1(vps: &[u8], sps: &[u8], pps: &[u8]) -> (Vec<u8>, (u16, u16)) {
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
pub(super) fn mpeg2_seq(d: &[u8]) -> Option<(Vec<u8>, (u16, u16))> {
    let s = start_codes(d).find(|&i| d.get(i + 3) == Some(&0xb3))?;
    let (a, b, c) = (*d.get(s + 4)? as u16, *d.get(s + 5)? as u16, *d.get(s + 6)? as u16);
    let end = start_codes(d).filter(|&i| i > s).find(|&i| d.get(i + 3) != Some(&0xb5)).unwrap_or(d.len());
    Some((d[s..end].to_vec(), (a << 4 | b >> 4, (b & 0x0f) << 8 | c)))
}

/// picture_coding_type of the first picture: 1 = I, 2 = P, 3 = B.
pub(super) fn mpeg2_picture(d: &[u8]) -> Option<u8> {
    let i = start_codes(d).find(|&i| d.get(i + 3) == Some(&0x00))?;
    d.get(i + 5).map(|b| (b >> 3) & 7)
}
