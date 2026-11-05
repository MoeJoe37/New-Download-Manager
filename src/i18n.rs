//! NDM's language files: `Languages\ndm_xx.lng`, made from `ndm_en.lng` (every UI string, generated from the
//! source by tools/ndm_lng.py). Each line is "English=translation"; the UI calls `tr("English")`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// (exact key -> text, key without the product name -> text); see `key`.
static TABLE: RwLock<Option<(HashMap<String, String>, HashMap<String, String>)>> =
    RwLock::new(None);
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
    beside_exe("Languages", "ndm_en.lng")
}

/// NDM's folder `name` (holding `file`): next to the exe, in a parent folder (the repo's when run from
/// target\release), or in a Mac app's Contents/Resources.
pub fn beside_exe(name: &str, file: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .flat_map(|a| [a.join(name), a.join("Resources").join(name)])
        .find(|d| d.join(file).exists())
}

/// (file stem, display name from its `lang=0x.. Name` line), sorted by name; English (ndm_en) is built in.
pub fn list() -> Vec<(String, String)> {
    let Some(d) = dir() else { return Vec::new() };
    let mut v: Vec<(String, String)> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".lng")
                .filter(|s| s.starts_with("ndm_") && *s != "ndm_en")?
                .to_string();
            let (_, header) = read(&e.path())?;
            let name = header
                .split_once(' ')
                .map_or(header.clone(), |h| h.1.trim().to_string());
            Some((
                stem.clone(),
                if rtl_stem(&stem) { rtl(&name) } else { name },
            ))
        })
        .collect();
    v.sort_by_cached_key(|item| item.1.to_lowercase());
    v
}

/// Use `stem` (e.g. "ndm_de"); "" = English.
pub fn load(stem: &str) {
    let table = (!stem.is_empty())
        .then(|| dir().and_then(|d| read(&d.join(format!("{stem}.lng")))))
        .flatten()
        .map(|(m, _)| own(m));
    *CURRENT.write().unwrap() = if table.is_some() {
        stem.to_string()
    } else {
        String::new()
    };
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
    m.get(&k)
        .or_else(|| {
            Some(unbranded(&k))
                .filter(|u| u.split(' ').count() >= 3)
                .and_then(|u| loose.get(&u))
        })
        .cloned()
}

/// The translation of an English UI string, or the string itself (laid out for display when it is text
/// put together in reading order, see `line`).
pub fn tr(s: &str) -> String {
    match lookup(s) {
        Some(t) => visual(&punct(s, &t)),
        None => visual(s),
    }
}

/// `tr` with the translation's own punctuation, for labels whose English is cut short (the toolbar's
/// "Delete Co..."): the dots belong to the English, not to the label.
pub fn tr_label(s: &str) -> String {
    visual(&lookup(s).unwrap_or_else(|| s.to_string()))
}

/// `tr` in logical order, for text Windows draws itself (title bars, the tray menu, message windows):
/// Windows runs the bidi algorithm on it, so the display-order text egui gets would come out reversed.
pub fn tr_os(s: &str) -> String {
    match lookup(s) {
        Some(t) => punct(s, &t),
        None => s.to_string(),
    }
}

/// The punctuation of the English call site wins ("Options" vs a file's "&Options..."); the same marks
/// spaced the language's way stay as the translation has them (French "Port :").
fn punct(s: &str, t: &str) -> String {
    let end = |x: &str| {
        x.len()
            - x.trim_end_matches(['.', ':', ' ', '\u{2026}', '\u{ff1a}'])
                .len()
    };
    let (se, te) = (&s[s.len() - end(s)..], &t[t.len() - end(t)..]);
    if se.replace(' ', "") == te.replace(' ', "") {
        return format!("{}{}", t.trim_end(), &se[se.trim_end().len()..]);
    }
    format!("{}{}", &t[..t.len() - end(t)], se)
}

/// `trf` in reading order, for a line put together from several pieces (see `line`).
pub fn trf_os(en: &str, args: &[&dyn std::fmt::Display]) -> String {
    fill(
        &lookup(en).map_or_else(|| en.to_string(), |t| punct(en, &t)),
        args,
    )
}

/// A line put together in reading order (pieces from `tr_os` / `trf_os`, numbers, names) laid out for egui:
/// right-to-left languages get the whole line reordered at once, so the pieces keep their order. Text
/// already laid out stays as it is.
pub fn line(s: &str) -> String {
    visual(s)
}

/// A name, path or address in a right-to-left line: kept left to right as a whole (a bidi isolate), so
/// "/home/x" doesn't come out as "home/x/".
pub fn ltr(s: &str) -> String {
    if is_rtl() {
        format!("\u{2066}{s}\u{2069}")
    } else {
        s.to_string()
    }
}

