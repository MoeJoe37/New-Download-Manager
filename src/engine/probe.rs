use crate::engine::{Agent, Response, call, content_range, disposition_name, ftp_open, is_ftp, media_name, mime_ext, request, sanitize, url_name, validate_http_response};
pub struct Probe {
    pub name: String,
    pub size: Option<u64>,
    pub ranges: bool,
    pub modified: String,
    pub etag: String,
    pub stream: bool,
    /// The address is a web page (text/html): perhaps one yt-dlp knows a video on.
    pub page: bool,
    /// yt-dlp found a video at the address: it downloads it (`Info::ytdlp`).
    pub ytdlp: bool,
}

pub fn probe(agent: &Agent, url: &str, referrer: &str, cookies: &str) -> Result<Probe, String> {
    if crate::stream::is_manifest(url) {
        return crate::stream::probe(agent, url, referrer, cookies);
    }
    if crate::ytdlp::is_video_page(url) {
        return crate::ytdlp::probe(agent, url, referrer, cookies);
    }
    if crate::stream::is_media_url(url) {
        // a live mms / rtmp / rtsp stream: no size until it is recorded
        return Ok(Probe { name: sanitize(&url_name(url)), size: None, ranges: false, modified: String::new(), etag: String::new(), stream: true, page: false, ytdlp: false });
    }
    if is_ftp(url) {
        let (size, ranges, _) = ftp_open(url, 0)?;
        return Ok(Probe { name: sanitize(&url_name(url)), size, ranges, modified: String::new(), etag: String::new(), stream: false, page: false, ytdlp: false });
    }
    let r = call(request(agent, url, referrer, cookies).set("Range", "bytes=0-0"))?;
    if r.header("Content-Type").is_some_and(crate::stream::is_manifest_type) {
        return crate::stream::probe(agent, url, referrer, cookies);
    }
    validate_http_response(&r, Some((0, Some(0))), None)?;
    Ok(probe_of(&r))
}

/// Does NDM get the file the browser is downloading? A download the browser starts comes to NDM as an address,
/// and some addresses give NDM something else: a file a site's service worker makes in the page (Telegram
/// Web), an expired or one-time link, a login page. The browser's `mime` and `size` (0: unknown) and file name
/// are compared with what the server sends NDM; Err says why the browser should keep the download.
pub fn same_file(agent: &Agent, url: &str, referrer: &str, cookies: &str, mime: &str, size: u64, filename: &str) -> Result<(), String> {
    if crate::stream::is_manifest(url) || crate::stream::is_media_url(url) || crate::ytdlp::is_video_page(url) || is_ftp(url) {
        return Ok(());
    }
    let r = call(request(agent, url, referrer, cookies).set("Range", "bytes=0-0"))?;
    let got = r.header("Content-Type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    validate_http_response(&r, Some((0, Some(0))), None)?;
    let p = probe_of(&r);
    drop(r); // not read: a server that ignores the range would send the whole file
    let page = |m: &str| m == "text/html" || m == "application/xhtml+xml";
    let want = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let named_page = matches!(filename.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(), Some("htm" | "html" | "xhtml" | "mht" | "mhtml"));
    if page(&got) && !page(&want) && !(want.is_empty() && named_page) {
        return Err(format!("the server sends a web page ({got}), not the file the browser gets ({})", if want.is_empty() { "?" } else { &want }));
    }
    if let (Some(want_ext), Some(got_ext)) = (mime_ext(&want), mime_ext(&got)) {
        if want_ext != got_ext {
            return Err(format!("The server sends {got_ext}, but the browser received {want_ext}"));
        }
    }
    if let (true, Some(s)) = (size > 0, p.size) {
        if s != size {
            return Err(format!("the server sends {s} bytes, the browser gets {size}"));
        }
    }
    Ok(())
}

/// A Content-Type that says web page.
pub(super) fn is_page(content_type: Option<&str>) -> bool {
    let m = content_type.unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    m == "text/html" || m == "application/xhtml+xml"
}

pub(super) fn probe_of(r: &Response) -> Probe {
    let total = r.header("Content-Range").and_then(content_range).map(|(_, _, total)| total);
    let ranges = r.status() == 206 && total.is_some();
    let size = if ranges { total } else { r.header("Content-Length").and_then(|l| l.trim().parse().ok()) };
    let name = r.header("Content-Disposition").and_then(disposition_name).unwrap_or_else(|| url_name(r.get_url()));
    let name = sanitize(&name);
    let name = r.header("Content-Type").and_then(mime_ext).map_or_else(|| name.clone(), |ext| media_name(&name, ext));
    Probe { name, size, ranges, modified: r.header("Last-Modified").unwrap_or_default().to_string(), etag: r.header("ETag").unwrap_or_default().to_string(), stream: false, page: is_page(r.header("Content-Type")), ytdlp: false }
}
