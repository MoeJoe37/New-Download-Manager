use super::*;

/// "Use advanced browser integration": Windows' proxy for http points here (see `apply_integration`).
pub const PROXY_PORT: u16 = 9615;

#[derive(Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AdvProxy {
    /// The Proxy / Socks page's manual HTTP proxy (on, host, port, user, password), which NDM's advanced
    /// dialog repeats as "Use HTTP Proxy"; only carried while the dialog is open.
    #[serde(skip)]
    pub http: Option<(bool, String, String, String, String)>,
    pub https_on: bool,
    pub https_host: String,
    pub https_port: String,
    pub https_user: String,
    pub https_pass: String,
    pub socks_on: bool,
    pub socks_host: String,
    pub socks_port: String,
    pub socks_user: String,
    pub socks_pass: String,
    /// Socks 5 (else 4).
    pub socks5: bool,
    /// "Proxy DNS" unticked: NDM looks names up itself and gives the SOCKS 5 server the address.
    /// Missing in older state files, which keeps their behaviour (the proxy looks names up).
    pub local_dns: bool,
    /// FTP proxy (logs in as user@host).
    pub ftp_on: bool,
    pub ftp_host: String,
    pub ftp_port: String,
    pub ftp_user: String,
    pub ftp_pass: String,
}

/// `scheme://user:pass@host:port`; `host` may already carry ":port" and a scheme prefix.
fn proxy_url(scheme: &str, host: &str, port: &str, user: &str, pass: &str) -> String {
    let host = host.trim();
    let host = host.split_once("://").map_or(host, |(_, h)| h).trim_end_matches('/');
    let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
    let auth = if user.is_empty() { String::new() } else { format!("{}:{}@", enc(user), enc(pass)) };
    let port = port.trim();
    if port.is_empty() || host.contains(':') { format!("{scheme}://{auth}{host}") } else { format!("{scheme}://{auth}{host}:{port}") }
}

pub(super) fn pac_route(pac: &str, url: &str) -> String {
    match crate::win::pac_proxy(pac, url) {
        Some(route) if route.is_empty() || route.contains("://") => route,
        Some(host) => proxy_url("http", &host, "", "", ""),
        None => "invalid-proxy://".into(),
    }
}

/// The host matches one of the space separated patterns ("10.*", "intranet", "*.corp.com").
fn bypassed(patterns: &str, host: &str) -> bool {
    !host.is_empty() && patterns.split_whitespace().any(|p| crate::grabber::mask_match(p, host) || host.starts_with(p))
}

impl Settings {
    /// Proxy URL for the engine from NDM-style manual proxy fields; empty = direct.
    /// The proxy to use for `url` ("" = direct), following NDM's Proxy / Socks page: manual
    /// HTTP proxy for the ticked protocols, the advanced HTTPS proxy for https, SOCKS for the rest
    ///, or Windows' settings.
    pub fn proxy_for(&self, url: &str) -> String {
        let u = url::Url::parse(url).ok();
        let (scheme, host) = (u.as_ref().map_or("", |u| u.scheme()), u.as_ref().and_then(|u| u.host_str()).unwrap_or(""));
        match self.proxy_mode {
            // PAC: Windows' WinHTTP runs FindProxyForURL; the first proxy it names is used
            2 => pac_route(&self.pac_url, url),
            1 => {
                // NDM's own downloads don't go through NDM's proxy
                let (on, server, bypass) = self.saved_proxy.clone().unwrap_or_else(crate::win::system_proxy);
                if !on || bypassed(&bypass.replace(';', " ").replace("<local>", ""), host) {
                    return String::new();
                }
                // "host:port" for all, or "http=h:p;https=h:p;socks=h:p"
                let pick = |k: &str| server.split(';').find_map(|p| p.strip_prefix(&format!("{k}="))).map(str::to_string);
                if !server.contains('=') {
                    return proxy_url("http", server.trim(), "", "", "");
                }
                pick(scheme).map(|hp| proxy_url("http", &hp, "", "", "")).or_else(|| pick("socks").map(|hp| proxy_url("socks5h", &hp, "", "", ""))).unwrap_or_default()
            }
            3 => {
                if self.proxy_bypass_on && bypassed(&self.proxy_bypass, host) {
                    return String::new();
                }
                let a = &self.adv;
                if scheme == "ftp" && a.ftp_on && !a.ftp_host.trim().is_empty() {
                    return proxy_url("ftp", &a.ftp_host, &a.ftp_port, &a.ftp_user, &a.ftp_pass);
                }
                if scheme == "https" && a.https_on && !a.https_host.trim().is_empty() {
                    return proxy_url("http", &a.https_host, &a.https_port, &a.https_user, &a.https_pass);
                }
                let ticked = (scheme == "http" && self.proxy_http) || (scheme == "https" && self.proxy_https) || (scheme == "ftp" && self.proxy_ftp);
                if ticked && !self.proxy_host.trim().is_empty() {
                    return proxy_url("http", &self.proxy_host, &self.proxy_port, &self.proxy_user, &self.proxy_pass);
                }
                if a.socks_on && !a.socks_host.trim().is_empty() {
                    let scheme = if !a.socks5 { "socks4" } else if a.local_dns { "socks5" } else { "socks5h" };
                    return proxy_url(scheme, &a.socks_host, &a.socks_port, &a.socks_user, &a.socks_pass);
                }
                String::new()
            }
            _ => String::new(),
        }
    }