/// Whether `s` is right-to-left text still in reading order: Hebrew, or Arabic letters not yet joined
/// (laying out turns them into presentation forms, so laid-out text isn't laid out twice).
fn unshaped(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '\u{5d0}'..='\u{5ea}' | '\u{620}'..='\u{64a}' | '\u{66e}'..='\u{6d3}' | '\u{6fa}'..='\u{6ff}'))
}

/// Right-to-left text in reading order wrapped to `width` (as `measure` sizes a display-order line) and laid
/// out for egui. egui wraps a display-order paragraph from its left end, which would put the paragraph's
/// last words on its first line; here the words are packed into lines from the start of the text instead.
pub fn wrap_rtl(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> String {
    let mut lines = Vec::new();
    for para in text.replace('\r', "").split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let next = if cur.is_empty() {
                word.to_string()
            } else {
                format!("{cur} {word}")
            };
            if !cur.is_empty() && measure(&rtl(&next)) > width {
                lines.push(rtl(&cur));
                cur = word.to_string();
            } else {
                cur = next;
            }
        }
        lines.push(rtl(&cur));
    }
    lines.join("\n")
}

/// Messages NDM puts values into (grabber and connection status lines, download errors). They are kept in
/// English where they are made and translated where they are shown (`tr_msg_os`).
const MESSAGES: [&str; 13] = [
    "Exploring %s",
    "Done. %d web pages processed, %d files found",
    "Error: %s, retrying...",
    "%d links",
    "network %s is gone",
    "Kerberos sign-in failed (%s); for NTLM put a user name and password in the address",
    "%s sign-in needs a user name and password in the address",
    "%s authentication failed",
    "%s authentication is not available",
    "pkexec is missing: install the update by hand with %s",
    "%s has no address",
    "The \"%s\" category already exists.",
    "Do you really want to delete \"%s\" category from the categories list?",
];

/// The translation of a text that may be one of `MESSAGES` with its values in (the template translated,
/// then filled with the same values), in reading order: for a line put together from pieces (`line`).
pub fn tr_msg_os(s: &str) -> String {
    match message(s) {
        Some((t, args)) => trf_os(
            t,
            &args
                .iter()
                .map(|a| a as &dyn std::fmt::Display)
                .collect::<Vec<_>>(),
        ),
        None => tr_os(s),
    }
}

/// The template among `MESSAGES` that `s` fills in, with the values it puts in its %s / %d slots.
fn message(s: &str) -> Option<(&'static str, Vec<String>)> {
    MESSAGES.iter().find_map(|t| {
        let parts: Vec<&str> = t.split("%s").flat_map(|p| p.split("%d")).collect();
        let mut rest = s.strip_prefix(parts[0])?;
        let mut args = Vec::new();
        for (k, lit) in parts.iter().enumerate().skip(1) {
            // the last literal ends the text; the others are the next place they occur
            let at = if k == parts.len() - 1 {
                rest.strip_suffix(lit).map(str::len)?
            } else {
                rest.find(lit).filter(|_| !lit.is_empty())?
            };
            args.push(rest[..at].to_string());
            rest = &rest[at + lit.len()..];
        }
        (rest.is_empty() && args.iter().all(|a| !a.is_empty())).then_some((*t, args))
    })
}

/// egui lays text out left to right without shaping, so right-to-left text is handed over in display order:
/// letters joined into their contextual forms (ar-reshaper), then each line reordered by the Unicode bidi
/// algorithm. In a right-to-left language the line's base direction is right to left; in the others it is
/// left to right, as Windows lays out an Arabic or Hebrew file name in an English window ("فيلم.mp4" reads
/// its Arabic right to left, then ".mp4"). Text with no right-to-left letters stays as it is.
fn visual(t: &str) -> String {
    if !unshaped(t) {
        return t.to_string();
    }
    if rtl_stem(&current()) {
        rtl(t)
    } else {
        reorder(t, unicode_bidi::Level::ltr())
    }
}

/// A file name, folder, address or description as it is shown (the download list, File Info, Properties):
/// written as it was, whatever the language, with its Arabic or Hebrew in reading order (see `visual`).
pub fn shown(t: &str) -> String {
    visual(t)
}

/// Shape and reorder right-to-left text for egui (see `visual`). Brackets in right-to-left runs are mirrored
/// (bidi rule L4, which unicode-bidi leaves to the renderer) so "(" still opens toward its text.
fn rtl(t: &str) -> String {
    reorder(t, unicode_bidi::Level::rtl())
}

