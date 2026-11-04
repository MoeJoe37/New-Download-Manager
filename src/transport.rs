//! HTTP transport for downloads: one HTTP/1.1 client (ureq) for every route. Each of a download's
//! connections is a TCP connection of its own (HTTP/2 would share one between them, and servers that
//! limit the speed per connection would then give the whole download one connection's speed). Redirects
//! are followed here, so a request's context (cookies, headers, the route) stays scoped to its origin.
use std::io::{self, Read};
use std::sync::Arc;
use url::Url;

#[derive(Clone)]
pub struct Agent {
    pub(crate) legacy: ureq::Agent,
    no_redirect: ureq::Agent,
    pub(crate) proxy: String,
    user_agent: String,
    pub(crate) invalid_proxy: bool,
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
            routes: None,
        }
    }

    fn for_url(&self, url: &str) -> Self {
        let Some(routes) = &self.routes else {
            return self.clone();
        };
        let proxy = crate::net::agent_proxy(&routes(url));
        if proxy == self.proxy {
            return self.clone();
        }
        let mut agent = crate::engine::agent(&proxy, &self.user_agent);
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
}

impl Request {
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
}

impl Response {
    pub fn status(&self) -> u16 {
        match self {
            Self::Legacy(r) => r.status(),
        }
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        match self {
            Self::Legacy(r) => r.header(name),
        }
    }
    pub fn all(&self, name: &str) -> Vec<&str> {
        match self {
            Self::Legacy(r) => r.all(name),
        }
    }
    pub fn get_url(&self) -> &str {
        match self {
            Self::Legacy(r) => r.get_url(),
        }
    }
    pub fn into_reader(self) -> Box<dyn Read + Send> {
        match self {
            Self::Legacy(r) => r.into_reader(),
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

/// "HTTP 404 Not Found": the status and the server's own reason phrase (a standard one when it sent none).
pub(crate) fn http_error(code: u16, reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() { format!("HTTP {code}") } else { format!("HTTP {code} {reason}") }
}

pub fn call(request: Request) -> Result<Response, String> {
    if request.agent.invalid_proxy {
        return Err("Invalid proxy configuration".into());
    }
    let mut url = Url::parse(&request.url).map_err(|_| "Invalid download address")?;
    let mut headers = request.headers;
    let mut agent = request.agent;
    for _ in 0..=10 {
        if !matches!(url.scheme(), "http" | "https") {
            return Err("Unsupported HTTP redirect protocol".into());
        }
        let mut send = agent.no_redirect.get(url.as_str());
        for (key, value) in &headers {
            send = send.set(key, value);
        }
        let response = Response::Legacy(crate::engine::call_legacy(send).map_err(|err| redact(&err))?);
        let code = response.status();
        if !matches!(code, 301 | 302 | 303 | 307 | 308) {
            if code >= 400 {
                let Response::Legacy(r) = &response;
                return Err(http_error(code, r.status_text()));
            }
            return Ok(response);
        }
        let location = response
            .header("Location")
            .ok_or("HTTP redirect has no location")?;
        let next = url
            .join(location)
            .map_err(|_| "Invalid HTTP redirect location")?;
        if (!next.username().is_empty() || next.password().is_some())
            && (next.origin() != url.origin()
                || next.username() != url.username()
                || next.password() != url.password())
        {
            return Err("HTTP redirect contains credentials".into());
        }
        if next.origin() != url.origin() {
            headers.retain(|(key, _)| {
                matches!(
                    key.to_ascii_lowercase().as_str(),
                    "accept" | "accept-encoding" | "user-agent" | "range"
                )
            });
        }
        if agent.routes.is_some() {
            agent = agent.for_url(next.as_str());
            if agent.invalid_proxy {
                return Err("Invalid proxy configuration".into());
            }
        }
        url = next;
    }
    Err("Too many HTTP redirects".into())
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
    use std::time::Duration;

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
        let agent = crate::engine::agent("", "");
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
        let invalid = crate::engine::agent("invalid-proxy://", "");
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
        let agent = crate::engine::agent("", "");
        let error = call(request(&agent, &url, "", "")).err().unwrap();
        assert!(error.starts_with("HTTP 403 Forbidden"));
        assert!(crate::engine::permanent_error(&error));
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
        let mut agent = crate::engine::agent("", "");
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
        let agent = crate::engine::agent(&route, "");
        let error = call(request(&agent, "https://private.invalid/media", "", ""))
            .err()
            .unwrap();
        assert_eq!(error, "HTTP 407 Proxy Authentication Required");
        server.join().unwrap();
    }

    /// HTTPS through the one client: a certificate the system (here: the test) trusts is accepted, one it
    /// doesn't is refused, and two requests at once get two TCP connections of their own (a download's
    /// segments each have their own connection's speed).
    #[test]
    fn https_trusts_added_roots_and_gives_each_request_its_own_connection() {
        use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der: CertificateDer<'static> = certificate.cert.der().clone();
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![der.clone()], PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into())
                .unwrap(),
        );
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://localhost:{}/file", server.local_addr().unwrap().port());
        let (accepted, active, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (a, b, c) = (accepted.clone(), active.clone(), peak.clone());
        std::thread::spawn(move || {
            for socket in server.incoming().flatten() {
                let (config, accepted, active, peak) = (config.clone(), a.clone(), b.clone(), c.clone());
                std::thread::spawn(move || {
                    accepted.fetch_add(1, SeqCst);
                    let mut tls = rustls::StreamOwned::new(rustls::ServerConnection::new(config).unwrap(), socket);
                    let mut reader = BufReader::new(&mut tls);
                    let mut line = String::new();
                    // a client that refused the certificate ends the handshake here
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    while reader.read_line(&mut line).unwrap_or(0) > 2 {}
                    drop(reader);
                    peak.fetch_max(active.fetch_add(1, SeqCst) + 1, SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    let _ = tls.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/3\r\nContent-Length: 3\r\n\r\nabc");
                    let _ = tls.flush();
                    active.fetch_sub(1, SeqCst);
                    std::thread::sleep(Duration::from_millis(300));
                });
            }
        });
        // not trusted: refused
        let untrusted = crate::engine::agent("", "");
        assert!(call(request(&untrusted, &url, "", "")).is_err(), "a certificate nobody trusts");
        let agent = crate::engine::agent_with_tls("", "", crate::engine::tls_config(&[der]));
        let read = || {
            let response = call(request(&agent, &url, "", "").set("Range", "bytes=0-")).unwrap();
            assert_eq!(response.status(), 206);
            assert_eq!(response.into_string().unwrap(), "abc");
        };
        let before = accepted.load(SeqCst);
        std::thread::scope(|scope| {
            scope.spawn(read);
            scope.spawn(read);
        });
        assert_eq!(peak.load(SeqCst), 2, "both requests at once");
        assert_eq!(accepted.load(SeqCst) - before, 2, "each on a connection of its own");
    }
}
