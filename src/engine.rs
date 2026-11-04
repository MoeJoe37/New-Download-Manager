//! Multi-connection download engine: segmented HTTP Range downloads, dynamic
//! segmentation (idle connections split the biggest remaining segment), resume, speed limits.
//! Connections ramp up like NDM's HTTPSession::TryStartNewOptionalConnections:
//! start with one, then at most once a second, while every open connection is receiving data,
//! double the count up to the maximum; not while a speed limit is >= 85% used.
//! Resuming re-fetches the 64 bytes before the resume point and checks them against the file.
//! ftp:// downloads go through a small passive-mode client (`ftp_open`), segmented with REST.
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
/// A connection's network left the download (`fetch`).
const LANE_GONE: &str = "network left the download";
/// A network whose connections all gave up is tried again after this long (it may come back).
const LANE_COOLDOWN: Duration = Duration::from_secs(30);
/// How often a network may double its connections: 1, 2, 4, 8, 16 within a second.
const RAMP_EVERY: Duration = Duration::from_millis(250);
/// A split hands over at least this much...
const MIN_STEAL: u64 = 64 * 1024;
/// ...and only if it saves more than twice what a new request costs before its first byte (seconds).
const STEAL_STARTUP: f64 = 1.0;
/// Bytes before a resume point that are fetched again and compared with the file.
const VERIFY: u64 = 64;
/// How long a new connection waits for the server's answer while the download's other connections
/// receive, before it gives up (the server allows fewer connections).
const NO_ANSWER: Duration = Duration::from_secs(6);
const NO_ANSWER_ERR: &str = "no answer from the server";
pub const CHANGED: &str = "the file on the server has changed";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";

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
    /// An HLS / DASH stream (`stream::run`), also when the address doesn't say so.
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

pub use crate::transport::{call, request, Agent, Request, Response};

pub struct Probe {
    pub name: String,
    pub size: Option<u64>,
    pub ranges: bool,
    pub modified: String,
    pub etag: String,
    pub stream: bool,
    /// The address is a web page (text/html): perhaps one yt-dlp knows a video on.
    pub page: bool,
    /// yt-dlp found a video at the address: it downloads it (`Info::ytdlp`).
    pub ytdlp: bool,
}

/// `old` ("a=1; b=2") with each `name=value` of `set` replacing the cookie of that name or added.
fn merge_cookies(old: &str, set: &[String]) -> String {
    let name = |c: &str| c.split('=').next().unwrap_or("").trim().to_string();
    let mut all: Vec<String> = old.split(';').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect();
    for c in set {
        all.retain(|o| name(o) != name(c));
        all.push(c.clone());
    }
    all.join("; ")
}

fn is_ftp(url: &str) -> bool {
    strip_ci(url, "ftp://").is_some()
}

/// The data connection of an FTP transfer; the control connection stays open alongside it.
struct Ftp {
    data: TcpStream,
    _ctl: BufReader<TcpStream>,
}

impl Read for Ftp {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.data.read(b)
    }
}

/// Send `cmd` (none for the greeting) and read the reply, skipping multi-line `123-` parts.
fn ftp_cmd(c: &mut BufReader<TcpStream>, cmd: &str) -> Result<(u32, String), String> {
    if !cmd.is_empty() {
        c.get_mut().write_all(format!("{cmd}\r\n").as_bytes()).map_err(|e| e.to_string())?;
    }
    let mut line = String::new();
    loop {
        line.clear();
        if c.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("FTP connection closed".into());
        }
        if let (Some(code), Some(b' ')) = (line.get(..3).and_then(|x| x.parse().ok()), line.as_bytes().get(3)) {
            return Ok((code, line[4..].trim().to_string()));
        }
    }
}