/// `rtl` with the line's base direction `base`.
fn reorder(t: &str, base: unicode_bidi::Level) -> String {
    t.replace('\r', "")
        .split('\n')
        .map(|line| {
            let shaped = ar_reshaper::reshape_line(line);
            let info = unicode_bidi::BidiInfo::new(&shaped, Some(base));
            let mut out = String::with_capacity(shaped.len());
            for p in &info.paragraphs {
                let (levels, runs) = info.visual_runs(p, p.range.clone());
                for run in runs {
                    match levels[run.start].is_rtl() {
                        true => out.extend(shaped[run].chars().rev().map(mirror)),
                        false => out.push_str(&shaped[run]),
                    }
                }
            }
            // the isolates and marks have done their work; egui would draw them as boxes
            out.retain(|c| {
                !matches!(
                    c,
                    '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{61c}'
                )
            });
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A character's mirror image in right-to-left text (Unicode's Bidi_Mirroring_Glyph for the ones UI text uses).
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '\u{ab}' => '\u{bb}',
        '\u{bb}' => '\u{ab}',
        '\u{2039}' => '\u{203a}',
        '\u{203a}' => '\u{2039}',
        '\u{2264}' => '\u{2265}',
        '\u{2265}' => '\u{2264}',
        // not mirrored by Unicode, but "from -> to" reads the other way too
        '\u{2190}' => '\u{2192}',
        '\u{2192}' => '\u{2190}',
        c => c,
    }
}

/// Tip of the Day texts, in the loaded language, in reading order (the tip window wraps them).
pub fn tips() -> Vec<String> {
    vec![
        tr_os("NDM takes over downloads from your browser once its extension is installed. Hold Del while clicking a link to let the browser download it instead, or Ins to make NDM take it."),
        tr_os("Right-click a page in your browser and choose \"Download all links with New Download Manager\" to pick files from every link on the page."),
        tr_os("Downloads are split into parts that arrive at the same time. A broken download resumes where it stopped."),
        tr_os("Put files in a queue with \"Download Later\" and let the Scheduler start the queue at night."),
        tr_os("The Site Grabber downloads all pictures, documents or a whole site from a start page."),
        tr_os("Copy a download address and NDM offers to download it (Options -> General)."),
        tr_os("Drag a link onto the drop target (Tasks -> Show drop target) to download it."),
        tr_os("The Speed Limiter keeps some bandwidth free for browsing while NDM downloads."),
        tr_os("Video panels over web players list the streams the page plays; NDM joins HLS and DASH pieces into one file."),
        tr_os("Choose Downloads -> Set yt-dlp to download videos and audio from supported websites. Paste the video's page address or right-click it in your browser and choose Download this video with New Download Manager. NDM fetches direct streams in segments and yt-dlp fetches HLS/DASH fragments in parallel."),
        tr_os("Closing the main window keeps NDM running in the notification area. Use Tasks -> Exit to quit."),
    ]
}

/// `tr` for printf-style texts ("Queue # %ld", "Default download directory for \"%s\" category"): translate
/// the template, then fill its %s / %d / %ld / %I64d slots in order.
pub fn trf(en: &str, args: &[&dyn std::fmt::Display]) -> String {
    // fill the slots in reading order, then lay the whole line out (right-to-left languages)
    let Some(t) = lookup(en).map(|t| punct(en, &t)) else {
        return fill(en, args);
    };
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
    let s = s
        .split('\t')
        .next()
        .unwrap_or("")
        .replace('&', "")
        .replace("New Download Manager", "NDM");
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', ':', ' ', '!'])
        .to_lowercase()
}

