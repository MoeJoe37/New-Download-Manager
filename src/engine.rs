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
/// Bytes before a resume point that are fetched again and compared with the file.
const VERIFY: u64 = 64;
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
    /// Which queue (core::Queue::id) a queued download belongs to.
    pub queue: u32,
    pub limit_kb: u64,
    /// "Parent web page": the page the link was on.
    pub parent: String,
    /// Server's Last-Modified; a different one on resume means the file changed (option
    /// "Ignore file modification time changes when resuming a download").
    pub modified: String,
    /// Temporary directory for the .part file (Options -> Save to); "" = next to the file.
    pub temp: String,
    /// An HLS / DASH stream (`stream::run`), also when the address doesn't say so.
    pub stream: bool,
    /// A Site Grabber download with "Look up files in IE cache before downloading".
    pub ie_cache: bool,
    /// The browser's own proxy when the extension handed the download over ("http://h:p", "socks5://h:p"
    /// or "pac:URL"), used after a failure with "Use proxy/socks from a browser's request".
    pub browser_proxy: String,
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
            return self.size.unwrap_or_else(|| self.segs.iter().map(|s| s.next - s.start).sum());
        }
        match self.size {
            Some(s) if !self.segs.is_empty() => s - self.segs.iter().map(Seg::left).sum::<u64>().min(s),
            _ => self.segs.iter().map(|s| s.next - s.start).sum(),
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
    /// "Download over several network connections at once": (network, its agent), set by the core
    /// before each start. Connection c goes over lanes[c % n]; empty = the job's agent.
    pub lanes: Mutex<Vec<(String, ureq::Agent)>>,
}

type Body = Box<dyn Read + Send + Sync>;

pub struct Probe {
    pub name: String,
    pub size: Option<u64>,
    pub ranges: bool,
    pub modified: String,
    pub stream: bool,
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

pub fn agent(proxy: &str, user_agent: &str) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .user_agent(if user_agent.trim().is_empty() { UA } else { user_agent.trim() })
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(30));
    if !proxy.trim().is_empty() {
        match ureq::Proxy::new(proxy.trim()) {
            Ok(p) => b = b.proxy(p),
            Err(e) => eprintln!("bad proxy {proxy}: {e}"),
        }
    }
    b.build()
}

pub(crate) fn request(agent: &ureq::Agent, url: &str, referrer: &str, cookies: &str) -> ureq::Request {
    let mut r = agent.get(url);
    if !referrer.is_empty() {
        r = r.set("Referer", referrer);
    }
    if !cookies.is_empty() {
        r = r.set("Cookie", cookies);
    }
    r
}

