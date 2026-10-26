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
    /// Step 1 "Advanced": don't open these (logout) pages, wildcards allowed.
    pub logout_on: bool,
    pub logout: String,
    /// Step 3 "Advanced": explore pages within these paths/domains only / not within these.
    pub explore_in_on: bool,
    pub explore_in: String,
    pub explore_not_on: bool,
    pub explore_not: String,
    /// Step 4 "Advanced": download files within these paths/domains only / not within these.
    pub files_in_on: bool,
    pub files_in: String,
    pub files_not_on: bool,
    pub files_not: String,
    /// Step 2: add the checked files to the main list and queue when the grabber closes.
    pub add_on_close: bool,
    /// Step 2: keep the site's folders under the chosen folder; rewrite links in downloaded pages for
    /// offline browsing; overwrite files instead of numbering them (index_2.html).
    pub subfolders: bool,
    pub convert: bool,
    pub overwrite: bool,
    /// Step 4 "Advanced": "Not less than" / "Not more than" sizes, units index `SIZE_UNITS`.
    pub min_on: bool,
    pub max_on: bool,
    pub size: [u32; 2],
    pub units: [u8; 2],
    /// Step 1 "Advanced": "Enter login and password manually at the following web page".
    pub manual_on: bool,
    pub manual: String,
    /// Step 3 "Process JavaScript": also take relative paths and joined string literals from scripts.
    pub js: bool,
    /// The browser's cookies after the manual login; not saved with the project.
    #[serde(skip)]
    pub cookies: String,
    /// The project template picked on step 1 (`TEMPLATES` or a saved one); "" = Custom settings.
    pub template: String,
}

/// Grabber Settings (template 262) a crawl runs with: pages explored at the same time, "Use Advanced web
/// page processing" (srcset, CSS url(), data-src, meta refresh).
#[derive(Clone, Copy)]
pub struct Tuning {
    pub explore: usize,
    pub adv: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { explore: 1, adv: false }
    }
}

/// NDM's built-in project templates (combo 1511 on step 1; applied by NDM.
pub const TEMPLATES: [&str; 7] = [
    "Custom settings",
    "All Pictures of the web site",
    "Audio files from the start page and from one next level",
    "All Video files of the web site",
    "All files of the web site except web pages and images",
    "Complete start page with all pictures, styles, and scripts for offline browsing",
    "The whole web site",
];

/// Built-in template `k` of `TEMPLATES` NDM, `mask` = a named file filter's masks.
/// Returns the Save To mode and the category it names (Music / Video), None for Custom settings.
pub fn builtin_template(k: usize, opts: &mut Opts, mask: &dyn Fn(&str) -> String) -> Option<(u8, Option<&'static str>)> {
    if k == 0 {
        return None;
    }
    (opts.exclude, opts.convert, opts.subfolders, opts.whole_site, opts.levels_other) = (String::new(), false, false, true, 0);
    Some(match k {
        1 => {
            opts.masks = mask("Images Files");
            (0, None)
        }
        2 => {
            (opts.masks, opts.whole_site, opts.levels) = (mask("Audio Files"), false, 1);
            (1, Some("Music"))
        }
        3 => {
            opts.masks = mask("Video Files");
            (1, Some("Video"))
        }
        4 => {
            (opts.masks, opts.exclude) = ("*.*".into(), format!("*.htm;*.html;*.shtml;*.php;*.asp;*.aspx;*.jsp;{};*.svg;*.ico", mask("Images Files")));
            (0, None)
        }
        _ => {
            // offline copies: one folder with the site's subfolders, links converted
            (opts.masks, opts.files_same_site, opts.hide_dups) = (if k == 5 { mask("Web content") } else { "*.*".into() }, false, false);
            (opts.convert, opts.subfolders) = (true, true);
            if k == 5 {
                (opts.whole_site, opts.levels) = (false, 0);
            }
            (2, None)
        }
    })
}