/// A key with the product name left out ("customize ndm menu items" = "customize menu items").
fn unbranded(k: &str) -> String {
    k.replace("with ndm", "")
        .split_whitespace()
        .filter(|w| !matches!(*w, "ndm" | "ndm's"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A language file: ("English" -> text, the `lang=` header). UTF-8, with or without a BOM.
fn read(path: &Path) -> Option<(HashMap<String, String>, String)> {
    let b = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&b)).into_owned();
    let mut lines = text.lines();
    let header = lines.next()?.trim().strip_prefix("lang=")?.to_string();
    let map = lines
        .filter(|l| !l.starts_with("//"))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim_end_matches('\r').to_string()))
        .collect();
    Some((map, header))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        let (m, _) = own(HashMap::from([
            ("&Options...".to_string(), "Optionen".to_string()),
            ("Line one\\nline two".into(), "A\\nB".into()),
        ]));
        assert_eq!(
            (
                m.get(&key("Options")).map(String::as_str),
                m.get(&key("Line one\nline two")).map(String::as_str)
            ),
            (Some("Optionen"), Some("A\nB"))
        );
        assert!(rtl_stem("ndm_ar") && !rtl_stem("ndm_en") && !rtl_stem(""));
        // display order of "a (b) 7 -> 8": "8 <- 7 (b) a", the arrow and the brackets facing the right way
        let v: Vec<char> = rtl("\u{627} (\u{628}) 7 -> 8").chars().collect();
        assert_eq!((v[0], v[2], v[3], v[5]), ('8', '<', '-', '7'), "{v:?}");
        assert_eq!((v[7], v[9], v[10]), ('(', ')', ' '), "{v:?}");
        // a path stays in one piece, without the isolate characters around it
        let p: String = rtl("\u{627}: \u{2066}/a/b\u{2069}");
        assert!(p.starts_with("/a/b") && !p.contains('\u{2066}'), "{p:?}");
        assert!(
            unshaped("\u{627}\u{628}") && !unshaped(&rtl("\u{627}\u{628}")) && !unshaped("abc")
        );
        load("ndm_en");
        assert_eq!(
            (current(), tr("Options")),
            ("ndm_en".to_string(), "Options".to_string()),
            "Languages\\ndm_en.lng loads"
        );
        assert_eq!(tr("no such string"), "no such string");
        assert_eq!(tips().len(), 11);
        load("");
        assert_eq!(tr("Options"), "Options");
        assert_eq!(key("&Options...\tCtrl+O"), "options");
        assert_eq!(
            key("Download with New Download Manager"),
            key("Download with NDM")
        );
        assert_eq!(trf("Queue # %ld", &[&3]), "Queue # 3");
        assert_eq!(
            message("Done. 4 web pages processed, 17 files found"),
            Some((
                "Done. %d web pages processed, %d files found",
                vec!["4".to_string(), "17".into()]
            ))
        );
        assert_eq!(
            (
                message("12 links").map(|m| m.1),
                message("Exploring"),
                tr_msg_os("Exploring http://a/")
            ),
            (
                Some(vec!["12".to_string()]),
                None,
                "Exploring http://a/".to_string()
            )
        );
        assert_eq!(
            (
                punct("Port:", "Port :"),
                punct("Result: ", "Résultat :"),
                punct("Options", "Opciones..."),
                punct("Save To...", "Guardar en")
            ),
            (
                "Port :".to_string(),
                "Résultat : ".into(),
                "Opciones".into(),
                "Guardar en...".into()
            )
        );
        // right-to-left lines wrap from the start of the text: the first words on the first line
        let w = wrap_rtl("\u{627}\u{628} \u{62a}\u{62b} \u{62c}\u{62d}", 5.0, |l| {
            l.chars().count() as f32
        });
        assert_eq!(
            w.lines().map(|l| l.chars().count()).collect::<Vec<_>>(),
            [5, 2],
            "{w}"
        );
        assert!(
            w.lines().next().unwrap().ends_with(&rtl("\u{627}\u{628}")),
            "{w}"
        );
        assert_eq!(
            trf("Downloaded %s (%I64d Bytes) 100%%", &[&"1 MB", &1048576]),
            "Downloaded 1 MB (1048576 Bytes) 100%"
        );
    }

    /// A downloaded file's Arabic or Hebrew name is shown as it is written, in an English window too:
    /// its letters joined and its words in reading order, the Latin parts left to right around them.
    #[test]
    fn names_read_as_written_in_a_left_to_right_window() {
        let ltr = |t: &str| reorder(t, unicode_bidi::Level::ltr());
        let back = |t: &str| {
            ar_reshaper::reshape_line(t)
                .chars()
                .rev()
                .collect::<String>()
        };
        assert_eq!(
            shown("report 2024.pdf"),
            "report 2024.pdf",
            "Latin names stay as they are"
        );
        assert_eq!(ltr("فيلم.mp4"), format!("{}.mp4", back("فيلم")));
        assert_eq!(
            ltr("Lecture 2 - مقدمة.pdf"),
            format!("Lecture 2 - {}.pdf", back("مقدمة"))
        );
        // two Arabic words: the first one on the right
        assert_eq!(
            ltr("أغنية جميلة.mp3"),
            format!("{}.mp3", back("أغنية جميلة"))
        );
        assert_eq!(ltr("שלום.txt"), "םולש.txt");
        // the letters come out joined (presentation forms), not the separate letters
        assert!(!unshaped(&ltr("فيلم.mp4")), "joined forms");
        assert!(unshaped("فيلم.mp4"));
    }
}
