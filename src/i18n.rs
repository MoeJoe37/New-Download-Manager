//! NDM's language files: `Languages\ndm_xx.lng`, made from `ndm_en.lng` (every UI string, generated from the
//! source by tools/ndm_lng.py). Each line is "English=translation"; the UI calls `tr("English")`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// (exact key -> text, key without the product name -> text); see `key`.
static TABLE: RwLock<Option<(HashMap<String, String>, HashMap<String, String>)>> = RwLock::new(None);
static CURRENT: RwLock<String> = RwLock::new(String::new());

/// The loaded language's file stem ("" = English).
pub fn current() -> String {
    CURRENT.read().unwrap().clone()
}

/// Arabic, Farsi, Hebrew: windows are mirrored (`dlu::D` and the main window follow).
pub fn is_rtl() -> bool {
    rtl_stem(&CURRENT.read().unwrap())
}

fn rtl_stem(stem: &str) -> bool {
    matches!(stem.rsplit('_').next(), Some("ar" | "fa" | "iw" | "he")) && stem.contains('_')
}

/// `Languages` next to the exe or in a parent folder (the repo's copy when run from target\release).
pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|a| a.join("Languages")).find(|d| d.join("ndm_en.lng").exists())
}

/// (file stem, display name from its `lang=0x.. Name` line), sorted by name; English (ndm_en) is built in.
pub fn list() -> Vec<(String, String)> {
    let Some(d) = dir() else { return Vec::new() };
    let mut v: Vec<(String, String)> = std::fs::read_dir(d).into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".lng").filter(|s| s.starts_with("ndm_") && *s != "ndm_en")?.to_string();
        let (_, header) = read(&e.path())?;
        let name = header.split_once(' ').map_or(header.clone(), |h| h.1.trim().to_string());
        Some((stem.clone(), if rtl_stem(&stem) { rtl(&name) } else { name }))
    }).collect();
    v.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    v
}

/// Use `stem` (e.g. "ndm_de"); "" = English.
pub fn load(stem: &str) {
    let table = (!stem.is_empty()).then(|| dir().and_then(|d| read(&d.join(format!("{stem}.lng"))))).flatten().map(|(m, _)| own(m));
    *CURRENT.write().unwrap() = if table.is_some() { stem.to_string() } else { String::new() };
    *TABLE.write().unwrap() = table;
}

/// A language file's "English=translation" lines as the lookup table (`\n` = a new line).
fn own(lines: HashMap<String, String>) -> (HashMap<String, String>, HashMap<String, String>) {
    let (mut m, mut loose) = (HashMap::new(), HashMap::new());
    for (en, t) in lines.into_iter().filter(|(_, t)| !t.trim().is_empty()) {
        let (k, t) = (key(&en.replace("\\n", "\n")), t.replace("\\n", "\n"));
        loose.entry(unbranded(&k)).or_insert_with(|| t.clone());
        m.insert(k, t);
    }
    (m, loose)
}

/// The loaded language's text for an English UI string, if it has one.
fn lookup(s: &str) -> Option<String> {
    let t = TABLE.read().unwrap();
    let (m, loose) = t.as_ref()?;
    let k = key(s);
    // without the product name only for longer texts: "Download with NDM" must not become "Download"
    m.get(&k).or_else(|| Some(unbranded(&k)).filter(|u| u.split(' ').count() >= 3).and_then(|u| loose.get(&u))).cloned()
}

/// The translation of an English UI string, or the string itself.
pub fn tr(s: &str) -> String {
    match lookup(s) {
        Some(t) => visual(&punct(s, &t)),
        None => brand(s),
    }
}

/// `tr` in logical order, for text Windows draws itself (title bars, the tray menu, message windows):
/// Windows runs the bidi algorithm on it, so the display-order text egui gets would come out reversed.
pub fn tr_os(s: &str) -> String {
    match lookup(s) {
        Some(t) => punct(s, &t),
        None => brand(s),
    }
}

/// The punctuation of the English call site wins ("Options" vs a file's "&Options...").
fn punct(s: &str, t: &str) -> String {
    let end = |x: &str| x.len() - x.trim_end_matches(['.', ':', ' ', '\u{2026}', '\u{ff1a}']).len();
    format!("{}{}", &t[..t.len() - end(t)], &s[s.len() - end(s)..])
}

/// egui lays text out left to right without shaping, so right-to-left languages are handed over in display
/// order: letters joined into their contextual forms (ar-reshaper), then each line reordered by the Unicode
/// bidi algorithm with a right-to-left base.
fn visual(t: &str) -> String {
    if !rtl_stem(&current()) {
        return t.to_string();
    }
    rtl(t)
}

