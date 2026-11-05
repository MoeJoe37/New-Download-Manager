//! Remuxing downloaded stream pieces into one file, without ffmpeg:
//! - MPEG-TS (H.264 / H.265 / MPEG-2 video with AAC, MP3, AC-3, E-AC-3 or Opus, what HLS sends) and packed
//!   audio -> one MP4, as NDM saves HLS videos;
//! - DASH WebM video + WebM audio -> one WebM.
//!   Anything else (VC-1, DTS...) answers Ok(false) and the caller keeps the pieces as they are.

mod audio;
mod bits;
mod mp4;
mod mp4_boxes;
mod regroup;
mod ts;
mod video;
mod webm;

pub use regroup::{Kind, Regroup};
pub use ts::ts_to_mp4;
pub use webm::webm_merge;

#[cfg(test)]
mod tests;

pub type Pieces<'a> = &'a mut dyn Iterator<Item = Result<Vec<u8>, String>>;

fn io(e: std::io::Error) -> String {
    e.to_string()
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
