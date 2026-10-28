//! How a TCP connection leaves the computer: direct, through SOCKS, or bound to one network.
//! ureq's agents can't bind a socket and always send names to a SOCKS 5 proxy, so the agents that
//! need either go through NDM's own SOCKS 5 server on 127.0.0.1 (`local_proxy`).

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Opens a TCP connection to host:port by `route`:
/// ""                      direct
/// "socks5h://[u:p@]h:p"   SOCKS 5, the proxy looks the name up
/// "socks5://[u:p@]h:p"    SOCKS 5, NDM looks the name up and sends the address
/// "socks4://h:p"          SOCKS 4 (always an address)
/// "nic://<name>"          bound to that network
/// Connect timeout 20 s, like the engine's agents.
pub fn connect(route: &str, host: &str, port: u16) -> io::Result<TcpStream> {
    let route = route.trim();
    if route.is_empty() {
        return connect_any(lookup(host, port)?, |a| TcpStream::connect_timeout(&a, CONNECT_TIMEOUT));
    }
    let bad = |e: String| io::Error::new(io::ErrorKind::InvalidInput, e);
    let u = url::Url::parse(route).map_err(|e| bad(format!("bad route {route}: {e}")))?;
    let dec = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
    if u.scheme() == "nic" {
        let name = dec(u.host_str().unwrap_or_default());
        let nic = crate::win::nics().into_iter().find(|n| n.name == name).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("network {name} is gone")))?;
        let addrs: Vec<SocketAddr> = lookup(host, port)?.into_iter().filter(|a| nic.ips.iter().any(|ip| ip.is_ipv4() == a.is_ipv4())).collect();
        return connect_any(addrs, |a| connect_on(&nic, a));
    }
    let proxy = (u.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string(), u.port().unwrap_or(1080));
    let (user, pass) = (dec(u.username()), dec(u.password().unwrap_or_default()));
    let five = |target: socks::TargetAddr| {
        let proxy = (proxy.0.as_str(), proxy.1);
        if user.is_empty() { socks::Socks5Stream::connect(proxy, target) } else { socks::Socks5Stream::connect_with_password(proxy, target, &user, &pass) }.map(|s| s.into_inner())
    };
    match u.scheme() {
        "socks5h" => five(socks::TargetAddr::Domain(host.trim_matches(['[', ']']).to_string(), port)),
        "socks5" => five(socks::TargetAddr::Ip(lookup(host, port)?[0])),
        "socks4" => {
            let v4 = lookup(host, port)?.into_iter().find(|a| a.is_ipv4()).ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "SOCKS 4 needs an IPv4 address"))?;
            socks::Socks4Stream::connect((proxy.0.as_str(), proxy.1), v4, "").map(|s| s.into_inner())
        }
        s => Err(bad(format!("unknown route {s}"))),
    }
}

fn lookup(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let addrs: Vec<_> = (host.trim_matches(['[', ']']), port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host} has no address")));
    }
    Ok(addrs)
}

/// Tries each address in turn; the last error if none answers.
fn connect_any(addrs: Vec<SocketAddr>, f: impl Fn(SocketAddr) -> io::Result<TcpStream>) -> io::Result<TcpStream> {
    let mut err = io::Error::new(io::ErrorKind::AddrNotAvailable, "no address to connect to");
    for a in addrs {
        match f(a) {
            Ok(s) => return Ok(s),
            Err(e) => err = e,
        }
    }
    Err(err)
}

/// A network the computer is connected to: "Wi-Fi" on Windows, "wlp2s0" on Linux (also the device to bind).
#[derive(Clone, PartialEq, Debug)]
pub struct Nic {
    pub name: String,
    pub ips: Vec<std::net::IpAddr>,
}

/// A connection that leaves by `nic`: bound to its address of `addr`'s family (on Linux also to
/// the device, since Linux routes by destination, not by source address).
pub fn connect_on(nic: &Nic, addr: SocketAddr) -> io::Result<TcpStream> {
    use socket2::{Domain, SockAddr, Socket, Type};
    let ip = nic.ips.iter().find(|ip| ip.is_ipv4() == addr.is_ipv4()).ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, format!("{} has no address for {addr}", nic.name)))?;
    let s = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    #[cfg(target_os = "linux")]
    s.bind_device(Some(nic.name.as_bytes()))?;
    s.bind(&SockAddr::from(SocketAddr::new(*ip, 0)))?;
    s.connect_timeout(&SockAddr::from(addr), CONNECT_TIMEOUT)?;
    Ok(s.into())
}

/// Can NDM pick the network of a connection? Windows: always. Linux: binding to a device without
/// root needs kernel 5.7 or later, so it is tried once on "lo".
pub fn nic_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *OK.get_or_init(|| socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).and_then(|s| s.bind_device(Some(b"lo"))).is_ok())
    }
    #[cfg(not(target_os = "linux"))]
    true
}