/// Shape and reorder right-to-left text for egui (see `visual`).
fn rtl(t: &str) -> String {
    t.split('\n')
        .map(|line| {
            let shaped = ar_reshaper::reshape_line(line);
            let info = unicode_bidi::BidiInfo::new(&shaped, Some(unicode_bidi::Level::rtl()));
            info.paragraphs.iter().map(|p| info.reorder_line(p, p.range.clone()).into_owned()).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tip of the Day texts, in the loaded language.
pub fn tips() -> Vec<String> {
    vec![
        tr("NDM takes over downloads from your browser once its extension is installed. Hold Del while clicking a link to let the browser download it instead, or Ins to make NDM take it."),
        tr("Right-click a page in your browser and choose \"Download all links with New Download Manager\" to pick files from every link on the page."),
        tr("Downloads are split into parts that arrive at the same time. A broken download resumes where it stopped."),
        tr("Put files in a queue with \"Download Later\" and let the Scheduler start the queue at night."),
        tr("The Site Grabber downloads all pictures, documents or a whole site from a start page."),
        tr("Copy a download address and NDM offers to download it (Options -> General)."),
        tr("Drag a link onto the drop target (Tasks -> Show drop target) to download it."),
        tr("The Speed Limiter keeps some bandwidth free for browsing while NDM downloads."),
        tr("Video panels over web players list the streams the page plays; NDM joins HLS and DASH pieces into one file."),
        tr("Closing the main window keeps NDM running in the notification area. Use Tasks -> Exit to quit."),
    ]
}

/// `tr` for printf-style texts ("Queue # %ld", "Default download directory for \"%s\" category"): translate
/// the template, then fill its %s / %d / %ld / %I64d slots in order.
pub fn trf(en: &str, args: &[&dyn std::fmt::Display]) -> String {
    // fill the slots in reading order, then lay the whole line out (right-to-left languages)
    let Some(t) = lookup(en).map(|t| punct(en, &t)) else { return fill(&brand(en), args) };
    visual(&fill(&t, args))
}

fn fill(t: &str, args: &[&dyn std::fmt::Display]) -> String {
    let (mut out, mut it, mut chars) = (String::new(), args.iter(), t.chars().peekable());
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        while let Some(&n) = chars.peek() {
            chars.next();
            if matches!(n, 's' | 'd' | 'u' | 'S') {
                break;
            }
        }
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
    }
    out
}

/// Lookup key: no '&' accelerators or "\tCtrl+N" shortcuts, the product name as "ndm", no trailing ':' /
/// '...', lower case, single spaces.
fn key(s: &str) -> String {
    let s = s.split('\t').next().unwrap_or("").replace('&', "").replace("New Download Manager", "NDM").replace("New Download Manager", "NDM").replace("NDM", "NDM");
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(['.', ':', ' ', '!']).to_lowercase()
}

/// A key with the product name left out ("customize ndm menu items" = "customize menu items").
fn unbranded(k: &str) -> String {
    k.replace("with ndm", "").split_whitespace().filter(|w| !matches!(*w, "ndm" | "ndm's")).collect::<Vec<_>>().join(" ")
}

/// UI texts written as another program words them get NDM's name: "New Download Manager", and "NDM" as
/// a word (not inside e.g. "ndman").
fn brand(t: &str) -> String {
    let t = t.replace("New Download Manager", "New Download Manager");
    let mut out = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(i) = rest.find("NDM") {
        let before = rest[..i].chars().next_back();
        let after = rest[i + 3..].chars().next();
        let word = !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric);
        out.push_str(&rest[..i]);
        out.push_str(if word { "NDM" } else { "NDM" });
        rest = &rest[i + 3..];
    }
    out.push_str(rest);
    out
}

/// A language file: ("English" -> text, the `lang=` header). UTF-8, with or without a BOM.
fn read(path: &Path) -> Option<(HashMap<String, String>, String)> {
    let b = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&b)).into_owned();
    let mut lines = text.lines();
    let header = lines.next()?.trim().strip_prefix("lang=")?.to_string();
    let map = lines.filter(|l| !l.starts_with("//")).filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim_end_matches('\r').to_string())).collect();
    Some((map, header))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        let (m, _) = own(HashMap::from([("&Options...".to_string(), "Optionen".to_string()), ("Line one\\nline two".into(), "A\\nB".into())]));
        assert_eq!((m.get(&key("Options")).map(String::as_str), m.get(&key("Line one\nline two")).map(String::as_str)), (Some("Optionen"), Some("A\nB")));
        assert!(rtl_stem("ndm_ar") && !rtl_stem("ndm_en") && !rtl_stem(""));
        load("ndm_en");
        assert_eq!((current(), tr("Options")), ("ndm_en".to_string(), "Options".to_string()), "Languages\\ndm_en.lng loads");
        assert_eq!(tr("no such string"), "no such string");
        assert_eq!(tips().len(), 10);
        load("");
        assert_eq!(tr("Options"), "Options");
        assert_eq!(key("&Options...\tCtrl+O"), "options");
        assert_eq!(key("Download with New Download Manager"), key("Download with NDM"));
        assert_eq!(brand("Mit NDM laden NDM"), "Mit NDM laden NDM", "only the word is replaced");
        assert_eq!(trf("Queue # %ld", &[&3]), "Queue # 3");
        assert_eq!(trf("Downloaded %s (%I64d Bytes) 100%%", &[&"1 MB", &1048576]), "Downloaded 1 MB (1048576 Bytes) 100%");
    }
}