pub const SIZE_UNITS: [&str; 3] = ["KB", "MB", "GB"];

impl Opts {
    fn limit(&self, k: usize) -> u64 {
        self.size[k] as u64 * (1024u64 << (10 * self.units[k].min(2) as u64))
    }

    /// A file of `size` bytes passes the size filter (unknown sizes pass).
    pub fn size_ok(&self, size: Option<u64>) -> bool {
        size.is_none_or(|s| (!self.min_on || s >= self.limit(0)) && (!self.max_on || s <= self.limit(1)))
    }
}

/// "host\dir\sub" of a file's address, for "Use original relative subfolders".
pub fn site_dir(u: &Url) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(crate::engine::sanitize(u.host_str().unwrap_or("")));
    let segs: Vec<&str> = u.path_segments().map(|s| s.collect()).unwrap_or_default();
    for s in segs.iter().take(segs.len().saturating_sub(1)).filter(|s| !s.is_empty()) {
        p.push(crate::engine::sanitize(&percent_encoding::percent_decode_str(s).decode_utf8_lossy()));
    }
    p
}

/// `name`, else `name_2.ext`, `name_3.ext`... (the grabber's numbering) whichever `free` accepts.
pub fn numbered(name: &str, free: &dyn Fn(&str) -> bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    std::iter::once(name.to_string()).chain((2..).map(|i| format!("{stem}_{i}{ext}"))).find(|n| free(n)).unwrap()
}

/// "Convert the links ... for offline browsing": every href/src in a downloaded page becomes the
/// relative path of the downloaded file it points to (`local`), else an absolute internet address.
pub fn localize(html: &str, base: &Url, page: &std::path::Path, local: &dyn Fn(&Url) -> Option<std::path::PathBuf>) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    let mut spots = vec![];
    for attr in ["href=", "src="] {
        spots.extend(lower.match_indices(attr).map(|(i, _)| i + attr.len()));
    }
    spots.sort_unstable();
    let from = page.parent().unwrap_or(page);
    for v in spots {
        let rest = &html[v..];
        let (s, e) = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => match rest[1..].find(q) {
                Some(e) => (v + 1, v + 1 + e),
                None => continue,
            },
            _ => (v, v + rest.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(rest.len())),
        };
        let val = html[s..e].trim().replace("&amp;", "&");
        let lv = val.to_ascii_lowercase();
        if s < at || val.is_empty() || ["javascript:", "mailto:", "data:", "#"].iter().any(|p| lv.starts_with(p)) {
            continue;
        }
        let Ok(mut u) = base.join(&val) else { continue };
        let frag = u.fragment().map(|f| format!("#{f}")).unwrap_or_default();
        u.set_fragment(None);
        let new = match local(&u) {
            Some(p) => format!("{}{frag}", relative(from, &p)),
            None if u.scheme().starts_with("http") => format!("{u}{frag}"),
            None => continue,
        };
        out.push_str(&html[at..s]);
        out.push_str(&new);
        at = e;
    }
    out.push_str(&html[at..]);
    out
}

/// `to` relative to the folder `from`, with forward slashes ("../img/a.png").
fn relative(from: &std::path::Path, to: &std::path::Path) -> String {
    let low = |p: &std::path::Path| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
    let (f, t) = (low(from), low(to));
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    // non-ASCII is %-encoded too, so a Latin-1 page can take the link back byte for byte
    const SEG: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS.add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'\'').add(b'`');
    let names = to.components().skip(common).map(|c| percent_encoding::utf8_percent_encode(&c.as_os_str().to_string_lossy(), SEG).to_string());
    std::iter::repeat_n("..".to_string(), f.len() - common).chain(names).collect::<Vec<_>>().join("/")
}

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Found {
    pub url: String,
    pub name: String,
    pub text: String,
    pub checked: bool,
    /// The explored page that links to it ("Link View").
    pub page: String,
    /// Asked for (HEAD) when a size filter is on.
    pub size: Option<u64>,
}