/// Send a request; a 401 asking for Digest authentication is answered with the URL's login (NDM's
/// header parser 0x58ad70 handles Basic and Digest), NTLM / Negotiate through Windows (`sspi`).
/// ureq itself sends Basic from the URL.
pub(crate) fn call(r: ureq::Request) -> Result<ureq::Response, String> {
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
/// Elsewhere NTLMv2 is done here (MS-NLMP), with the URL's login only: there is no signed-in Windows user.
/// Negotiate gets the same NTLM tokens (SPNEGO servers accept raw NTLMSSP).
// ponytail: no Kerberos; a Negotiate server that refuses NTLM fails
#[cfg(not(windows))]
fn sspi(r: &ureq::Request, u: &url::Url, pkg: &str) -> Result<ureq::Response, String> {
    let (login, pass) = (pct_decode(u.username()), pct_decode(u.password().unwrap_or("")));
    if login.is_empty() {
        return Err(format!("{pkg} sign-in needs a user name and password in the address"));
    }
    let (domain, user) = login.split_once('\\').unwrap_or(("", &login));
    let send = |token: &[u8]| {
        r.clone().set("Authorization", &format!("{pkg} {}", b64(token))).call().map_err(|e| match e {
            ureq::Error::Status(401, resp) => {
                let next = resp.all("WWW-Authenticate").iter().find_map(|c| strip_ci(c.trim(), &format!("{pkg} ")).map(str::to_string));
                let _ = resp.into_string(); // the next leg goes on this keep-alive connection
                next.map_or(Err("401 Unauthorized".to_string()), Ok)
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
        ureq::Error::Status(code, r) => format!("HTTP {code} {}", r.status_text()),
        ureq::Error::Transport(t) => t.to_string(),
    }
}

pub fn probe(agent: &ureq::Agent, url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    if crate::stream::is_manifest(url) {
        return crate::stream::probe(agent, url, referrer, cookies);
    }
    if crate::stream::is_media_url(url) {
        // a live mms / rtmp / rtsp stream: no size until it is recorded
        return Ok(Probe { name: sanitize(&url_name(url)), size: None, ranges: false, modified: String::new(), stream: true });
    }
    if is_ftp(url) {
        let (size, ranges, _) = ftp_open(url, 0)?;
        return Ok(Probe { name: sanitize(&url_name(url)), size, ranges, modified: String::new(), stream: false });
    }
    let r = call(request(agent, url, referrer, cookies).set("Range", "bytes=0-0"))?;
    if r.header("Content-Type").is_some_and(crate::stream::is_manifest_type) {
        return crate::stream::probe(agent, url, referrer, cookies);
    }
    Ok(probe_of(&r))
}

fn probe_of(r: &ureq::Response) -> Probe {
    let total = r.header("Content-Range").and_then(|c| c.rsplit('/').next()).and_then(|t| t.trim().parse().ok());
    let ranges = r.status() == 206 && total.is_some();
    let size = if ranges { total } else { r.header("Content-Length").and_then(|l| l.trim().parse().ok()) };
    let name = r.header("Content-Disposition").and_then(disposition_name).unwrap_or_else(|| url_name(r.get_url()));
    Probe { name: sanitize(&name), size, ranges, modified: r.header("Last-Modified").unwrap_or_default().to_string(), stream: false }
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
        limiter.rate.store(info.limit_kb * 1024, Relaxed);
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
            lanes: Mutex::new(vec![]),
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

    pub fn set_limit_kb(&self, kb: u64) {
        self.info().limit_kb = kb;
        self.limiter.rate.store(kb * 1024, Relaxed);
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

    pub fn start(self: &Arc<Self>, agent: ureq::Agent, global: Arc<Limiter>) {
        if self.info().status == Status::Complete || self.running.swap(true, Relaxed) {
            return;
        }
        self.stop.store(false, Relaxed);
        self.info().status = Status::Connecting;
        let job = self.clone();
        thread::spawn(move || {
            let r = job.run(&agent, &global);
            let mut i = job.info();
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
        i.probed = false;
        i.status = Status::Stopped;
    }

    fn run(&self, agent: &ureq::Agent, global: &Limiter) -> Result<bool, String> {
        let (url, referrer, cookies, probed) = {
            let mut i = self.info();
            i.last_try = chrono::Local::now().timestamp();
            (i.url.clone(), i.referrer.clone(), i.cookies.clone(), i.probed)
        };
        if crate::stream::is_media_url(&url) {
            return crate::stream::run_media(self);
        }
        if crate::stream::is_manifest(&url) || self.info().stream {
            return crate::stream::run(self, agent, global);
        }
        // streams above stay on one network; here connection c goes over lane c % n
        let lanes = self.lanes.lock().unwrap().clone();
        let lane = |c: usize| lanes.get(c % lanes.len().max(1)).map_or((agent, ""), |(n, a)| (a, n.as_str()));
        let mut first = None;
        // "Look up files in IE cache before downloading": a cached copy is offered to the
        // server as If-Modified-Since; on 304 Not Modified (0x7573) the cached file is copied into place.
        let cached = if self.info().ie_cache && !probed && !is_ftp(&url) { crate::win::url_cache(&url) } else { None };
        if !probed {
            // Like NDM (0x589960) the first request is the download itself, `bytes=0-`; no separate probe.
            let (p, r): (Probe, Body) = if is_ftp(&url) {
                let (size, ranges, r) = ftp_open(&url, 0)?;
                (Probe { name: sanitize(&url_name(&url)), size, ranges, modified: String::new(), stream: false }, r)
            } else {
                let first_req = |agent: &ureq::Agent| {
                    let mut req = request(agent, &url, &referrer, &cookies).set("Range", "bytes=0-");
                    if let Some((_, since)) = &cached {
                        req = req.set("If-Modified-Since", since);
                    }
                    call(req)
                };
                // a first network that doesn't answer leaves the download to the default one
                let r = first_req(lane(0).0).or_else(|e| if lanes.is_empty() { Err(e) } else { first_req(agent) })?;
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
                if r.header("Content-Type").is_some_and(crate::stream::is_manifest_type) {
                    drop(r);
                    self.info().stream = true;
                    return crate::stream::run(self, agent, global);
                }
                // the server's cookies go along on the other connections
                let set: Vec<String> = r.all("Set-Cookie").iter().filter_map(|c| c.split(';').next()).map(|c| c.trim().to_string()).collect();
                if !set.is_empty() {
                    let mut i = self.info();
                    i.cookies = merge_cookies(&i.cookies, &set);
                }
                (probe_of(&r), r.into_reader())
            };
            let mut i = self.info();
            if i.name.is_empty() {
                i.name = unique_name(&i.folder, &p.name, &|_| false);
            }
            if i.size != p.size || i.ranges != p.ranges {
                i.segs.clear(); // remote file changed (or first contact): start over
            }
            (i.size, i.ranges, i.probed) = (p.size, p.ranges, true);
            if i.segs.is_empty() {
                i.modified = p.modified;
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
            // Servers without Range support can't resume, so they always start over.
            if i.segs.is_empty() || !i.ranges || !part.exists() {
                // One segment for the whole file; more connections split it as they open.
                i.segs = vec![Seg { stop: i.size, ..Default::default() }];
                let f = File::create(&part).map_err(|e| e.to_string())?;
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
        thread::scope(|sc| {
            let mut hs = Vec::new();
            let mut last = Instant::now();
            let mut started = false;
            let mut conn = 0;
            loop {
                let (done, live): (Vec<_>, Vec<_>) = hs.into_iter().partition(|h: &thread::ScopedJoinHandle<_>| h.is_finished());
                errors.extend(done.into_iter().filter_map(|h| h.join().unwrap_or(Some("worker panicked".into()))));
                hs = live;
                if hs.is_empty() && started {
                    break; // every connection finished, failed or was stopped
                }
                let want = if !started {
                    1
                } else if !self.stopping() && hs.len() < max && last.elapsed() >= Duration::from_secs(1) && self.ramp_ok(global) {
                    (hs.len() * 2).min(max) - hs.len()
                } else {
                    0
                };
                for _ in 0..want {
                    match self.claim() {
                        Some(k) => {
                            let pre = first.take();
                            let (agent, net) = lane(conn);
                            conn += 1;
                            hs.push(sc.spawn(move || self.worker(k, part, (agent, net), global, pre)));
                            started = true;
                        }
                        None => break,
                    }
                    last = Instant::now();
                }
                if hs.is_empty() {
                    break; // nothing left to claim
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        let mut i = self.info();
        if errors.iter().any(|e| e == CHANGED) {
            i.segs.clear(); // the partial data is stale: the next start downloads from scratch
            i.probed = false;
            return Err(CHANGED.into());
        }
        if i.segs.iter().all(Seg::done) {
            if i.size.is_none() {
                let len = i.segs.iter().map(|s| s.next).max().unwrap_or(0);
                OpenOptions::new().write(true).open(&part).and_then(|f| f.set_len(len)).map_err(|e| e.to_string())?;
            }
            // rename, or copy + delete when the temporary directory is on another drive
            if fs::rename(part, i.path()).is_err() {
                fs::copy(part, i.path()).map_err(|e| e.to_string())?;
                let _ = fs::remove_file(part);
            }
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

    fn worker(&self, mut k: usize, part: &Path, (agent, net): (&ureq::Agent, &str), global: &Limiter, mut pre: Option<Body>) -> Option<String> {
        let mut fails = 0;
        loop {
            if self.stopping() {
                return None;
            }
            let before = {
                let mut i = self.info();
                i.segs[k].net = net.to_string();
                i.segs[k].next
            };
            match self.fetch(k, part, agent, global, pre.take()) {
                Ok(()) if self.stopping() => return None,
                Err(e) if e == CHANGED => {
                    self.stop();
                    return Some(e);
                }
                Ok(()) => match self.claim() {
                    Some(n) => (k, fails) = (n, 0),
                    None => return None,
                },
                Err(e) => {
                    fails = if self.info().segs[k].next > before { 1 } else { fails + 1 };
                    {
                        // released before the wait, so the other connections (other networks) can take it
                        let mut i = self.info();
                        i.segs[k].info = format!("Error: {e}, retrying...");
                        i.segs[k].owned = false;
                    }
                    if fails > RETRIES {
                        return Some(e);
                    }
                    let until = Instant::now() + Duration::from_secs(2u64.pow(fails).min(30));
                    while !self.stopping() && Instant::now() < until {
                        thread::sleep(Duration::from_millis(100));
                    }
                    match self.claim() {
                        Some(n) => k = n,
                        None => return None,
                    }
                }
            }
        }
    }

    fn fetch(&self, k: usize, part: &Path, agent: &ureq::Agent, global: &Limiter, pre: Option<Body>) -> Result<(), String> {
        let (url, referrer, cookies, ranges, start, stop, back) = {
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
            (i.url.clone(), i.referrer.clone(), i.cookies.clone(), ranges, start, stop, back)
        };
        let from = start - back;
        let mut r = match pre.filter(|_| start == 0) {
            Some(r) => r,
            None if is_ftp(&url) => ftp_open(&url, from)?.2,
            None => {
                let mut req = request(agent, &url, &referrer, &cookies);
                if ranges {
                    let range = stop.map_or(format!("bytes={from}-"), |e| format!("bytes={from}-{}", e - 1));
                    req = req.set("Range", &range);
                }
                let resp = call(req)?;
                if ranges && resp.status() != 206 {
                    return Err("server ignored the Range request".into());
                }
                let lm = resp.header("Last-Modified").unwrap_or_default();
                let old = self.info().modified.clone();
                if !lm.is_empty() && !old.is_empty() && lm != old && !self.ignore_date.load(Relaxed) {
                    return Err(CHANGED.into());
                }
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
        self.info().segs[k].info = "Receiving data...".into();
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
            self.info().segs[k].next += n as u64;
            self.throttle(global, n);
        }
    }

    /// Count `n` received bytes and wait as the speed limits ask (app-wide per-file rate, this file's).
    pub(crate) fn throttle(&self, global: &Limiter, n: usize) {
        global.received.fetch_add(n as u64, Relaxed);
        self.global_bucket.take_at(global.rate.load(Relaxed), n, &self.stop);
        self.limiter.take(n, &self.stop);
    }

    /// Every open connection is receiving data and the speed limits leave room.
    fn ramp_ok(&self, global: &Limiter) -> bool {
        let busy = self.info().segs.iter().filter(|s| s.owned).all(|s| s.info.starts_with("Receiving"));
        let speed = self.speed();
        let full = |l: &Limiter| {
            let r = l.rate.load(Relaxed);
            r > 0 && speed >= r as f64 * 0.85
        };
        busy && !full(global) && !full(&self.limiter)
    }

    /// Next segment for a connection: an unfinished one nobody owns (resume), else a split.
    fn claim(&self) -> Option<usize> {
        let mut i = self.info();
        if let Some(k) = i.segs.iter().position(|s| !s.owned && !s.done()) {
            i.segs[k].owned = true;
            return Some(k);
        }
        drop(i);
        self.steal()
    }

    /// Dynamic segmentation: split the biggest remaining segment and hand back its second half.
    /// mid is >= MIN_SEG past `next`, so the owner's in-flight chunk (<= CHUNK) never crosses it.
    fn steal(&self) -> Option<usize> {
        let mut i = self.info();
        if !i.ranges {
            return None;
        }
        let (k, best) = i.segs.iter().enumerate().max_by_key(|(_, s)| s.left())?;
        if best.left() < 2 * MIN_SEG {
            return None;
        }
        let mid = best.next + best.left() / 2;
        let stop = best.stop;
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

    /// Tiny HTTP server: paths starting with /r honour Range, others always send 200 + full body.
    fn serve(data: Arc<Vec<u8>>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let data = data.clone();
                thread::spawn(move || {
                    let mut r = BufReader::new(&s);
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let path = line.split(' ').nth(1).unwrap_or("").to_string();
                    let mut range = None;
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some(v) = strip_ci(&h, "range: bytes=") {
                            let (a, b) = v.trim().split_once('-').unwrap();
                            let a: usize = a.parse().unwrap();
                            let b = b.parse().unwrap_or(data.len() - 1).min(data.len() - 1);
                            range = Some((a, b));
                        }
                    }
                    let mut w = &s;
                    let (head, body) = match range.filter(|_| path.starts_with("/r")) {
                        Some((a, b)) => (format!("HTTP/1.1 206 Partial\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nLast-Modified: Sun, 06 Nov 1994 08:49:37 GMT\r\n\r\n", data.len(), b + 1 - a), &data[a..=b]),
                        None => (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()), &data[..]),
                    };
                    let _ = w.write_all(head.as_bytes());
                    // "/rslow..." paths trickle ~1.3 MB/s per connection, so extra connections pay off
                    let step = if path.starts_with("/rslow") { 64 * 1024 } else { body.len().max(1) };
                    for c in body.chunks(step) {
                        if w.write_all(c).is_err() {
                            break;
                        }
                        if step < body.len() {
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                });
            }
        });
        base
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

    /// Range-aware HTTP server trickling ~1.3 MB/s per connection; logs each request's User-Agent.
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
                    let (mut range, mut line) = ((0, data.len() - 1), String::new());
                    while r.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                        if let Some(v) = strip_ci(&line, "user-agent: ") {
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
                        thread::sleep(Duration::from_millis(50));
                    }
                });
            }
        });
        (url, log)
    }

    fn lanes_run(lanes: Vec<(String, ureq::Agent)>, tag: &str) -> (Arc<Job>, Arc<Vec<u8>>, Arc<Mutex<Vec<String>>>, PathBuf) {
        let data: Arc<Vec<u8>> = Arc::new((0..4 * 1024 * 1024 + 3).map(|i: u32| (i * 17 % 249) as u8).collect());
        let (url, log) = serve_ua(data.clone());
        let dir = std::env::temp_dir().join(format!("ndm-test-lanes-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Job::new(Info { conns: 4, ..info(url, &dir) });
        *j.lanes.lock().unwrap() = lanes;
        j.start(agent("", "main"), Arc::new(Limiter::default()));
        let until = Instant::now() + Duration::from_secs(60);
        while j.running() && Instant::now() < until {
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
        let resp = call(ureq::AgentBuilder::new().build().get(&url)).unwrap();
        assert_eq!(resp.into_string().unwrap(), "ok");
    }

    #[test]
    fn steal_keeps_segments_contiguous() {
        let j = Job::new(Info { ranges: true, segs: vec![Seg { stop: Some(10 * MIN_SEG), owned: true, ..Default::default() }], ..Default::default() });
        let k = j.steal().unwrap();
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