/// App-wide FTP options (Options -> Proxy / Socks): passive mode, and the route of an ftp:// address
/// (`Settings::proxy_for`: "" direct, `ftp://` FTP proxy, `http://` HTTP proxy, `socks…`).
#[derive(Clone)]
pub struct FtpCfg {
    pub pasv: bool,
    pub route: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

// ponytail: one global, as the options are app-wide; per-job copies if downloads ever need their own
pub static FTP: Mutex<FtpCfg> = Mutex::new(FtpCfg { pasv: true, route: None });

/// Log in (anonymous unless the URL has a login), binary mode, SIZE, passive (or active, PORT) data
/// connection, REST `from`, RETR. Returns (size, resumable, data). Through an FTP proxy the login is
/// `USER user@host[:port] [proxy user]`, `PASS`, then `ACCT proxy password`. Through an HTTP proxy the
/// proxy does the FTP (`ftp_http`); through SOCKS both connections go through it, always passive.
fn ftp_open(url: &str, from: u64) -> Result<(Option<u64>, bool, Body), String> {
    let cfg = FTP.lock().unwrap().clone();
    let route = cfg.route.as_ref().map(|f| f(url)).unwrap_or_default();
    if route.starts_with("http://") || route.starts_with("https://") {
        return ftp_http(url, from, &route);
    }
    let u = url::Url::parse(url).map_err(|e| e.to_string())?;
    let host = u.host_str().ok_or("no host")?.trim_matches(['[', ']']).to_string();
    let port = u.port().unwrap_or(21);
    let fp = route.strip_prefix("ftp://").and_then(|_| url::Url::parse(&route).ok());
    let socks = if fp.is_none() { route.as_str() } else { "" };
    let s = match &fp {
        Some(p) => crate::net::connect("", p.host_str().unwrap_or_default(), p.port().unwrap_or(21)),
        None => crate::net::connect(socks, &host, port),
    }
    .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    let mut c = BufReader::new(s);
    let ok = |r: (u32, String), class: u32| if r.0 / 100 == class { Ok(r) } else { Err(format!("FTP {} {}", r.0, r.1)) };
    ok(ftp_cmd(&mut c, "")?, 2)?;
    let mut user = if u.username().is_empty() { "anonymous".into() } else { pct_decode(u.username()) };
    let mut acct = String::new();
    if let Some(p) = &fp {
        user = format!("{user}@{host}{}", u.port().map_or(String::new(), |p| format!(":{p}")));
        if !p.username().is_empty() {
            user = format!("{user} {}", pct_decode(p.username()));
        }
        acct = p.password().map(pct_decode).unwrap_or_default();
    }
    let mut r = ftp_cmd(&mut c, &format!("USER {user}"))?;
    if r.0 == 331 {
        r = ftp_cmd(&mut c, &format!("PASS {}", u.password().map_or("ndm@".into(), pct_decode)))?;
    }
    if r.0 == 332 {
        r = ftp_cmd(&mut c, &format!("ACCT {acct}"))?;
    }
    ok(r, 2)?;
    ok(ftp_cmd(&mut c, "TYPE I")?, 2)?;
    let path = pct_decode(u.path());
    let size = ftp_cmd(&mut c, &format!("SIZE {path}"))?;
    let size = Some(size).filter(|r| r.0 == 213).and_then(|r| r.1.parse().ok());
    let mut listen = None;
    let mut data = None;
    // a SOCKS server can't take the server's PORT connection
    if cfg.pasv || !socks.is_empty() {
        // 227 Entering Passive Mode (h1,h2,h3,h4,p1,p2): the port; the address is the server's own
        let (_, pasv) = ok(ftp_cmd(&mut c, "PASV")?, 2)?;
        let n: Vec<u16> = pasv.split(|ch: char| !ch.is_ascii_digit()).filter_map(|x| x.parse().ok()).collect();
        let dport = match n[..] {
            [.., p1, p2] if n.len() >= 6 => p1 * 256 + p2,
            _ => return Err(format!("FTP {pasv}")),
        };
        // through SOCKS the peer is the proxy, so the data connection goes to the server's name
        let d = if socks.is_empty() {
            let ip = c.get_ref().peer_addr().map_err(|e| e.to_string())?.ip();
            TcpStream::connect((ip, dport))
        } else {
            crate::net::connect(socks, &host, dport)
        };
        data = Some(d.map_err(|e| e.to_string())?);
    } else {
        // active mode: the server connects back to us (PORT h1,h2,h3,h4,p1,p2)
        let me = c.get_ref().local_addr().map_err(|e| e.to_string())?;
        let l = std::net::TcpListener::bind((me.ip(), 0)).map_err(|e| e.to_string())?;
        let (ip, port) = (me.ip().to_string().replace('.', ","), l.local_addr().map_err(|e| e.to_string())?.port());
        ok(ftp_cmd(&mut c, &format!("PORT {ip},{},{}", port / 256, port % 256))?, 2)?;
        listen = Some(l);
    }
    let ranges = ftp_cmd(&mut c, &format!("REST {from}"))?.0 == 350;
    if from > 0 && !ranges {
        return Err("server ignored the Range request".into());
    }
    ok(ftp_cmd(&mut c, &format!("RETR {path}"))?, 1)?;
    let data = match (data, listen) {
        (Some(d), _) => d,
        (None, Some(l)) => l.accept().map_err(|e| e.to_string())?.0,
        _ => unreachable!(),
    };
    data.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    Ok((size, ranges && size.is_some(), Box::new(Ftp { data, _ctl: c })))
}

/// An ftp:// address fetched by an HTTP proxy: `GET ftp://… HTTP/1.0` (1.0, so the reply is never
/// chunked) with `Range: bytes=from-`. Returns (size, resumable, body) like `ftp_open`.
fn ftp_http(url: &str, from: u64, proxy: &str) -> Result<(Option<u64>, bool, Body), String> {
    let p = url::Url::parse(proxy).map_err(|e| e.to_string())?;
    let u = url::Url::parse(url).map_err(|e| e.to_string())?;
    let s = crate::net::connect("", p.host_str().unwrap_or_default(), p.port_or_known_default().unwrap_or(8080)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    let host = u.host_str().unwrap_or_default().to_string() + &u.port().map_or(String::new(), |p| format!(":{p}"));
    let mut head = format!("GET {url} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: {UA}\r\nRange: bytes={from}-\r\n");
    if !p.username().is_empty() {
        let login = format!("{}:{}", pct_decode(p.username()), p.password().map(pct_decode).unwrap_or_default());
        head += &format!("Proxy-Authorization: Basic {}\r\n", b64(login.as_bytes()));
    }
    head += "Connection: close\r\n\r\n";
    (&s).write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut status = line.trim_end().splitn(3, ' ').skip(1);
    let code: u32 = status.next().and_then(|c| c.parse().ok()).ok_or_else(|| format!("bad reply from the proxy: {}", line.trim()))?;
    let reason = status.next().unwrap_or_default().to_string();
    let (mut length, mut range) = (None, None);
    loop {
        line.clear();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 || line.trim().is_empty() {
            break;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        if k.eq_ignore_ascii_case("Content-Length") {
            length = v.parse::<u64>().ok();
        } else if k.eq_ignore_ascii_case("Content-Range") {
            range = v.rsplit('/').next().and_then(|t| t.trim().parse::<u64>().ok());
        }
    }
    match code {
        206 => Ok((range, range.is_some(), Box::new(r))),
        200 if from > 0 => Err("server ignored the Range request".into()),
        200 => Ok((length, false, Box::new(r))),
        _ => Err(format!("HTTP {code} {reason}")),
    }
}

/// Name lookups shared by every connection: a server's addresses are looked up once a minute, not once per
/// connection, and IPv4 comes first, so a network whose IPv6 is broken doesn't stall each new connection
/// for the connect timeout before trying IPv4.
struct DnsCache;

impl ureq::Resolver for DnsCache {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        use std::net::ToSocketAddrs;
        static CACHE: Mutex<Vec<(String, Instant, Vec<std::net::SocketAddr>)>> = Mutex::new(Vec::new());
        const TTL: Duration = Duration::from_secs(60);
        if let Some(hit) = CACHE.lock().unwrap().iter().find(|e| e.0 == netloc && e.1.elapsed() < TTL) {
            return Ok(hit.2.clone());
        }
        let mut addrs: Vec<std::net::SocketAddr> = netloc.to_socket_addrs()?.collect();
        addrs.sort_by_key(|a| a.is_ipv6()); // stable: the system's order within each family
        let mut cache = CACHE.lock().unwrap();
        cache.retain(|e| e.0 != netloc && e.1.elapsed() < TTL);
        if !addrs.is_empty() {
            cache.push((netloc.to_string(), Instant::now(), addrs.clone()));
        }
        Ok(addrs)
    }
}

/// The certificate authorities HTTPS trusts: the bundled list (webpki-roots) and the system's own
/// (Windows' certificate store, the macOS keychain, the Linux CA files), so a certificate that antivirus
/// HTTPS scanning or a company network installs on the system is trusted as the browser trusts it.
/// `extra`: more, for the tests' local servers.
pub(crate) fn tls_config(extra: &[rustls::pki_types::CertificateDer<'static>]) -> std::sync::Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let native = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(native.certs);
    roots.add_parsable_certificates(extra.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        .unwrap() // ring supports both
        .with_root_certificates(roots)
        .with_no_client_auth();
    std::sync::Arc::new(config)
}

/// `tls_config`, read from the system once.
fn tls() -> std::sync::Arc<rustls::ClientConfig> {
    static TLS: std::sync::OnceLock<std::sync::Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    TLS.get_or_init(|| tls_config(&[])).clone()
}

pub fn agent(proxy: &str, user_agent: &str) -> Agent {
    agent_with_tls(proxy, user_agent, tls())
}

pub(crate) fn agent_with_tls(proxy: &str, user_agent: &str, tls: std::sync::Arc<rustls::ClientConfig>) -> Agent {
    let user_agent = if user_agent.trim().is_empty() { UA } else { user_agent.trim() };
    let proxy = proxy.trim();
    let invalid = !proxy.is_empty() && (!url::Url::parse(proxy).is_ok_and(|url| url.host_str().is_some())
        || ureq::Proxy::new(proxy).is_err());
    let build = || {
        let mut builder = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .user_agent(user_agent)
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(30))
            .max_idle_connections_per_host(64)
            .max_idle_connections(256)
            .tls_config(tls.clone())
            .resolver(DnsCache);
        if !proxy.is_empty() {
            // Invalid configuration fails closed, including ancillary callers using the ureq agent.
            let route = if invalid { "http://127.0.0.1:0" } else { proxy };
            builder = builder.proxy(ureq::Proxy::new(route).expect("validated proxy"));
        }
        builder
    };
    Agent::new(build().build(), build().redirects(0).build(), proxy, user_agent, invalid)
}

fn with_headers(mut request: Request, headers: &[(String, String)]) -> Request {
    for (key, value) in headers {
        if !matches!(key.to_ascii_lowercase().as_str(), "host" | "connection" | "proxy-authorization"
            | "content-length" | "transfer-encoding" | "te" | "upgrade" | "accept-encoding") {
            request = request.set(key, value);
        }
    }
    request
}

/// Send a request; a 401 asking for Digest authentication is answered with the URL's login (NDM's
/// header parser 0x58ad70 handles Basic and Digest), NTLM / Negotiate through Windows (`sspi`).
/// ureq itself sends Basic from the URL.
pub(crate) fn call_legacy(r: ureq::Request) -> Result<ureq::Response, String> {
    match r.clone().call() {
        Err(ureq::Error::Status(401, resp)) => {
            let challenges: Vec<String> = resp.all("WWW-Authenticate").iter().map(|s| s.to_string()).collect();
            let challenge = challenges.first().cloned().unwrap_or_default();
            let u = url::Url::parse(r.url()).map_err(|e| e.to_string())?;
            if let Some(pkg) = ["Negotiate", "NTLM"].into_iter().find(|p| challenges.iter().any(|c| c.trim().eq_ignore_ascii_case(p))) {
                let _ = resp.into_string(); // read it all so the connection goes back to the pool for the next leg
                return sspi(&r, &u, pkg);
            }
            match strip_ci(challenge.trim(), "Digest ").filter(|_| !u.username().is_empty()) {
                Some(params) => {
                    let (user, pass) = (pct_decode(u.username()), pct_decode(u.password().unwrap_or("")));
                    let uri = &u[url::Position::BeforePath..url::Position::AfterQuery];
                    let auth = digest(params, &user, &pass, r.method(), uri);
                    r.set("Authorization", &auth).call().map_err(err)
                }
                None => Err(err(ureq::Error::Status(401, resp))),
            }
        }
        x => x.map_err(err),
    }
}

/// NTLM / Negotiate: Windows builds the tokens (SSPI), with the URL's login ("DOMAIN\user" allowed) or,
/// without one, the signed-in Windows user. NTLM's legs must share the TCP connection: ureq's pool hands
/// the same keep-alive connection back as long as each response is read to the end.
/// Elsewhere Negotiate first tries Kerberos with the signed-in user's ticket (kinit, or the desktop's
/// sign-in), through the system's GSSAPI library (`gss`); then, as for NTLM, NTLMv2 is done here (MS-NLMP)
/// with the URL's login (SPNEGO servers accept raw NTLMSSP).
#[cfg(not(windows))]
fn sspi(r: &ureq::Request, u: &url::Url, pkg: &str) -> Result<ureq::Response, String> {
    let (login, pass) = (pct_decode(u.username()), pct_decode(u.password().unwrap_or("")));
    let mut kerberos = None;
    if pkg == "Negotiate" {
        match gss::negotiate(r, u) {
            Ok(resp) => return Ok(resp),
            Err(e) => kerberos = Some(e),
        }
    }
    if login.is_empty() {
        return Err(match kerberos {
            Some(e) => format!("Kerberos sign-in failed ({e}); for NTLM put a user name and password in the address"),
            None => format!("{pkg} sign-in needs a user name and password in the address"),
        });
    }
    let (domain, user) = login.split_once('\\').unwrap_or(("", &login));
    let send = |token: &[u8]| {
        r.clone().set("Authorization", &format!("{pkg} {}", b64(token))).call().map_err(|e| match e {
            ureq::Error::Status(401, resp) => {
                let next = resp.all("WWW-Authenticate").iter().find_map(|c| strip_ci(c.trim(), &format!("{pkg} ")).map(str::to_string));
                let _ = resp.into_string(); // the next leg goes on this keep-alive connection
                next.ok_or("401 Unauthorized".to_string())
            }
            e => Err(err(e)),
        })
    };
    match send(&ntlm::negotiate()) {
        Ok(resp) => Ok(resp),
        Err(Ok(challenge)) => {
            let auth = ntlm::authenticate(&unb64(&challenge), user, domain, &pass).ok_or(format!("{pkg} authentication failed"))?;
            send(&auth).map_err(|e| e.map_or_else(|e| e, |_| "401 Unauthorized".into()))
        }
        Err(Err(e)) => Err(e),
    }
}

/// Kerberos for Negotiate (SPNEGO, RFC 4559) through the system's GSSAPI library (MIT krb5 or Heimdal; GSS
/// on macOS), opened at run time: NDM runs without it, and a system without Kerberos simply has no ticket.
#[cfg(not(windows))]
mod gss {
    use super::{b64, err, strip_ci, unb64};
    use std::ffi::c_void;
    use std::sync::OnceLock;

    #[repr(C)]
    struct Buffer {
        length: usize,
        value: *mut c_void,
    }
    #[repr(C)]
    struct Oid {
        length: u32,
        elements: *const c_void,
    }
    type ImportName = unsafe extern "C" fn(*mut u32, *const Buffer, *const Oid, *mut *mut c_void) -> u32;
    type InitSecContext = unsafe extern "C" fn(*mut u32, *mut c_void, *mut *mut c_void, *mut c_void, *const Oid, u32, u32, *mut c_void, *const Buffer, *mut *const Oid, *mut Buffer, *mut u32, *mut u32) -> u32;
    type ReleaseBuffer = unsafe extern "C" fn(*mut u32, *mut Buffer) -> u32;
    type DeleteSecContext = unsafe extern "C" fn(*mut u32, *mut *mut c_void, *mut Buffer) -> u32;
    type ReleaseName = unsafe extern "C" fn(*mut u32, *mut *mut c_void) -> u32;
    struct Lib {
        import_name: ImportName,
        init: InitSecContext,
        release_buffer: ReleaseBuffer,
        delete: DeleteSecContext,
        release_name: ReleaseName,
    }

    /// GSS_C_NT_HOSTBASED_SERVICE (1.2.840.113554.1.2.1.4) and SPNEGO (1.3.6.1.5.5.2)
    const HOSTBASED: [u8; 10] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x01, 0x04];
    const SPNEGO: [u8; 6] = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
    const MUTUAL: u32 = 2;
    const CONTINUE_NEEDED: u32 = 1;

    fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(|| unsafe {
            // MIT krb5 or Heimdal on Linux, the system's GSS framework on macOS
            let h = ["libgssapi_krb5.so.2", "libgssapi.so.3", "/System/Library/Frameworks/GSS.framework/GSS"].iter().map(|n| libc::dlopen(format!("{n}\0").as_ptr().cast(), libc::RTLD_NOW)).find(|h| !h.is_null())?;
            let sym = |n: &str| Some(libc::dlsym(h, format!("{n}\0").as_ptr().cast())).filter(|p| !p.is_null());
            Some(Lib {
                import_name: std::mem::transmute::<*mut c_void, ImportName>(sym("gss_import_name")?),
                init: std::mem::transmute::<*mut c_void, InitSecContext>(sym("gss_init_sec_context")?),
                release_buffer: std::mem::transmute::<*mut c_void, ReleaseBuffer>(sym("gss_release_buffer")?),
                delete: std::mem::transmute::<*mut c_void, DeleteSecContext>(sym("gss_delete_sec_context")?),
                release_name: std::mem::transmute::<*mut c_void, ReleaseName>(sym("gss_release_name")?),
            })
        })
        .as_ref()
    }

    /// A security context for HTTP@host; each `step` turns the server's token into the next one to send.
    struct Ctx {
        lib: &'static Lib,
        name: *mut c_void,
        ctx: *mut c_void,
    }

    impl Ctx {
        fn new(host: &str) -> Result<Ctx, String> {
            let lib = lib().ok_or("no Kerberos (GSSAPI) library")?;
            let service = format!("HTTP@{host}");
            let buf = Buffer { length: service.len(), value: service.as_ptr() as *mut c_void };
            let oid = Oid { length: HOSTBASED.len() as u32, elements: HOSTBASED.as_ptr().cast() };
            let (mut minor, mut name) = (0, std::ptr::null_mut());
            let major = unsafe { (lib.import_name)(&mut minor, &buf, &oid, &mut name) };
            if major >> 16 != 0 {
                return Err(format!("GSSAPI error {major:#x}"));
            }
            Ok(Ctx { lib, name, ctx: std::ptr::null_mut() })
        }

        /// The next token, and whether the server should answer with one more.
        fn step(&mut self, input: &[u8]) -> Result<(Vec<u8>, bool), String> {
            let mech = Oid { length: SPNEGO.len() as u32, elements: SPNEGO.as_ptr().cast() };
            let inp = Buffer { length: input.len(), value: input.as_ptr() as *mut c_void };
            let mut out = Buffer { length: 0, value: std::ptr::null_mut() };
            let mut minor = 0;
            let null = std::ptr::null_mut();
            let major = unsafe { (self.lib.init)(&mut minor, null, &mut self.ctx, self.name, &mech, MUTUAL, 0, null, if input.is_empty() { std::ptr::null() } else { &inp }, std::ptr::null_mut(), &mut out, null.cast(), null.cast()) };
            let token = if out.value.is_null() { vec![] } else { unsafe { std::slice::from_raw_parts(out.value as *const u8, out.length).to_vec() } };
            unsafe { (self.lib.release_buffer)(&mut minor, &mut out) };
            if major >> 16 != 0 {
                // no ticket, an unknown service, an expired ticket...
                return Err(format!("GSSAPI error {major:#x}, minor {minor}"));
            }
            Ok((token, major & CONTINUE_NEEDED != 0))
        }
    }

    impl Drop for Ctx {
        fn drop(&mut self) {
            let mut minor = 0;
            unsafe {
                if !self.ctx.is_null() {
                    (self.lib.delete)(&mut minor, &mut self.ctx, std::ptr::null_mut());
                }
                (self.lib.release_name)(&mut minor, &mut self.name);
            }
        }
    }

    /// The request again with "Authorization: Negotiate <token>", for as many legs as the server needs.
    pub(super) fn negotiate(r: &ureq::Request, u: &url::Url) -> Result<ureq::Response, String> {
        let host = u.host_str().ok_or("no host")?;
        let mut ctx = Ctx::new(host)?;
        let (mut token, _) = ctx.step(&[])?;
        for _ in 0..4 {
            match r.clone().set("Authorization", &format!("Negotiate {}", b64(&token))).call() {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(401, resp)) => {
                    let next = resp.all("WWW-Authenticate").iter().find_map(|c| strip_ci(c.trim(), "Negotiate ").map(str::to_string));
                    let _ = resp.into_string();
                    let Some(next) = next else { return Err("the server refused the Kerberos ticket".into()) };
                    let (t, _) = ctx.step(&unb64(&next))?;
                    token = t;
                }
                Err(e) => return Err(err(e)),
            }
        }
        Err("Kerberos sign-in didn't finish".into())
    }

    /// Against a real Kerberos site: NDM_KERBEROS_URL=http://host/protected cargo test kerberos_site -- --ignored
    /// (with a ticket from kinit)
    #[test]
    #[ignore]
    fn kerberos_site() {
        let url = std::env::var("NDM_KERBEROS_URL").expect("NDM_KERBEROS_URL");
        let resp = super::call_legacy(ureq::agent().get(&url)).unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn without_a_ticket_it_fails_cleanly() {
        // CI has no Kerberos realm: either no library, or a context that can't get a ticket
        if let Ok(mut c) = Ctx::new("ndm-test.invalid") {
            assert!(c.step(&[]).is_err());
        }
    }
}

/// NTLM's messages (MS-NLMP): NEGOTIATE, then AUTHENTICATE with an NTLMv2 response to the CHALLENGE.
#[cfg(not(windows))]
mod ntlm {
    use md5::Digest;

    // UNICODE | OEM | REQUEST_TARGET | NTLM | ALWAYS_SIGN | EXTENDED_SESSIONSECURITY | TARGET_INFO | 128 | 56
    const FLAGS: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0004 | 0x0000_0200 | 0x0000_8000 | 0x0008_0000 | 0x0080_0000 | 0x2000_0000 | 0x8000_0000;

    pub fn negotiate() -> Vec<u8> {
        let mut m = b"NTLMSSP\0".to_vec();
        m.extend(1u32.to_le_bytes());
        m.extend(FLAGS.to_le_bytes());
        m.extend([0u8; 16]); // no domain, no workstation
        m
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
        let mut k = [0u8; 64];
        k[..key.len()].copy_from_slice(key); // keys here are 16 bytes
        let mut inner = md5::Md5::new();
        inner.update(k.map(|b| b ^ 0x36));
        parts.iter().for_each(|p| inner.update(p));
        let mut outer = md5::Md5::new();
        outer.update(k.map(|b| b ^ 0x5c));
        outer.update(inner.finalize());
        outer.finalize().into()
    }

    /// NTOWFv2: HMAC-MD5(MD4(password), UPPER(user) + domain), UTF-16LE.
    fn ntowf(user: &str, domain: &str, pass: &str) -> [u8; 16] {
        use md4::Md4;
        let nt: [u8; 16] = Md4::digest(utf16(pass)).into();
        hmac_md5(&nt, &[&utf16(&(user.to_uppercase() + domain))])
    }

    /// The AUTHENTICATE message for a CHALLENGE (server challenge at 24, target info at 40).
    pub fn authenticate(challenge: &[u8], user: &str, domain: &str, pass: &str) -> Option<Vec<u8>> {
        authenticate_at(challenge, user, domain, pass, client_nonce(), now())
    }

