//! Site grabber: crawls web pages from a start address and collects links to files matching masks.
use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use url::Url;

const MAX_PAGES: usize = 3000; // ponytail: hard cap instead of NDM's per-project limits
const MAX_PAGE_BYTES: u64 = 4 << 20;
const PAGE_EXTS: [&str; 10] = ["", "htm", "html", "shtml", "php", "asp", "aspx", "jsp", "cgi", "pl"];

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Opts {
    pub name: String,
    pub start: String,
    pub auth: bool,
    pub login: String,
    pub pass: String,
    pub main_domain: bool,
    pub exclude: String,
    pub files_same_site: bool,
    pub hide_dups: bool,
    pub start_all: bool,
    pub whole_site: bool,
    pub levels: u32,
    pub levels_other: u32,
    pub no_parent: bool,
    pub masks: String,
}

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Found {
    pub url: String,
    pub name: String,
    pub text: String,
    pub checked: bool,
}

#[derive(Default)]
pub struct State {
    pub pages: usize,
    /// Links looked at ("Explored" in NDM Grabber Statistics, template 271).
    pub explored: usize,
    pub files: Vec<Found>,
    /// The start page answered 401: its realm, for "Connecting to the site" (template 279).
    pub need_login: Option<String>,
    pub status: String,
    pub done: bool,
}

#[derive(Default)]
pub struct Crawl {
    pub stop: AtomicBool,
    pub auto_started: AtomicBool,
    pub state: Mutex<State>,
}

/// `*.zip,*.rar` style masks, case-insensitive, `*` and `?` wildcards.
pub fn mask_match(masks: &str, name: &str) -> bool {
    fn wild(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, _) => s.is_empty(),
            (Some(b'*'), _) => wild(&p[1..], s) || (!s.is_empty() && wild(p, &s[1..])),
            (Some(b'?'), Some(_)) => wild(&p[1..], &s[1..]),
            (Some(a), Some(b)) => a == b && wild(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let name = name.to_ascii_lowercase();
    !name.is_empty() && masks.split([',', ';', ' ']).map(str::trim).filter(|m| !m.is_empty()).any(|m| wild(m.to_ascii_lowercase().as_bytes(), name.as_bytes()))
}

/// Links in href= / src= attributes, resolved against `base`, with the text right after the tag.
pub fn links(base: &Url, html: &str) -> Vec<(Url, String)> {
    let lower = html.to_ascii_lowercase(); // same byte offsets as `html`
    let mut out = vec![];
    for attr in ["href=", "src="] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(attr) {
            let v = from + i + attr.len();
            from = v;
            let rest = &html[v..];
            let (val, end) = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => match rest[1..].find(q) {
                    Some(e) => (&rest[1..e + 1], e + 2),
                    None => continue,
                },
                _ => {
                    let e = rest.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(rest.len());
                    (&rest[..e], e)
                }
            };
            let val = val.trim().replace("&amp;", "&");
            let lv = val.to_ascii_lowercase();
            if val.is_empty() || ["javascript:", "mailto:", "data:", "#"].iter().any(|p| lv.starts_with(p)) {
                continue;
            }
            let Ok(mut u) = base.join(&val) else { continue };
            if u.scheme() != "http" && u.scheme() != "https" {
                continue;
            }
            u.set_fragment(None);
            let after = &html[v + end..];
            let text = after.find('>').and_then(|g| after[g + 1..].split('<').next()).unwrap_or("");
            out.push((u, text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect()));
        }
    }
    out
}

/// Last two host labels, e.g. "files.example.com" -> "example.com".
fn main_domain(host: &str) -> String {
    let parts: Vec<&str> = host.rsplitn(3, '.').collect();
    if parts.len() >= 2 { format!("{}.{}", parts[1], parts[0]) } else { host.into() }
}

fn is_page(u: &Url) -> bool {
    let last = u.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let ext = last.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    PAGE_EXTS.contains(&ext.as_str())
}

