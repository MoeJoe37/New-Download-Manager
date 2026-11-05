//! What Linux (`linux.rs`) and macOS (`mac.rs`) share of `win.rs`'s functions: Firefox's cache, PAC
//! scripts, the networks (getifaddrs), the clipboard, the decimal separator, the single-instance check and
//! the browser extension's native messaging host.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

// ---------- browser cache ----------

/// Firefox's cached copy of `url` in the profiles under `roots`, copied out to a temporary file: (file, its
/// Last-Modified).
// ponytail: reads every cache entry per lookup; keep an index if big caches make it slow
pub fn firefox_url_cache(roots: &[PathBuf], url: &str) -> Option<(PathBuf, String)> {
    let profiles: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flatten()
        .flatten()
        .map(|p| p.path())
        .collect();
    profiles
        .iter()
        .find_map(|p| firefox_cache(&p.join("cache2/entries"), url))
}

fn firefox_cache(entries: &Path, url: &str) -> Option<(PathBuf, String)> {
    for e in std::fs::read_dir(entries).ok()?.flatten() {
        let Ok(data) = std::fs::read(e.path()) else {
            continue;
        };
        let Some((body, key, head)) = cache2_entry(&data) else {
            continue;
        };
        // the key is "[origin attributes,]a,:<url>"; with a Content-Encoding the stored body isn't the file
        let key_url = key
            .find(":http")
            .or_else(|| key.find(":ftp"))
            .map(|i| &key[i + 1..]);
        if key_url != Some(url)
            || header(&head, "content-encoding")
                .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
        {
            continue;
        }
        let modified = header(&head, "last-modified")?;
        let out =
            std::env::temp_dir().join(format!("ndm-cache-{}", e.file_name().to_string_lossy()));
        std::fs::write(&out, body).ok()?;
        return Some((out, modified));
    }
    None
}

/// One cache2 file: (body, key, response head). The last 4 bytes say where the metadata starts: a 4-byte
/// hash and 2 bytes per 256 KiB chunk, eight 32-bit fields, the key and a 0, then "name\0value\0" pairs.
fn cache2_entry(d: &[u8]) -> Option<(&[u8], String, String)> {
    let be = |at: usize| {
        d.get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let meta = be(d.len().checked_sub(4)?)?;
    let hdr = meta + 4 + 2 * meta.div_ceil(262144);
    let key_len = be(hdr + 24)?;
    let key = String::from_utf8_lossy(d.get(hdr + 32..hdr + 32 + key_len)?).into_owned();
    let elems = d.get(hdr + 33 + key_len..d.len() - 4)?;
    let mut it = elems.split(|b| *b == 0);
    let mut head = String::new();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if k == b"response-head" {
            head = String::from_utf8_lossy(v).into_owned();
        }
    }
    Some((&d[..meta], key, head))
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    })
}