    fn authenticate_at(challenge: &[u8], user: &str, domain: &str, pass: &str, cc: [u8; 8], now: u64) -> Option<Vec<u8>> {
        if !challenge.starts_with(b"NTLMSSP\0\x02\0\0\0") {
            return None;
        }
        let server = challenge.get(24..32)?;
        let field = |at: usize| -> Option<&[u8]> {
            let len = u16::from_le_bytes(challenge.get(at..at + 2)?.try_into().ok()?) as usize;
            let off = u32::from_le_bytes(challenge.get(at + 4..at + 8)?.try_into().ok()?) as usize;
            challenge.get(off..off + len)
        };
        let info = field(40).unwrap_or(&[]);
        // the server's time when it sends one (MsvAvTimestamp, id 7); then no LMv2 response either
        let mut ts = None;
        let mut i = 0;
        while let (Some(id), Some(len)) = (info.get(i..i + 2), info.get(i + 2..i + 4)) {
            let (id, len) = (u16::from_le_bytes([id[0], id[1]]), u16::from_le_bytes([len[0], len[1]]) as usize);
            if id == 7 && len == 8 {
                ts = info.get(i + 4..i + 12).map(|t| t.to_vec());
            }
            if id == 0 {
                break;
            }
            i += 4 + len;
        }
        let key = ntowf(user, domain, pass);
        let mut blob = vec![1, 1, 0, 0, 0, 0, 0, 0];
        blob.extend(ts.clone().unwrap_or_else(|| now.to_le_bytes().to_vec()));
        blob.extend(cc);
        blob.extend([0u8; 4]);
        blob.extend(info);
        blob.extend([0u8; 4]);
        let proof = hmac_md5(&key, &[server, &blob]);
        let nt = [&proof[..], &blob].concat();
        let lm = if ts.is_some() { vec![0u8; 24] } else { [&hmac_md5(&key, &[server, &cc])[..], &cc].concat() };
        let (d, u, ws) = (utf16(domain), utf16(user), Vec::new());
        // header: signature, type, 6 fields (len, max, offset), flags; then the payload
        let mut m = b"NTLMSSP\0".to_vec();
        m.extend(3u32.to_le_bytes());
        let mut payload: Vec<u8> = Vec::new();
        for part in [&lm, &nt, &d, &u, &ws, &Vec::new()] {
            let off = 64 + payload.len();
            m.extend((part.len() as u16).to_le_bytes());
            m.extend((part.len() as u16).to_le_bytes());
            m.extend((off as u32).to_le_bytes());
            payload.extend(part.iter());
        }
        m.extend(FLAGS.to_le_bytes());
        m.extend(payload);
        Some(m)
    }

    /// 100 ns ticks since 1601 (Windows FILETIME).
    fn now() -> u64 {
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        (unix.as_nanos() / 100) as u64 + 116_444_736_000_000_000
    }

    fn client_nonce() -> [u8; 8] {
        use std::hash::BuildHasher;
        std::collections::hash_map::RandomState::new().hash_one(now()).to_le_bytes()
    }

