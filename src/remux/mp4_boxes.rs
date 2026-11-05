//! Shared MP4 box and sample-entry encoders.

pub(super) fn bx(t: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let body = parts.concat();
    [&((body.len() + 8) as u32).to_be_bytes()[..], t, &body].concat()
}

pub(super) fn full(t: &[u8; 4], flags: u32, parts: &[&[u8]]) -> Vec<u8> {
    bx(t, &[&flags.to_be_bytes(), &parts.concat()])
}

pub(super) fn audio_entry(t: &[u8; 4], ch: u16, rate: u32, child: &[u8]) -> Vec<u8> {
    bx(
        t,
        &[
            &[0; 6],
            &1u16.to_be_bytes(),
            &[0; 8],
            &ch.to_be_bytes(),
            &16u16.to_be_bytes(),
            &[0; 4],
            &(rate.min(65535) << 16).to_be_bytes(),
            child,
        ],
    )
}

pub(super) fn visual(t: &[u8; 4], (w, h): (u16, u16), cfg: &[u8]) -> Vec<u8> {
    bx(
        t,
        &[
            &[0; 6],
            &1u16.to_be_bytes(),
            &[0; 16],
            &w.to_be_bytes(),
            &h.to_be_bytes(),
            &0x0048_0000u32.to_be_bytes(),
            &0x0048_0000u32.to_be_bytes(),
            &[0; 4],
            &1u16.to_be_bytes(),
            &[0; 32],
            &0x18u16.to_be_bytes(),
            &0xffffu16.to_be_bytes(),
            cfg,
        ],
    )
}

pub(super) fn avc1(sps: &[u8], pps: &[u8], size: (u16, u16)) -> Vec<u8> {
    let avcc = bx(
        b"avcC",
        &[
            &[1, sps[1], sps[2], sps[3], 0xff, 0xe1],
            &(sps.len() as u16).to_be_bytes(),
            sps,
            &[1],
            &(pps.len() as u16).to_be_bytes(),
            pps,
        ],
    );
    visual(b"avc1", size, &avcc)
}

/// `esds` box: object type `oti`, stream type byte `st` (0x15 audio, 0x11 visual), decoder-specific info.
pub(super) fn esds(oti: u8, st: u8, dsi: &[u8]) -> Vec<u8> {
    let desc = |tag: u8, body: &[u8]| {
        let n = body.len() as u32;
        [
            &[
                tag,
                0x80 | (n >> 21) as u8 & 0x7f,
                0x80 | (n >> 14) as u8 & 0x7f,
                0x80 | (n >> 7) as u8 & 0x7f,
                n as u8 & 0x7f,
            ][..],
            body,
        ]
        .concat()
    };
    let dsi = if dsi.is_empty() {
        Vec::new()
    } else {
        desc(0x05, dsi)
    };
    let dcd = desc(0x04, &[&[oti, st, 0, 0, 0][..], &[0; 8], &dsi].concat());
    full(
        b"esds",
        0,
        &[&desc(
            0x03,
            &[&[0, 0, 0][..], &dcd, &desc(0x06, &[2])].concat(),
        )],
    )
}

/// `mp4a`: object type 0x40 = AAC (with its AudioSpecificConfig), 0x6b / 0x69 = MPEG-1 / 2 audio.
pub(super) fn mp4a(oti: u8, asc: &[u8], rate: u32, ch: u16) -> Vec<u8> {
    audio_entry(b"mp4a", ch, rate, &esds(oti, 0x15, asc))
}
