use crate::ytdlp::{host_of, last_error, on_site, sanitize, Path};

pub(super) fn selection_error(err: &str, quality: &str) -> String {
    let message = last_error(err);
    if let Some(height) = quality
        .parse::<u32>()
        .ok()
        .filter(|_| message.contains("Requested format is not available"))
    {
        return crate::i18n::trf_os(
            "NDM could not verify a video format at or below %dp. Choose Best to download without a resolution limit.",
            &[&height],
        );
    }
    message
}

/// With ffmpeg: the best video and audio, merged by yt-dlp into an MP4.
pub(super) const MERGED: &str = "bv*+ba/b/ba/bv";

/// The single file with both, when a site has no separate streams.
const SINGLE: &str = "b";

/// Without ffmpeg: the best video-only stream and the audio NDM can join it with (MP4 and M4A, else WebM
/// and WebM), else the single file.
pub(super) const PAIR: &str = "bv*[acodec=none][ext=mp4]+ba[vcodec=none][ext=m4a]/bv*[acodec=none][ext=webm]+ba[vcodec=none][ext=webm]/b/ba/bv";

/// The formats asked for on `url`'s site, `ff`: with ffmpeg. TikTok and Douyin: the one file that has both
/// (H.264 first, which every player plays), not their video-only streams (some broken) with the separate
/// "music" track, which isn't always the video's own sound. YouTube without ffmpeg: its separate streams,
/// which NDM joins itself. Other sites without ffmpeg: the one file that has both when there is one, since
/// their separate streams may be whole MP4 files that only ffmpeg joins.
pub(super) fn formats(url: &str, ff: bool) -> String {
    let host = host_of(url);
    let both = if ff { MERGED } else { PAIR };
    if ["tiktok.com", "douyin.com"]
        .iter()
        .any(|s| on_site(&host, s))
    {
        format!("b[vcodec^=h264]/b[vcodec^=avc]/b/{both}")
    } else if ff
        || ["youtube.com", "youtu.be"]
            .iter()
            .any(|s| on_site(&host, s))
    {
        both.to_string()
    } else {
        format!("{SINGLE}/{PAIR}")
    }
}

/// The formats for a download's Quality choice (`YtOpts::quality`): only the sound ("m4a" / "mp3": the
/// M4A track when there is one), or the site's formats with the video at most `quality` lines high, with no fallback above it.
pub(super) fn chosen(url: &str, ff: bool, quality: &str) -> String {
    let all = formats(url, ff);
    if quality == "m4a" || quality == "mp3" {
        return "ba[ext=m4a]/ba/b".into();
    }
    let Ok(h) = quality.parse::<u32>() else {
        return all;
    };
    let cap = |alt: &str| {
        alt.split('+')
            .map(|part| {
                let name = part.split('[').next().unwrap_or("");
                if matches!(name, "b" | "bv" | "bv*" | "best" | "bestvideo") {
                    format!("{name}[height<={h}]{}", &part[name.len()..])
                } else {
                    part.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    };
    let capped: Vec<String> = all
        .split('/')
        .filter(|a| a.split('+').any(|p| !p.starts_with("ba")))
        .map(cap)
        .collect();
    capped.join("/")
}

/// The video's file name for a Quality choice: MP3 and M4A keep the title with their own type, a video
/// named for its sound is an MP4 again.
pub fn named(name: &str, quality: &str) -> String {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match quality {
        "mp3" | "m4a" => format!("{stem}.{quality}"),
        _ if ext == "mp3" || ext == "m4a" => format!("{stem}.mp4"),
        _ => name.to_string(),
    }
}

/// One stream yt-dlp's look-up (-J) picked: its format id, address, type and size (0: unknown), the
/// headers it is fetched with, and whether it is a plain file NDM can fetch itself (http / https; DASH and
/// HLS streams stay with yt-dlp).
pub(super) struct Stream {
    pub(super) id: String,
    pub(super) url: String,
    pub(super) ext: String,
    pub(super) size: u64,
    pub(super) headers: Vec<(String, String)>,
    pub(super) direct: bool,
}

impl Stream {
    pub(super) fn of(f: &serde_json::Value) -> Option<Stream> {
        let text = |k: &str| f.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let size = f
            .get("filesize")
            .and_then(|s| s.as_f64())
            .or_else(|| f.get("filesize_approx").and_then(|s| s.as_f64()))
            .unwrap_or(0.0) as u64;
        let headers = f
            .get("http_headers")
            .and_then(|h| h.as_object())
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        // the stream's cookies (yt-dlp keeps them out of its headers: "name=value; Domain=...; Path=/; ...")
        let mut headers: Vec<(String, String)> = headers;
        let cookie = cookie_header(&text("cookies"));
        if !cookie.is_empty() {
            headers.retain(|h: &(String, String)| !h.0.eq_ignore_ascii_case("cookie"));
            headers.push(("Cookie".into(), cookie));
        }
        let url = text("url");
        // Leave fragments, live streams, and requests requiring TLS impersonation to yt-dlp, even
        // when their URL looks like a plain HTTP file.
        let direct = matches!(text("protocol").as_str(), "http" | "https")
            && (url.starts_with("http://") || url.starts_with("https://"))
            && f.get("fragments").is_none_or(|v| v.is_null())
            && !f.get("is_live").and_then(|v| v.as_bool()).unwrap_or(false)
            && f.get("impersonate").is_none_or(|v| v.is_null())
            && f.pointer("/downloader_options/impersonate")
                .is_none_or(|v| v.is_null());
        Some(Stream {
            id: f.get("format_id")?.as_str()?.to_string(),
            url,
            ext: text("ext"),
            size,
            headers,
            direct,
        })
    }
}

/// yt-dlp's cookie list for a stream ("a=1; Domain=.x.com; Path=/; Secure; Expires=...; b=2; ...") as a
/// Cookie header ("a=1; b=2").
pub(super) fn cookie_header(list: &str) -> String {
    const ATTRS: [&str; 7] = [
        "domain", "path", "secure", "expires", "version", "httponly", "max-age",
    ];
    list.split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| {
            !ATTRS.contains(
                &p.split('=')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
            )
        })
        .filter(|p| p.contains('='))
        .collect::<Vec<_>>()
        .join("; ")
}

/// What the look-up picked: the video and audio streams, or the one stream that has both.
pub(super) fn picked(v: &serde_json::Value) -> Option<Vec<Stream>> {
    match v
        .get("requested_formats")
        .and_then(|r| r.as_array())
        .filter(|r| r.len() == 2)
    {
        Some(r) => r.iter().map(Stream::of).collect(),
        None => Some(vec![Stream::of(v)?]),
    }
}

/// Multi-video posts and playlist-only extractors wrap the selected video in `entries`.
pub(super) fn video_info(mut v: serde_json::Value) -> Result<serde_json::Value, String> {
    for _ in 0..16 {
        if let Some(entries) = v.get_mut("entries").and_then(|e| e.as_array_mut()) {
            v = entries
                .iter_mut()
                .find(|e| !e.is_null())
                .map(serde_json::Value::take)
                .ok_or("yt-dlp found no video in this playlist")?;
        } else {
            return Ok(v);
        }
    }
    Err("yt-dlp returned too many nested playlists".into())
}

pub(super) fn video_name(v: &serde_json::Value) -> String {
    let title = v.get("title").and_then(|v| v.as_str()).unwrap_or("video");
    let ext = v
        .get("ext")
        .and_then(|v| v.as_str())
        .filter(|e| !e.is_empty())
        .unwrap_or("mp4");
    sanitize(&format!("{title}.{ext}"))
}
