//! NDM's language files (`Languages\ndm_*.lng`, copied from NDM): `ID=text` lines, `template.lng` being the
//! English master. NDM's strings are NDM's English ones, so a translation is: English text -> ID (template)
//! -> text (chosen file). Strings without an NDM counterpart stay English.
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

/// Arabic, Farsi, Hebrew: NDM mirrors its windows for these (WS_EX_LAYOUTRTL); `dlu::D` and the main
/// window follow.
pub fn is_rtl() -> bool {
    matches!(CURRENT.read().unwrap().as_str(), "ndm_ar" | "ndm_fa" | "ndm_iw")
}

/// `Languages` next to the exe or in a parent folder (the repo's copy when run from target\release).
pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|a| a.join("Languages")).find(|d| d.join("template.lng").exists())
}

/// (file stem, display name from its `lang=0x.. Name` line), sorted by name.
pub fn list() -> Vec<(String, String)> {
    let Some(d) = dir() else { return Vec::new() };
    let mut v: Vec<(String, String)> = std::fs::read_dir(d).into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".lng").filter(|s| s.starts_with("ndm_"))?.to_string();
        let (_, header) = read(&e.path())?;
        let name = header.split_once(' ').map_or(header.clone(), |h| h.1.trim().to_string());
        // a right-to-left name (Hebrew) in display order
        Some((stem, if name.chars().any(|c| ('\u{590}'..'\u{900}').contains(&c)) { rtl(&name) } else { name }))
    }).collect();
    v.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    v
}

/// (language, closer language whose NDM file fills its gaps), in order of preference.
const FALLBACK: [(&str, &str); 3] = [("ndm_ba", "ndm_sr"), ("ndm_no", "ndm_dk"), ("ndm_be", "ndm_nl")];

/// Use `stem` (e.g. "ndm_de"); "" = English.
pub fn load(stem: &str) {
    let table = (!stem.is_empty()).then(|| dir().and_then(|d| {
        let mut tr = read(&d.join(format!("{stem}.lng")))?.0;
        // older NDM files lack newer texts: take them from the closest language that has them
        for fb in FALLBACK.iter().filter(|f| f.0 == stem).map(|f| f.1) {
            for (id, t) in read(&d.join(format!("{fb}.lng"))).map(|r| r.0).unwrap_or_default() {
                tr.entry(id).or_insert(t);
            }
        }
        Some((read(&d.join("template.lng"))?.0, tr))
    })).flatten().map(|(en, tr)| {
        let (mut m, mut loose) = (HashMap::new(), HashMap::new());
        for (id, text) in en {
            if let Some(t) = tr.get(&id).filter(|t| !t.trim().is_empty()) {
                let k = key(&clean(&text));
                loose.entry(unbranded(&k)).or_insert_with(|| clean(t));
                m.entry(k).or_insert_with(|| clean(t));
            }
        }
        for (en, t) in extra(stem) {
            m.entry(key(&en)).or_insert(t);
        }
        (m, loose)
    });
    *CURRENT.write().unwrap() = if table.is_some() { stem.to_string() } else { String::new() };
    *TABLE.write().unwrap() = table;
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

/// The punctuation of the English call site wins ("Options" vs NDM's "&Options...").
fn punct(s: &str, t: &str) -> String {
    let end = |x: &str| x.len() - x.trim_end_matches(['.', ':', ' ', '\u{2026}', '\u{ff1a}']).len();
    format!("{}{}", &t[..t.len() - end(t)], &s[s.len() - end(s)..])
}

/// egui lays text out left to right without shaping, so right-to-left languages (NDM's Arabic, Farsi,
/// Hebrew files) are handed over in display order: letters joined into their contextual forms
/// (ar-reshaper), then each line reordered by the Unicode bidi algorithm with a right-to-left base.
fn visual(t: &str) -> String {
    if !matches!(current().as_str(), "ndm_ar" | "ndm_fa" | "ndm_iw") {
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

/// NDM's own strings (texts NDM's files don't have) for `stem`: `Languages\ndm.lng`, `[ndm_xx]` sections of
/// "English=translation" lines.
fn extra(stem: &str) -> Vec<(String, String)> {
    let text = dir().and_then(|d| std::fs::read_to_string(d.join("ndm.lng")).ok()).unwrap_or_default();
    let head = format!("[{stem}]");
    text.lines()
        .skip_while(|l| l.trim() != head)
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.starts_with("//"))
        .filter_map(|l| l.split_once('=').map(|(a, b)| (a.replace("\\n", "\n"), b.trim_end().replace("\\n", "\n"))))
        .collect()
}

/// Tip of the Day texts, one per line: the language's `tips=` file, else NDM's English `tips.txt`
/// (CTipDlg, NDM, in that language's code page; ';' lines are comments (MFC GetNextTipString).
pub fn tips() -> Vec<String> {
    let Some(d) = dir() else { return Vec::new() };
    let stem = current();
    let lng = (!stem.is_empty()).then(|| read(&d.join(format!("{stem}.lng")))).flatten();
    let file = lng.as_ref().and_then(|l| l.0.get("tips")).map(|t| d.join(t.trim())).filter(|p| p.exists());
    let (path, id) = match file {
        Some(p) => (p, lang_id(&lng.unwrap().1)),
        None => (d.join("tips.txt"), 9),
    };
    let Ok(b) = std::fs::read(path) else { return Vec::new() };
    decode(&b, id).lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(';')).map(|l| visual(&clean(l))).collect()
}

/// `tr` for NDM's printf-style texts ("Queue # %ld", "Default download directory for \"%s\" category"):
/// translate the template, then fill its %s / %d / %ld / %I64d slots in order.
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
        let mut spec = String::new();
        while let Some(&n) = chars.peek() {
            chars.next();
            if matches!(n, 's' | 'd' | 'u' | 'S') {
                break;
            }
            spec.push(n);
        }
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
    }
    out
}

