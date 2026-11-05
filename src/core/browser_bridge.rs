use crate::core::{Arc, Browser, BufRead, BufReader, Core, Deserialize, Duration, HashMap, Local, PROXY_PORT, Read, Relaxed, Sender, Status, TcpListener, Write, browser_kind, engine, ext, thread};

/// The browser endpoint's ports: the first one free. Windows can reserve a port range for Hyper-V, WSL or
/// Docker (`netsh int ipv4 show excludedportrange`), so 9614 may not be NDM's to take; the extension tries
/// the same list (extension/background.js, PORTS).
pub const PORTS: [u16; 4] = [9614, 19614, 29614, 39614];

/// Asks each of `PORTS` whether NDM answers there: its port, if a copy is running. NDM marks its replies
/// with "X-NDM: 1"; NDM 1.3 and older (on 9614 only) are known by their /config reply.
pub fn running_port() -> Option<u16> {
    PORTS.into_iter().find(|p| {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], *p));
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
            return false;
        }
        let r = ureq::post(&format!("http://127.0.0.1:{p}/config")).set("X-NDM", "1").timeout(Duration::from_secs(3)).call();
        r.is_ok_and(|r| r.header("X-NDM").is_some() || r.into_string().is_ok_and(|b| b.contains("\"menu_dl\"")))
    })
}

/// A download handed over by the browser extension.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Incoming {
    pub url: String,
    pub referrer: String,
    pub cookies: String,
    pub filename: String,
    pub mime: String,
    pub force: bool,
    /// The browser's proxy (`Info::browser_proxy`).
    pub proxy: String,
    /// A download the browser started (the extension holds it meanwhile): NDM takes it only if the server
    /// sends NDM the same file (`engine::probe::same_file`), with the browser's `size` (0: unknown) and `mime`.
    pub check: bool,
    pub size: u64,
    /// The download panel on a page whose video the browser can't hand over (a player streaming it in
    /// pieces): the page goes to yt-dlp, whatever the site.
    pub video: bool,
    pub stream: bool,
    pub output: String,
    pub variant: Option<usize>,
    /// "Download all links": the page's links; `url` is the page.
    pub links: Vec<Link>,
    /// From the command line:
    /// folder (/p), silent (/n), add to queue only (/a), exit when done (/q), start the main queue (/s).
    pub cli: bool,
    pub folder: String,
    pub silent: bool,
    pub later: bool,
    pub quit: bool,
    pub start_queue: bool,
}

/// Command-line arguments as an `Incoming` (None when there are none).
pub fn cli_args(args: &[String]) -> Option<Incoming> {
    let mut inc = Incoming { cli: true, force: true, ..Default::default() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_ascii_lowercase().as_str() {
            "/d" => inc.url = it.next()?.clone(),
            "/p" => inc.folder = it.next()?.clone(),
            "/f" => inc.filename = it.next()?.clone(),
            "/n" => inc.silent = true,
            "/a" => inc.later = true,
            "/q" => inc.quit = true,
            "/s" => inc.start_queue = true,
            _ => {} // /h hangs up a dial-up connection: nothing to do
        }
    }
    (!inc.url.is_empty() || inc.start_queue).then_some(inc)
}

