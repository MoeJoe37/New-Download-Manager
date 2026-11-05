//! HTTP transport for downloads. Direct TLS connections share a pool that negotiates HTTP/2.
//! Proxy routes and connection-oriented authentication retain the original HTTP/1.1 client.
pub(crate) mod auth;
pub(crate) mod client;

#[cfg(test)]
use self::client::client_builder;
use self::client::{call_with, tls_client};
use std::collections::HashSet;
use std::io::{self, Read};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct Agent {
    pub(crate) legacy: ureq::Agent,
    no_redirect: ureq::Agent,
    pub(crate) proxy: String,
    user_agent: String,
    pub(crate) invalid_proxy: bool,
    http1_hosts: Arc<[url::Host<String>]>,
    pub(crate) routes: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

impl Agent {
    pub(crate) fn new(
        legacy: ureq::Agent,
        no_redirect: ureq::Agent,
        proxy: &str,
        user_agent: &str,
        invalid_proxy: bool,
    ) -> Self {
        Self {
            legacy,
            no_redirect,
            proxy: proxy.into(),
            user_agent: user_agent.into(),
            invalid_proxy,
            http1_hosts: Arc::default(),
            routes: None,
        }
    }

    pub(crate) fn with_http1_hosts(mut self, hosts: &str) -> Self {
        self.http1_hosts = hosts
            .split_whitespace()
            .filter_map(|host| url::Host::parse(host).ok())
            .collect();
        self
    }

    fn http1_only(&self, url: &Url) -> bool {
        url.host()
            .is_some_and(|host| self.http1_hosts.iter().any(|entry| entry == &host))
    }

    fn for_url(&self, url: &str) -> Self {
        let Some(routes) = &self.routes else {
            return self.clone();
        };
        let proxy = crate::net::agent_proxy(&routes(url));
        if proxy == self.proxy {
            return self.clone();
        }
        let mut agent = client::agent(&proxy, &self.user_agent);
        agent.http1_hosts = self.http1_hosts.clone();
        agent.routes = self.routes.clone();
        agent
    }
}

impl std::ops::Deref for Agent {
    type Target = ureq::Agent;
    fn deref(&self) -> &Self::Target {
        &self.legacy
    }
}

pub struct Request {
    agent: Agent,
    url: String,
    headers: Vec<(String, String)>,
    https_only: bool,
    prefer_http1: bool,
}

impl Request {
    pub(crate) fn https_only(mut self) -> Self {
        self.https_only = true;
        self
    }

    /// Extra requests can use separate TCP connections on hosts that accept HTTP/1.1.
    pub(crate) fn prefer_http1(mut self, extra: bool) -> Self {
        self.prefer_http1 = extra;
        self
    }

    pub fn set(mut self, key: &str, value: &str) -> Self {
        self.headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case(key));
        self.headers.push((key.into(), value.into()));
        self
    }
}

pub fn request(agent: &Agent, url: &str, referrer: &str, cookies: &str) -> Request {
    let mut request = Request {
        agent: agent.for_url(url),
        url: url.into(),
        headers: vec![],
        https_only: false,
        prefer_http1: false,
    };
    if let (Ok(source), Ok(mut page)) = (Url::parse(url), Url::parse(referrer)) {
        if matches!(page.scheme(), "http" | "https")
            && !(page.scheme() == "https" && source.scheme() == "http")
        {
            if source.origin() == page.origin() {
                page.set_fragment(None);
                let _ = page.set_password(None);
                let _ = page.set_username("");
                request = request.set("Referer", page.as_str());
            } else {
                request = request.set(
                    "Referer",
                    &format!("{}/", page.origin().ascii_serialization()),
                );
            }
        }
    }
    if !cookies.is_empty() {
        request = request.set("Cookie", cookies);
    }
    request.set("Accept-Encoding", "identity")
}

