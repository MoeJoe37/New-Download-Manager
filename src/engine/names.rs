use super::*;

/// The file type a Content-Type names, as an extension (None: not a type NDM files by).
pub fn mime_ext(ct: &str) -> Option<&'static str> {
    let m = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match m.as_str() {
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        "video/x-flv" => "flv",
        "video/mp2t" => "ts",
        "video/3gpp" => "3gp",
        "video/x-ms-wmv" => "wmv",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" | "audio/vnd.wave" => "wav",
        "application/pdf" => "pdf",
        "application/zip" | "application/x-zip-compressed" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-iso9660-image" => "iso",
        "application/x-msdownload" | "application/x-msdos-program" | "application/vnd.microsoft.portable-executable" => "exe",
        "application/x-msi" | "application/x-ms-installer" => "msi",
        "application/vnd.android.package-archive" => "apk",
        "application/x-apple-diskimage" => "dmg",
        "application/vnd.debian.binary-package" => "deb",
        "application/x-rpm" => "rpm",
        "application/epub+zip" => "epub",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    })
}

/// A name that says what the file is: one with no extension, or a script's (download.php, get.aspx...),
/// takes the type's extension.
pub fn typed_name(name: &str, ext: Option<&str>) -> String {
    let Some(ext) = ext else { return name.to_string() };
    let (stem, cur) = name.rsplit_once('.').filter(|(s, e)| !s.is_empty() && e.len() <= 5 && !e.contains(' ')).unwrap_or((name, ""));
    let script = ["", "php", "asp", "aspx", "jsp", "cgi", "do", "ashx", "pl", "py", "action", "bin", "dat", "tmp"].contains(&cur.to_ascii_lowercase().as_str());
    if !script || cur.eq_ignore_ascii_case(ext) {
        return name.to_string();
    }
    format!("{stem}.{ext}")
}

pub(crate) fn media_name(name: &str, actual: &str) -> String {
    const CONTAINERS: &[&str] = &["ts", "mp4", "m4a", "mkv", "webm", "mov", "avi", "flv", "3gp"];
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    if CONTAINERS.contains(&actual) && CONTAINERS.contains(&ext.to_ascii_lowercase().as_str())
        && !ext.eq_ignore_ascii_case(actual) && !(actual == "mp4" && ext.eq_ignore_ascii_case("m4a")) {
        format!("{stem}.{actual}")
    } else {
        typed_name(name, Some(actual))
    }
}

/// What a file is from its first bytes, as an extension (None: can't tell).
pub fn sniff(path: &Path) -> Option<&'static str> {
    let mut head = vec![0u8; 512];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).ok()?;
    let d = &head[..n];
    let at = |i: usize, sig: &[u8]| d.get(i..i + sig.len()) == Some(sig);
    Some(if at(4, b"ftyp") {
        match d.get(8..12) {
            Some(b"M4A ") | Some(b"M4B ") => "m4a",
            Some(b"qt  ") => "mov",
            Some(b) if b.starts_with(b"3g") => "3gp",
            _ => "mp4",
        }
    } else if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        if d.windows(4).any(|w| w == b"webm") { "webm" } else { "mkv" }
    } else if at(0, b"ID3") || (d.len() > 1 && d[0] == 0xFF && matches!(d[1], 0xFB | 0xF3 | 0xF2)) {
        "mp3"
    } else if at(0, b"OggS") {
        "ogg"
    } else if at(0, b"fLaC") {
        "flac"
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        "wav"
    } else if at(0, b"RIFF") && at(8, b"AVI ") {
        "avi"
    } else if at(0, b"FLV") {
        "flv"
    } else if at(0, b"%PDF") {
        "pdf"
    } else if at(0, b"PK\x03\x04") {
        "zip"
    } else if at(0, b"Rar!") {
        "rar"
    } else if at(0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        "7z"
    } else if at(0, &[0x1F, 0x8B]) {
        "gz"
    } else if at(0, b"MZ") {
        "exe"
    } else if at(0, b"\x89PNG") {
        "png"
    } else if at(0, &[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if at(0, b"GIF8") {
        "gif"
    } else if [188, 192, 204].into_iter().any(|stride| {
        let offset = if stride == 192 { 4 } else { 0 };
        d.get(offset) == Some(&0x47) && d.get(offset + stride) == Some(&0x47)
    }) {
        "ts"
    } else {
        return None;
    })
}

pub fn disposition_name(h: &str) -> Option<String> {
    let mut plain = None;
    for p in h.split(';').map(str::trim) {
        if let Some(v) = strip_ci(p, "filename*=") {
            let v = v.trim_matches('"');
            return Some(pct_decode(v.splitn(3, '\'').nth(2).unwrap_or(v)));
        }
        if let Some(v) = strip_ci(p, "filename=") {
            plain = Some(v.trim_matches('"').to_string());
        }
    }
    plain
}

pub fn url_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let path = path.split_once("://").map_or(path, |(_, rest)| rest);
    path.split_once('/').map_or("", |(_, p)| p).rsplit('/').next().map(pct_decode).unwrap_or_default()
}

pub fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim_matches([' ', '.']);
    if s.is_empty() { "download".into() } else { s.into() }
}

/// `name`, or `name (1).ext`, `name (2).ext`... whichever is free in `folder`.
pub fn unique_name(folder: &str, name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let free = |n: &str| {
        let p = Path::new(folder).join(n);
        !p.exists() && !Path::new(&format!("{}.part", p.display())).exists() && !taken(n)
    };
    if free(name) {
        return name.into();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|i| format!("{stem} ({i}){ext}")).find(|n| free(n)).unwrap()
}