#[test]
fn firefox_cache_entry() {
    let dir = std::env::temp_dir().join(format!("ndm-test-ff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (body, key) = (
        b"cached copy".to_vec(),
        b"O^partitionKey=x,a,:http://h/f.txt".to_vec(),
    );
    let mut f = body.clone();
    f.extend([0u8; 6]); // hash + one chunk's hash
    for v in [3u32, 1, 0, 0, 0, 0, key.len() as u32, 0] {
        f.extend(v.to_be_bytes());
    }
    f.extend(&key);
    f.push(0);
    f.extend(
        b"response-head\0HTTP/1.1 200 OK\r\nLast-Modified: Tue, 01 Mar 2022 10:00:00 GMT\r\n\0",
    );
    f.extend((body.len() as u32).to_be_bytes());
    std::fs::write(dir.join("ABC"), f).unwrap();
    let (file, since) = firefox_cache(&dir, "http://h/f.txt").expect("found");
    assert_eq!(
        (std::fs::read(file).unwrap(), since.as_str()),
        (body, "Tue, 01 Mar 2022 10:00:00 GMT")
    );
    assert!(firefox_cache(&dir, "http://h/other").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- PAC scripts ----------

/// The PAC helper functions browsers provide (Netscape's list); dnsResolve and myIpAddress come from Rust.
const PAC_UTILS: &str = r#"
function dnsDomainIs(h, d) { return h.length >= d.length && h.substring(h.length - d.length) == d; }
function dnsDomainLevels(h) { return h.split('.').length - 1; }
function isPlainHostName(h) { return h.indexOf('.') < 0; }
function isResolvable(h) { return dnsResolve(h) != null; }
function localHostOrDomainIs(h, hd) { return h == hd || hd.lastIndexOf(h + '.', 0) == 0; }
function _ip(a) { var b = a.split('.'); return ((b[0] << 24) | (b[1] << 16) | (b[2] << 8) | (b[3] | 0)) >>> 0; }
function isInNet(h, pat, mask) {
  if (!/^\d+\.\d+\.\d+\.\d+$/.test(h)) { h = dnsResolve(h); if (h == null) return false; }
  var m = _ip(mask); return ((_ip(h) & m) >>> 0) == ((_ip(pat) & m) >>> 0);
}
function shExpMatch(s, p) {
  return new RegExp('^' + p.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.') + '$').test(s);
}
function _gmt(a) { return a.length > 0 && a[a.length - 1] == 'GMT'; }
function weekdayRange() {
  var days = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'], a = arguments, g = _gmt(a), n = g ? a.length - 1 : a.length, d = new Date();
  var t = g ? d.getUTCDay() : d.getDay(), d1 = days.indexOf(a[0]), d2 = n > 1 ? days.indexOf(a[1]) : d1;
  return d1 <= d2 ? t >= d1 && t <= d2 : t >= d1 || t <= d2;
}
function timeRange() {
  var a = arguments, g = _gmt(a), n = g ? a.length - 1 : a.length, d = new Date();
  var s = (g ? d.getUTCHours() : d.getHours()) * 3600 + (g ? d.getUTCMinutes() : d.getMinutes()) * 60 + (g ? d.getUTCSeconds() : d.getSeconds());
  var v = [].slice.call(a, 0, n).map(Number), lo, hi;
  if (n == 1) { lo = v[0] * 3600; hi = lo + 3599; }
  else if (n == 2) { lo = v[0] * 3600; hi = v[1] == v[0] ? lo + 3599 : v[1] * 3600 - 1; }
  else if (n == 4) { lo = v[0] * 3600 + v[1] * 60; hi = v[2] * 3600 + v[3] * 60; }
  else if (n == 6) { lo = v[0] * 3600 + v[1] * 60 + v[2]; hi = v[3] * 3600 + v[4] * 60 + v[5]; }
  else return false;
  return lo <= hi ? s >= lo && s <= hi : s >= lo || s <= hi;
}
function dateRange() {
  // each argument is a day (1-31), a month name or a year (> 31); compared on the fields given
  var mon = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
  var a = arguments, g = _gmt(a), n = g ? a.length - 1 : a.length, d = new Date();
  var now = [g ? d.getUTCFullYear() : d.getFullYear(), g ? d.getUTCMonth() : d.getMonth(), g ? d.getUTCDate() : d.getDate()];
  var ps = [].slice.call(a, 0, n).map(function (x) { var m = mon.indexOf(x); return m >= 0 ? [1, m] : x > 31 ? [0, +x] : [2, +x]; });
  var half = n > 1 ? n / 2 : 1, from = ps.slice(0, half), to = n > 1 ? ps.slice(half) : from;
  var used = [false, false, false]; from.forEach(function (p) { used[p[0]] = true; });
  function key(list) { var k = now.slice(); (list || []).forEach(function (p) { k[p[0]] = p[1]; });
    var r = 0; for (var i = 0; i < 3; i++) if (used[i]) r = r * 10000 + k[i]; return r; }
  var lo = key(from), hi = key(to), c = key(null);
  return lo <= hi ? c >= lo && c <= hi : c >= lo || c <= hi;
}
"#;

/// The proxy ("host:port") a PAC script picks for `url`, run by QuickJS; None = DIRECT or failure.
pub fn pac_proxy(pac: &str, url: &str) -> Option<String> {
    // each script is fetched once (WinHTTP keeps its copy too)
    static SCRIPTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let cached = SCRIPTS
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.0 == pac)
        .map(|s| s.1.clone());
    let script = match cached {
        Some(s) => s,
        None => {
            let s = match pac
                .strip_prefix("file://")
                .or_else(|| pac.starts_with('/').then_some(pac))
            {
                Some(p) => std::fs::read_to_string(p).ok()?,
                None => ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()
                    .get(pac)
                    .call()
                    .ok()?
                    .into_string()
                    .ok()?,
            };
            SCRIPTS.lock().unwrap().push((pac.to_string(), s.clone()));
            s
        }
    };
    let host = url::Url::parse(url).ok()?.host_str()?.to_string();
    pac_pick(&find_proxy(&script, url, &host)?)
}

fn find_proxy(script: &str, url: &str, host: &str) -> Option<String> {
    use rquickjs::{Context, Function, Runtime};
    let rt = Runtime::new().ok()?;
    let ctx = Context::full(&rt).ok()?;
    ctx.with(|ctx| {
        let g = ctx.globals();
        let resolve = |h: String| -> Option<String> {
            use std::net::ToSocketAddrs;
            (h.as_str(), 0)
                .to_socket_addrs()
                .ok()?
                .find(|a| a.is_ipv4())
                .map(|a| a.ip().to_string())
        };
        // the address outgoing traffic leaves from (connecting a UDP socket sends nothing)
        let my_ip = || -> String {
            let s = std::net::UdpSocket::bind("0.0.0.0:0").ok();
            s.and_then(|s| {
                s.connect("8.8.8.8:53")
                    .ok()
                    .and_then(|_| s.local_addr().ok())
            })
            .map_or("127.0.0.1".into(), |a| a.ip().to_string())
        };
        g.set("dnsResolve", Function::new(ctx.clone(), resolve).ok()?)
            .ok()?;
        g.set("myIpAddress", Function::new(ctx.clone(), my_ip).ok()?)
            .ok()?;
        ctx.eval::<(), _>(PAC_UTILS).ok()?;
        ctx.eval::<(), _>(script).ok()?;
        let f: Function = g.get("FindProxyForURL").ok()?;
        f.call::<_, String>((url, host)).ok()
    })
}

/// First PAC route. An empty result explicitly requests DIRECT; None means evaluation failed.
fn pac_pick(answer: &str) -> Option<String> {
    let first = answer.split(';').map(str::trim).find(|e| !e.is_empty())?;
    let (kind, hp) = first.split_once(char::is_whitespace).unwrap_or((first, ""));
    let host = hp.trim();
    match kind.to_ascii_uppercase().as_str() {
        "DIRECT" => Some(String::new()),
        "PROXY" | "HTTP" if !host.is_empty() => Some(host.into()),
        "HTTPS" if !host.is_empty() => Some(format!("https://{host}")),
        "SOCKS" | "SOCKS5" if !host.is_empty() => Some(format!("socks5h://{host}")),
        "SOCKS4" if !host.is_empty() => Some(format!("socks4://{host}")),
        _ => None,
    }
}

#[test]
fn pac_scripts() {
    let s = r#"function FindProxyForURL(url, host) {
        if (isPlainHostName(host) || shExpMatch(host, "*.local") || isInNet(host, "10.0.0.0", "255.0.0.0")) return "DIRECT";
        if (dnsDomainIs(host, ".example.com") && weekdayRange("SUN", "SAT") && timeRange(0, 24) && dateRange(1, 31)) return "PROXY px.example.com:3128; DIRECT";
        return "SOCKS s:1080";
    }"#;
    let pick = |host: &str| {
        pac_pick(&find_proxy(s, &format!("http://{host}/f"), host).expect("script runs"))
    };
    assert_eq!(
        pick("a.example.com").as_deref(),
        Some("px.example.com:3128")
    );
    for host in ["intranet", "box.local", "10.1.2.3"] {
        assert_eq!(pick(host).as_deref(), Some(""), "{host}");
    }
    assert_eq!(pick("other.org").as_deref(), Some("socks5h://s:1080"));
    assert_eq!(pac_pick("bad route"), None);
}

// ---------- clipboard and locale ----------

/// X11, Wayland and macOS keep no clipboard change counter NDM can read: a hash of the clipboard text
/// stands in for it.
pub fn clip_seq() -> u32 {
    use std::hash::{Hash, Hasher};
    thread_local!(static CLIP: std::cell::RefCell<Option<arboard::Clipboard>> = std::cell::RefCell::new(arboard::Clipboard::new().ok()));
    let text = CLIP
        .with(|c| c.borrow_mut().as_mut().and_then(|c| c.get_text().ok()))
        .unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish() as u32
}

/// The locale's decimal separator (LC_NUMERIC), "." if unknown.
pub fn decimal_sep() -> &'static str {
    static SEP: OnceLock<String> = OnceLock::new();
    SEP.get_or_init(|| {
        let o = Command::new("locale")
            .arg("decimal_point")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if o.is_empty() {
            ".".into()
        } else {
            o
        }
    })
}

// ---------- networks ----------

/// The networks (`crate::net::Nic`) among `routed`, the devices with a default route: up, not loopback,
/// with an address that isn't link-local.
pub fn ifaddrs(routed: &[String]) -> Vec<crate::net::Nic> {
    let mut out: Vec<crate::net::Nic> = vec![];
    // SAFETY: getifaddrs' list is only read before freeifaddrs; each ifa_addr is checked for null and its family before the cast
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut p = list;
        while !p.is_null() {
            let a = &*p;
            p = a.ifa_next;
            let flags = a.ifa_flags as i32;
            if a.ifa_addr.is_null()
                || flags & libc::IFF_UP == 0
                || flags & libc::IFF_RUNNING == 0
                || flags & libc::IFF_LOOPBACK != 0
            {
                continue;
            }
            let ip: std::net::IpAddr = match (*a.ifa_addr).sa_family as i32 {
                libc::AF_INET => std::net::Ipv4Addr::from(u32::from_be(
                    (*(a.ifa_addr as *const libc::sockaddr_in)).sin_addr.s_addr,
                ))
                .into(),
                libc::AF_INET6 => {
                    let v6 = std::net::Ipv6Addr::from(
                        (*(a.ifa_addr as *const libc::sockaddr_in6))
                            .sin6_addr
                            .s6_addr,
                    );
                    if v6.segments()[0] & 0xffc0 == 0xfe80 {
                        continue; // link-local
                    }
                    v6.into()
                }
                _ => continue,
            };
            let name = std::ffi::CStr::from_ptr(a.ifa_name)
                .to_string_lossy()
                .into_owned();
            if !routed.contains(&name) {
                continue;
            }
            match out.iter_mut().find(|n| n.name == name) {
                Some(n) => n.ips.push(ip),
                None => out.push(crate::net::Nic {
                    name,
                    ips: vec![ip],
                }),
            }
        }
        libc::freeifaddrs(list);
    }
    out
}

