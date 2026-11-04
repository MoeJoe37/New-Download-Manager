//! Multi-connection download engine: segmented HTTP Range downloads, dynamic
//! segmentation (idle connections split the biggest remaining segment), resume, speed limits.
//! Connections ramp up like NDM's HTTPSession::TryStartNewOptionalConnections:
//! start with one, then at most once a second, while every open connection is receiving data,
//! double the count up to the maximum; not while a speed limit is >= 85% used.
//! Resuming re-fetches the 64 bytes before the resume point and checks them against the file.
//! ftp:// downloads go through a small passive-mode client (`ftp_open`), segmented with REST.
pub(crate) mod ftp;
pub(crate) mod probe;
pub(crate) mod range;
pub(crate) mod names;
mod job;
#[cfg(test)]
mod tests;

use self::ftp::{ftp_open, is_ftp};
use self::names::{disposition_name, media_name, mime_ext, sanitize, sniff, unique_name, url_name};
use self::probe::{Probe, is_page, probe_of};
use self::range::{content_range, permanent_error, retry_delay, valid_layout, validate_http_response};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub const CHUNK: usize = 64 * 1024;
pub const MIN_SEG: u64 = 256 * 1024; // a segment is only split if at least 2x this is left
pub(crate) const RETRIES: u32 = 5;

pub const CHANGED: &str = "the file on the server has changed";

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug, Default)]
pub enum Status {
    #[default]
    Stopped,
    Connecting,
    Downloading,
    Complete,
    Error(String),
}

/// How long a queue waits before it tries a download again after `fails` failures in a row: 15 s, then twice
/// as long each time, at most 10 minutes (a server that is down or busy gets time to come back).
pub fn retry_wait(fails: u32) -> Duration {
    Duration::from_secs((15u64 << fails.saturating_sub(1).min(6)).min(600))
}

/// Byte range [start, stop). `next` is the next byte to fetch; `stop` None = length unknown.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Seg {
    pub start: u64,
    pub next: u64,
    pub stop: Option<u64>,
    #[serde(skip)]
    pub info: String,
    /// A connection is working on this segment.
    #[serde(skip)]
    pub owned: bool,
    /// The network of this segment's connection ("" = the default one).
    #[serde(skip)]
    pub net: String,
    /// Its connection's speed (bytes/s, smoothed), 0 until measured.
    #[serde(skip)]
    pub rate: f64,
    /// The network its last failed connection was on: another one gets it first.
    #[serde(skip)]
    pub failed_on: String,
}