/// NDM's own SOCKS 5 server on 127.0.0.1 whose connections leave by `route`:
/// "socks5://<token>:<token>@127.0.0.1:<port>". One server per route, started on first use.
pub fn local_proxy(route: &str) -> String {
    static SERVERS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let mut servers = SERVERS.lock().unwrap();
    if let Some((_, url)) = servers.iter().find(|(r, _)| r == route) {
        return url.clone();
    }
    let token = {
        use std::hash::BuildHasher;
        format!("{:016x}", std::collections::hash_map::RandomState::new().hash_one((route, std::time::SystemTime::now())))
    };
    let Ok(l) = TcpListener::bind("127.0.0.1:0") else { return String::new() };
    let url = format!("socks5://{token}:{token}@127.0.0.1:{}", l.local_addr().map(|a| a.port()).unwrap_or(0));
    let route = route.to_string();
    servers.push((route.clone(), url.clone()));
    thread::spawn(move || {
        for c in l.incoming().flatten() {
            let (route, token) = (route.clone(), token.clone());
            thread::spawn(move || {
                let mut c = c;
                if let Ok((_, host, port)) = socks_accept(&mut c, Some((&token, &token))) {
                    let out = connect(&route, &host, port);
                    if socks_reply(&mut c, out.as_ref().err()).is_ok() {
                        if let Ok(out) = out {
                            relay(c, out);
                        }
                    }
                }
            });
        }
    });
    url
}

/// The proxy string to hand `engine::agent` for `route` (`Settings::proxy_for`'s answer).
pub fn agent_proxy(route: &str) -> String {
    let route = route.trim();
    if let Some(rest) = route.strip_prefix("socks5h://") {
        format!("socks5://{rest}") // ureq sends names to SOCKS 5
    } else if route.starts_with("socks5://") {
        local_proxy(route)
    } else if route.starts_with("ftp://") || route.starts_with("nic://") {
        String::new()
    } else {
        route.to_string()
    }
}

/// The server side of one SOCKS 5 client up to its CONNECT: the login is checked when `login` is
/// given (else no login is asked). Returns the address type and the target.
fn socks_accept(c: &mut TcpStream, login: Option<(&str, &str)>) -> io::Result<(u8, String, u16)> {
    let fail = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    c.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut b = [0u8; 2];
    c.read_exact(&mut b)?;
    if b[0] != 5 {
        return Err(fail("not SOCKS 5"));
    }
    let mut methods = vec![0u8; b[1] as usize];
    c.read_exact(&mut methods)?;
    let want = if login.is_some() { 2 } else { 0 };
    if !methods.contains(&want) {
        c.write_all(&[5, 0xFF])?;
        return Err(fail("no usable login method"));
    }
    c.write_all(&[5, want])?;
    if let Some((user, pass)) = login {
        // 01 ulen user plen pass
        let mut ver = [0u8; 1];
        c.read_exact(&mut ver)?;
        let mut field = || -> io::Result<Vec<u8>> {
            let mut n = [0u8; 1];
            c.read_exact(&mut n)?;
            let mut v = vec![0u8; n[0] as usize];
            c.read_exact(&mut v)?;
            Ok(v)
        };
        let (u, p) = (field()?, field()?);
        let ok = u == user.as_bytes() && p == pass.as_bytes();
        c.write_all(&[1, if ok { 0 } else { 1 }])?;
        if !ok {
            return Err(fail("wrong login"));
        }
    }
    let mut h = [0u8; 4];
    c.read_exact(&mut h)?;
    let host = match h[3] {
        1 => {
            let mut a = [0u8; 4];
            c.read_exact(&mut a)?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        4 => {
            let mut a = [0u8; 16];
            c.read_exact(&mut a)?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            c.read_exact(&mut n)?;
            let mut a = vec![0u8; n[0] as usize];
            c.read_exact(&mut a)?;
            String::from_utf8_lossy(&a).into_owned()
        }
        _ => return Err(fail("bad address type")),
    };
    let mut p = [0u8; 2];
    c.read_exact(&mut p)?;
    if h[1] != 1 {
        c.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])?;
        return Err(fail("only CONNECT"));
    }
    c.set_read_timeout(None)?;
    Ok((h[3], host, u16::from_be_bytes(p)))
}

/// Answers a CONNECT: success, or 05 refused / 04 unreachable / 01 other.
fn socks_reply(c: &mut TcpStream, err: Option<&io::Error>) -> io::Result<()> {
    use io::ErrorKind::*;
    let code = match err.map(|e| e.kind()) {
        None => 0,
        Some(ConnectionRefused) => 5,
        Some(HostUnreachable | NetworkUnreachable | NotFound | TimedOut | AddrNotAvailable) => 4,
        Some(_) => 1,
    };
    c.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
}