/// Lookup key: no '&' accelerators or "\tCtrl+N" shortcuts, the product name (NDM / Internet Download
/// Manager / ours) as "ndm", no trailing ':' / '...', lower case, single spaces.
fn key(s: &str) -> String {
    let s = s.split('\t').next().unwrap_or("").replace('&', "").replace("New Download Manager", "NDM").replace("New Download Manager", "NDM");
    // NDM's English typos, so our corrected text still finds the entry
    let s = s.replace("recommened", "recommended").replace("key(s) key", "key(s)");
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(['.', ':', ' ', '!']).to_lowercase()
}

/// A key with the product name left out ("customize ndm menu items" = "customize menu items").
fn unbranded(k: &str) -> String {
    k.replace("with ndm", "").split_whitespace().filter(|w| !matches!(*w, "ndm" | "ndm's")).collect::<Vec<_>>().join(" ")
}

/// A .lng text as shown: escapes decoded, accelerators and shortcuts dropped, NDM's name replaced by ours.
fn clean(t: &str) -> String {
    let t = t.split("\\t").next().unwrap_or("").replace("\\\\", "\u{2}").replace("\\\"", "\"").replace("\\'", "'");
    let t = t.replace("\\r\\n", "\n").replace("\\n", "\n").replace("&&", "\u{1}").replace('&', "").replace('\u{1}', "&").replace('\u{2}', "\\");
    brand(&t)
}

/// NDM's name replaced by ours: "New Download Manager", and "NDM" as a word (not inside e.g. "ndman").
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

/// (ID -> text, header after "lang=") of a .lng file. Decoding follows NDM: UTF-8 BOM = UTF-8,
/// else the ANSI code page of the `lang=0x..` language (IDs below 0x401 get SUBLANG_DEFAULT).
/// Files that are valid UTF-8 without a BOM are read as UTF-8.
fn read(path: &Path) -> Option<(HashMap<String, String>, String)> {
    let b = std::fs::read(path).ok()?;
    let first = b.split(|c| *c == b'\n').next().unwrap_or(&[]);
    let text = decode(&b, String::from_utf8_lossy(first).strip_prefix("lang=").map_or(9, lang_id));
    let mut lines = text.lines();
    let header = lines.next()?.trim().strip_prefix("lang=")?.to_string();
    let map = lines.filter(|l| !l.starts_with("//")).filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim_end_matches('\r').to_string())).collect();
    Some((map, header))
}

/// The language ID of a `lang=` header ("0x407 German" -> 0x407; English if unreadable).
fn lang_id(header: &str) -> u32 {
    header.trim().split(' ').next().and_then(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16).ok()).unwrap_or(9)
}

/// UTF-8 with a BOM or valid UTF-8 as is, else the ANSI code page of language `id`.
fn decode(b: &[u8], id: u32) -> String {
    match b.strip_prefix(b"\xef\xbb\xbf") {
        Some(rest) => String::from_utf8_lossy(rest).into_owned(),
        None => match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => crate::win::decode_ansi(b, if id < 0x401 { id | 0x400 } else { id }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        let langs = list();
        assert!(langs.len() >= 30, "language files found: {}", langs.len());
        // every file decodes and translates something
        for (stem, name) in &langs {
            load(stem);
            let n = ["Options", "File Name", "Size", "Status", "Description", "Categories"].iter().filter(|s| tr(s) != **s).count();
            assert!(n >= 1, "{stem} ({name}) translated nothing");
            // NDM's own strings: every language has all of them (the [ndm_de] list is the reference)
            let own = extra("ndm_de");
            assert!(own.len() >= 8);
            // (a translation may equal the English, e.g. Romanian "Font", so check the lookup itself)
            for (en, _) in own.iter().chain(&extra(stem)) {
                assert!(lookup(en).is_some(), "{stem} ({name}) lacks \"{en}\" in Languages\\ndm.lng");
            }
            assert!(!tr("Options").contains('\u{fffd}'), "{stem} ({name}) decoded badly");
        }
        load("ndm_iw");
        let he = lookup("Options").unwrap();
        assert_eq!(tr("Options"), he.chars().rev().collect::<String>(), "RTL text is handed to egui in display order");
        assert_eq!(trf("Queue # %ld", &[&12]).contains("12"), true, "numbers keep their order");
        load("ndm_de");
        assert_eq!(tr("no such string"), "no such string");
        let de = tips();
        assert!(de.len() >= 5 && de[0].starts_with("NDM übernimmt"), "German tips: {:?}", de.first());
        load("ndm_dk"); // no tips_dk.txt: English
        assert_eq!(tips().len(), 12);
        load("");
        assert_eq!(tr("Options"), "Options");
        assert_eq!(key("&Options...\tCtrl+O"), "options");
        assert_eq!(unbranded(&key("Customize NDM menu items in context menu of browsers")), key("Customize menu items in context menu of browsers"));
        assert_eq!(key("Download with New Download Manager"), key("Download with NDM"));
        assert_eq!(clean("Mit NDM laden NDM"), "Mit NDM laden NDM");
        assert_eq!(clean(r#"on \"Options->File Types\" tab, it\'s C:\\x"#), r#"on "Options->File Types" tab, it's C:\x"#);
        assert_eq!(trf("Queue # %ld", &[&3]), "Queue # 3");
        assert_eq!(trf("Downloaded %s (%I64d Bytes) 100%%", &[&"1 MB", &1048576]), "Downloaded 1 MB (1048576 Bytes) 100%");
    }
}