impl Seg {
    pub fn left(&self) -> u64 {
        self.stop.map_or(0, |s| s.saturating_sub(self.next))
    }
    pub fn done(&self) -> bool {
        self.stop.is_some_and(|s| self.next >= s)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Info {
    pub id: u64,
    pub url: String,
    pub referrer: String,
    pub cookies: String,
    pub folder: String,
    pub name: String,
    pub size: Option<u64>,
    pub ranges: bool,
    pub probed: bool,
    pub status: Status,
    pub segs: Vec<Seg>,
    pub description: String,
    pub category: String,
    pub added: i64,
    pub last_try: i64,
    pub conns: usize,
    pub queued: bool,
    /// Which queue (core::queue::Queue::id) a queued download belongs to.
    pub queue: u32,
    pub limit_kb: u64,
    /// "Parent web page": the page the link was on.
    pub parent: String,
    /// Server's Last-Modified; a different one on resume means the file changed (option
    /// "Ignore file modification time changes when resuming a download").
    pub modified: String,
    pub etag: String,
    /// The server labels the file a web page (text/html): then a part labelled so is no sign of trouble.
    pub page: bool,
    /// The browser extension fetched the file inside its web page and handed it over (a file the page makes
    /// itself, as Telegram Web does): NDM can't download it again from the address.
    pub fed: bool,
    /// After the data is in: what NDM is doing with it (joining video and audio, moving the file...) and how
    /// far along, 0..1; below 0 = can't tell. Shown in the same bar as the download.
    #[serde(skip)]
    pub post: Option<(String, f32)>,
    /// Request headers the address needs besides Referer and Cookie (a stream yt-dlp found: its
    /// User-Agent and the like).
    pub headers: Vec<(String, String)>,
    /// When the step in `post` began (its time left).
    #[serde(skip)]
    pub post_since: Option<Instant>,
    /// A page whose video yt-dlp gets, on any site (the extension's download panel); pages of the sites
    /// yt-dlp is known for go to it anyway (`ytdlp::is_video_page`).
    pub ytdlp: bool,
    /// A file to fetch as it is, never handed to yt-dlp: a stream yt-dlp found for a video (its address
    /// can be on the video's own site, as TikTok's are, and would otherwise be looked up again, over and over).
    #[serde(default)]
    pub plain: bool,
    /// Temporary directory for the .part file (Options -> Save to); "" = next to the file.
    pub temp: String,
    /// An HLS / DASH stream (`stream::download::run`), also when the address doesn't say so.
    pub stream: bool,
    pub media_output: String,
    pub media_variant: Option<usize>,
    /// A Site Grabber download with "Look up files in IE cache before downloading".
    pub ie_cache: bool,
    /// The browser's own proxy when the extension handed the download over ("http://h:p", "socks5://h:p"
    /// or "pac:URL"), used after a failure with "Use proxy/socks from a browser's request".
    pub browser_proxy: String,
    /// What yt-dlp gets for a video page (File Info's Quality, Subtitles and Thumbnail).
    pub yt: YtOpts,
}

/// A video download's choices: the quality ("" best, "1080"... at most that many lines, "m4a" / "mp3"
/// only the sound), and the extras saved next to the video.
#[derive(Clone, Serialize, Deserialize, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct YtOpts {
    pub quality: String,
    pub subs: bool,
    pub thumb: bool,
    pub playlist_entry: bool,
    /// A browser's cookie jar, loaded by yt-dlp for this entry alone.
    pub browser: String,
}

impl Info {
    pub fn path(&self) -> PathBuf {
        Path::new(&self.folder).join(&self.name)
    }
    pub fn part(&self) -> PathBuf {
        if self.temp.is_empty() {
            PathBuf::from(format!("{}.part", self.path().display()))
        } else {
            Path::new(&self.temp).join(format!("{}_{}.part", self.id, self.name))
        }
    }
    pub fn done(&self) -> u64 {
        if self.status == Status::Complete {
            return self.size.unwrap_or_else(|| self.segs.iter().map(|s| s.next.saturating_sub(s.start)).fold(0, u64::saturating_add));
        }
        match self.size {
            Some(s) if !self.segs.is_empty() => s - self.segs.iter().map(Seg::left).fold(0, u64::saturating_add).min(s),
            _ => self.segs.iter().map(|s| s.next.saturating_sub(s.start)).fold(0, u64::saturating_add),
        }
    }
}

/// Token-bucket style limiter. rate in bytes/s, 0 = unlimited.
#[derive(Default)]
pub struct Limiter {
    pub rate: AtomicU64,
    /// Bytes received by every download (the app-wide limiter only); feeds the "Download limits" quota.
    pub received: AtomicU64,
    next: Mutex<Option<Instant>>,
}

impl Limiter {
    fn take(&self, n: usize, stop: &AtomicBool) {
        self.take_at(self.rate.load(Relaxed), n, stop)
    }

    fn take_at(&self, rate: u64, n: usize, stop: &AtomicBool) {
        if rate == 0 {
            return;
        }
        let until = {
            let mut next = self.next.lock().unwrap();
            let now = Instant::now();
            let t = next.filter(|t| *t > now).unwrap_or(now) + Duration::from_secs_f64(n as f64 / rate as f64);
            *next = Some(t);
            t
        };
        while !stop.load(Relaxed) && Instant::now() < until {
            thread::sleep((until - Instant::now()).min(Duration::from_millis(100)));
        }
    }
}

pub struct Job {
    info: Mutex<Info>,
    stop: AtomicBool,
    running: AtomicBool,
    pub just_completed: AtomicBool,
    /// Set when a run completes or fails (not when stopped); the core plays the event sound.
    pub ended: AtomicBool,
    pub limiter: Limiter,
    /// This file's bucket for the app-wide Speed Limiter rate: NDM limits "download speed for one file".
    global_bucket: Limiter,
    speed: Mutex<(Instant, u64, f64)>,
    /// Options set by the core before each start: give the file the server's date (Save to page), skip
    /// the Last-Modified check on resume (Downloads page).
    pub set_date: AtomicBool,
    pub ignore_date: AtomicBool,
    /// Failed runs in a row; the queue retries up to its "Number of retries" (template 306).
    pub fails: AtomicU32,
    /// When the last run failed: the queue waits longer after each failure before it tries again.
    pub failed_at: Mutex<Option<Instant>>,
    /// "Download over several network connections at once": (network, its agent), set by the core
    /// before each start. Connection c goes over lanes[c % n]; empty = the job's agent.
    pub lanes: Mutex<Vec<(String, Agent)>>,
    /// Bytes received over each network this run (the progress window's per-network line).
    pub net_bytes: Mutex<std::collections::HashMap<String, u64>>,
    /// The core may spread this run over networks as they come and go (not through the browser's proxy).
    pub multi_ok: AtomicBool,
    /// A network's most connections this run, once its server turned more away (503 / 429 while others
    /// were receiving): the connections that worked.
    caps: Mutex<std::collections::HashMap<String, usize>>,
}

type Body = Box<dyn Read + Send>;

use crate::transport::{call, request, Agent, Request, Response};
#[cfg(test)]
use crate::transport::client::agent;
use crate::transport::auth::{b64, pct_decode, strip_ci};
