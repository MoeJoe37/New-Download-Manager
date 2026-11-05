use crate::engine::{Arc, Body, BufRead, BufReader, Duration, Mutex, Read, TcpStream, Write, b64, pct_decode, strip_ci};
use crate::transport::client::UA;

pub(super) fn is_ftp(url: &str) -> bool {
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
pub(super) fn ftp_open(url: &str, from: u64) -> Result<(Option<u64>, bool, Body), String> {
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
pub(super) fn ftp_http(url: &str, from: u64, proxy: &str) -> Result<(Option<u64>, bool, Body), String> {
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