    #[test]
    fn ntlmv2_reference() {
        // MS-NLMP 4.2.4: User / Domain / Password, server challenge 0123456789abcdef, client challenge aa*8,
        // time 0, target info: NetBIOS domain "Domain", NetBIOS server "Server"
        assert_eq!(ntowf("User", "Domain", "Password"), [0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0, 0x2e, 0x3f]);
        let info: Vec<u8> = [&[2u8, 0, 12, 0][..], &utf16("Domain"), &[1, 0, 12, 0], &utf16("Server"), &[0, 0, 0, 0]].concat();
        let mut ch = b"NTLMSSP\0\x02\0\0\0".to_vec();
        ch.extend([0u8; 8]); // target name
        ch.extend(FLAGS.to_le_bytes());
        ch.extend([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        ch.extend([0u8; 8]);
        ch.extend((info.len() as u16).to_le_bytes());
        ch.extend((info.len() as u16).to_le_bytes());
        ch.extend(48u32.to_le_bytes());
        ch.extend(&info);
        let m = authenticate_at(&ch, "User", "Domain", "Password", [0xaa; 8], 0).unwrap();
        let nt_off = u32::from_le_bytes(m[24..28].try_into().unwrap()) as usize;
        // NTProofStr from the spec
        assert_eq!(&m[nt_off..nt_off + 16], &[0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef, 0x6a, 0x1c]);
    }
}

#[cfg(windows)]
fn sspi(r: &ureq::Request, u: &url::Url, pkg: &str) -> Result<ureq::Response, String> {
    use windows_sys::Win32::Security::Authentication::Identity::*;
    use windows_sys::Win32::Security::Credentials::SecHandle;
    #[repr(C)]
    struct Identity { user: *const u16, user_len: u32, domain: *const u16, domain_len: u32, pass: *const u16, pass_len: u32, flags: u32 }
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let (login, pass) = (pct_decode(u.username()), pct_decode(u.password().unwrap_or("")));
    let (domain, user) = login.split_once('\\').unwrap_or(("", &login));
    let (wu, wd, wp) = (w(user), w(domain), w(&pass));
    let id = Identity { user: wu.as_ptr(), user_len: wu.len() as u32, domain: wd.as_ptr(), domain_len: wd.len() as u32, pass: wp.as_ptr(), pass_len: wp.len() as u32, flags: 2 }; // SEC_WINNT_AUTH_IDENTITY_UNICODE
    let spn: Vec<u16> = format!("HTTP/{}", u.host_str().unwrap_or("")).encode_utf16().chain([0]).collect();
    let pkg_w: Vec<u16> = pkg.encode_utf16().chain([0]).collect();
    unsafe {
        let mut cred: SecHandle = std::mem::zeroed();
        let auth = if login.is_empty() { std::ptr::null() } else { &id as *const Identity as *const core::ffi::c_void };
        if AcquireCredentialsHandleW(std::ptr::null(), pkg_w.as_ptr(), SECPKG_CRED_OUTBOUND, std::ptr::null(), auth, None, std::ptr::null(), &mut cred, std::ptr::null_mut()) != 0 {
            return Err(format!("{pkg} authentication is not available"));
        }
        let mut ctx: SecHandle = std::mem::zeroed();
        let mut input: Vec<u8> = Vec::new();
        let mut result = Err(format!("{pkg} authentication failed"));
        for leg in 0..3 {
            let mut out_buf = SecBuffer { cbBuffer: 0, BufferType: SECBUFFER_TOKEN, pvBuffer: std::ptr::null_mut() };
            let mut out = SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: 1, pBuffers: &mut out_buf };
            let mut in_buf = SecBuffer { cbBuffer: input.len() as u32, BufferType: SECBUFFER_TOKEN, pvBuffer: input.as_mut_ptr().cast() };
            let in_desc = SecBufferDesc { ulVersion: SECBUFFER_VERSION, cBuffers: 1, pBuffers: &mut in_buf };
            let (mut attrs, mut expiry) = (0u32, 0i64);
            let st = InitializeSecurityContextW(&cred, if leg == 0 { std::ptr::null() } else { &ctx }, spn.as_ptr(), ISC_REQ_ALLOCATE_MEMORY | ISC_REQ_CONNECTION, 0, SECURITY_NATIVE_DREP,
                if leg == 0 { std::ptr::null() } else { &in_desc }, 0, &mut ctx, &mut out, &mut attrs, &mut expiry);
            if st < 0 {
                break;
            }
            let token = std::slice::from_raw_parts(out_buf.pvBuffer as *const u8, out_buf.cbBuffer as usize).to_vec();
            FreeContextBuffer(out_buf.pvBuffer);
            result = r.clone().set("Authorization", &format!("{pkg} {}", b64(&token))).call().map_err(|e| match e {
                ureq::Error::Status(401, resp) => {
                    // the server's next challenge: "NTLM <base64>"
                    let next = resp.all("WWW-Authenticate").iter().find_map(|c| strip_ci(c.trim(), &format!("{pkg} ")).map(str::to_string));
                    let _ = resp.into_string();
                    next.map_or("401 Unauthorized".to_string(), |n| format!("\u{1}{n}"))
                }
                e => err(e),
            });
            match &result {
                Err(e) if e.starts_with('\u{1}') => input = unb64(&e[1..]),
                _ => break,
            }
        }
        DeleteSecurityContext(&ctx);
        FreeCredentialsHandle(&cred);
        result.map_err(|e| if e.starts_with('\u{1}') { "401 Unauthorized".into() } else { e })
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64(data: &[u8]) -> String {
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            s.push(if k <= c.len() { B64[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

fn unb64(s: &str) -> Vec<u8> {
    let bits: Vec<u32> = s.bytes().filter_map(|b| B64.iter().position(|x| *x == b).map(|v| v as u32)).collect();
    let mut out = Vec::new();
    for c in bits.chunks(4) {
        let n = c.iter().enumerate().fold(0u32, |n, (k, v)| n | v << (18 - 6 * k));
        out.extend_from_slice(&n.to_be_bytes()[1..c.len()]);
    }
    out
}

/// RFC 7616 Digest response (MD5, qop "auth" when offered) for the challenge's parameters.
fn digest(params: &str, user: &str, pass: &str, method: &str, uri: &str) -> String {
    let mut p = std::collections::HashMap::new();
    let mut rest = params.trim();
    while let Some((k, v)) = rest.split_once('=') {
        let v = v.trim_start();
        let (val, tail) = match v.strip_prefix('"') {
            Some(q) => q.split_once('"').unwrap_or((q, "")),
            None => v.split_once(',').map_or((v, ""), |(a, b)| (a, b)),
        };
        p.insert(k.trim().trim_start_matches(',').trim().to_ascii_lowercase(), val.to_string());
        rest = tail.trim_start_matches([',', ' ']);
    }
    let g = |k: &str| p.get(k).cloned().unwrap_or_default();
    let (realm, nonce) = (g("realm"), g("nonce"));
    let ha1 = md5_hex(&format!("{user}:{realm}:{pass}"));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let cnonce = format!("{:016x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64);
    let qop = g("qop").split(',').map(str::trim).any(|q| q == "auth");
    let mut h = format!(r#"Digest username="{user}", realm="{realm}", nonce="{nonce}", uri="{uri}", algorithm=MD5"#);
    if qop {
        let resp = md5_hex(&format!("{ha1}:{nonce}:00000001:{cnonce}:auth:{ha2}"));
        h += &format!(r#", response="{resp}", qop=auth, nc=00000001, cnonce="{cnonce}""#);
    } else {
        h += &format!(r#", response="{}""#, md5_hex(&format!("{ha1}:{nonce}:{ha2}")));
    }
    if p.contains_key("opaque") {
        h += &format!(r#", opaque="{}""#, g("opaque"));
    }
    h
}

/// MD5 from Windows' CNG (BCryptHash with the built-in MD5 provider handle).
#[cfg(windows)]
fn md5_hex(s: &str) -> String {
    use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_MD5_ALG_HANDLE};
    let mut out = [0u8; 16];
    unsafe { BCryptHash(BCRYPT_MD5_ALG_HANDLE, std::ptr::null(), 0, s.as_ptr(), s.len() as u32, out.as_mut_ptr(), 16) };
    out.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(not(windows))]
fn md5_hex(s: &str) -> String {
    use md5::Digest;
    md5::Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => crate::transport::http_error(code),
        ureq::Error::Transport(error) => {
            use ureq::ErrorKind::*;
            match error.kind() {
                ProxyUnauthorized => "HTTP 407 Proxy Authentication Required".into(),
                InvalidProxyUrl => "Invalid proxy configuration".into(),
                ProxyConnect => "Could not connect through the configured proxy".into(),
                kind => format!("HTTP transport error: {kind:?}"),
            }
        }
    }
}

pub fn probe(agent: &Agent, url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    if crate::stream::is_manifest(url) {
        return crate::stream::probe(agent, url, referrer, cookies);
    }
    if crate::ytdlp::is_video_page(url) {
        return crate::ytdlp::probe(agent, url, referrer, cookies);
    }
    if crate::stream::is_media_url(url) {
        // a live mms / rtmp / rtsp stream: no size until it is recorded
        return Ok(Probe { name: sanitize(&url_name(url)), size: None, ranges: false, modified: String::new(), etag: String::new(), stream: true, page: false, ytdlp: false });
    }
    if is_ftp(url) {
        let (size, ranges, _) = ftp_open(url, 0)?;
        return Ok(Probe { name: sanitize(&url_name(url)), size, ranges, modified: String::new(), etag: String::new(), stream: false, page: false, ytdlp: false });
    }
    let r = call(request(agent, url, referrer, cookies).set("Range", "bytes=0-0"))?;
    if r.header("Content-Type").is_some_and(crate::stream::is_manifest_type) {
        return crate::stream::probe(agent, url, referrer, cookies);
    }
    validate_http_response(&r, Some((0, Some(0))), None)?;
    Ok(probe_of(&r))
}

/// Does NDM get the file the browser is downloading? A download the browser starts comes to NDM as an address,
/// and some addresses give NDM something else: a file a site's service worker makes in the page (Telegram
/// Web), an expired or one-time link, a login page. The browser's `mime` and `size` (0: unknown) and file name
/// are compared with what the server sends NDM; Err says why the browser should keep the download.
pub fn same_file(agent: &Agent, url: &str, referrer: &str, cookies: &str, mime: &str, size: u64, filename: &str) -> Result<(), String> {
    if crate::stream::is_manifest(url) || crate::stream::is_media_url(url) || crate::ytdlp::is_video_page(url) || is_ftp(url) {
        return Ok(());
    }
    let r = call(request(agent, url, referrer, cookies).set("Range", "bytes=0-0"))?;
    let got = r.header("Content-Type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    validate_http_response(&r, Some((0, Some(0))), None)?;
    let p = probe_of(&r);
    drop(r); // not read: a server that ignores the range would send the whole file
    let page = |m: &str| m == "text/html" || m == "application/xhtml+xml";
    let want = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let named_page = matches!(filename.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(), Some("htm" | "html" | "xhtml" | "mht" | "mhtml"));
    if page(&got) && !page(&want) && !(want.is_empty() && named_page) {
        return Err(format!("the server sends a web page ({got}), not the file the browser gets ({})", if want.is_empty() { "?" } else { &want }));
    }
    if let (Some(want_ext), Some(got_ext)) = (mime_ext(&want), mime_ext(&got)) {
        if want_ext != got_ext {
            return Err(format!("The server sends {got_ext}, but the browser received {want_ext}"));
        }
    }
    if let (true, Some(s)) = (size > 0, p.size) {
        if s != size {
            return Err(format!("the server sends {s} bytes, the browser gets {size}"));
        }
    }
    Ok(())
}

/// A Content-Type that says web page.
fn is_page(content_type: Option<&str>) -> bool {
    let m = content_type.unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    m == "text/html" || m == "application/xhtml+xml"
}

/// An inclusive, concrete byte interval. Unsatisfied and unknown-size ranges cannot be split.
pub(crate) fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (span, total) = value.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let (first, last, total) = (first.parse().ok()?, last.parse().ok()?, total.parse().ok()?);
    (first <= last && last < total).then_some((first, last, total))
}

pub(crate) fn validate_http_response(r: &Response, range: Option<(u64, Option<u64>)>, expected: Option<u64>) -> Result<(), String> {
    if r.header("Content-Encoding").is_some_and(|value| !value.eq_ignore_ascii_case("identity")) {
        return Err("Server returned an encoded body for a byte download".into());
    }
    let length = r.header("Content-Length").map(|n| n.trim().parse::<u64>()).transpose().map_err(|_| "Invalid Content-Length")?;
    match (r.status(), range) {
        (206, Some((start, end))) => {
            let (first, last, total) = r.header("Content-Range").and_then(content_range).ok_or("Invalid Content-Range")?;
            if expected.is_some_and(|size| total != size) {
                return Err(CHANGED.into());
            }
            if first != start || last != end.unwrap_or(total - 1).min(total - 1) || length.is_some_and(|n| n != last - first + 1) {
                return Err("Content-Range does not match the requested bytes".into());
            }
            Ok(())
        }
        (200, _) if expected.zip(length).is_some_and(|(old, new)| old != new) => Err(CHANGED.into()),
        (200, _) => Ok(()),
        _ => Err(format!("Unexpected HTTP {} response", r.status())),
    }
}

pub(crate) fn permanent_error(error: &str) -> bool {
    let code = error.strip_prefix("HTTP ").and_then(|text| text.split_whitespace().next()).and_then(|code| code.parse::<u16>().ok());
    code.is_some_and(|code| (400..500).contains(&code) && !matches!(code, 408 | 425 | 429))
        || error == CHANGED
        || error == "server ignored the Range request"
        || error == "Invalid Content-Range"
        || error == "Content-Range does not match the requested bytes"
        || error == "Invalid proxy configuration"
}

pub(crate) fn retry_delay(failures: u32) -> Duration {
    use std::hash::{BuildHasher, RandomState};
    let jitter = RandomState::new().hash_one(failures) % 500;
    Duration::from_millis((1000 * (1u64 << failures.min(5)) + jitter).min(30_000))
}

fn valid_layout(segs: &[Seg], size: Option<u64>) -> bool {
    if segs.len() == 1 && segs[0].stop.is_none() {
        return segs[0].start == 0 && size.is_none();
    }
    let mut ordered: Vec<_> = segs.iter().collect();
    ordered.sort_unstable_by_key(|segment| segment.start);
    let mut end = 0;
    for segment in ordered {
        let Some(stop) = segment.stop else { return false };
        if segment.start != end || segment.next < end || segment.next > stop {
            return false;
        }
        end = stop;
    }
    size.is_none_or(|size| size == end)
}

fn probe_of(r: &Response) -> Probe {
    let total = r.header("Content-Range").and_then(content_range).map(|(_, _, total)| total);
    let ranges = r.status() == 206 && total.is_some();
    let size = if ranges { total } else { r.header("Content-Length").and_then(|l| l.trim().parse().ok()) };
    let name = r.header("Content-Disposition").and_then(disposition_name).unwrap_or_else(|| url_name(r.get_url()));
    let name = sanitize(&name);
    let name = r.header("Content-Type").and_then(mime_ext).map_or_else(|| name.clone(), |ext| media_name(&name, ext));
    Probe { name, size, ranges, modified: r.header("Last-Modified").unwrap_or_default().to_string(), etag: r.header("ETag").unwrap_or_default().to_string(), stream: false, page: is_page(r.header("Content-Type")), ytdlp: false }
}

/// The file type a Content-Type names, as an extension (None: not a type NDM files by).
pub fn mime_ext(ct: &str) -> Option<&'static str> {
    let m = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match m.as_str() {
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        "video/x-flv" => "flv",
        "video/mp2t" => "ts",
        "video/3gpp" => "3gp",
        "video/x-ms-wmv" => "wmv",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/vnd.wave" => "wav",
        "application/pdf" => "pdf",
        "application/zip" | "application/x-zip-compressed" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-iso9660-image" => "iso",
        "application/x-msdownload" | "application/x-msdos-program" | "application/vnd.microsoft.portable-executable" => "exe",
        "application/x-msi" | "application/x-ms-installer" => "msi",
        "application/vnd.android.package-archive" => "apk",
        "application/x-apple-diskimage" => "dmg",
        "application/vnd.debian.binary-package" => "deb",
        "application/x-rpm" => "rpm",
        "application/epub+zip" => "epub",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    })
}

/// A name that says what the file is: one with no extension, or a script's (download.php, get.aspx...),
/// takes the type's extension.
pub fn typed_name(name: &str, ext: Option<&str>) -> String {
    let Some(ext) = ext else { return name.to_string() };
    let (stem, cur) = name.rsplit_once('.').filter(|(s, e)| !s.is_empty() && e.len() <= 5 && !e.contains(' ')).unwrap_or((name, ""));
    let script = ["", "php", "asp", "aspx", "jsp", "cgi", "do", "ashx", "pl", "py", "action", "bin", "dat", "tmp"].contains(&cur.to_ascii_lowercase().as_str());
    if !script || cur.eq_ignore_ascii_case(ext) {
        return name.to_string();
    }
    format!("{stem}.{ext}")
}

pub(crate) fn media_name(name: &str, actual: &str) -> String {
    const CONTAINERS: &[&str] = &["ts", "mp4", "m4a", "mkv", "webm", "mov", "avi", "flv", "3gp"];
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    if CONTAINERS.contains(&actual) && CONTAINERS.contains(&ext.to_ascii_lowercase().as_str())
        && !ext.eq_ignore_ascii_case(actual) && !(actual == "mp4" && ext.eq_ignore_ascii_case("m4a")) {
        format!("{stem}.{actual}")
    } else {
        typed_name(name, Some(actual))
    }
}

/// What a file is from its first bytes, as an extension (None: can't tell).
pub fn sniff(path: &Path) -> Option<&'static str> {
    let mut head = vec![0u8; 512];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).ok()?;
    let d = &head[..n];
    let at = |i: usize, sig: &[u8]| d.get(i..i + sig.len()) == Some(sig);
    Some(if at(4, b"ftyp") {
        match d.get(8..12) {
            Some(b"M4A ") | Some(b"M4B ") => "m4a",
            Some(b"qt  ") => "mov",
            Some(b) if b.starts_with(b"3g") => "3gp",
            _ => "mp4",
        }
    } else if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        if d.windows(4).any(|w| w == b"webm") { "webm" } else { "mkv" }
    } else if at(0, b"ID3") || (d.len() > 1 && d[0] == 0xFF && matches!(d[1], 0xFB | 0xF3 | 0xF2)) {
        "mp3"
    } else if at(0, b"OggS") {
        "ogg"
    } else if at(0, b"fLaC") {
        "flac"
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        "wav"
    } else if at(0, b"RIFF") && at(8, b"AVI ") {
        "avi"
    } else if at(0, b"FLV") {
        "flv"
    } else if at(0, b"%PDF") {
        "pdf"
    } else if at(0, b"PK\x03\x04") {
        "zip"
    } else if at(0, b"Rar!") {
        "rar"
    } else if at(0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        "7z"
    } else if at(0, &[0x1F, 0x8B]) {
        "gz"
    } else if at(0, b"MZ") {
        "exe"
    } else if at(0, b"\x89PNG") {
        "png"
    } else if at(0, &[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if at(0, b"GIF8") {
        "gif"
    } else if [188, 192, 204].into_iter().any(|stride| {
        let offset = if stride == 192 { 4 } else { 0 };
        d.get(offset) == Some(&0x47) && d.get(offset + stride) == Some(&0x47)
    }) {
        "ts"
    } else {
        return None;
    })
}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len()).filter(|p| p.eq_ignore_ascii_case(prefix)).map(|_| &s[prefix.len()..])
}

fn pct_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

pub fn disposition_name(h: &str) -> Option<String> {
    let mut plain = None;
    for p in h.split(';').map(str::trim) {
        if let Some(v) = strip_ci(p, "filename*=") {
            let v = v.trim_matches('"');
            return Some(pct_decode(v.splitn(3, '\'').nth(2).unwrap_or(v)));
        }
        if let Some(v) = strip_ci(p, "filename=") {
            plain = Some(v.trim_matches('"').to_string());
        }
    }
    plain
}

pub fn url_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let path = path.split_once("://").map_or(path, |(_, rest)| rest);
    path.split_once('/').map_or("", |(_, p)| p).rsplit('/').next().map(pct_decode).unwrap_or_default()
}

pub fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim_matches([' ', '.']);
    if s.is_empty() { "download".into() } else { s.into() }
}

/// `name`, or `name (1).ext`, `name (2).ext`... whichever is free in `folder`.
pub fn unique_name(folder: &str, name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let free = |n: &str| {
        let p = Path::new(folder).join(n);
        !p.exists() && !Path::new(&format!("{}.part", p.display())).exists() && !taken(n)
    };
    if free(name) {
        return name.into();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|i| format!("{stem} ({i}){ext}")).find(|n| free(n)).unwrap()
}

impl Job {
    pub fn new(info: Info) -> Arc<Job> {
        let limiter = Limiter::default();
        limiter.rate.store(info.limit_kb.saturating_mul(1024), Relaxed);
        let done = info.done();
        Arc::new(Job {
            info: Mutex::new(info),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            just_completed: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            limiter,
            global_bucket: Limiter::default(),
            speed: Mutex::new((Instant::now(), done, 0.0)),
            set_date: AtomicBool::new(false),
            ignore_date: AtomicBool::new(false),
            fails: AtomicU32::new(0),
            failed_at: Mutex::new(None),
            lanes: Mutex::new(vec![]),
            net_bytes: Mutex::default(),
            multi_ok: AtomicBool::new(false),
            caps: Mutex::default(),
        })
    }

    pub fn info(&self) -> MutexGuard<'_, Info> {
        self.info.lock().unwrap()
    }

    pub fn running(&self) -> bool {
        self.running.load(Relaxed)
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Relaxed)
    }

    /// Bytes/s, re-sampled at most once a second.
    pub fn speed(&self) -> f64 {
        let done = self.info().done();
        let mut s = self.speed.lock().unwrap();
        let dt = s.0.elapsed().as_secs_f64();
        if dt >= 1.0 {
            *s = (Instant::now(), done, done.saturating_sub(s.1) as f64 / dt);
        }
        if self.running() { s.2 } else { 0.0 }
    }

    /// Shows what NDM does with the data after the download (see `Info::post`); `frac` below 0 = unknown.
    pub fn set_post(&self, label: &str, frac: f32) {
        let mut i = self.info();
        if i.post.as_ref().is_none_or(|p| p.0 != label) {
            i.post_since = Some(Instant::now());
        }
        i.post = Some((label.to_string(), frac.min(1.0)));
    }

    /// The finished file copied from the temporary directory to its folder on another drive, then the
    /// temporary copy removed; the bar shows the copy.
    fn copy_out(&self, from: &Path, to: &Path) -> Result<(), String> {
        let total = fs::metadata(from).map(|m| m.len()).unwrap_or(0).max(1);
        let tmp = PathBuf::from(format!("{}.ndm-copy", to.display()));
        let copy = || -> std::io::Result<()> {
            let (mut src, mut dst) = (File::open(from)?, File::create(&tmp)?);
            let mut buf = vec![0u8; 4 << 20];
            let mut done = 0u64;
            loop {
                if self.stopping() { return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped")); }
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n])?;
                done += n as u64;
                self.set_post("Moving the file to its folder...", done as f32 / total as f32);
            }
            dst.sync_all()?;
            drop(dst);
            fs::rename(&tmp, to)
        };
        let r = copy();
        self.info().post = None;
        if let Err(e) = r {
            let _ = fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        let _ = fs::remove_file(from);
        Ok(())
    }

    /// Runs this download here and now (not on a thread of its own, no status kept): a part of another
    /// download, such as the video or audio stream of a video page (`ytdlp::run`).
    pub(crate) fn run_part(&self, agent: &Agent, global: &Limiter) -> Result<bool, String> {
        self.run(agent, global)
    }

    pub fn start(self: &Arc<Self>, agent: Agent, global: Arc<Limiter>) {
        if self.info().status == Status::Complete || self.running.swap(true, Relaxed) {
            return;
        }
        let changed = matches!(&self.info().status, Status::Error(error) if error == CHANGED);
        if changed {
            // Queue retries skip identity mismatches. A subsequent user start explicitly
            // requests a new transfer, after the changed-resource error was shown.
            self.reset();
        }
        self.stop.store(false, Relaxed);
        self.net_bytes.lock().unwrap().clear();
        self.info().status = Status::Connecting;
        let job = self.clone();
        thread::spawn(move || {
            let r = job.run(&agent, &global);
            let mut i = job.info();
            i.post = None;
            i.status = match r {
                Ok(true) => {
                    i.queued = false;
                    job.just_completed.store(true, Relaxed);
                    Status::Complete
                }
                Ok(false) => Status::Stopped,
                Err(e) => Status::Error(e),
            };
            job.ended.store(!matches!(i.status, Status::Stopped), Relaxed);
            match i.status {
                Status::Error(_) => job.fails.fetch_add(1, Relaxed),
                _ => job.fails.swap(0, Relaxed),
            };
            *job.failed_at.lock().unwrap() = matches!(i.status, Status::Error(_)).then(Instant::now);
            for s in &mut i.segs {
                s.info.clear();
                s.owned = false;
            }
            job.running.store(false, Relaxed);
        });
    }

    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
    }

    /// Forget all progress so the next start downloads from scratch.
    pub fn reset(&self) {
        let mut i = self.info();
        let _ = fs::remove_file(i.part());
        let _ = fs::remove_dir_all(i.part()); // a stream's pieces
        i.segs.clear();
        i.etag.clear();
        i.modified.clear();
        i.probed = false;
        i.status = Status::Stopped;
    }