pub(crate) fn same_origin(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// Strip query strings and user info from downloader diagnostics, including third-party errors.
pub(crate) fn redact(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|word| {
            if let Some(at) = word.find("://") {
                let start = word[..at]
                    .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                    .len();
                let end = word.trim_end_matches(char::is_whitespace).len();
                if let Ok(url) =
                    Url::parse(word[start..end].trim_end_matches(['"', '\'', ')', ',', ']']))
                {
                    return format!(
                        "{}{}{}",
                        &word[..start],
                        url.origin().ascii_serialization(),
                        &word[end..]
                    );
                }
                return format!("{}[URL]{}", &word[..start], &word[end..]);
            }
            word.to_string()
        })
        .collect()
}

pub enum Response {
    Legacy(ureq::Response),
    Tls(reqwest::blocking::Response),
}

impl Response {
    pub fn status(&self) -> u16 {
        match self {
            Self::Legacy(r) => r.status(),
            Self::Tls(r) => r.status().as_u16(),
        }
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        match self {
            Self::Legacy(r) => r.header(name),
            Self::Tls(r) => r.headers().get(name)?.to_str().ok(),
        }
    }
    pub fn all(&self, name: &str) -> Vec<&str> {
        match self {
            Self::Legacy(r) => r.all(name),
            Self::Tls(r) => r
                .headers()
                .get_all(name)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .collect(),
        }
    }
    pub fn get_url(&self) -> &str {
        match self {
            Self::Legacy(r) => r.get_url(),
            Self::Tls(r) => r.url().as_str(),
        }
    }
    pub fn into_reader(self) -> Box<dyn Read + Send> {
        match self {
            Self::Legacy(r) => r.into_reader(),
            Self::Tls(r) => Box::new(r),
        }
    }
    pub fn into_string(self) -> io::Result<String> {
        let mut text = String::new();
        self.into_reader()
            .take((8 << 20) + 1)
            .read_to_string(&mut text)?;
        if text.len() > 8 << 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "media manifest exceeds 8 MiB",
            ));
        }
        Ok(text)
    }
}

pub(crate) fn http_error(code: u16) -> String {
    let reason = reqwest::StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("Request failed");
    format!("HTTP {code} {reason}")
}

pub fn call(request: Request) -> Result<Response, String> {
    call_with(request, tls_client)
}

