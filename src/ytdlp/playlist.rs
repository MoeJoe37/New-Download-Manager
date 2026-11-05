use crate::ytdlp::{command, last_error, output_with_timeout, yt_dlp, Job};

pub const PLAYLIST_LIMIT: usize = 200;

pub struct PlaylistEntry {
    pub url: String,
    pub title: String,
}

pub struct Playlist {
    pub entries: Vec<PlaylistEntry>,
    pub limited: bool,
}

pub fn playlist_url(text: &str) -> Option<String> {
    let mut url = url::Url::parse(text.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url.into())
}

pub(super) fn browser_args(args: &mut Vec<String>, browser: &str) -> Result<(), String> {
    let source = crate::browsers::cookie_source(browser).map_err(|error| match error {
        crate::browsers::CookieSourceError::Unsupported => "Unsupported browser cookie source.".to_string(),
        crate::browsers::CookieSourceError::MissingProfile => crate::i18n::tr_os("No profile was found for the selected browser. Open it and sign in to the site, then try again."),
    })?;
    if !source.is_empty() {
        args.extend(["--cookies-from-browser".into(), source]);
    }
    Ok(())
}

pub(super) fn playlist_entries(value: &serde_json::Value) -> Result<Playlist, String> {
    let entries = value
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or("yt-dlp did not find a playlist at this address.")?;
    let mut seen = std::collections::HashSet::new();
    let playable: Vec<_> = entries
        .iter()
        .take(PLAYLIST_LIMIT)
        .filter_map(|entry| {
            if matches!(
                entry.get("_type").and_then(|v| v.as_str()),
                Some("playlist" | "multi_video")
            ) {
                return None;
            }
            let field = |name| entry.get(name).and_then(|v| v.as_str());
            let url = field("webpage_url")
                .and_then(playlist_url)
                .or_else(|| field("url").and_then(playlist_url))
                .or_else(|| {
                    let id = field("id")?;
                    (field("ie_key") == Some("Youtube")
                        && id.len() == 11
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                    .then(|| format!("https://www.youtube.com/watch?v={id}"))
                })?;
            if !seen.insert(url.clone()) {
                return None;
            }
            let title = field("title")
                .filter(|t| !t.is_empty())
                .unwrap_or("Video")
                .chars()
                .take(300)
                .collect();
            Some(PlaylistEntry { url, title })
        })
        .collect();
    if playable.is_empty() {
        return Err(
            "The playlist contains no individual web video links that NDM can queue.".into(),
        );
    }
    Ok(Playlist {
        entries: playable,
        limited: entries.len() > PLAYLIST_LIMIT,
    })
}

/// Flat extraction lists entries without resolving every track or inheriting the browser page's headers.
pub fn list_playlist(
    job: &Job,
    agent: &crate::transport::Agent,
    url: &str,
    browser: &str,
) -> Result<Playlist, String> {
    if agent.invalid_proxy {
        return Err("Invalid proxy configuration".into());
    }
    let url = playlist_url(url)
        .ok_or("Enter an HTTP or HTTPS playlist address without embedded credentials.")?;
    let program = yt_dlp()?;
    let mut args = vec![
        "--ignore-config".into(),
        "--yes-playlist".into(),
        "--flat-playlist".into(),
    ];
    browser_args(&mut args, browser)?;
    let proxy = crate::transport::external_proxy(&agent.proxy);
    let mut cmd = command(&program);
    cmd.args(args).args([
        "--playlist-end",
        &(PLAYLIST_LIMIT + 1).to_string(),
        "--skip-download",
        "--socket-timeout",
        "15",
        "--extractor-retries",
        "1",
        "--proxy",
        &proxy,
        "--dump-single-json",
        "--",
        url.as_str(),
    ]);
    let output = output_with_timeout(job, cmd, Some(std::time::Duration::from_secs(90)))?
        .ok_or("Playlist lookup cancelled.")?;
    if !output.status.success() {
        return Err(last_error(&String::from_utf8_lossy(&output.stderr)));
    }
    let value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "yt-dlp returned an invalid playlist list.")?;
    playlist_entries(&value)
}
