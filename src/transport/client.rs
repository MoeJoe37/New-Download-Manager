use crate::transport::{
    http_error, redact, Agent, Duration, HashSet, Mutex, OnceLock, Request, Response, Url,
};
use std::time::Instant;

pub(crate) const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";

/// Name lookups shared by every connection: a server's addresses are looked up once a minute, not once per
/// connection, and IPv4 comes first, so a network whose IPv6 is broken doesn't stall each new connection
/// for the connect timeout before trying IPv4.
struct DnsCache;

impl ureq::Resolver for DnsCache {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        use std::net::ToSocketAddrs;
        static CACHE: Mutex<Vec<(String, Instant, Vec<std::net::SocketAddr>)>> =
            Mutex::new(Vec::new());
        const TTL: Duration = Duration::from_secs(60);
        if let Some(hit) = CACHE
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.0 == netloc && e.1.elapsed() < TTL)
        {
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
pub(crate) fn tls_config(
    extra: &[rustls::pki_types::CertificateDer<'static>],
) -> std::sync::Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let native = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(native.certs);
    roots.add_parsable_certificates(extra.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
    .unwrap() // ring supports both
    .with_root_certificates(roots)
    .with_no_client_auth();
    std::sync::Arc::new(config)
}

/// `tls_config`, read from the system once.
fn tls() -> std::sync::Arc<rustls::ClientConfig> {
    static TLS: std::sync::OnceLock<std::sync::Arc<rustls::ClientConfig>> =
        std::sync::OnceLock::new();
    TLS.get_or_init(|| tls_config(&[])).clone()
}

pub fn agent(proxy: &str, user_agent: &str) -> Agent {
    agent_with_tls(proxy, user_agent, tls())
}

pub(crate) fn agent_with_tls(
    proxy: &str,
    user_agent: &str,
    tls: std::sync::Arc<rustls::ClientConfig>,
) -> Agent {
    let user_agent = if user_agent.trim().is_empty() {
        UA
    } else {
        user_agent.trim()
    };
    let proxy = proxy.trim();
    let invalid = !proxy.is_empty()
        && (!url::Url::parse(proxy).is_ok_and(|url| url.host_str().is_some())
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
    Agent::new(
        build().build(),
        build().redirects(0).build(),
        proxy,
        user_agent,
        invalid,
    )
}

pub(super) fn tls_client(http1_only: bool) -> Option<&'static reqwest::blocking::Client> {
    static AUTO: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    static HTTP1: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    let cell = if http1_only { &HTTP1 } else { &AUTO };
    let builder = || {
        let builder = client_builder();
        if http1_only {
            builder.http1_only()
        } else {
            builder
        }
    };
    cell.get_or_init(|| {
        builder()
            .build()
            .or_else(|_| builder().tls_built_in_native_certs(false).build())
            .ok()
    })
    .as_ref()
}

pub(super) fn client_builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(16)
}

pub(super) fn call_with<'a>(
    request: Request,
    tls: impl Fn(bool) -> Option<&'a reqwest::blocking::Client>,
) -> Result<Response, String> {
    if request.agent.invalid_proxy {
        return Err("Invalid proxy configuration".into());
    }
    let mut url = Url::parse(&request.url).map_err(|_| "Invalid download address")?;
    let mut headers = request.headers;
    let mut agent = request.agent;
    for _ in 0..=10 {
        if request.https_only && url.scheme() != "https" {
            return Err("An HTTPS-only request was redirected to an insecure address".into());
        }
        if !matches!(url.scheme(), "http" | "https") {
            return Err("Unsupported HTTP redirect protocol".into());
        }
        let mut response = None;
        if url.scheme() == "https" && agent.proxy.is_empty() && url.username().is_empty() {
            static NEGOTIATE: OnceLock<Mutex<HashSet<url::Origin>>> = OnceLock::new();
            let negotiate = NEGOTIATE.get_or_init(Mutex::default);
            let force = agent.http1_only(&url);
            let origin = url.origin();
            let prefer =
                request.prefer_http1 && !force && !negotiate.lock().unwrap().contains(&origin);
            if let Some(client) =
                tls(force || prefer).or_else(|| prefer.then(|| tls(false)).flatten())
            {
                let send = |client: &reqwest::blocking::Client| {
                    let mut req = client
                        .get(url.clone())
                        .header("User-Agent", &agent.user_agent);
                    for (key, value) in &headers {
                        req = req.header(key, value);
                    }
                    req.send()
                };
                let mut received = send(client);
                if prefer {
                    let rejected = match &received {
                        // No headers or bytes were returned. Exclude timeouts and local request errors.
                        Err(error) => !error.is_builder() && !error.is_timeout(),
                        Ok(response) => {
                            response.status() == reqwest::StatusCode::HTTP_VERSION_NOT_SUPPORTED
                        }
                    };
                    if let Some(auto) = rejected
                        .then(|| tls(false))
                        .flatten()
                        .filter(|auto| !std::ptr::eq(*auto, client))
                    {
                        received = send(auto);
                        if received.as_ref().is_ok_and(|r| {
                            r.version() == reqwest::Version::HTTP_2 && r.status().is_success()
                        }) {
                            negotiate.lock().unwrap().insert(origin);
                        }
                    }
                }
                let received = received.map_err(|err| {
                    if err.is_timeout() {
                        "HTTP request timed out".to_string()
                    } else if err.is_connect() {
                        "Could not connect to the HTTPS server".to_string()
                    } else {
                        "HTTPS request failed".to_string()
                    }
                })?;
                if received.status() != reqwest::StatusCode::UNAUTHORIZED {
                    response = Some(Response::Tls(received));
                }
            }
        }
        let response = match response {
            Some(response) => response,
            None => {
                let mut send = agent.no_redirect.get(url.as_str());
                for (key, value) in &headers {
                    send = send.set(key, value);
                }
                Response::Legacy(super::auth::call_legacy(send).map_err(|err| redact(&err))?)
            }
        };
        let code = response.status();
        if !matches!(code, 301 | 302 | 303 | 307 | 308) {
            if code >= 400 {
                return Err(http_error(code));
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