#[derive(Default)]
pub struct State {
    pub pages: usize,
    /// The pages explored, in order ("Link View" in the results window).
    pub page_list: Vec<String>,
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

/// `u` lies within one of the `;`-separated paths/domains: "example.com" (and its subdomains),
/// "example.com/docs/" (a path prefix), or a wildcard pattern matched against host + path.
pub fn in_paths(list: &str, u: &Url) -> bool {
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let at = format!("{host}{}", u.path()).to_ascii_lowercase();
    list.split(';').map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).any(|e| {
        let e = e.trim_start_matches("http://").trim_start_matches("https://").to_string();
        if e.contains(['*', '?']) {
            mask_match(&e, &at)
        } else if e.contains('/') {
            at.starts_with(&e)
        } else {
            host == e || host.ends_with(&format!(".{e}"))
        }
    })
}

/// A link value as written in the page, resolved against `base`; None for scripts, mail, anchors.
fn resolve(base: &Url, val: &str) -> Option<Url> {
    let val = val.trim().replace("&amp;", "&");
    let lv = val.to_ascii_lowercase();
    if val.is_empty() || ["javascript:", "mailto:", "data:", "#", "tel:"].iter().any(|p| lv.starts_with(p)) {
        return None;
    }
    let mut u = base.join(&val).ok()?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return None;
    }
    u.set_fragment(None);
    Some(u)
}

/// The quoted value after each `attr` (e.g. `href=`), with its end offset in `html`.
fn attr_values<'a>(html: &'a str, lower: &str, attr: &str) -> Vec<(&'a str, usize)> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = lower[from..].find(attr) {
        let v = from + i + attr.len();
        from = v;
        // a whole attribute name only: "src=" must not match inside "data-src=" unless asked for
        if lower[..v - attr.len()].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            continue;
        }
        let rest = &html[v..];
        match rest.chars().next() {
            Some(q @ ('"' | '\'')) => {
                if let Some(e) = rest[1..].find(q) {
                    out.push((&rest[1..e + 1], v + e + 2));
                }
            }
            _ => {
                let e = rest.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(rest.len());
                out.push((&rest[..e], v + e));
            }
        }
    }
    out
}

/// String literals in script text: absolute http(s) addresses always (NDM screens scripts even with
/// "Process JavaScript" off); with `js` also relative paths with a file extension, and literals joined
/// with `+` ("files/" + "a.zip").
// ponytail: literals are screened, scripts are never run (no JS engine); computed addresses are missed
fn script_links(code: &str, js: bool) -> Vec<String> {
    let mut lits: Vec<(usize, usize, String)> = vec![];
    let b = code.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let q = b[i];
        if q == b'"' || q == b'\'' || q == b'`' {
            let start = i;
            i += 1;
            let mut s = String::new();
            while i < b.len() && b[i] != q && b[i] != b'\n' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                let ch = code[i..].chars().next().unwrap();
                s.push(ch);
                i += ch.len_utf8();
            }
            lits.push((start, i, s));
        }
        i += 1;
    }
    let mut out = vec![];
    let mut k = 0;
    while k < lits.len() {
        let mut s = lits[k].2.clone();
        // "a" + "b" + 'c'
        while js && k + 1 < lits.len() && code[lits[k].1 + 1..lits[k + 1].0].trim() == "+" {
            k += 1;
            s.push_str(&lits[k].2);
        }
        k += 1;
        let l = s.to_ascii_lowercase();
        let abs = l.starts_with("http://") || l.starts_with("https://") || l.starts_with("//");
        let file = !s.contains(char::is_whitespace) && !s.contains(['<', '>', '{', '}', '(', ')', ',']) && s.rsplit('/').next().is_some_and(|n| n.rsplit_once('.').is_some_and(|(a, e)| !a.is_empty() && (1..=5).contains(&e.len()) && e.bytes().all(|c| c.is_ascii_alphanumeric())));
        if (abs && !s.contains(char::is_whitespace)) || (js && file) {
            out.push(s);
        }
    }
    out
}