    fn run(&self, agent: &Agent, global: &Limiter) -> Result<bool, String> {
        let (url, referrer, cookies, probed, headers) = {
            let mut i = self.info();
            i.last_try = chrono::Local::now().timestamp();
            (i.url.clone(), i.referrer.clone(), i.cookies.clone(), i.probed, i.headers.clone())
        };
        if self.info().fed {
            return Err(crate::i18n::tr_os("This file came from its web page through the browser: download it again from the page."));
        }
        if crate::stream::is_media_url(&url) {
            return crate::stream::run_media(self, agent);
        }
        if crate::ytdlp::wants(&self.info()) {
            return crate::ytdlp::run(self, agent, global);
        }
        if crate::stream::is_manifest(&url) || self.info().stream {
            return crate::stream::run(self, agent, global);
        }
        // streams above stay on one network; here every network in `lanes` runs its own connections
        let lanes = self.lanes();
        let mut first_net = lanes.first().map(|l| l.0.clone()).unwrap_or_default();
        let mut first = None;
        // "Look up files in IE cache before downloading": a cached copy is offered to the
        // server as If-Modified-Since; on 304 Not Modified (0x7573) the cached file is copied into place.
        let cached = if self.info().ie_cache && !probed && !is_ftp(&url) { crate::win::url_cache(&url) } else { None };
        if !probed {
            // Like NDM (0x589960) the first request is the download itself, `bytes=0-`; no separate probe.
            let (p, r): (Probe, Body) = if is_ftp(&url) {
                let (size, ranges, r) = ftp_open(&url, 0)?;
                (Probe { name: sanitize(&url_name(&url)), size, ranges, modified: String::new(), etag: String::new(), stream: false, page: false, ytdlp: false }, r)
            } else {
                let first_req = |agent: &Agent| {
                    let mut req = with_headers(request(agent, &url, &referrer, &cookies), &headers).set("Range", "bytes=0-");
                    if let Some((_, since)) = &cached {
                        req = req.set("If-Modified-Since", since);
                    }
                    call(req)
                };
                // a first network that doesn't answer leaves the download to the default one
                let r = match lanes.first() {
                    Some((_, a)) => first_req(a).or_else(|_| {
                        first_net.clear();
                        first_req(agent)
                    })?,
                    None => first_req(agent)?,
                };
                if let (304, Some((file, _))) = (r.status(), &cached) {
                    drop(r);
                    let mut i = self.info();
                    if i.name.is_empty() {
                        i.name = unique_name(&i.folder, &sanitize(&url_name(&url)), &|_| false);
                    }
                    fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
                    let n = fs::copy(file, i.path()).map_err(|e| e.to_string())?;
                    (i.size, i.probed) = (Some(n), true);
                    i.segs = vec![Seg { next: n, stop: Some(n), ..Default::default() }];
                    return Ok(true);
                }
                validate_http_response(&r, Some((0, None)), None)?;
                if r.header("Content-Type").is_some_and(crate::stream::is_manifest_type) {
                    drop(r);
                    self.info().stream = true;
                    return crate::stream::run(self, agent, global);
                }
                // Queued, silent CLI and "start downloading immediately" jobs can reach here without
                // the File Info probe. Let yt-dlp try HTML from any site, while explicit HTML downloads
                // (and the Site Grabber's pages) keep their ordinary file behavior.
                let video_page = {
                    let i = self.info();
                    let html = |name: &str| matches!(crate::core::ext(name).as_str(), "html" | "htm" | "xhtml");
                    !i.plain && !i.page && !html(&i.name) && !html(&url_name(&i.url)) && is_page(r.header("Content-Type"))
                };
                if video_page {
                    drop(r);
                    self.info().ytdlp = true;
                    return crate::ytdlp::run(self, agent, global);
                }
                // the server's cookies go along on the other connections
                let set: Vec<String> = r.all("Set-Cookie").iter().filter_map(|c| c.split(';').next()).map(|c| c.trim().to_string()).collect();
                if !set.is_empty() && r.get_url() == url {
                    let mut i = self.info();
                    i.cookies = merge_cookies(&i.cookies, &set);
                }
                self.info().page = is_page(r.header("Content-Type"));
                (probe_of(&r), r.into_reader())
            };
            let mut i = self.info();
            if i.name.is_empty() {
                i.name = unique_name(&i.folder, &p.name, &|_| false);
            }
            if !i.segs.is_empty() && i.done() > 0 && (i.size != p.size || i.ranges != p.ranges
                || (!i.etag.is_empty() && !p.etag.is_empty() && i.etag != p.etag)
                || (!i.modified.is_empty() && !p.modified.is_empty() && i.modified != p.modified && !self.ignore_date.load(Relaxed))) {
                return Err(CHANGED.into());
            }
            (i.size, i.ranges, i.probed) = (p.size, p.ranges, true);
            if i.segs.is_empty() {
                i.modified = p.modified;
                i.etag = p.etag;
                first = Some(r); // becomes the first connection
            }
        }
        if self.stopping() {
            return Ok(false);
        }
        let part = {
            let mut i = self.info();
            fs::create_dir_all(&i.folder).map_err(|e| e.to_string())?;
            if !i.temp.is_empty() {
                fs::create_dir_all(&i.temp).map_err(|e| e.to_string())?;
            }
            let part = i.part();
            if !i.segs.is_empty() && !valid_layout(&i.segs, i.size) {
                return Err("Saved download ranges are invalid. Restart the download to recover.".into());
            }
            if i.ranges && i.done() > 0 && part.exists()
                && fs::metadata(&part).map_err(|e| e.to_string())?.len() < i.segs.iter().map(|s| s.next).max().unwrap_or(0) {
                return Err("The partial download is shorter than its saved progress".into());
            }
            // Servers without Range support can't resume, so they always start over.
            if i.segs.is_empty() || !i.ranges || !part.exists() {
                // One segment for the whole file; more connections split it as they open.
                i.segs = vec![Seg { stop: i.size, ..Default::default() }];
                let f = File::create(&part).map_err(|e| e.to_string())?;
                // NTFS fills the gap with zeros before a write far into a preallocated file, which stalls
                // the connections that start in the middle of a big file; a sparse file skips that
                #[cfg(windows)]
                crate::win::sparse(&f);
                if let Some(s) = i.size {
                    f.set_len(s).map_err(|e| e.to_string())?;
                }
            }
            i.status = Status::Downloading;
            part
        };
        let part = part.as_path();
        let max = self.info().conns.max(1);
        let mut errors: Vec<String> = Vec::new();
        // Each network ("" = the default one) runs up to `max` connections of its own, as one network
        // alone would: it starts with one, and doubles at most every RAMP_EVERY while every one of them is
        // receiving and the speed limits leave room. The networks are looked at on every round, so one
        // that joins gets connections and one that leaves loses them (see `worker`). A network whose
        // last connection gave up waits LANE_COOLDOWN before it is tried again.
        thread::scope(|sc| {
            let mut hs: Vec<(thread::ScopedJoinHandle<Option<String>>, String)> = Vec::new();
            let mut grown: std::collections::HashMap<String, Instant> = Default::default();
            let mut gave_up: std::collections::HashMap<String, Instant> = Default::default();
            let mut started = false;
            self.caps.lock().unwrap().clear();
            loop {
                let (done, live): (Vec<_>, Vec<_>) = hs.into_iter().partition(|(h, _)| h.is_finished());
                hs = live;
                for (h, net) in done {
                    if let Some(e) = h.join().unwrap_or(Some("worker panicked".into())) {
                        gave_up.insert(net, Instant::now());
                        errors.push(e);
                    }
                }
                if self.stopping() {
                    if hs.is_empty() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let lanes = self.lanes_or(agent);
                for (net, lane_agent) in &lanes {
                    let n = hs.iter().filter(|(_, l)| l == net).count();
                    // fewer once the server turned connections away (`worker`)
                    let max = self.caps.lock().unwrap().get(net).copied().unwrap_or(max).min(max);
                    let want = if n == 0 {
                        // its first connection, unless it gave up a moment ago
                        usize::from(gave_up.get(net).is_none_or(|t| t.elapsed() >= LANE_COOLDOWN))
                    } else if n < max && grown.get(net).is_none_or(|t| t.elapsed() >= RAMP_EVERY) && self.ramp_ok(global, net) {
                        (n * 2).min(max) - n
                    } else {
                        0
                    };
                    for _ in 0..want {
                        let Some(k) = self.claim(net) else { break };
                        // the first request's body becomes the first connection on its network
                        let pre = if *net == first_net { first.take() } else { None };
                        let (a, l) = (lane_agent.clone(), net.clone());
                        hs.push((sc.spawn(move || self.worker(k, part, (a, l), global, pre, n > 0)), net.clone()));
                        grown.insert(net.clone(), Instant::now());
                        started = true;
                    }
                }
                if first.is_some() && !lanes.iter().any(|(n, _)| *n == first_net) {
                    first = None; // its network left before it was taken: the others start from scratch
                }
                if hs.is_empty() && (started || first.is_none()) {
                    // nothing left to claim, or every network has given up for now
                    let waiting = lanes.iter().any(|(net, _)| gave_up.get(net).is_some_and(|t| t.elapsed() < LANE_COOLDOWN));
                    if !waiting || self.info().segs.iter().all(Seg::done) || lanes.iter().all(|(net, _)| gave_up.contains_key(net)) {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        if !errors.iter().any(|e| e == CHANGED) && self.info().segs.iter().all(Seg::done) {
            // the finished file in place: a rename, or a copy (with its progress, the lock free) when the
            // temporary directory is on another drive
            let (size, len, dest) = {
                let i = self.info();
                (i.size, i.segs.iter().map(|s| s.next).max().unwrap_or(0), i.path())
            };
            if size.is_none() {
                OpenOptions::new().write(true).open(part).and_then(|f| f.set_len(len)).map_err(|e| e.to_string())?;
            }
            let output = self.info().media_output.clone();
            if matches!(output.as_str(), "mp4" | "mkv") {
                if sniff(part) != Some("ts") { return Err("The downloaded source is no longer a transport stream".into()); }
                let ffmpeg = crate::media::toolchain().ok_or("Install FFmpeg and ffprobe to convert this transport stream")?;
                let mut converted = dest.with_extension(&output);
                if converted != dest && converted.exists() {
                    let folder = converted.parent().unwrap_or(Path::new(".")).to_string_lossy();
                    let name = unique_name(&folder, &converted.file_name().unwrap_or_default().to_string_lossy(), &|_| false);
                    converted = converted.with_file_name(name);
                }
                let stage = crate::media::Stage::new(&converted)?;
                let result = crate::media::mux(self, &ffmpeg, part, None, stage.path(), &output, output == "mp4");
                if self.stopping() { return Ok(false); }
                result?;
                stage.finish(&converted)?;
                let size = fs::metadata(&converted).map_err(|err| err.to_string())?.len();
                let _ = fs::remove_file(part);
                let mut info = self.info();
                info.name = converted.file_name().unwrap_or_default().to_string_lossy().into_owned();
                info.size = Some(size);
                info.segs = vec![Seg { next: size, stop: Some(size), ..Default::default() }];
                return Ok(true);
            }
            if fs::rename(part, &dest).is_err() {
                let copied = self.copy_out(part, &dest);
                if self.stopping() && copied.is_err() { return Ok(false); }
                copied?;
            }
        }
        let mut i = self.info();
        if errors.iter().any(|e| e == CHANGED) {
            // Preserve existing data and identity. An explicit restart can discard them.
            return Err(CHANGED.into());
        }
        if i.segs.iter().all(Seg::done) && !part.exists() {
            if i.size.is_none() {
                i.size = Some(i.done());
            }
            // "Set file creation date as provided by the server"
            let date = chrono::DateTime::parse_from_rfc2822(&i.modified).ok().filter(|_| self.set_date.load(Relaxed));
            if let (Some(t), Ok(f)) = (date, File::options().write(true).open(i.path())) {
                let t = std::time::SystemTime::from(t);
                #[cfg(windows)]
                let _ = { use std::os::windows::fs::FileTimesExt; f.set_times(fs::FileTimes::new().set_modified(t).set_created(t)) };
                #[cfg(not(windows))]
                let _ = f.set_times(fs::FileTimes::new().set_modified(t)); // no creation time to set
            }
            Ok(true)
        } else if self.stopping() || errors.is_empty() {
            Ok(false)
        } else {
            Err(errors.last().unwrap().clone())
        }
    }

    fn worker(&self, mut k: usize, part: &Path, (agent, net): (Agent, String), global: &Limiter, mut pre: Option<Body>, extra: bool) -> Option<String> {
        let mut fails = 0;
        loop {
            // a network that is gone (unplugged, unticked) hands its part back to the others
            if self.stopping() || !self.lane_present(&net) {
                self.info().segs[k].owned = false;
                return None;
            }
            let before = {
                let mut i = self.info();
                i.segs[k].net = net.clone();
                i.segs[k].next
            };
            match self.fetch(k, part, &agent, global, pre.take(), extra) {
                Ok(()) if self.stopping() => return None,
                Err(e) if e == CHANGED => {
                    self.stop();
                    return Some(e);
                }
                Err(e) if permanent_error(&e) => {
                    self.info().segs[k].owned = false;
                    return Some(e);
                }
                Err(e) if e == LANE_GONE => {
                    self.info().segs[k].owned = false;
                    return None;
                }
                Ok(()) => {
                    let n = self.claim(&net)?;
                    (k, fails) = (n, 0)
                },
                Err(e) if e == NO_ANSWER_ERR || ((e.starts_with("HTTP 503") || e.starts_with("HTTP 429") || self.info().segs[k].next == before) && self.others_receiving(k, &net) > 0) => {
                    // the server allows fewer connections than this (it says busy, or a new connection
                    // gets nothing: turned away, or left hanging until the read times out while the others
                    // receive): this one ends and hands its part back, and the network keeps to the
                    // connections that work (`caps`)
                    let working = self.others_receiving(k, &net).max(1);
                    let mut caps = self.caps.lock().unwrap();
                    let cap = caps.get(&net).map_or(working, |c| working.min(*c));
                    caps.insert(net.clone(), cap);
                    drop(caps);
                    let mut i = self.info();
                    let s = &mut i.segs[k];
                    (s.owned, s.rate, s.info) = (false, 0.0, String::new());
                    return None;
                }
                Err(e) => {
                    fails = if self.info().segs[k].next > before { 1 } else { fails + 1 };
                    {
                        // released before the wait, so the other connections can take it; another
                        // network gets the first go at it
                        let mut i = self.info();
                        let s = &mut i.segs[k];
                        s.info = format!("Error: {e}, retrying...");
                        s.owned = false;
                        s.rate = 0.0;
                        if !net.is_empty() {
                            s.failed_on = net.clone();
                        }
                    }
                    if fails > RETRIES {
                        return Some(e);
                    }
                    let until = Instant::now() + retry_delay(fails);
                    while !self.stopping() && Instant::now() < until && self.lane_present(&net) {
                        thread::sleep(Duration::from_millis(100));
                    }
                    if !self.lane_present(&net) {
                        return None;
                    }
                    {
                        let n = self.claim(&net)?;
                        k = n
                    }
                }
            }
        }
    }

    fn fetch(&self, k: usize, part: &Path, agent: &Agent, global: &Limiter, pre: Option<Body>, extra: bool) -> Result<(), String> {
        let (url, referrer, cookies, ranges, start, stop, back, headers) = {
            let mut i = self.info();
            let ranges = i.ranges;
            let s = &mut i.segs[k];
            if !ranges {
                (s.start, s.next) = (0, 0);
            }
            if s.done() {
                return Ok(());
            }
            s.info = "Connecting...".into();
            let (start, stop) = (s.next, s.stop);
            let back = if ranges { (s.next - s.start).min(VERIFY) } else { 0 };
            (i.url.clone(), i.referrer.clone(), i.cookies.clone(), ranges, start, stop, back, i.headers.clone())
        };
        let from = start - back;
        let mut r = match pre.filter(|_| start == 0) {
            Some(r) => r,
            None if is_ftp(&url) => ftp_open(&url, from)?.2,
            None => {
                let mut req = with_headers(request(agent, &url, &referrer, &cookies), &headers).prefer_http1(extra && ranges);
                if ranges {
                    let range = stop.map_or(format!("bytes={from}-"), |e| format!("bytes={from}-{}", e - 1));
                    req = req.set("Range", &range);
                    let etag = self.info().etag.clone();
                    if etag.starts_with('"') { req = req.set("If-Range", &etag); }
                }
                let Some(resp) = self.answer(req)? else { return Ok(()) };
                let mut partial = ranges;
                if ranges && resp.status() != 206 {
                    let mut info = self.info();
                    if resp.status() == 200 && start == 0 && info.done() == 0 && info.segs.len() == 1 {
                        info.ranges = false;
                        partial = false;
                    } else {
                        return Err("server ignored the Range request".into());
                    }
                }
                validate_http_response(&resp, partial.then_some((from, stop.map(|end| end - 1))), self.info().size)?;
                // a page where the file was (an expired link, a login or error page): not written into the file
                if is_page(resp.header("Content-Type")) && !self.info().page {
                    return Err("the server sends a web page instead of the file".into());
                }
                let lm = resp.header("Last-Modified").unwrap_or_default();
                let etag = resp.header("ETag").unwrap_or_default();
                let mut info = self.info();
                if (!lm.is_empty() && !info.modified.is_empty() && lm != info.modified && !self.ignore_date.load(Relaxed))
                    || (!etag.is_empty() && !info.etag.is_empty() && etag != info.etag) {
                    return Err(CHANGED.into());
                }
                if info.modified.is_empty() { info.modified = lm.into(); }
                if info.etag.is_empty() { info.etag = etag.into(); }
                resp.into_reader()
            }
        };
        let mut f = OpenOptions::new().read(true).write(true).open(part).map_err(|e| e.to_string())?;
        if back > 0 {
            // NDM compares the re-fetched bytes with the file (0x565ef0, "Check w_e_of_cn failed!").
            let (mut old, mut new) = (vec![0; back as usize], vec![0; back as usize]);
            f.seek(SeekFrom::Start(from)).and_then(|_| f.read_exact(&mut old)).map_err(|e| e.to_string())?;
            r.read_exact(&mut new).map_err(|e| e.to_string())?;
            if old != new {
                return Err(CHANGED.into());
            }
        }
        f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let mut buf = vec![0; CHUNK];
        let net = {
            let mut i = self.info();
            i.segs[k].info = "Receiving data...".into();
            i.segs[k].net.clone()
        };
        // the connection's speed, measured over half-second windows
        let (mut window, mut got) = (Instant::now(), 0u64);
        loop {
            if self.stopping() {
                return Ok(());
            }
            let want = {
                let i = self.info();
                let s = &i.segs[k];
                if s.stop.is_some() { CHUNK.min(s.left() as usize) } else { CHUNK }
            };
            if want == 0 {
                return Ok(());
            }
            let n = r.read(&mut buf[..want]).map_err(|e| e.to_string())?;
            if n == 0 {
                let mut i = self.info();
                let s = &mut i.segs[k];
                if s.stop.is_some() {
                    return Err("connection closed early".into());
                }
                s.stop = Some(s.next);
                return Ok(());
            }
            f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            if !net.is_empty() && !self.lane_present(&net) {
                self.info().segs[k].next += n as u64;
                return Err(LANE_GONE.into()); // unticked or gone: the part goes back to the others
            }
            got += n as u64;
            {
                let mut i = self.info();
                let s = &mut i.segs[k];
                s.next += n as u64;
                let dt = window.elapsed().as_secs_f64();
                if dt >= 0.5 {
                    let r = got as f64 / dt;
                    s.rate = if s.rate > 0.0 { (s.rate + r) / 2.0 } else { r };
                    (window, got) = (Instant::now(), 0);
                }
            }
            *self.net_bytes.lock().unwrap().entry(net.clone()).or_default() += n as u64;
            self.throttle(global, n);
        }
    }

    /// Count `n` received bytes and wait as the speed limits ask (app-wide per-file rate, this file's).
    pub(crate) fn throttle(&self, global: &Limiter, n: usize) {
        global.received.fetch_add(n as u64, Relaxed);
        self.global_bucket.take_at(global.rate.load(Relaxed), n, &self.stop);
        self.limiter.take(n, &self.stop);
    }

    /// How many other connections on `net` than part `k`'s are receiving data (a finished part keeps its
    /// owner's last state, so it doesn't count).
    /// A connection's request, waited for while the download runs: a server that leaves a new connection
    /// unanswered for NO_ANSWER while the others receive (it allows fewer connections), or twice that once
    /// it has sent part of the file, ends it instead of after the read timeout. None: the download stopped.
    fn answer(&self, req: Request) -> Result<Option<Response>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(call(req));
        });
        let since = Instant::now();
        let before = self.info().done();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => return r.map(Some),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err("the request failed".into()),
                Err(_) if self.stopping() => return Ok(None),
                Err(_) => {
                    // the other connections received meanwhile; or, alone, a server that has been sending (it
                    // may still count the connection just closed) gets twice as long
                    let done = self.info().done();
                    let waited = since.elapsed();
                    if (waited >= NO_ANSWER && done > before) || (waited >= 2 * NO_ANSWER && done > 0) {
                        return Err(NO_ANSWER_ERR.into());
                    }
                }
            }
        }
    }

    fn others_receiving(&self, k: usize, net: &str) -> usize {
        self.info().segs.iter().enumerate().filter(|(j, s)| *j != k && s.owned && !s.done() && s.net == net && s.info.starts_with("Receiving")).count()
    }

    /// Every open connection on `net` is receiving data and the speed limits leave room.
    fn ramp_ok(&self, global: &Limiter, net: &str) -> bool {
        let busy = self.info().segs.iter().filter(|s| s.owned && s.net == net).all(|s| s.info.starts_with("Receiving"));
        let speed = self.speed();
        let full = |l: &Limiter| {
            let r = l.rate.load(Relaxed);
            r > 0 && speed >= r as f64 * 0.85
        };
        busy && !full(global) && !full(&self.limiter)
    }

    /// The networks the core set for this download (`lanes`).
    fn lanes(&self) -> Vec<(String, Agent)> {
        self.lanes.lock().unwrap().clone()
    }

    /// `lanes`, or the default network alone when there are none.
    fn lanes_or(&self, agent: &Agent) -> Vec<(String, Agent)> {
        let l = self.lanes();
        if l.is_empty() { vec![(String::new(), agent.clone())] } else { l }
    }

    /// Is `net` still one of this download's networks? The default one ("") is while there are no others.
    fn lane_present(&self, net: &str) -> bool {
        let l = self.lanes.lock().unwrap();
        if net.is_empty() { l.is_empty() } else { l.iter().any(|(n, _)| n == net) }
    }

    /// Next segment for a connection on `net`: an unfinished one nobody owns (resume, or handed back),
    /// else a split. A part that failed on `net` is left to another network while there is one.
    fn claim(&self, net: &str) -> Option<usize> {
        let others = !net.is_empty() && self.lanes.lock().unwrap().iter().any(|(n, _)| n != net);
        let mut i = self.info();
        let free = |s: &Seg| !s.owned && !s.done();
        let k = i.segs.iter().position(|s| free(s) && !(others && s.failed_on == net)).or_else(|| (!others).then(|| i.segs.iter().position(free)).flatten());
        if let Some(k) = k {
            i.segs[k].owned = true;
            return Some(k);
        }
        drop(i);
        self.steal(net)
    }

    /// Dynamic segmentation: split the segment that will take longest and hand back its end. Split in
    /// proportion to the two connections' speeds, so both finish together: a slow network no longer
    /// holds up the end of the download, as a part that would take it much longer goes mostly to the
    /// faster one, down to the last MIN_STEAL. Without speeds yet, the biggest segment is halved
    /// (at least MIN_SEG each).
    /// The split is always >= CHUNK past `next`, so the owner's in-flight chunk never crosses it.
    fn steal(&self, net: &str) -> Option<usize> {
        let mut i = self.info();
        if !i.ranges {
            return None;
        }
        // this network's speed per connection: its fastest segment now, else none known
        let mine = i.segs.iter().filter(|s| s.net == net && s.rate > 0.0).map(|s| s.rate).fold(0.0, f64::max);
        let eta = |s: &Seg| if s.rate > 0.0 { s.left() as f64 / s.rate } else { 0.0 };
        let timed = mine > 0.0 && i.segs.iter().filter(|s| s.owned && s.left() > 0).all(|s| s.rate > 0.0);
        let (k, best) = if timed {
            i.segs.iter().enumerate().filter(|(_, s)| s.left() > 0).max_by(|a, b| eta(a.1).total_cmp(&eta(b.1)))?
        } else {
            i.segs.iter().enumerate().max_by_key(|(_, s)| s.left())?
        };
        let (next, left, stop) = (best.next, best.left(), best.stop);
        let mid = if timed && best.rate > 0.0 {
            // where both would finish together; worth a new connection only if it saves real time
            let keep = (left as f64 * best.rate / (best.rate + mine)) as u64;
            let mid = next + keep.max(CHUNK as u64);
            let taken = (next + left).saturating_sub(mid);
            // a big part splits as always; a small one at the end only when the new connection would
            // clearly finish it first (it saves more than twice what a new request costs)
            let alone = left as f64 / best.rate;
            if taken < MIN_STEAL || (left < 2 * MIN_SEG && alone < 2.0 * (STEAL_STARTUP + taken as f64 / mine)) {
                return None;
            }
            mid
        } else {
            if left < 2 * MIN_SEG {
                return None;
            }
            next + left / 2
        };
        i.segs[k].stop = Some(mid);
        i.segs.push(Seg { start: mid, next: mid, stop, owned: true, ..Default::default() });
        Some(i.segs.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    use crate::test_support::serve;

    #[test]
    fn retries_wait_longer() {
        let w: Vec<u64> = (1..=9).map(|n| retry_wait(n).as_secs()).collect();
        assert_eq!(w, [15, 30, 60, 120, 240, 480, 600, 600, 600]);
    }

    #[test]
    fn browser_file_is_checked() {
        // /file: a 1000-byte video; /app: the site's web page (what a service worker's address gives NDM);
        // /gone: an expired link
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(&s);
                r.read_line(&mut line).unwrap();
                while { let mut h = String::new(); r.read_line(&mut h).unwrap_or(0) > 2 } {}
                let head = match line.split(' ').nth(1).unwrap_or("") {
                    "/file" => "HTTP/1.1 206 Partial\r\nContent-Type: video/mp4\r\nContent-Range: bytes 0-0/1000\r\nContent-Length: 1\r\n\r\nx",
                    "/app" => "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 4\r\n\r\n<p/>",
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                };
                let _ = (&s).write_all(head.as_bytes());
            }
        });
        let a = agent("", "");
        let check = |path: &str, mime: &str, size: u64, name: &str| same_file(&a, &format!("{base}{path}"), "", "", mime, size, name);
        assert!(check("/file", "video/mp4", 1000, "v.mp4").is_ok());
        assert!(check("/file", "", 0, "v.mp4").is_ok(), "nothing known from the browser");
        assert!(check("/file", "video/mp4", 1_000_000, "v.mp4").is_err(), "another size");
        assert!(check("/app", "video/mp4", 12_000_000, "v.mp4").is_err(), "a page for a video");
        assert!(check("/app", "", 0, "song.mp3").is_err(), "a page for an mp3");
        assert!(check("/app", "text/html", 0, "page.html").is_ok(), "a page is a page");
        assert!(check("/gone", "audio/mpeg", 0, "a.mp3").is_err(), "an error");
    }