#[test]
fn command_line() {
    let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let i = cli_args(&a("/n /d http://x.com/a.zip /p C:\\dl /f b.zip /a")).unwrap();
    assert!((i.silent, i.later, i.start_queue) == (true, true, false) && i.url == "http://x.com/a.zip" && i.folder == "C:\\dl" && i.filename == "b.zip");
    assert!(cli_args(&a("/s")).unwrap().start_queue);
    assert!(cli_args(&[]).is_none() && cli_args(&a("/d")).is_none());
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Link {
    pub url: String,
    pub text: String,
}

/// One program's connection to NDM's http proxy ("Use advanced browser integration"): the request goes
/// to the server; a download the browser-capture rules want (`captures`, `wants`) is returned for the app
/// and the program gets "HTTP/1.0 204 Intercepted by NDM"; anything else is passed through unchanged.
pub(super) fn proxy_conn(core: &Core, client: std::net::TcpStream) -> Option<Incoming> {
    let head = |r: &mut dyn BufRead| -> Option<(String, Vec<(String, String)>)> {
        let mut first = String::new();
        r.read_line(&mut first).ok()?;
        let mut h = vec![];
        loop {
            let mut l = String::new();
            if r.read_line(&mut l).ok()? == 0 || l.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                h.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        Some((first.trim_end().to_string(), h))
    };
    let get = |h: &[(String, String)], k: &str| h.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map_or(String::new(), |x| x.1.clone());
    let hop = |k: &str| ["connection", "proxy-connection", "keep-alive", "proxy-authorization"].iter().any(|x| k.eq_ignore_ascii_case(x));
    let mut cr = BufReader::new(client.try_clone().ok()?);
    let (line, h) = head(&mut cr)?;
    let mut p = line.splitn(3, ' ');
    let (method, target, ver) = (p.next()?.to_string(), p.next()?.to_string(), p.next().unwrap_or("HTTP/1.1").to_string());
    let u = url::Url::parse(&target).ok().filter(|u| u.scheme() == "http")?;
    let mut out = &client;
    let Ok(mut server) = std::net::TcpStream::connect((u.host_str()?, u.port_or_known_default()?)) else {
        let _ = out.write_all(b"HTTP/1.0 502 Bad Gateway\r\n\r\n");
        return None;
    };
    // ponytail: one request per connection (Connection: close both ways); keep-alive if pages get slow
    let mut req = format!("{method} {} {ver}\r\n", &u[url::Position::BeforePath..]);
    h.iter().filter(|(k, _)| !hop(k)).for_each(|(k, v)| req += &format!("{k}: {v}\r\n"));
    server.write_all(format!("{req}Connection: close\r\n\r\n").as_bytes()).ok()?;
    let body: u64 = get(&h, "content-length").parse().unwrap_or(0);
    std::io::copy(&mut (&mut cr).take(body), &mut server).ok()?;
    let mut sr = BufReader::new(server.try_clone().ok()?);
    let (status, rh) = head(&mut sr)?;
    let code = status.split(' ').nth(1).unwrap_or("");
    if method == "GET" && (code == "200" || code == "206") {
        let inc = Incoming {
            url: target.clone(),
            referrer: get(&h, "referer"),
            cookies: get(&h, "cookie"),
            filename: engine::names::disposition_name(&get(&rh, "content-disposition")).unwrap_or_default(),
            mime: get(&rh, "content-type").split(';').next().unwrap_or("").trim().to_string(),
            ..Default::default()
        };
        if core.captures(&get(&h, "user-agent")) && core.wants(&inc) {
            let _ = out.write_all(b"HTTP/1.0 204 Intercepted by NDM\r\n\r\n");
            return Some(inc);
        }
    }
    let mut resp = format!("{status}\r\n");
    rh.iter().filter(|(k, _)| !hop(k)).for_each(|(k, v)| resp += &format!("{k}: {v}\r\n"));
    out.write_all(format!("{resp}Connection: close\r\n\r\n").as_bytes()).ok()?;
    let _ = std::io::copy(&mut sr, &mut out);
    None
}

/// An extension request: (path, body, User-Agent) of `POST /add` or `POST /config`.
pub(super) fn handle(s: &std::net::TcpStream) -> Option<(String, Vec<u8>, String)> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    let mut h = HashMap::new();
    {
        let mut headers = (&mut r).take(32 << 10);
        if headers.read_line(&mut line).ok()? == 0 || !line.ends_with('\n') {
            return None;
        }
        loop {
            let mut l = String::new();
            if headers.read_line(&mut l).ok()? == 0 || !l.ends_with('\n') {
                return None;
            }
            if l.trim().is_empty() { break; }
            let (k, v) = l.split_once(':')?;
            if h.insert(k.trim().to_ascii_lowercase(), v.trim().to_string()).is_some() {
                return None; // no ambiguous lengths, origins or control headers
            }
        }
    }
    let origin_ok = h.get("origin").is_none_or(|origin| {
        origin == "chrome-extension://fjkomnlijmdmohmlbfgkepgdieleinoa" || origin.strip_prefix("moz-extension://").is_some_and(|id| {
            id.len() == 36 && id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
        })
    });
    let path = line.strip_prefix("POST ")?.split(' ').next()?.to_string();
    if (!["/add", "/config", "/cookies", "/show", "/formats"].contains(&path.as_str()) && !path.starts_with("/feed?")) || h.get("x-ndm").map(String::as_str) != Some("1") || !origin_ok || h.contains_key("transfer-encoding") {
        return None;
    }
    // the extension sends at most 512 KB at a time (`feed`): a bigger body is refused, not cut short
    let len: usize = h.get("content-length").map_or(Some(0), |l| l.parse().ok()).filter(|l| *l <= 1 << 20)?;
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some((path, body, h.get("user-agent").cloned().unwrap_or_default()))
}

impl Core {
    /// A file the browser extension fetches inside its web page and hands over in pieces (POST /feed): when
    /// the server sends NDM something else than the browser gets (a page's service worker makes the file,
    /// Telegram Web for one). `op=start` (body: the download as `Incoming`) adds it to the list and answers
    /// {"id"}; `op=data&id&at` writes a piece at `at`; `op=end&id&ok` finishes it (ok=0: the browser stopped).
    pub fn feed(&self, path: &str, body: &[u8]) -> Result<String, String> {
        let q: HashMap<String, String> = url::form_urlencoded::parse(path.split_once('?').map_or("", |p| p.1).as_bytes()).into_owned().collect();
        let num = |k: &str| q.get(k).and_then(|v| v.parse::<u64>().ok());
        match q.get("op").map(String::as_str) {
            Some("start") => {
                let inc: Incoming = serde_json::from_slice(body).map_err(|e| e.to_string())?;
                let mut i = self.new_info(&inc.url);
                let name = engine::names::sanitize(inc.filename.replace('\\', "/").rsplit('/').next().unwrap_or(""));
                let name = if name.is_empty() { engine::names::sanitize(&engine::names::url_name(&inc.url)) } else { name };
                {
                    let s = self.settings();
                    i.category = s.category_for(&name, &inc.url);
                    i.folder = s.folder_for(&i.category);
                }
                i.name = self.unique_name(&i.folder, &name);
                (i.referrer, i.fed, i.status) = (inc.referrer.clone(), true, Status::Downloading);
                i.size = Some(inc.size).filter(|s| *s > 0);
                i.last_try = Local::now().timestamp();
                i.segs = vec![engine::Seg { start: 0, next: 0, stop: i.size, info: "Receiving data...".into(), ..Default::default() }];
                let part = i.part();
                if let Some(d) = part.parent() {
                    std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                }
                let f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
                let id = i.id;
                self.add(i);
                self.fed.lock().unwrap().insert(id, f);
                Ok(serde_json::json!({ "id": id }).to_string())
            }
            Some("data") => {
                use std::io::{Seek, SeekFrom, Write};
                let (id, at) = (num("id").ok_or("no id")?, num("at").ok_or("no at")?);
                let mut fed = self.fed.lock().unwrap();
                let f = fed.get_mut(&id).ok_or("no such download")?;
                f.seek(SeekFrom::Start(at)).and_then(|_| f.write_all(body)).map_err(|e| e.to_string())?;
                if let Some(j) = self.find(id) {
                    let mut i = j.info();
                    if let Some(s) = i.segs.first_mut() {
                        s.next = s.next.max(at + body.len() as u64);
                    }
                }
                Ok(String::new())
            }
            Some("end") => {
                let id = num("id").ok_or("no id")?;
                let f = self.fed.lock().unwrap().remove(&id).ok_or("no such download")?;
                let len = f.metadata().map_or(0, |m| m.len());
                drop(f);
                let j = self.find(id).ok_or("no such download")?;
                let ok = num("ok") == Some(1) && len > 0;
                {
                    let mut i = j.info();
                    let (part, path) = (i.part(), i.path());
                    let moved = ok && (std::fs::rename(&part, &path).is_ok() || (std::fs::copy(&part, &path).is_ok() && std::fs::remove_file(&part).is_ok()));
                    if moved {
                        i.size = Some(len);
                        i.segs = vec![engine::Seg { start: 0, next: len, stop: Some(len), ..Default::default() }];
                        i.status = Status::Complete;
                    } else {
                        i.status = Status::Error(crate::i18n::tr_os("The browser stopped sending the file: download it again from its page."));
                        for s in &mut i.segs {
                            s.info.clear();
                        }
                    }
                }
                j.ended.store(true, Relaxed); // the complete / failed sound
                self.save();
                Ok(String::new())
            }
            _ => Err("unknown op".into()),
        }
    }

    /// Is the browser that sent a request ticked in "Capture downloads from the following browsers"?
    /// Browsers not in the list (or not recognised) are captured.
    pub fn captures(&self, ua: &str) -> bool {
        let kind = browser_kind(ua);
        let s = self.settings();
        let mine: Vec<&Browser> = s.browsers.iter().filter(|b| !kind.is_empty() && browser_kind(&format!("{} {}", b.name, b.exe)) == kind).collect();
        mine.is_empty() || mine.iter().any(|b| b.on)
    }

    /// Should a download the browser started be taken over?
    pub fn wants(&self, inc: &Incoming) -> bool {
        if !inc.url.starts_with("http://") && !inc.url.starts_with("https://") {
            return false;
        }
        if inc.force {
            return true;
        }
        let host = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
        let (h, r) = (host(&inc.url), host(&inc.referrer));
        if self.settings().skip_urls.iter().any(|p| crate::grabber::mask_match(p, &inc.url)) {
            return false;
        }
        if self.settings().skip_sites.split_whitespace().map(str::to_ascii_lowercase).any(|s| (!h.is_empty() && h.ends_with(&s)) || (!r.is_empty() && r.ends_with(&s))) {
            return false;
        }
        let name = if inc.filename.is_empty() { engine::names::url_name(&inc.url) } else { inc.filename.replace('\\', "/") };
        let e = ext(name.rsplit('/').next().unwrap_or(""));
        if e.is_empty() {
            return !inc.mime.starts_with("text/") && !inc.mime.starts_with("image/");
        }
        self.settings().file_types.split_whitespace().any(|t| t.trim_start_matches('.').eq_ignore_ascii_case(&e))
    }

    /// Localhost endpoint for the browser extension: POST /add with `X-NDM: 1`.
    /// Web pages can't send that header cross-origin without a CORS preflight, which we refuse,
    /// and we also reject any Origin that isn't a browser extension.
    pub fn spawn_server(self: &Arc<Self>, tx: Sender<Incoming>, notify: impl Fn() + Send + Sync + 'static) {
        let inspections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let notify = Arc::new(notify);
        if let Ok(l) = TcpListener::bind(("127.0.0.1", PROXY_PORT)) {
            #[cfg(target_os = "macos")]
            self.proxy_ready.store(true, std::sync::atomic::Ordering::SeqCst);
            let (core, tx, notify) = (self.clone(), tx.clone(), notify.clone());
            thread::spawn(move || {
                for c in l.incoming().flatten() {
                    let (core, tx, notify) = (core.clone(), tx.clone(), notify.clone());
                    thread::spawn(move || {
                        if let Some(inc) = proxy_conn(&core, c) {
                            let _ = tx.send(inc);
                            notify();
                        }
                    });
                }
            });
        }
        let core = self.clone();
        thread::spawn(move || {
            let Some(l) = PORTS.iter().find_map(|p| TcpListener::bind(("127.0.0.1", *p)).map_err(|e| eprintln!("port {p}: {e}")).ok()) else {
                return eprintln!("browser integration disabled: none of the ports {PORTS:?} is free");
            };
            for s in l.incoming().flatten() {
                let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
                let req = handle(&s);
                let reply = |status: &str, body: &str| {
                    let _ = (&s).write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-NDM: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                };
                if req.as_ref().is_some_and(|(path, ..)| path == "/show") {
                    // NDM started again (Linux): bring the window back
                    crate::win::show_main();
                    reply("200 OK", "");
                    notify();
                    continue;
                }
                if let Some((path, body, _)) = req.as_ref().filter(|(path, ..)| path.starts_with("/feed?")) {
                    // a file the extension fetches inside its page, in pieces (`feed`)
                    match core.feed(path, body) {
                        Ok(r) => reply("200 OK", &r),
                        Err(e) => reply("404 Not Found", &serde_json::json!({ "error": e }).to_string()),
                    }
                    notify();
                    continue;
                }
                if req.as_ref().is_some_and(|(path, ..)| path == "/config") {
                    // what the extension shows: context menu items (334) and download panel exceptions (370)
                    let c = {
                        let st = core.settings();
                        let types: Vec<&str> = st.panel_types.iter().filter(|t| t.1).map(|t| t.0.as_str()).collect();
                        serde_json::json!({ "menu_dl": st.menu_dl, "menu_all": st.menu_all, "panel_skip": st.panel_skip, "keys": st.keys,
                            "panel_types": types, "panel_min": st.panel_min, "panel_mini": st.panel_mini, "panel_nocap": st.panel_nocap,
                            "panel_drm": st.panel_drm, "sel_mode": st.sel_mode, "sel_sites": st.sel_sites, "sel_mini": st.sel_mini,
                            "want_cookies": *core.login_wait.lock().unwrap(),
                            // the extension reloads itself from NDM's copy when NDM is newer (NDM refreshes that copy)
                            "ext_version": env!("CARGO_PKG_VERSION") })
                    };
                    reply("200 OK", &c.to_string());
                    continue;
                }
                if let Some((_, body, _)) = req.as_ref().filter(|(path, ..)| path == "/formats") {
                    let inc: Incoming = serde_json::from_slice(body).unwrap_or_default();
                    if (!inc.stream && inc.mime != "video/mp2t") || inc.cookies.len() > 32 * 1024 || !core.wants(&inc) {
                        reply("400 Bad Request", "");
                        continue;
                    }
                    if inspections.fetch_add(1, Relaxed) >= 4 {
                        inspections.fetch_sub(1, Relaxed);
                        reply("503 Service Unavailable", "");
                        continue;
                    }
                    let (core, inspections) = (core.clone(), inspections.clone());
                    thread::spawn(move || {
                        let agent = core.agent(&inc.url);
                        let result = if inc.stream {
                            crate::stream::formats(&agent, &inc.url, &inc.referrer, &inc.cookies)
                        } else {
                            crate::stream::transport_formats(&agent, &inc.url, &inc.referrer, &inc.cookies)
                        };
                        inspections.fetch_sub(1, Relaxed);
                        let (status, body) = match result {
                            Ok(choices) => ("200 OK", serde_json::to_string(&choices).unwrap_or_else(|_| "[]".into())),
                            Err(error) => ("422 Unprocessable Content", serde_json::json!({"error": crate::transport::redact(&error)}).to_string()),
                        };
                        let _ = (&s).write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-NDM: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                    });
                    continue;
                }
                if let Some((_, body, _)) = req.as_ref().filter(|(path, ..)| path == "/cookies") {
                    // the grabber's manual login page: the browser's cookies for the site the user logged in to
                    let inc: Incoming = serde_json::from_slice(body).unwrap_or_default();
                    let host = url::Url::parse(&inc.url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
                    let want = core.login_wait.lock().unwrap().clone().unwrap_or_default();
                    if !want.is_empty() && crate::grabber::same_domain(&host, &want) {
                        *core.login_cookies.lock().unwrap() = inc.cookies;
                    }
                    reply("200 OK", "");
                    continue;
                }
                let accepted = req.and_then(|(_, body, ua)| serde_json::from_slice::<Incoming>(&body).ok().map(|i| (i, ua)))
                    .filter(|(inc, ua)| inc.cli || core.captures(ua) && core.wants(inc))
                    .filter(|(inc, _)| matches!(inc.output.as_str(), "" | "mp4" | "ts" | "mkv" | "webm") && (inc.stream || inc.variant.is_none() && (inc.output.is_empty() || inc.mime == "video/mp2t" && matches!(inc.output.as_str(), "mp4" | "mkv"))) &&
                        inc.links.len() <= 5000 && inc.cookies.len() <= 32 * 1024 && inc.url.len() <= 32 * 1024)
                    .map(|(inc, _)| inc);
                let inc = match accepted {
                    Some(inc) if inc.check => inc,
                    other => {
                        reply(if other.is_some() { "200 OK" } else { "204 No Content" }, "");
                        if let Some(inc) = other {
                            let _ = tx.send(inc);
                            notify();
                        }
                        continue;
                    }
                };
                // a download the browser started: taken only when the server sends NDM the same file, else the
                // browser keeps it (checked on its own thread; no answer in 15 s leaves it to the browser too)
                let (core, tx, notify) = (core.clone(), tx.clone(), notify.clone());
                thread::spawn(move || {
                    let (done, wait) = std::sync::mpsc::channel();
                    let i = inc.clone();
                    let agent = core.agent(&i.url);
                    thread::spawn(move || {
                        let _ = done.send(engine::probe::same_file(&agent, &i.url, &i.referrer, &i.cookies, &i.mime, i.size, &i.filename));
                    });
                    let same = wait.recv_timeout(Duration::from_secs(15)).unwrap_or_else(|_| Err("no answer".into()));
                    if let Err(e) = &same {
                        eprintln!("Left to the browser: {}", crate::transport::redact(e));
                    }
                    // 409: the server sends NDM something else; the extension may fetch it in the page (`feed`)
                    let _ = (&s).write_all(format!("HTTP/1.1 {}\r\nContent-Type: application/json\r\nX-NDM: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", if same.is_ok() { "200 OK" } else { "409 Conflict" }).as_bytes());
                    if same.is_ok() {
                        let _ = tx.send(inc);
                        notify();
                    }
                });
            }
        });
    }
}