/// Links in href= / src= attributes (and in scripts, see `script_links`), resolved against `base`, with
/// the text right after the tag. `adv` ("Use Advanced web page processing") also reads srcset, data-src,
/// CSS url(...) and <meta http-equiv="refresh">.
pub fn links(base: &Url, html: &str, js: bool, adv: bool) -> Vec<(Url, String)> {
    let lower = html.to_ascii_lowercase(); // same byte offsets as `html`
    let mut out = vec![];
    let text_after = |end: usize| -> String {
        let after = &html[end.min(html.len())..];
        let t = after.find('>').and_then(|g| after[g + 1..].split('<').next()).unwrap_or("");
        t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect()
    };
    let mut attrs = vec!["href=", "src="];
    if adv {
        attrs.extend(["data-src=", "data-href=", "poster="]);
    }
    for attr in attrs {
        for (val, end) in attr_values(html, &lower, attr) {
            if let Some(u) = resolve(base, val) {
                out.push((u, text_after(end)));
            }
        }
    }
    if adv {
        for (val, _) in attr_values(html, &lower, "srcset=") {
            out.extend(val.split(',').filter_map(|c| resolve(base, c.split_whitespace().next().unwrap_or(""))).map(|u| (u, String::new())));
        }
        let mut from = 0;
        while let Some(i) = lower[from..].find("url(") {
            let v = from + i + 4;
            from = v;
            if let Some(e) = html[v..].find(')') {
                if let Some(u) = resolve(base, html[v..v + e].trim().trim_matches(['"', '\''])) {
                    out.push((u, String::new()));
                }
            }
        }
        // <meta http-equiv="refresh" content="5; url=next.html">
        for (val, _) in attr_values(html, &lower, "content=") {
            let at = val.to_ascii_lowercase().find("url=").filter(|_| val.trim_start().starts_with(|c: char| c.is_ascii_digit()));
            if let Some(a) = at {
                out.extend(resolve(base, val[a + 4..].trim_matches(['"', '\'', ' '])).map(|u| (u, String::new())));
            }
        }
    }
    // scripts: <script> bodies and on...= handlers
    let mut code = String::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<script") {
        let s = from + i;
        let Some(g) = lower[s..].find('>') else { break };
        let body = s + g + 1;
        let end = lower[body..].find("</script").map_or(html.len(), |e| body + e);
        code.push_str(&html[body..end]);
        code.push('\n');
        from = end;
    }
    for ev in ["onclick=", "onload=", "onmouseover=", "onchange=", "onsubmit="] {
        for (val, _) in attr_values(html, &lower, ev) {
            code.push_str(val);
            code.push('\n');
        }
    }
    out.extend(script_links(&code, js).iter().filter_map(|s| resolve(base, s)).map(|u| (u, String::new())));
    out
}

fn main_domain(host: &str) -> String {
    let parts: Vec<&str> = host.rsplitn(3, '.').collect();
    if parts.len() >= 2 { format!("{}.{}", parts[1], parts[0]) } else { host.into() }
}

/// `host` is `site` or one of its subdomains, or both share the main domain.
pub fn same_domain(host: &str, site: &str) -> bool {
    host == site || host.ends_with(&format!(".{site}")) || main_domain(host) == main_domain(site)
}

fn is_page(u: &Url) -> bool {
    let last = u.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let ext = last.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    PAGE_EXTS.contains(&ext.as_str())
}

/// The file name an explored page is saved under: "index.html" for a folder, ".html" added when the
/// name doesn't end in .htm/.html (so it opens in a browser offline).
pub fn page_name(u: &Url) -> String {
    let last = u.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let name = if last.is_empty() { "index.html".to_string() } else { crate::engine::sanitize(&percent_encoding::percent_decode_str(last).decode_utf8_lossy()) };
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if ["htm", "html"].contains(&ext.as_str()) { name } else { format!("{name}.html") }
}