    /// A server that serves at most `most` requests at once and answers 503 to the rest (`hang`: leaves
    /// them unanswered, as some video sites do); returns its address and how many it turned away.
    fn serve_limited(data: Arc<Vec<u8>>, most: usize, hang: bool) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::AtomicUsize;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        let (active, refused) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let refused2 = refused.clone();
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (data, active, refused) = (data.clone(), active.clone(), refused.clone());
                thread::spawn(move || loop {
                    // keep-alive: one request after another on this connection
                    let mut r = BufReader::new(&s);
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut range = (0, data.len() - 1);
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some(v) = strip_ci(&h, "range: bytes=") {
                            let (a, b) = v.trim().split_once('-').unwrap();
                            range = (a.parse().unwrap(), b.parse().unwrap_or(data.len() - 1).min(data.len() - 1));
                        }
                    }
                    let mut w = &s;
                    if active.fetch_add(1, Relaxed) >= most {
                        active.fetch_sub(1, Relaxed);
                        refused.fetch_add(1, Relaxed);
                        if hang {
                            thread::sleep(Duration::from_secs(120));
                            return;
                        }
                        let _ = w.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
                        continue;
                    }
                    let (a, b) = range;
                    let head = format!("HTTP/1.1 206 Partial\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\n\r\n", data.len(), b + 1 - a);
                    let mut ok = w.write_all(head.as_bytes()).is_ok();
                    for c in data[a..=b].chunks(64 * 1024) {
                        ok = ok && w.write_all(c).is_ok();
                        thread::sleep(Duration::from_millis(30));
                    }
                    active.fetch_sub(1, Relaxed);
                    if !ok {
                        return;
                    }
                });
            }
        });
        (base, refused2)
    }

    #[test]
    fn keeps_to_a_servers_connection_limit() {
        let data: Arc<Vec<u8>> = Arc::new((0..3 * 1024 * 1024).map(|i: u32| (i * 13 % 251) as u8).collect());
        let (base, refused) = serve_limited(data.clone(), 2, false);
        let dir = std::env::temp_dir().join(format!("ndm-test-limit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(Info { conns: 8, ..info(format!("{base}f.bin"), &dir) });
        let t = Instant::now();
        run(&j);
        let (status, segs) = { let i = j.info(); (i.status.clone(), i.segs.iter().map(|s| (s.start, s.next, s.stop, s.owned, s.info.clone())).collect::<Vec<_>>()) };
        assert_eq!(status, Status::Complete, "{segs:?}");
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        // the connections the server turned away end at once instead of retrying for a minute
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
        assert!(refused.load(Relaxed) <= 12, "{} requests refused", refused.load(Relaxed));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A server that leaves the connections over its limit unanswered: they give up after NO_ANSWER while
    /// the others receive, instead of waiting out the read timeout again and again (a download that seemed
    /// stuck).
    #[test]
    fn keeps_to_a_server_that_leaves_connections_hanging() {
        let data: Arc<Vec<u8>> = Arc::new((0..3 * 1024 * 1024).map(|i: u32| (i * 7 % 251) as u8).collect());
        let (base, _) = serve_limited(data.clone(), 1, true);
        let dir = std::env::temp_dir().join(format!("ndm-test-hang-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(Info { conns: 8, ..info(format!("{base}f.bin"), &dir) });
        let t = Instant::now();
        run(&j);
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        assert!(t.elapsed() < Duration::from_secs(25), "{:?}", t.elapsed());
        let _ = fs::remove_dir_all(&dir);
    }

    fn run(job: &Arc<Job>) {
        job.start(agent("", ""), Arc::new(Limiter::default()));
        while job.running() {
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn info(url: String, dir: &Path) -> Info {
        Info { url, folder: dir.display().to_string(), conns: 4, ..Default::default() }
    }

    /// The finished file copied out of a temporary directory on another drive: in place, the temporary
    /// copy gone, and no processing left showing.
    #[test]
    fn file_types() {
        assert_eq!(mime_ext("video/mp4; codecs=avc1"), Some("mp4"));
        assert_eq!(mime_ext("text/html"), None);
        assert_eq!(typed_name("watch", Some("mp4")), "watch.mp4");
        assert_eq!(typed_name("download.php", Some("zip")), "download.zip");
        assert_eq!(typed_name("setup.exe", Some("zip")), "setup.exe", "a real extension stays");
        assert_eq!(typed_name("v 1.2 final", Some("pdf")), "v 1.2 final.pdf");
        assert_eq!(typed_name("a.zip", None), "a.zip");
        let dir = std::env::temp_dir().join(format!("ndm-sniff-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for (bytes, want) in [
            (b"\0\0\0\x18ftypisom\0\0\0\0".to_vec(), Some("mp4")),
            (b"\0\0\0\x18ftypM4A \0\0\0\0".to_vec(), Some("m4a")),
            ([&[0x1A, 0x45, 0xDF, 0xA3][..], b"\x9fB\x82\x84webm"].concat(), Some("webm")),
            (b"ID3\x04\0".to_vec(), Some("mp3")),
            (b"%PDF-1.7".to_vec(), Some("pdf")),
            (b"PK\x03\x04rest".to_vec(), Some("zip")),
            (b"MZ\x90\0".to_vec(), Some("exe")),
            (b"just some text".to_vec(), None),
        ] {
            let p = dir.join("f");
            fs::write(&p, &bytes).unwrap();
            assert_eq!(sniff(&p), want, "{bytes:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_out_moves_the_file() {
        let dir = std::env::temp_dir().join(format!("ndm-copyout-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("a.part"), dir.join("a.bin"));
        let data: Vec<u8> = (0..9_000_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(&from, &data).unwrap();
        let j = Job::new(Info::default());
        j.copy_out(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), data);
        assert!(!from.exists() && !dir.join("a.bin.ndm-copy").exists());
        assert!(j.info().post.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn downloads_resumes_and_splits() {
        let data: Arc<Vec<u8>> = Arc::new((0..3 * 1024 * 1024 + 123).map(|i: u32| (i * 7 % 251) as u8).collect());
        let base = serve(data.clone());
        let dir = std::env::temp_dir().join(format!("ndm-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        // one connection would take ~2.4 s, so the engine ramps 1 -> 2 after a second
        let j = Job::new(info(format!("{base}rslow1.bin"), &dir));
        j.set_date.store(true, Relaxed);
        run(&j);
        assert_eq!(j.info().status, Status::Complete);
        assert!(j.info().segs.len() >= 2, "{}", j.info().segs.len());
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let t = fs::metadata(j.info().path()).unwrap().modified().unwrap();
        assert_eq!(t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), 784111777, "server's Last-Modified");

        let j = Job::new(info(format!("{base}plain.bin"), &dir));
        run(&j);
        assert_eq!(j.info().status, Status::Complete);
        assert!(!j.info().ranges);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);

        // pause mid-way, round-trip through JSON (as on restart), resume
        let j = Job::new(Info { limit_kb: 1024, ..info(format!("{base}r2.bin"), &dir) });
        j.start(agent("", ""), Arc::new(Limiter::default()));
        thread::sleep(Duration::from_millis(800));
        j.stop();
        while j.running() {
            thread::sleep(Duration::from_millis(20));
        }
        let saved = serde_json::to_string(&*j.info()).unwrap();
        let mut i: Info = serde_json::from_str(&saved).unwrap();
        assert!(i.status == Status::Stopped && i.done() > 0 && i.done() < data.len() as u64, "{:?} {}", i.status, i.done());
        i.limit_kb = 0;
        let j = Job::new(i);
        run(&j);
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn ie_cache_not_modified() {
        use std::io::{BufRead, BufReader, Write};
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::Networking::WinInet::{CommitUrlCacheEntryW, CreateUrlCacheEntryW, DeleteUrlCacheEntryW, NORMAL_CACHE_ENTRY};
        // a server that answers 304 when asked If-Modified-Since, else sends "fresh"
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/cached.txt", l.local_addr().unwrap());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (mut ims, mut r) = (false, BufReader::new(&s));
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                    ims |= line.to_ascii_lowercase().starts_with("if-modified-since:");
                    line.clear();
                }
                let reply = if ims { "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_string() } else { "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfresh".to_string() };
                let _ = (&s).write_all(reply.as_bytes());
            }
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-iec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (wu, ext) = (w(&url), w("txt"));
        // WinINet only commits files it handed out itself (CreateUrlCacheEntryW)
        let mut name = [0u16; 260];
        let made = unsafe { CreateUrlCacheEntryW(wu.as_ptr(), 0, ext.as_ptr(), name.as_mut_ptr(), 0) };
        let cache_file = std::path::PathBuf::from(String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(0)]));
        let t = FILETIME { dwLowDateTime: 0xd53e_8000, dwHighDateTime: 0x01d9_0000 }; // some time in 2022
        let ok = made != 0
            && fs::write(&cache_file, "cached copy").is_ok()
            && unsafe { CommitUrlCacheEntryW(wu.as_ptr(), name.as_ptr(), FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }, t, NORMAL_CACHE_ENTRY, std::ptr::null(), 0, ext.as_ptr(), std::ptr::null()) } != 0;
        if !ok {
            eprintln!("ie_cache_not_modified skipped: CommitUrlCacheEntryW failed ({})", std::io::Error::last_os_error());
            return; // no WinINet cache on this machine (e.g. a service account): nothing to look up
        }
        let found = crate::win::url_cache(&url);
        let out = dir.join("out");
        let j = Job::new(Info { ie_cache: true, name: "f.txt".into(), ..info(url.clone(), &out) });
        run(&j);
        let plain = Job::new(Info { name: "g.txt".into(), ..info(url.clone(), &out) });
        run(&plain);
        unsafe { DeleteUrlCacheEntryW(wu.as_ptr()) };
        let (_, since) = found.expect("the entry just committed");
        assert!(since.ends_with(" GMT") && since.contains("2022"), "{since}");
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read_to_string(j.info().path()).unwrap(), "cached copy", "304: the cached file is used");
        assert_eq!(fs::read_to_string(plain.info().path()).unwrap(), "fresh", "without the option the server's copy");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_invalid_ranges_and_preserves_partial_when_etag_changes() {
        for value in ["bytes 5-4/6", "bytes 0-6/6", "bytes */6", "items 0-1/6", "bytes 0-1/*"] {
            assert_eq!(content_range(value), None, "{value}");
        }
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/video.mp4?signature=abc", server.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut request = String::new();
            let mut reader = BufReader::new(&socket);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() <= 2 { break; }
                request.push_str(&line);
            }
            assert!(request.to_ascii_lowercase().contains("if-range: \"old\""));
            socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/4\r\nETag: \"new\"\r\n\r\nXYZ!").unwrap();
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-etag-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let info = Info { name: "video.mp4".into(), size: Some(4), ranges: true, probed: true, etag: "\"old\"".into(),
            segs: vec![Seg { next: 2, stop: Some(4), ..Default::default() }], ..info(url, &dir) };
        fs::write(info.part(), b"AB\0\0").unwrap();
        let job = Job::new(info);
        run(&job);
        assert_eq!(job.info().status, Status::Error(CHANGED.into()));
        let current = job.info();
        assert!(!current.path().exists());
        assert_eq!(current.done(), 2);
        assert_eq!(fs::read(current.part()).unwrap(), b"AB\0\0");
        drop(current);
        handle.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn initial_full_response_is_downloaded_once_without_splitting() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/video", server.local_addr().unwrap());
        let data = [0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 0, 0];
        let handle = thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut reader = BufReader::new(&socket);
            while reader.read_line(&mut String::new()).unwrap() > 2 {}
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 16\r\n\r\n").unwrap();
            socket.write_all(&data).unwrap();
        });
        let dir = std::env::temp_dir().join(format!("ndm-test-full-range-{}", std::process::id()));
        let job = Job::new(Info { conns: 8, ..info(url, &dir) });
        run(&job);
        let current = job.info();
        assert_eq!(current.status, Status::Complete);
        assert!(!current.ranges);
        assert_eq!(current.segs.len(), 1);
        assert_eq!(fs::read(current.path()).unwrap(), data);
        drop(current);
        handle.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resume_detects_changed_file() {
        let a: Arc<Vec<u8>> = Arc::new((0..2 * 1024 * 1024).map(|i: u32| (i % 251) as u8).collect());
        let b: Arc<Vec<u8>> = Arc::new(a.iter().map(|x| x ^ 0xff).collect());
        let (ua, ub) = (serve(a), serve(b.clone()));
        let dir = std::env::temp_dir().join(format!("ndm-test-chg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(Info { limit_kb: 512, conns: 1, ..info(format!("{ua}r.bin"), &dir) });
        j.start(agent("", ""), Arc::new(Limiter::default()));
        thread::sleep(Duration::from_millis(600));
        j.stop();
        while j.running() {
            thread::sleep(Duration::from_millis(20));
        }
        let mut i = j.info().clone();
        assert!(i.done() > VERIFY);
        (i.url, i.limit_kb) = (format!("{ub}r.bin"), 0); // same size, different bytes
        let j = Job::new(i);
        run(&j);
        assert_eq!(j.info().status, Status::Error(CHANGED.into()));
        assert!(j.ended.load(Relaxed), "a failed run fires the Download failed sound");
        run(&j);
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *b);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Tiny passive-mode FTP server for one file, honouring REST.
    fn ftp_serve(data: Arc<Vec<u8>>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ftp://{}/dir/f.bin", l.local_addr().unwrap());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let data = data.clone();
                thread::spawn(move || {
                    let mut w = s.try_clone().unwrap();
                    let mut r = BufReader::new(s);
                    let _ = w.write_all(b"220-hello\r\n220 ready\r\n");
                    let (mut rest, mut pasv, mut port) = (0, None, None);
                    let mut line = String::new();
                    while r.read_line(&mut line).unwrap_or(0) > 0 {
                        let l = line.trim().to_string();
                        line.clear();
                        let (cmd, arg) = l.split_once(' ').unwrap_or((&l, ""));
                        let reply = match cmd {
                            "USER" => "331 password please".into(),
                            "SIZE" => format!("213 {}", data.len()),
                            "PASV" => {
                                let p = TcpListener::bind("127.0.0.1:0").unwrap();
                                let port = p.local_addr().unwrap().port();
                                pasv = Some(p);
                                format!("227 Entering Passive Mode (127,0,0,1,{},{}).", port / 256, port % 256)
                            }
                            "PORT" => {
                                let n: Vec<u16> = arg.split(',').map(|x| x.parse().unwrap()).collect();
                                port = Some(format!("{}.{}.{}.{}:{}", n[0], n[1], n[2], n[3], n[4] * 256 + n[5]));
                                "200 port ok".into()
                            }
                            "REST" => {
                                rest = arg.parse().unwrap();
                                "350 restarting".into()
                            }
                            "RETR" => {
                                let _ = w.write_all(b"150 sending\r\n");
                                let mut d = match pasv.take() {
                                    Some(p) => p.accept().unwrap().0,
                                    None => std::net::TcpStream::connect(port.take().unwrap()).unwrap(),
                                };
                                for c in data[rest..].chunks(64 * 1024) {
                                    if d.write_all(c).is_err() {
                                        break;
                                    }
                                    thread::sleep(Duration::from_millis(20));
                                }
                                "226 done".into()
                            }
                            _ => "200 ok".into(),
                        };
                        let _ = w.write_all(format!("{reply}\r\n").as_bytes());
                    }
                });
            }
        });
        url
    }

    /// Range-aware HTTP server trickling ~1.3 MB/s per connection (User-Agent "slow": ~130 KB/s); logs
    /// each request's User-Agent.
    fn serve_ua(data: Arc<Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/f.bin", l.local_addr().unwrap());
        let log = Arc::new(Mutex::new(vec![]));
        let log2 = log.clone();
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (data, log) = (data.clone(), log2.clone());
                thread::spawn(move || {
                    let mut r = BufReader::new(&s);
                    let (mut range, mut line, mut pause) = ((0, data.len() - 1), String::new(), 50);
                    while r.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                        if let Some(v) = strip_ci(&line, "user-agent: ") {
                            if v.trim() == "slow" {
                                pause = 500; // a network a tenth as fast
                            }
                            log.lock().unwrap().push(v.trim().to_string());
                        } else if let Some(v) = strip_ci(&line, "range: bytes=") {
                            let (a, b) = v.trim().split_once('-').unwrap();
                            range = (a.parse().unwrap(), b.parse().unwrap_or(data.len() - 1).min(data.len() - 1));
                        }
                        line.clear();
                    }
                    let (a, b) = range;
                    let mut w = &s;
                    let _ = w.write_all(format!("HTTP/1.1 206 Partial\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\n\r\n", data.len(), b + 1 - a).as_bytes());
                    for c in data[a..=b].chunks(64 * 1024) {
                        if w.write_all(c).is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(pause));
                    }
                });
            }
        });
        (url, log)
    }

    fn lanes_run(lanes: Vec<(String, Agent)>, tag: &str) -> (Arc<Job>, Arc<Vec<u8>>, Arc<Mutex<Vec<String>>>, PathBuf) {
        lanes_run_then(lanes, tag, None)
    }

    /// As `lanes_run`; after a second the download's networks become `later`, as the core sets them
    /// when a network comes or goes.
    /// A download over `lanes`, which become `later` after `after` (if given).
    fn lanes_run_then(lanes: Vec<(String, Agent)>, tag: &str, later: Option<(Duration, Vec<(String, Agent)>)>) -> (Arc<Job>, Arc<Vec<u8>>, Arc<Mutex<Vec<String>>>, PathBuf) {
        let data: Arc<Vec<u8>> = Arc::new((0..4 * 1024 * 1024 + 3).map(|i: u32| (i * 17 % 249) as u8).collect());
        let (url, log) = serve_ua(data.clone());
        let dir = std::env::temp_dir().join(format!("ndm-test-lanes-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(Info { conns: 4, ..info(url, &dir) });
        *j.lanes.lock().unwrap() = lanes;
        j.start(agent("", "main"), Arc::new(Limiter::default()));
        let (start, until) = (Instant::now(), Instant::now() + Duration::from_secs(60));
        let mut later = later;
        while j.running() && Instant::now() < until {
            if later.as_ref().is_some_and(|(after, _)| start.elapsed() >= *after) {
                *j.lanes.lock().unwrap() = later.take().unwrap().1;
            }
            thread::sleep(Duration::from_millis(20));
        }
        (j, data, log, dir)
    }

    #[test]
    fn segments_use_every_lane() {
        let (j, data, log, dir) = lanes_run(vec![("a".into(), agent("", "lane-a")), ("b".into(), agent("", "lane-b"))], "every");
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let uas = log.lock().unwrap();
        assert!(uas.iter().any(|u| u == "lane-a") && uas.iter().any(|u| u == "lane-b") && !uas.iter().any(|u| u == "main"), "{uas:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lane_joins_mid_download() {
        // "b" is plugged in while "a" ramps up (four connections finish the file in about a second)
        let both = vec![("a".into(), agent("", "lane-a")), ("b".into(), agent("", "lane-b"))];
        let (j, data, log, dir) = lanes_run_then(vec![("a".into(), agent("", "lane-a"))], "join", Some((Duration::from_millis(300), both)));
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        assert!(log.lock().unwrap().iter().any(|u| u == "lane-b"), "the network that joined got connections");
        assert!(j.net_bytes.lock().unwrap().get("b").is_some_and(|n| *n > 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lane_leaves_mid_download() {
        // "b" is unplugged after a second: its parts go to "a", and the download completes without an error
        let both = vec![("a".into(), agent("", "lane-a")), ("b".into(), agent("", "lane-b"))];
        let (j, data, _, dir) = lanes_run_then(both, "leave", Some((Duration::from_secs(1), vec![("a".into(), agent("", "lane-a"))])));
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn slow_lane_doesnt_hold_up_the_end() {
        // a network a tenth as fast would take ~8 s for its quarter alone: the fast one takes the bigger share
        // (how much bigger depends on how busy the machine is), and the end comes when it would on the fast
        // network alone
        let t = Instant::now();
        let (j, data, _, dir) = lanes_run(vec![("fast".into(), agent("", "lane-a")), ("slow".into(), agent("", "slow"))], "slow");
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let bytes = j.net_bytes.lock().unwrap().clone();
        let (fast, slow) = (bytes.get("fast").copied().unwrap_or(0), bytes.get("slow").copied().unwrap_or(0));
        assert!(slow > 0 && fast > slow, "fast {fast}, slow {slow}");
        assert!(t.elapsed() < Duration::from_secs(8), "took {:?}", t.elapsed());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_lane_doesnt_fail_download() {
        // port 9 refuses: every connection on the "dead" network fails at once
        let (j, data, _, dir) = lanes_run(vec![("ok".into(), agent("", "")), ("dead".into(), agent("socks5://127.0.0.1:9", ""))], "dead");
        assert!(!j.running(), "finished within 60 s");
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let _ = fs::remove_dir_all(&dir);
    }

    /// `FTP` is global and tests run in parallel: every FTP test holds this.
    static FTP_TESTS: Mutex<()> = Mutex::new(());

    fn ftp_route(route: String) {
        *FTP.lock().unwrap() = FtpCfg { pasv: true, route: Some(Arc::new(move |_: &str| route.clone())) };
    }

    #[test]
    fn ftp_downloads_and_resumes() {
        let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *FTP.lock().unwrap() = FtpCfg { pasv: true, route: None };
        let data: Arc<Vec<u8>> = Arc::new((0..2 * 1024 * 1024 + 77).map(|i: u32| (i * 13 % 241) as u8).collect());
        let url = ftp_serve(data.clone());
        let dir = std::env::temp_dir().join(format!("ndm-test-ftp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(Info { limit_kb: 512, ..info(url, &dir) });
        j.start(agent("", ""), Arc::new(Limiter::default()));
        thread::sleep(Duration::from_millis(700));
        j.stop();
        while j.running() {
            thread::sleep(Duration::from_millis(20));
        }
        let mut i = j.info().clone();
        assert!(i.ranges && i.name == "f.bin" && i.done() > VERIFY && i.done() < data.len() as u64, "{:?} {}", i.status, i.done());
        i.limit_kb = 0;
        // resume in active mode through an "FTP proxy" (the test server, reached as user@host)
        let addr = i.url.split('/').nth(2).unwrap().to_string();
        let proxy = format!("ftp://{addr}");
        *FTP.lock().unwrap() = FtpCfg { pasv: false, route: Some(Arc::new(move |_: &str| proxy.clone())) };
        i.url = i.url.replace(&addr, "files.example");
        let j = Job::new(i);
        run(&j);
        *FTP.lock().unwrap() = FtpCfg { pasv: true, route: None };
        assert_eq!(j.info().status, Status::Complete);
        assert_eq!(fs::read(j.info().path()).unwrap(), *data);
        let _ = fs::remove_dir_all(&dir);
    }

    /// An HTTP proxy that fetches ftp:// addresses: records each request head and answers from `data`,
    /// with 206 + Content-Range when `honour_range`, else 200 + the whole file.
    fn fake_http_proxy(data: Arc<Vec<u8>>, honour_range: bool) -> (u16, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let heads = Arc::new(Mutex::new(vec![]));
        let heads2 = heads.clone();
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (data, heads) = (data.clone(), heads2.clone());
                thread::spawn(move || {
                    let mut r = BufReader::new(&s);
                    let mut head = String::new();
                    while r.read_line(&mut head).unwrap_or(0) > 0 && !head.ends_with("\r\n\r\n") {}
                    let from: usize = head.split("Range: bytes=").nth(1).and_then(|v| v.split('-').next()).and_then(|v| v.parse().ok()).unwrap_or(0);
                    heads.lock().unwrap().push(head);
                    let mut w = &s;
                    let (h, body) = if honour_range {
                        (format!("HTTP/1.0 206 Partial\r\nContent-Range: bytes {from}-{}/{}\r\nContent-Length: {}\r\n\r\n", data.len() - 1, data.len(), data.len() - from), &data[from..])
                    } else {
                        (format!("HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()), &data[..])
                    };
                    let _ = w.write_all(h.as_bytes());
                    for c in body.chunks(64 * 1024) {
                        if w.write_all(c).is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                });
            }
        });
        (port, heads)
    }

    fn ftp_via(route: String, url: &str, data: &[u8], tag: &str) -> Info {
        let dir = std::env::temp_dir().join(format!("ndm-test-ftp-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        ftp_route(route);
        let j = Job::new(Info { conns: 4, ..info(url.into(), &dir) });
        run(&j);
        *FTP.lock().unwrap() = FtpCfg { pasv: true, route: None };
        let i = j.info().clone();
        assert_eq!(i.status, Status::Complete);
        assert_eq!(fs::read(i.path()).unwrap(), data);
        let _ = fs::remove_dir_all(&dir);
        i
    }

    #[test]
    fn ftp_through_http_proxy() {
        let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let data: Arc<Vec<u8>> = Arc::new((0..1024 * 1024 + 5).map(|i: u32| (i * 7 % 251) as u8).collect());
        let (port, heads) = fake_http_proxy(data.clone(), true);
        let i = ftp_via(format!("http://u:p@127.0.0.1:{port}"), "ftp://h.example/f.bin", &data, "http");
        assert!(i.ranges);
        let first = &heads.lock().unwrap()[0];
        assert!(first.starts_with("GET ftp://h.example/f.bin HTTP/1.0\r\n"), "{first}");
        assert!(first.contains("\r\nRange: bytes=0-\r\n") && first.contains("\r\nProxy-Authorization: Basic dTpw\r\n"), "{first}");
    }

    #[test]
    fn ftp_http_proxy_without_ranges() {
        let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let data: Arc<Vec<u8>> = Arc::new((0..1024 * 1024 + 5).map(|i: u32| (i * 11 % 251) as u8).collect());
        let (port, heads) = fake_http_proxy(data.clone(), false);
        let i = ftp_via(format!("http://127.0.0.1:{port}"), "ftp://h.example/f.bin", &data, "norange");
        assert!(!i.ranges);
        assert_eq!(heads.lock().unwrap().len(), 1, "one connection");
    }

    #[test]
    fn ftp_through_socks() {
        let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let data: Arc<Vec<u8>> = Arc::new((0..1024 * 1024 + 9).map(|i: u32| (i * 3 % 253) as u8).collect());
        let url = ftp_serve(data.clone());
        let i = ftp_via(crate::net::local_proxy(""), &url, &data, "socks");
        assert!(i.ranges);
    }

    /// NTLM's first leg on one keep-alive connection: 401 "NTLM" -> the request carries Windows' NTLMSSP
    /// negotiate message (the server here accepts it without the challenge leg).
    #[test]
    fn ntlm_negotiate_leg() {
        assert_eq!([b64(b"ab"), b64(b"abc"), b64(b"a")], ["YWI=", "YWJj", "YQ=="]);
        assert_eq!(unb64(&b64(b"\x00\x01binary\xff")), b"\x00\x01binary\xff");
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://u:p@{}/f", l.local_addr().unwrap());
        thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            for _ in 0..2 {
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.strip_prefix("Authorization: NTLM ") {
                        auth = v.trim().to_string();
                    }
                }
                let reply = if unb64(&auth).starts_with(b"NTLMSSP\0\x01") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: NTLM\r\nContent-Length: 0\r\n\r\n"
                };
                (&s).write_all(reply.as_bytes()).unwrap();
            }
        });
        let resp = call(request(&agent("", ""), &url, "", "")).unwrap();
        assert_eq!(resp.into_string().unwrap(), "ok");
    }

    #[test]
    fn steal_keeps_segments_contiguous() {
        let j = Job::new(Info { ranges: true, segs: vec![Seg { stop: Some(10 * MIN_SEG), owned: true, ..Default::default() }], ..Default::default() });
        let k = j.steal("").unwrap();
        let i = j.info();
        assert_eq!(i.segs[0].stop, Some(i.segs[k].start));
        assert_eq!(i.segs[k].stop, Some(10 * MIN_SEG));
    }

    #[test]
    fn names() {
        assert_eq!(disposition_name(r#"attachment; filename="a b.zip""#).unwrap(), "a b.zip");
        assert_eq!(disposition_name("attachment; filename=x.bin; filename*=UTF-8''%E2%82%AC.txt").unwrap(), "€.txt");
        assert_eq!(url_name("https://h.com/dir/my%20file.iso?x=1"), "my file.iso");
        assert_eq!(url_name("https://h.com"), "");
        assert_eq!(sanitize("a:b?.."), "a_b_");
        assert_eq!(merge_cookies("a=1; b=2", &["b=3".into(), "c=4".into()]), "a=1; b=3; c=4");
        // RFC 2617 section 3.5 example
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        let h = digest(r#"realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#, "Mufasa", "Circle Of Life", "GET", "/dir/index.html");
        assert!(h.contains(r#"realm="testrealm@host.com""#) && h.contains("qop=auth, nc=00000001") && h.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#), "{h}");
        let ha1 = md5_hex("Mufasa:testrealm@host.com:Circle Of Life");
        let cn = h.split("cnonce=\"").nth(1).unwrap().split('"').next().unwrap();
        let want = md5_hex(&format!("{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:00000001:{cn}:auth:{}", md5_hex("GET:/dir/index.html")));
        assert!(h.contains(&format!(r#"response="{want}""#)));
        assert_eq!(ha1, "939e7578ed9e3c518a452acee763bce9");
    }
}