/// The legacy SOCKS client passes hostnames to SOCKS5. yt-dlp needs the explicit remote-DNS
/// spelling for the equivalent route, including our local forwarding proxy.
pub(crate) fn external_proxy(proxy: &str) -> String {
    proxy
        .strip_prefix("socks5://")
        .map_or_else(|| proxy.to_string(), |host| format!("socks5h://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::Arc;

    fn head(socket: &TcpStream) -> String {
        let mut reader = BufReader::new(socket);
        let mut result = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            result.push_str(&line);
        }
        result.to_ascii_lowercase()
    }

    #[test]
    fn redirect_keeps_same_origin_context_and_strips_it_at_another_port() {
        let source = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/first", source.local_addr().unwrap());
        let other = format!("http://{}/last?token=fixture", target.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for location in ["/second", &other] {
                let (mut socket, _) = source.accept().unwrap();
                let request = head(&socket);
                assert!(request.contains("cookie: session=test"));
                assert!(request.contains("authorization: bearer example"));
                assert!(request.contains("x-signed: private"));
                write!(socket, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            let (mut socket, _) = target.accept().unwrap();
            let request = head(&socket);
            assert!(request.contains("/last?token=fixture"));
            assert!(request.contains("range: bytes=0-2"));
            for header in ["authorization:", "cookie:", "referer:", "x-signed:"] {
                assert!(!request.contains(header), "{request}");
            }
            socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 0-2/3\r\n\r\nabc").unwrap();
        });
        let agent = client::agent("", "");
        let request = request(&agent, &url, &url, "session=test")
            .set("Authorization", "Bearer example")
            .set("X-Signed", "private")
            .set("Range", "bytes=0-2");
        assert_eq!(call(request).unwrap().into_string().unwrap(), "abc");
        server.join().unwrap();
    }

    #[test]
    fn permanent_http_errors_and_bad_proxy_do_not_bypass_the_route() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/signed?token=secret",
            server.local_addr().unwrap()
        );
        let invalid = client::agent("invalid-proxy://", "");
        assert_eq!(
            call(request(&invalid, &url, "", "")).err().unwrap(),
            "Invalid proxy configuration"
        );
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let _ = head(&socket);
            socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let agent = client::agent("", "");
        let error = call(request(&agent, &url, "", "")).err().unwrap();
        assert!(error.starts_with("HTTP 403 Forbidden"));
        assert!(crate::engine::range::permanent_error(&error));
        assert!(!error.contains("secret"));
        assert!(!redact(&format!("Error fetching {url}")).contains("secret"));
        worker.join().unwrap();
    }

    #[test]
    fn redirect_uses_configured_route_for_the_new_address() {
        let source = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/initial", source.local_addr().unwrap());
        let route = format!("http://{}", proxy.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = source.accept().unwrap();
            let _ = head(&socket);
            socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://private.invalid/signed\r\nContent-Length: 0\r\n\r\n").unwrap();
            let (mut socket, _) = proxy.accept().unwrap();
            let request = head(&socket);
            assert!(request.starts_with("get http://private.invalid/signed http/1.1"));
            assert!(!request.contains("cookie:"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc")
                .unwrap();
        });
        let mut agent = client::agent("", "");
        agent.routes = Some(Arc::new(move |url| {
            if url.starts_with("http://private.invalid/") {
                route.clone()
            } else {
                String::new()
            }
        }));
        let reply = call(request(&agent, &url, "", "session=private")).unwrap();
        assert_eq!(reply.into_string().unwrap(), "abc");
        server.join().unwrap();
        assert_eq!(
            redact("…https://example.com/path?token=secret"),
            "…https://example.com"
        );
    }

    #[test]
    fn https_connect_forwards_proxy_credentials_and_reports_407() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let route = format!("http://alice:secret@{}", proxy.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = proxy.accept().unwrap();
            let headers = head(&socket);
            assert!(headers.starts_with("connect private.invalid:443 http/1.1"));
            assert!(headers.contains("proxy-authorization: basic ywxpy2u6c2vjcmv0"));
            socket
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
        });
        let agent = client::agent(&route, "");
        let error = call(request(&agent, "https://private.invalid/media", "", ""))
            .err()
            .unwrap();
        assert_eq!(error, "HTTP 407 Proxy Authentication Required");
        server.join().unwrap();
    }

    #[test]
    fn legacy_https_accepts_a_configured_root_and_rejects_an_unknown_root() {
        let (config, der) = crate::test_support::localhost_tls(&[]);
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // Userinfo selects the HTTP/1.1 transport, as do authenticated proxy routes.
        let url = format!(
            "https://user:pass@localhost:{}/file",
            listener.local_addr().unwrap().port()
        );
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut tls = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone()).unwrap(),
                    socket,
                );
                let mut headers = BufReader::new(&mut tls);
                let mut line = String::new();
                let read = headers.read_line(&mut line);
                if index == 0 {
                    assert!(read.is_err() || matches!(read, Ok(0)));
                    continue;
                }
                assert!(read.is_ok() && line.starts_with("GET /file HTTP/1.1"));
                while headers.read_line(&mut String::new()).is_ok_and(|n| n > 2) {}
                tls.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                )
                .unwrap();
                tls.flush().unwrap();
            }
        });
        let default = client::agent("", "");
        assert!(call(request(&default, &url, "", "")).is_err());
        let trusted = client::agent_with_tls("", "", client::tls_config(&[der]));
        assert_eq!(
            call(request(&trusted, &url, "", ""))
                .unwrap()
                .into_string()
                .unwrap(),
            "abc"
        );
        server.join().unwrap();
    }

    #[test]
    fn protocol_override_matches_exact_hosts_after_routing() {
        let mut agent = client::agent("", "").with_http1_hosts("EXAMPLE.org [::1] bücher.example");
        agent.routes = Some(Arc::new(|_| "http://127.0.0.1:9876".into()));
        let agent = agent.for_url("https://example.org:8443/media");
        assert_eq!(agent.proxy, "http://127.0.0.1:9876");
        let matches = |url: &str| agent.http1_only(&Url::parse(url).unwrap());
        assert!(matches("https://example.org:8443/media"));
        assert!(matches("https://[::1]/file"));
        assert!(matches("https://xn--bcher-kva.example/file"));
        assert!(!matches("https://cdn.example.org/file"));
        assert!(!matches("https://example.org.invalid/file"));
        assert!(!client::agent("", "").http1_only(&Url::parse("https://example.org/").unwrap()));
    }

    #[test]
    fn publisher_request_refuses_a_plain_http_route() {
        let agent = client::agent("", "");
        let request = request(&agent, "http://127.0.0.1:1/", "", "").https_only();
        assert_eq!(
            call(request).err().as_deref(),
            Some("An HTTPS-only request was redirected to an insecure address")
        );
    }

    #[test]
    fn negotiates_http2_and_multiplexes_within_server_limit() {
        let (tls, der) = crate::test_support::localhost_tls(&[b"h2"]);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://localhost:{}", server.local_addr().unwrap().port());
        server.set_nonblocking(true).unwrap();
        let peak = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let (peak_server, accepted_server) = (peak.clone(), accepted.clone());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::from_std(server).unwrap();
                loop {
                    let (socket, _) = listener.accept().await.unwrap();
                    let (acceptor, peak, accepted) = (
                        acceptor.clone(),
                        peak_server.clone(),
                        accepted_server.clone(),
                    );
                    tokio::spawn(async move {
                        let tls = acceptor.accept(socket).await.unwrap();
                        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
                        accepted.fetch_add(1, SeqCst);
                        let mut h2 = h2::server::Builder::new()
                            .max_concurrent_streams(2)
                            .handshake::<_, &'static [u8]>(tls)
                            .await
                            .unwrap();
                        let active = Arc::new(AtomicUsize::new(0));
                        while let Some(Ok((request, mut respond))) = h2.accept().await {
                            assert_eq!(request.version(), http::Version::HTTP_2);
                            if request.uri().path() == "/goaway" {
                                h2.graceful_shutdown();
                            }
                            let (active, peak) = (active.clone(), peak.clone());
                            tokio::spawn(async move {
                                peak.fetch_max(active.fetch_add(1, SeqCst) + 1, SeqCst);
                                tokio::time::sleep(Duration::from_millis(120)).await;
                                let response = http::Response::builder()
                                    .header("content-length", 3)
                                    .body(())
                                    .unwrap();
                                respond
                                    .send_response(response, false)
                                    .unwrap()
                                    .send_data(b"abc".as_slice(), true)
                                    .unwrap();
                                active.fetch_sub(1, SeqCst);
                            });
                        }
                    });
                }
            });
        });
        let client = client_builder()
            .add_root_certificate(reqwest::Certificate::from_der(der.as_ref()).unwrap())
            .build()
            .unwrap();
        let agent = client::agent("", "");
        let read = |path| {
            let response = call_with(request(&agent, &format!("{url}{path}"), "", ""), |_| {
                Some(&client)
            })
            .unwrap();
            assert!(
                matches!(&response, Response::Tls(r) if r.version() == reqwest::Version::HTTP_2)
            );
            assert_eq!(response.into_string().unwrap(), "abc");
        };
        read("/warm");
        std::thread::scope(|scope| {
            scope.spawn(|| read("/one"));
            scope.spawn(|| read("/two"));
        });
        assert_eq!(accepted.load(SeqCst), 1);
        assert_eq!(peak.load(SeqCst), 2);
        read("/goaway");
        read("/reconnect");
        assert_eq!(accepted.load(SeqCst), 2);
    }
}

#[cfg(test)]
#[path = "transport_bench.rs"]
mod benchmark;