/// What the explorers share: pages to explore (with their link levels), what was seen, files found.
struct Work {
    queue: VecDeque<(Url, u32, u32)>,
    busy: usize,
    seen: HashSet<String>,
    files: HashSet<String>,
    names: HashSet<String>,
}

/// Breadth-first crawl. `levels` = link hops explored past the start page within its site;
/// `levels_other` = hops allowed onto other sites. Files are collected from every explored page;
/// `t.explore` pages are fetched at the same time.
pub fn run(opts: Opts, agent: ureq::Agent, c: Arc<Crawl>, t: Tuning) {
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
    let dir = start.path()[..=start.path().rfind('/').unwrap_or(0)].to_string();
    let work = Mutex::new(Work { queue: VecDeque::from([(start.clone(), 0, 0)]), busy: 0, seen: HashSet::from([start.to_string()]), files: HashSet::new(), names: HashSet::new() });
    let wake = std::sync::Condvar::new();
    let req = |m: &str, u: &Url| {
        let r = agent.request(m, u.as_str());
        if opts.cookies.is_empty() { r } else { r.set("Cookie", &opts.cookies) }
    };
    // one file found: filters, size, duplicates (`work` must not be locked by the caller)
    let take = |l: &Url, name: String, text: String, page: &str, size: Option<u64>| -> Option<Found> {
        let wanted = !mask_match(&opts.exclude, &name)
            && (!opts.files_same_site || same_site(l))
            && (!opts.files_in_on || in_paths(&opts.files_in, l))
            && !(opts.files_not_on && in_paths(&opts.files_not, l));
        if !wanted || !work.lock().unwrap().files.insert(l.to_string()) {
            return None;
        }
        let size = size.or_else(|| if opts.min_on || opts.max_on { req("HEAD", l).call().ok().and_then(|r| r.header("Content-Length")?.parse().ok()) } else { None });
        // duplicates stay visible with relative subfolders: each copy has its own folder
        let dup = opts.hide_dups && !opts.subfolders && !work.lock().unwrap().names.insert(name.to_ascii_lowercase());
        (opts.size_ok(size) && !dup).then(|| Found { url: l.to_string(), name: crate::engine::sanitize(&name), text, checked: true, page: page.to_string(), size })
    };
    let explore = || loop {
        let next = {
            let mut w = work.lock().unwrap();
            loop {
                if c.stop.load(Relaxed) || c.state.lock().unwrap().pages >= MAX_PAGES {
                    wake.notify_all();
                    return;
                }
                if let Some(n) = w.queue.pop_front() {
                    w.busy += 1;
                    break n;
                }
                if w.busy == 0 {
                    wake.notify_all();
                    return;
                }
                w = wake.wait(w).unwrap();
            }
        };
        let (u, d, od) = next;
        let mut found = vec![];
        let mut page = None;
        set(&|s| s.status = format!("Exploring {u}"));
        match req("GET", &u).call() {
            Ok(resp) => {
                let base = Url::parse(resp.get_url()).unwrap_or(u.clone());
                let len = resp.header("Content-Length").and_then(|v| v.parse().ok());
                if !resp.header("Content-Type").unwrap_or("text/html").contains("html") {
                    // a "page" address that turned out to be a file (download.php?id=...)
                    let name = crate::engine::url_name(u.as_str());
                    if mask_match(&opts.masks, &name) {
                        found.extend(take(&u, name, String::new(), "", len));
                    }
                } else {
                    let mut body = vec![];
                    if resp.into_reader().take(MAX_PAGE_BYTES).read_to_end(&mut body).is_ok() {
                        let html = String::from_utf8_lossy(&body);
                        // the page itself, when the filters ask for web pages ("The whole web site", offline)
                        let pn = page_name(&base);
                        if mask_match(&opts.masks, &pn) {
                            found.extend(take(&base, pn, String::new(), base.as_str(), len));
                        }
                        let all = links(&base, &html, opts.js, t.adv);
                        c.state.lock().unwrap().explored += all.len();
                        for (l, text) in all {
                            let name = crate::engine::url_name(l.as_str());
                            if !is_page(&l) && mask_match(&opts.masks, &name) {
                                found.extend(take(&l, name, text, base.as_str(), None));
                                continue;
                            }
                            if !is_page(&l) || !work.lock().unwrap().seen.insert(l.to_string()) {
                                continue;
                            }
                            let at = |list: &str| in_paths(list, &l) || mask_match(list, l.as_str());
                            if (opts.logout_on && at(&opts.logout)) || (opts.explore_in_on && !in_paths(&opts.explore_in, &l)) || (opts.explore_not_on && in_paths(&opts.explore_not, &l)) {
                                continue;
                            }
                            let same = same_site(&l);
                            let (nd, nod) = if same { (d + 1, od) } else { (d, od + 1) };
                            let ok = if same { (opts.whole_site || nd <= opts.levels) && (!opts.no_parent || l.path().starts_with(&dir)) } else { nod <= opts.levels_other };
                            if ok {
                                work.lock().unwrap().queue.push_back((l, nd, nod));
                                wake.notify_one();
                            }
                        }
                        page = Some(base.to_string());
                    }
                }
            }
            Err(ureq::Error::Status(401, r)) if u == start => {
                let h = r.header("WWW-Authenticate").unwrap_or("");
                let realm = h.split_once("realm=").map_or("", |(_, v)| v.trim_start_matches('"').split('"').next().unwrap_or(""));
                c.state.lock().unwrap().need_login = Some(realm.to_string());
                c.stop.store(true, Relaxed);
            }
            Err(_) => {}
        }
        {
            let mut s = c.state.lock().unwrap();
            if let Some(p) = page {
                s.pages += 1;
                s.page_list.push(p);
            }
            s.files.extend(found);
        }
        work.lock().unwrap().busy -= 1;
        wake.notify_all();
    };
    std::thread::scope(|sc| {
        for _ in 1..t.explore.clamp(1, 20) {
            sc.spawn(explore);
        }
        explore();
    });
    let stopped = c.stop.load(Relaxed) && c.state.lock().unwrap().need_login.is_none();
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
    fn path_filters() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(in_paths("example.com", &u("https://cdn.example.com/a.zip")));
        assert!(!in_paths("example.com", &u("https://example.org/a.zip")));
        assert!(in_paths("other.net; example.com/docs/", &u("http://example.com/docs/x/y.pdf")));
        assert!(!in_paths("example.com/docs/", &u("http://example.com/img/y.pdf")));
        assert!(in_paths("*/logout*", &u("https://example.com/user/logout.php")));
    }

    #[test]
    fn script_and_advanced_links() {
        let base = Url::parse("http://h.com/d/p.html").unwrap();
        let urls = |html: &str, js, adv| links(&base, html, js, adv).into_iter().map(|(u, _)| u.to_string()).collect::<Vec<_>>();
        let page = r#"<script>var a = "https://cdn.com/x.zip"; var b = 'files/' + "y.zip"; var t = "text/javascript";</script><button onclick="go('z.pdf')">"#;
        assert_eq!(urls(page, false, false), ["https://cdn.com/x.zip"]);
        assert_eq!(urls(page, true, false), ["https://cdn.com/x.zip", "http://h.com/d/files/y.zip", "http://h.com/d/z.pdf"]);
        let adv = r#"<img data-src="lazy.jpg" srcset="s.jpg 1x, /l.jpg 2x"><div style="background:url('bg.png')"></div><meta http-equiv="refresh" content="3; url=next.html"><meta property="og:url" content="https://h.com/?url=x">"#;
        assert_eq!(urls(adv, false, false), Vec::<String>::new());
        assert_eq!(urls(adv, false, true), ["http://h.com/d/lazy.jpg", "http://h.com/d/s.jpg", "http://h.com/l.jpg", "http://h.com/d/bg.png", "http://h.com/d/next.html"]);
        assert_eq!(page_name(&Url::parse("http://h.com/a/").unwrap()), "index.html");
        assert_eq!(page_name(&Url::parse("http://h.com/a/view.php?id=1").unwrap()), "view.php.html");
    }

    #[test]
    fn templates() {
        let m = |f: &str| format!("<{f}>");
        let mut o = Opts { exclude: "*.x".into(), ..Opts::default() };
        assert_eq!(builtin_template(0, &mut o, &m), None);
        assert_eq!(o.exclude, "*.x");
        assert_eq!(builtin_template(2, &mut o, &m), Some((1, Some("Music"))));
        assert!(o.masks == "<Audio Files>" && !o.whole_site && o.levels == 1 && o.exclude.is_empty());
        assert_eq!(builtin_template(5, &mut o, &m), Some((2, None)));
        assert!(o.convert && o.subfolders && !o.whole_site && o.levels == 0 && o.masks == "<Web content>");
        assert_eq!(builtin_template(6, &mut o, &m), Some((2, None)));
        assert!(o.whole_site && o.masks == "*.*" && o.convert);
        assert_eq!(TEMPLATES.len(), 7);
    }

    #[test]
    fn offline_and_names() {
        use std::path::{Path, PathBuf};
        let base = Url::parse("http://h.com/docs/index.html").unwrap();
        let local = |u: &Url| match u.as_str() {
            "http://h.com/img/a%20b.png" => Some(PathBuf::from(r"C:\s\h.com\img\a b.png")),
            "http://h.com/docs/p2.html" => Some(PathBuf::from(r"C:\s\h.com\docs\p2.html")),
            _ => None,
        };
        let html = r##"<img src="../img/a%20b.png"><a href='p2.html#x'>2</a><a href=/z.zip>z</a><a href="#top">t</a>"##;
        let out = localize(html, &base, Path::new(r"C:\s\h.com\docs\index.html"), &local);
        assert_eq!(out, r##"<img src="../img/a%20b.png"><a href='p2.html#x'>2</a><a href=http://h.com/z.zip>z</a><a href="#top">t</a>"##);
        assert_eq!(site_dir(&Url::parse("http://h.com/a/b%20c/f.zip").unwrap()), PathBuf::from(r"h.com\a\b c"));
        assert_eq!(numbered("index.html", &|n| n == "index_3.html"), "index_3.html");
        let o = Opts { min_on: true, max_on: true, size: [1, 10], units: [0, 1], ..Opts::default() };
        assert!(o.size_ok(Some(2048)) && o.size_ok(None) && !o.size_ok(Some(100)) && !o.size_ok(Some(11 << 20)));
    }

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
        let l = links(&base, r##"<a href="f.zip">The File</a> <A HREF='/x/'>x</A> <img src=//cdn.com/i.png> <a href="javascript:void(0)">j</a> <a href="#top">t</a>"##, false, false);
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
            run(opts, ureq::agent(), c.clone(), Tuning::default());
            let s = c.state.lock().unwrap();
            (s.pages, s.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>())
        };
        assert_eq!(crawl(0), (1, vec!["a.zip".to_string()]));
        assert_eq!(crawl(1), (2, vec!["a.zip".to_string(), "b.zip".into()]));
        assert_eq!(crawl(5).1, ["a.zip", "b.zip", "c.zip"]);
        // four explorers at once; web pages wanted too (the offline templates save them)
        let c = Arc::new(Crawl::default());
        let opts = Opts { start: format!("{base}/"), levels: 5, masks: "*.zip,*.html".into(), ..Opts::default() };
        run(opts, ureq::agent(), c.clone(), Tuning { explore: 4, adv: true });
        let mut names: Vec<String> = c.state.lock().unwrap().files.iter().map(|f| f.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["a.zip", "b.zip", "c.zip", "deeper.html", "index.html", "index.html"]);
    }
}
