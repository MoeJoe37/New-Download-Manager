use super::audio::{audio_frame, pack, Audio};
use super::mp4_boxes::bx;
use super::video::{hevc_sps, sps_size};
use super::webm::{child, elements, uint, CLUSTER, EBML, INFO, TRACKS};
use super::{ts_to_mp4, webm_merge, Kind, Regroup};
use crate::stream::fmp4::{boxes, find};
use std::collections::VecDeque;
use std::fs;

/// A 640x480 baseline SPS, written bit by bit.
fn sps_640x480() -> Vec<u8> {
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
    while !bits.len().is_multiple_of(8) {
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
    while !bits.len().is_multiple_of(8) {
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

fn segment(vtype: u8, first: bool, t0: i64) -> Vec<u8> {
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