// ---------- the running copy and the browser extension ----------

/// Started by a browser as the extension's native messaging host (its origin or manifest as the argument).
pub fn is_host_launch(args: &[String]) -> bool {
    args.first().is_some_and(|a| {
        a.starts_with("chrome-extension://") || a.to_ascii_lowercase().ends_with(".json")
    })
}

/// The native messaging host: reads the extension's message, starts the app (`exe`, with `clean_env`'s
/// environment) unless it's running, answers.
pub fn native_host(title: &str, exe: Option<PathBuf>, clean_env: fn(&mut Command)) {
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    let mut len = [0u8; 4];
    if std::io::stdin().read_exact(&mut len).is_ok() {
        let _ = std::io::stdin()
            .take(u32::from_le_bytes(len) as u64)
            .read_to_end(&mut Vec::new());
    }
    if !running(title, false) {
        if let Some(exe) = exe {
            // its own process group, so closing the browser doesn't close the app
            let mut c = Command::new(exe);
            clean_env(&mut c);
            let _ = c
                .arg("/tray")
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    let reply = br#"{"ok":true}"#;
    let mut out = std::io::stdout();
    let _ = out
        .write_all(&(reply.len() as u32).to_le_bytes())
        .and_then(|_| out.write_all(reply))
        .and_then(|_| out.flush());
}

/// Is NDM running? Its browser endpoint answers on 127.0.0.1 (`core::browser_bridge::running_port`). With `activate` the
/// running copy is asked to show its window (POST /show).
pub fn running(_title: &str, activate: bool) -> bool {
    let Some(port) = crate::core::browser_bridge::running_port() else {
        return false;
    };
    if activate {
        let _ = ureq::post(&format!("http://127.0.0.1:{port}/show"))
            .set("X-NDM", "1")
            .timeout(std::time::Duration::from_secs(3))
            .call();
    }
    true
}