/// Breadth-first crawl. `levels` = link hops explored past the start page within its site;
/// `levels_other` = hops allowed onto other sites. Files are collected from every explored page.
pub fn run(opts: Opts, agent: ureq::Agent, c: Arc<Crawl>) {
    let set = |f: &dyn Fn(&mut State)| f(&mut c.state.lock().unwrap());
    let mut start = match Url::parse(opts.start.trim()) {
        Ok(u) if u.scheme().starts_with("http") => u,
        _ => return set(&|s| (s.status, s.done) = ("Invalid start address".into(), true)),
    };
    if opts.auth {
        // ureq sends Basic auth for credentials in the URL; relative links inherit them.
        let _ = start.set_username(&opts.login);
        let _ = start.set_password(Some(&opts.pass));
    }
    let host = start.host_str().unwrap_or("").to_string();
    let domain = main_domain(&host);
    let same_site = |u: &Url| {
        let h = u.host_str().unwrap_or("");
        h == host || (opts.main_domain && (h == domain || h.ends_with(&format!(".{domain}"))))
    };
    let mut names = HashSet::new();
    let dir = start.path()[..=start.path().rfind('/').unwrap_or(0)].to_string();
    let mut seen = HashSet::from([start.to_string()]);
    let mut files = HashSet::new();
    let mut queue = VecDeque::from([(start, 0u32, 0u32)]);
    // ponytail: one page at a time; NDM explores up to 10 in parallel
    while let Some((u, d, od)) = queue.pop_front() {
        if c.stop.load(Relaxed) || c.state.lock().unwrap().pages >= MAX_PAGES {
            break;
        }
        set(&|s| s.status = format!("Exploring {u}"));
        let resp = match agent.get(u.as_str()).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(401, r)) if c.state.lock().unwrap().pages == 0 => {
                let h = r.header("WWW-Authenticate").unwrap_or("");
                let realm = h.split_once("realm=").map_or("", |(_, v)| v.trim_start_matches('"').split('"').next().unwrap_or(""));
                c.state.lock().unwrap().need_login = Some(realm.to_string());
                break;
            }
            Err(_) => continue,
        };
        if !resp.header("Content-Type").unwrap_or("text/html").contains("html") {
            continue;
        }
        let base = Url::parse(resp.get_url()).unwrap_or(u);
        let mut body = vec![];
        if resp.into_reader().take(MAX_PAGE_BYTES).read_to_end(&mut body).is_err() {
            continue;
        }
        let html = String::from_utf8_lossy(&body);
        let mut found = vec![];
        let all = links(&base, &html);
        c.state.lock().unwrap().explored += all.len();
        for (l, text) in all {
            let name = crate::engine::url_name(l.as_str());
            if mask_match(&opts.masks, &name) {
                let wanted = !mask_match(&opts.exclude, &name) && (!opts.files_same_site || same_site(&l));
                if wanted && files.insert(l.to_string()) && (!opts.hide_dups || names.insert(name.to_ascii_lowercase())) {
                    found.push(Found { url: l.to_string(), name: crate::engine::sanitize(&name), text, checked: true });
                }
                continue;
            }
            if !is_page(&l) || !seen.insert(l.to_string()) {
                continue;
            }
            let same = same_site(&l);
            let (nd, nod) = if same { (d + 1, od) } else { (d, od + 1) };
            let ok = if same { (opts.whole_site || nd <= opts.levels) && (!opts.no_parent || l.path().starts_with(&dir)) } else { nod <= opts.levels_other };
            if ok {
                queue.push_back((l, nd, nod));
            }
        }
        let mut s = c.state.lock().unwrap();
        s.pages += 1;
        s.files.extend(found);
    }
    let stopped = c.stop.load(Relaxed);
    set(&|s| {
        s.status = if stopped { "Stopped".into() } else { format!("Done. {} web pages processed, {} files found", s.pages, s.files.len()) };
        s.done = true;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn domains() {
        assert_eq!(main_domain("a.b.example.com"), "example.com");
        assert_eq!(main_domain("localhost"), "localhost");
    }

    #[test]
    fn masks() {
        assert!(mask_match("*.zip,*.exe", "A.ZIP"));
        assert!(mask_match("img??.jpg", "img01.jpg"));
        assert!(!mask_match("*.zip", "a.zip.html"));
        assert!(!mask_match("", "a.zip"));
    }

    #[test]
    fn extracts_links() {
        let base = Url::parse("http://h.com/dir/page.html").unwrap();
        let l = links(&base, r##"<a href="f.zip">The File</a> <A HREF='/x/'>x</A> <img src=//cdn.com/i.png> <a href="javascript:void(0)">j</a> <a href="#top">t</a>"##);
        let urls: Vec<String> = l.iter().map(|(u, _)| u.to_string()).collect();
        assert_eq!(urls, ["http://h.com/dir/f.zip", "http://h.com/x/", "http://cdn.com/i.png"]);
        assert_eq!(l[0].1, "The File");
    }

    #[test]
    fn crawls_levels() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(&s);
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).map_or(false, |n| n > 2) {}
                let body = match line.split(' ').nth(1).unwrap() {
                    "/" => r#"<a href="a.zip">A</a><a href="sub/">next</a><a href="page.txt">t</a>"#,
                    "/sub/" => r#"<a href="b.zip">B</a><a href="deeper.html">d</a>"#,
                    "/sub/deeper.html" => r#"<a href="c.zip">C</a>"#,
                    _ => "",
                };
                let _ = (&s).write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        let crawl = |levels| {
            let c = Arc::new(Crawl::default());
            let opts = Opts { start: format!("{base}/"), levels, masks: "*.zip".into(), ..Opts::default() };
            run(opts, ureq::agent(), c.clone());
            let s = c.state.lock().unwrap();
            (s.pages, s.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>())
        };
        assert_eq!(crawl(0), (1, vec!["a.zip".to_string()]));
        assert_eq!(crawl(1), (2, vec!["a.zip".to_string(), "b.zip".into()]));
        assert_eq!(crawl(5).1, ["a.zip", "b.zip", "c.zip"]);
    }
}