/// Copies both ways until each side ends; an end on one side is passed on as a half close.
fn relay(a: TcpStream, b: TcpStream) {
    let (Ok(a2), Ok(b2)) = (a.try_clone(), b.try_clone()) else { return };
    let one = |mut from: TcpStream, mut to: TcpStream| {
        let _ = io::copy(&mut from, &mut to);
        let _ = to.shutdown(Shutdown::Write);
    };
    let t = thread::spawn(move || one(a, b2));
    one(b, a2);
    let _ = t.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A loopback HTTP server answering "hello" to every request.
    fn hello() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut b = [0u8; 4096];
                let _ = c.read(&mut b);
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
            }
        });
        port
    }

    /// A SOCKS 5 server that asks for `login` when given one, records the address type of every CONNECT, and relays.
    fn fake_socks(login: Option<(&'static str, &'static str)>) -> (u16, Arc<Mutex<Vec<u8>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(vec![]));
        let seen2 = seen.clone();
        thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let seen = seen2.clone();
                thread::spawn(move || {
                    if let Ok((atyp, host, port)) = socks_accept(&mut c, login) {
                        seen.lock().unwrap().push(atyp);
                        let out = connect("", &host, port);
                        if socks_reply(&mut c, out.as_ref().err()).is_ok() {
                            relay(c, out.unwrap());
                        }
                    }
                });
            }
        });
        (port, seen)
    }

    fn get(s: &mut TcpStream) -> String {
        s.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut t = String::new();
        s.read_to_string(&mut t).unwrap();
        t
    }

    #[test]
    fn agent_proxy_maps() {
        assert_eq!(agent_proxy("socks5h://u:p@sx:1080"), "socks5://u:p@sx:1080");
        let local = agent_proxy("socks5://sx:1080");
        assert!(local.starts_with("socks5://") && local.contains("@127.0.0.1:"), "{local}");
        assert_eq!(agent_proxy("socks5://sx:1080"), local, "one server per route");
        assert_eq!(agent_proxy("ftp://fx:21"), "");
        assert_eq!(agent_proxy("http://px:3128"), "http://px:3128");
        assert_eq!(agent_proxy(""), "");
    }

    #[test]
    fn local_proxy_relays_http() {
        let url = format!("http://localhost:{}/", hello());
        assert_eq!(crate::engine::agent(&local_proxy(""), "").get(&url).call().unwrap().into_string().unwrap(), "hello");
    }

    #[test]
    fn local_proxy_wants_its_login() {
        let u = url::Url::parse(&local_proxy("")).unwrap();
        let addr = ("127.0.0.1", u.port().unwrap());
        let target = ("127.0.0.1", hello());
        assert!(socks::Socks5Stream::connect_with_password(addr, target, "x", "y").is_err());
        assert!(socks::Socks5Stream::connect(addr, target).is_err());
        let mut s = socks::Socks5Stream::connect_with_password(addr, target, u.username(), u.password().unwrap()).unwrap().into_inner();
        assert!(get(&mut s).ends_with("hello"));
    }

    #[test]
    fn socks5_name_lookup_side() {
        let port = hello();
        let (f, atyp) = fake_socks(None);
        assert!(get(&mut connect(&format!("socks5h://127.0.0.1:{f}"), "localhost", port).unwrap()).ends_with("hello"));
        assert!(get(&mut connect(&format!("socks5://127.0.0.1:{f}"), "localhost", port).unwrap()).ends_with("hello"));
        let a = atyp.lock().unwrap().clone();
        assert_eq!(a[0], 3); // the name went to the proxy
        assert!(a[1] == 1 || a[1] == 4); // an address went to the proxy
    }

    #[test]
    fn socks5_login_passes_through() {
        let port = hello();
        let (f, _) = fake_socks(Some(("u", "p")));
        assert!(connect(&format!("socks5://u:p@127.0.0.1:{f}"), "localhost", port).is_ok());
        assert!(connect(&format!("socks5://u:x@127.0.0.1:{f}"), "localhost", port).is_err());
        assert!(connect(&format!("bogus://127.0.0.1:{f}"), "localhost", port).is_err());
    }

    #[test]
    fn connect_on_binds() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let lo = Nic { name: if cfg!(windows) { "Loopback".into() } else { "lo".into() }, ips: vec![std::net::Ipv4Addr::LOCALHOST.into()] };
        if !nic_supported() {
            return; // Linux before 5.7
        }
        let s = connect_on(&lo, l.local_addr().unwrap()).unwrap();
        assert_eq!(s.local_addr().unwrap().ip(), std::net::Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn nics_are_sane() {
        for n in crate::win::nics() {
            assert!(!n.name.is_empty() && !n.ips.is_empty() && n.ips.iter().all(|ip| !ip.is_loopback()), "{n:?}");
        }
    }
}