    /// Several networks for this download: only http(s) that goes direct, since a proxy has one way out.
    pub fn use_lanes(&self, url: &str) -> bool {
        self.multi_net && !url.trim().get(..6).is_some_and(|p| p.eq_ignore_ascii_case("ftp://")) && self.proxy_for(url).is_empty()
    }

    /// Adds the saved site login to a URL that has none. A site matches its host and subdomains.
    pub fn with_login(&self, url: &str) -> String {
        let Ok(mut u) = url::Url::parse(url.trim()) else { return url.trim().into() };
        let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
        let site = |l: &Login| l.site.trim().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/').to_ascii_lowercase();
        if u.username().is_empty() {
            if let Some(l) = self.logins.iter().find(|l| !site(l).is_empty() && (host == site(l) || host.ends_with(&format!(".{}", site(l))))) {
                let _ = u.set_username(&l.user);
                let _ = u.set_password(Some(&l.pass));
            }
        }
        u.into()
    }
}

impl Core {
    /// The agent for downloading `url`, with the proxy the settings choose for it.
    pub fn agent(&self, url: &str) -> engine::Agent {
        let (proxy, ua, use_routes, http1_hosts) = {
            let s = self.settings();
            (s.proxy_for(url), s.user_agent.clone(), s.proxy_mode != 0, s.http1_hosts.clone())
        };
        let proxy = crate::net::agent_proxy(&proxy);
        let mut agent = self.agents.lock().unwrap().entry(proxy.clone()).or_insert_with(|| engine::agent(&proxy, &ua).with_http1_hosts(&http1_hosts)).clone();
        if use_routes {
            agent.routes = engine::FTP.lock().unwrap().route.clone();
        }
        agent
    }

    /// The networks `url` is downloaded over, each with its agent: none unless the setting is on, the
    /// address goes direct, NDM can choose the network and at least two ticked networks are up.
    pub fn lanes(&self, url: &str) -> Vec<(String, engine::Agent)> {
        self.lanes_from(url, 2)
    }

    /// `lanes` once a download is spread: one network left is enough to keep its connections.
    fn lanes_from(&self, url: &str, least: usize) -> Vec<(String, engine::Agent)> {
        let (on, off, ua) = {
            let s = self.settings();
            (s.use_lanes(url), s.nets_off.clone(), s.user_agent.clone())
        };
        if !on || !crate::net::nic_supported() {
            return vec![];
        }
        let nets: Vec<String> = crate::win::nics().into_iter().map(|n| n.name).filter(|n| !off.contains(n)).collect();
        if nets.len() < least {
            return vec![];
        }
        let mut agents = self.agents.lock().unwrap();
        nets.into_iter()
            .map(|n| {
                let proxy = crate::net::local_proxy(&format!("nic://{}", percent_encoding::utf8_percent_encode(&n, percent_encoding::NON_ALPHANUMERIC)));
                let a = agents.entry(proxy.clone()).or_insert_with(|| engine::agent(&proxy, &ua)).clone();
                (n, a)
            })
            .collect()
    }

    /// Networks come and go while downloads run (a phone plugged in, Wi-Fi lost, one unticked in the
    /// Networks window): each spread download gets the ones there now, and one that wasn't spread is
    /// once a second network appears. Checked about every second.
    pub(super) fn tick_lanes(&self) {
        for job in self.jobs().into_iter().filter(|j| j.running() && j.multi_ok.load(Relaxed)) {
            let url = job.info().url.clone();
            let spread = !job.lanes.lock().unwrap().is_empty();
            let now = self.lanes_from(&url, if spread { 1 } else { 2 });
            let mut lanes = job.lanes.lock().unwrap();
            if lanes.iter().map(|l| &l.0).ne(now.iter().map(|l| &l.0)) {
                *lanes = now;
            }
        }
    }

    /// "Use advanced browser integration" (`on`), done without NDM's network driver: Windows' proxy for
    /// plain http is set to NDM's (`proxy_conn`) and the user's own proxy kept for the other protocols;
    /// off puts the old setting back. Run at start (on), on Options OK and at exit (off, so http works
    /// while NDM isn't running).
    #[cfg(not(target_os = "macos"))]
    pub fn apply_integration(&self, on: bool) {
        let mut s = self.settings();
        match (on, s.saved_proxy.clone()) {
            (true, None) => {
                let old = crate::win::system_proxy();
                let mut server = vec![format!("http=127.0.0.1:{PROXY_PORT}")];
                if old.0 && old.1.contains('=') {
                    server.extend(old.1.split(';').filter(|p| !p.trim().is_empty() && !p.starts_with("http=")).map(String::from));
                } else if old.0 && !old.1.trim().is_empty() {
                    server.extend(["https", "ftp"].map(|k| format!("{k}={}", old.1.trim())));
                }
                let bypass = if old.0 && !old.2.is_empty() { old.2.clone() } else { "<local>".into() };
                crate::win::set_system_proxy(true, &server.join(";"), &bypass);
                s.saved_proxy = Some(old);
            }
            (false, Some((was_on, server, bypass))) => {
                crate::win::set_system_proxy(was_on, &server, &bypass);
                s.saved_proxy = None;
            }
            _ => {}
        }
    }
}
