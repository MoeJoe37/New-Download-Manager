use crate::stream::{Get, HashMap, MAX_PARTS, PROTECTED, Part, Track, engine, join};

/// `A=1,B="x,y"` attribute lists.
pub(super) fn attrs(s: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut rest = s;
    while let Some((k, v)) = rest.split_once('=') {
        let (val, tail) = match v.strip_prefix('"') {
            Some(q) => {
                let (a, b) = q.split_once('"').unwrap_or((q, ""));
                (a, b.trim_start_matches(','))
            }
            None => v.split_once(',').unwrap_or((v, "")),
        };
        m.insert(k.trim().to_ascii_uppercase(), val.to_string());
        rest = tail;
    }
    m
}

pub(super) fn hls(get: Get, base: &str, text: &str, variant: Option<usize>) -> Result<Vec<Track>, String> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("not an HLS playlist".into());
    }
    if !text.contains("#EXT-X-STREAM-INF") {
        if variant.is_some() { return Err("The selected HLS quality is no longer available".into()); }
        return Ok(vec![hls_media(base, text)?]);
    }
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut variants = Vec::new(); // bandwidth, URI, audio group
    let mut audio = Vec::new(); // group, default, URI
    for (n, l) in lines.iter().enumerate() {
        if let Some(a) = l.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(a);
            let bw = a.get("BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0);
            let uri = lines[n + 1..].iter().find(|x| !x.is_empty() && !x.starts_with('#'));
            if let Some(uri) = uri {
                variants.push((bw, *uri, a.get("AUDIO").cloned().unwrap_or_default()));
            }
        } else if let Some(a) = l.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(a);
            if let (Some("AUDIO"), Some(uri)) = (a.get("TYPE").map(String::as_str), a.get("URI")) {
                audio.push((a.get("GROUP-ID").cloned().unwrap_or_default(), a.get("DEFAULT").is_some_and(|d| d == "YES"), uri.clone()));
            }
        }
    }
    let (_, uri, group) = match variant {
        Some(index) => variants.get(index).ok_or("The selected HLS quality is no longer available")?,
        None => variants.iter().max_by_key(|v| v.0).ok_or("the playlist has no streams")?,
    };
    let sub = |u: &str| -> Result<Track, String> {
        let (b, t) = get(&join(base, u)?)?;
        hls_media(&b, &t)
    };
    let mut tracks = vec![sub(uri)?];
    let mut a: Vec<_> = audio.iter().filter(|x| !group.is_empty() && x.0 == *group).collect();
    a.sort_by_key(|x| !x.1);
    if let Some(x) = a.first() {
        tracks.push(sub(&x.2)?);
    }
    Ok(tracks)
}

/// `len[@offset]`; without an offset the range follows the previous one.
fn byterange(v: &str, prev_end: u64) -> (u64, u64) {
    let (len, off) = v.split_once('@').map_or((v, None), |(l, o)| (l, o.trim().parse().ok()));
    let off = off.unwrap_or(prev_end);
    (off, (off + len.trim().parse::<u64>().unwrap_or(0)).saturating_sub(1))
}

pub(super) fn hls_media(base: &str, text: &str) -> Result<Track, String> {
    let mut t = Track { src: base.into(), ..Default::default() };
    let (mut seq, mut key, mut range, mut end, mut vod) = (0u64, None::<(String, Option<[u8; 16]>)>, None, 0, false);
    for l in text.lines().map(str::trim) {
        if let Some(v) = l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("#EXT-X-KEY:") {
            let a = attrs(v);
            key = match a.get("METHOD").map(String::as_str) {
                Some("AES-128") => {
                    let iv = a.get("IV").and_then(|iv| u128::from_str_radix(iv.trim_start_matches("0x").trim_start_matches("0X"), 16).ok());
                    Some((join(base, a.get("URI").ok_or("AES-128 key without a URI")?)?, iv.map(u128::to_be_bytes)))
                }
                Some("NONE") | None => None,
                _ => return Err(PROTECTED.into()),
            };
        } else if let Some(v) = l.strip_prefix("#EXT-X-MAP:") {
            let a = attrs(v);
            let url = join(base, a.get("URI").ok_or("EXT-X-MAP without a URI")?)?;
            t.init = Some(Part { url, range: a.get("BYTERANGE").map(|r| byterange(r, 0)), key: None, seq: 0, shift: 0.0 });
        } else if let Some(v) = l.strip_prefix("#EXT-X-TARGETDURATION:") {
            t.target = v.trim().parse().unwrap_or(0.0);
        } else if l == "#EXT-X-ENDLIST" || l == "#EXT-X-PLAYLIST-TYPE:VOD" {
            vod = true;
        } else if let Some(v) = l.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(byterange(v, end));
        } else if !l.is_empty() && !l.starts_with('#') {
            if let Some((_, b)) = range {
                end = b + 1;
            }
            let key = key.as_ref().map(|(u, iv)| (u.clone(), iv.unwrap_or((seq as u128).to_be_bytes())));
            if t.parts.len() >= MAX_PARTS { return Err("Media manifest has too many segments".into()); }
            t.parts.push(Part { url: join(base, l)?, range: range.take(), key, seq, shift: 0.0 });
            seq += 1;
        }
    }
    t.live = !vod;
    let first = t.parts.first().ok_or("the playlist has no segments")?;
    let ext = engine::names::url_name(&first.url).rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    t.ext = if t.init.is_some() { "mp4".into() } else if ["aac", "mp3", "ac3"].contains(&ext.as_str()) { ext } else { "ts".into() };
    Ok(t)
}
