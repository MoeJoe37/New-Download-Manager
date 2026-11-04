use super::*;

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn el<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.tag_name().name() == name)
}

pub(super) fn kids<'a, 'i: 'a>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.children().filter(move |c| c.tag_name().name() == name)
}

pub(super) fn num<T: std::str::FromStr>(n: Option<&str>, or: T) -> T {
    n.and_then(|v| v.trim().parse().ok()).unwrap_or(or)
}

/// ISO 8601 duration ("PT1H2M3.5S") in seconds.
fn iso_dur(s: &str) -> f64 {
    let (mut total, mut n, mut time) = (0.0, String::new(), false);
    for c in s.chars() {
        match c {
            'P' => {}
            'T' => time = true,
            '0'..='9' | '.' => n.push(c),
            _ => {
                let unit = match (c, time) {
                    ('Y', false) => 31_536_000.0,
                    ('M', false) => 2_592_000.0,
                    ('W', _) => 604_800.0,
                    ('D', _) => 86_400.0,
                    ('H', true) => 3600.0,
                    ('M', true) => 60.0,
                    ('S', true) => 1.0,
                    _ => 0.0,
                };
                total += n.parse::<f64>().unwrap_or(0.0) * unit;
                n.clear();
            }
        }
    }
    total
}

/// SegmentTemplate identifiers: `$RepresentationID$`, `$Bandwidth$`, `$Number%05d$`, `$Time$`, `$$`.
fn fill(t: &str, id: &str, bw: &str, number: u64, time: u64) -> String {
    let mut out = String::new();
    for (i, p) in t.split('$').enumerate() {
        if i % 2 == 0 {
            out += p;
            continue;
        }
        let (name, fmt) = p.split_once('%').unwrap_or((p, ""));
        let w: usize = num(Some(fmt.trim_end_matches('d')), 0);
        match name {
            "" => out.push('$'),
            "RepresentationID" => out += id,
            "Bandwidth" => out += bw,
            "Number" => out += &format!("{number:0w$}"),
            "Time" => out += &format!("{time:0w$}"),
            _ => out += &format!("${p}$"),
        }
    }
    out
}

fn base_of(b: &str, n: Node) -> String {
    el(n, "BaseURL").and_then(|x| join(b, x.text().unwrap_or("").trim()).ok()).unwrap_or_else(|| b.to_string())
}

/// Live timing of a dynamic manifest: seconds since the period became available, and how far back pieces stay.
#[derive(Clone, Copy)]
struct LiveAt {
    elapsed: f64,
    window: f64,
}

/// `now` = Unix time in seconds (a dynamic manifest's pieces depend on it). Every Period is joined, video
/// with video and audio with audio: a period whose initialization piece differs starts with it, and each
/// piece's `shift` puts its fragments on one timeline (`retime`).
pub(super) fn dash(base: &str, text: &str, now: f64) -> Result<Vec<Track>, String> {
    dash_variant(base, text, now, None)
}

pub(super) fn dash_variant(base: &str, text: &str, now: f64, variant: Option<usize>) -> Result<Vec<Track>, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let mpd = doc.root_element();
    let periods: Vec<Node> = kids(mpd, "Period").collect();
    if periods.is_empty() {
        return Err("the manifest has no Period".into());
    }
    let total = mpd.attribute("mediaPresentationDuration").map_or(0.0, iso_dur);
    let dynamic = mpd.attribute("type") == Some("dynamic");
    if variant.is_some() && (dynamic || periods.len() != 1) {
        return Err("The selected DASH quality is only available on a single-period recording".into());
    }
    let ast = mpd.attribute("availabilityStartTime").and_then(|t| chrono::DateTime::parse_from_rfc3339(t.trim()).ok()).map_or(now, |t| t.timestamp_millis() as f64 / 1000.0);
    let window = mpd.attribute("timeShiftBufferDepth").map_or(30.0, iso_dur);
    let update = mpd.attribute("minimumUpdatePeriod").map_or(2.0, iso_dur);
    let b_mpd = base_of(base, mpd);
    // a period starts where it says, else where the one before it ends
    let (mut starts, mut at) = (Vec::new(), 0.0);
    for p in &periods {
        let s = p.attribute("start").map_or(at, iso_dur);
        at = s + p.attribute("duration").map_or(0.0, iso_dur);
        starts.push(s);
    }
    let mut tracks: Vec<(&str, Track, Option<String>)> = Vec::new(); // kind, track, its latest init piece
    for (k, period) in periods.iter().enumerate() {
        let start = starts[k];
        let secs = period.attribute("duration").map(iso_dur).or_else(|| starts.get(k + 1).map(|n| n - start)).unwrap_or(total - start);
        let live = dynamic.then_some(LiveAt { elapsed: now - ast - start, window });
        if live.is_some_and(|l| l.elapsed < 0.0) {
            continue; // not on air yet
        }
        let b0 = base_of(&b_mpd, *period);
        for kind in ["video", "audio"] {
            let mut best: Option<(u64, Node, Node)> = None;
            let mut index = 0;
            for set in kids(*period, "AdaptationSet") {
                for rep in kids(set, "Representation") {
                    let mime = rep.attribute("mimeType").or(set.attribute("mimeType")).unwrap_or("");
                    if !mime.starts_with(kind) && set.attribute("contentType") != Some(kind) {
                        continue;
                    }
                    let bw = num(rep.attribute("bandwidth"), 0);
                    if let Some(selected) = variant.filter(|_| kind == "video") {
                        if selected == index { best = Some((bw, set, rep)); }
                        index += 1;
                    } else if best.is_none_or(|b| bw > b.0) {
                        best = Some((bw, set, rep));
                    }
                }
            }
            let Some((_, set, rep)) = best else {
                if kind == "video" && variant.is_some() {
                    return Err("The selected DASH quality is no longer available".into());
                }
                continue;
            };
            if set.descendants().any(|n| n.tag_name().name() == "ContentProtection") {
                return Err(PROTECTED.into());
            }
            let mut t = dash_track(&b0, *period, set, rep, secs, kind, live, start)?;
            let init = t.init.as_ref().map(|p| p.url.clone());
            match tracks.iter_mut().find(|x| x.0 == kind) {
                None => {
                    (t.live, t.dash, t.target, t.src) = (live.is_some(), true, update, base.to_string());
                    tracks.push((kind, t, init));
                }
                Some((_, first, last_init)) => {
                    if init.is_some() && init != *last_init {
                        first.parts.extend(t.init.take());
                        *last_init = init;
                    }
                    // a period rounded up to whole pieces can list the next one's first piece too
                    let had: HashSet<(String, Option<(u64, u64)>)> = first.parts.iter().map(|p| (p.url.clone(), p.range)).collect();
                    first.parts.extend(t.parts.into_iter().filter(|p| !had.contains(&(p.url.clone(), p.range))));
                }
            }
        }
    }
    if tracks.is_empty() {
        return Err("the manifest has no video or audio".into());
    }
    Ok(tracks.into_iter().map(|x| x.1).collect())
}

#[allow(clippy::too_many_arguments)]
fn dash_track(b0: &str, period: Node, set: Node, rep: Node, secs: f64, kind: &str, live: Option<LiveAt>, start: f64) -> Result<Track, String> {
    let base = base_of(&base_of(b0, set), rep);
    let (id, bw) = (rep.attribute("id").unwrap_or(""), rep.attribute("bandwidth").unwrap_or("0"));
    let mime = rep.attribute("mimeType").or(set.attribute("mimeType")).unwrap_or("");
    let ext = if mime.contains("webm") { "webm" } else if kind == "audio" { "m4a" } else { "mp4" };
    let mut t = Track { ext: ext.into(), ..Default::default() };
    // seq: the piece's number (or $Time$ on a timeline), which tells a live manifest's new pieces apart
    let part = |u: &str, range: Option<&str>, seq: u64| -> Result<Part, String> {
        let range = range.and_then(|r| r.split_once('-')).and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
        Ok(Part { url: join(&base, u)?, range, key: None, seq, shift: 0.0 })
    };
    let tmpl: Vec<Node> = [rep, set, period].into_iter().filter_map(|n| el(n, "SegmentTemplate")).collect();
    let ta = |k: &str| tmpl.iter().find_map(|t| t.attribute(k));
    if let Some(media) = ta("media") {
        let ts: u64 = num(ta("timescale"), 1u64).max(1);
        let start: u64 = num(ta("startNumber"), 1);
        if let Some(i) = ta("initialization") {
            t.init = Some(part(&fill(i, id, bw, 0, 0), None, 0)?);
        }
        let pto: f64 = num(ta("presentationTimeOffset"), 0.0);
        if let Some(tl) = tmpl.iter().find_map(|x| el(*x, "SegmentTimeline")) {
            let (mut time, mut n) = (0u64, start);
            // a live timeline lists the pieces on offer; r = -1 repeats up to the live edge or the period's end
            let end = pto + live.map_or(secs, |l| l.elapsed) * ts as f64;
            for s in kids(tl, "S") {
                let d: u64 = num(s.attribute("d"), 0);
                if d == 0 {
                    continue;
                }
                time = num(s.attribute("t"), time);
                let r: i64 = num(s.attribute("r"), 0);
                let left = (end - time as f64) / d as f64;
                let count = if r >= 0 { r as u64 + 1 } else if live.is_some() { left.floor().max(0.0) as u64 } else { left.ceil().max(1.0) as u64 };
                if count > (MAX_PARTS - t.parts.len()) as u64 { return Err("Media manifest has too many segments".into()); }
                for _ in 0..count {
                    t.parts.push(part(&fill(media, id, bw, n, time), None, time)?);
                    (time, n) = (time.checked_add(d).ok_or("Invalid DASH timeline")?, n.checked_add(1).ok_or("Invalid DASH sequence number")?);
                }
            }
        } else {
            let d: f64 = num(ta("duration"), 0.0);
            let ks = match live {
                // the pieces finished by now, as far back as the time-shift window reaches
                Some(l) if d > 0.0 => {
                    let last = (l.elapsed * ts as f64 / d).floor() as i64 - 1;
                    let first = (last + 1 - (l.window * ts as f64 / d).ceil() as i64).max(0);
                    first.max(0) as u64..last.saturating_add(1).max(0) as u64
                }
                _ if d <= 0.0 || secs <= 0.0 => return Err("the manifest doesn't say how long the stream is".into()),
                _ => 0..(secs * ts as f64 / d).ceil() as u64,
            };
            if ks.end.saturating_sub(ks.start) > MAX_PARTS as u64 { return Err("Media manifest has too many segments".into()); }
            for k in ks {
                t.parts.push(part(&fill(media, id, bw, start.saturating_add(k), (k as f64 * d) as u64), None, start.saturating_add(k))?);
            }
        }
    } else if let Some(sl) = [rep, set].into_iter().find_map(|n| el(n, "SegmentList")) {
        if let Some(i) = el(sl, "Initialization") {
            t.init = Some(part(i.attribute("sourceURL").unwrap_or(""), i.attribute("range"), 0)?);
        }
        let start: u64 = num(sl.attribute("startNumber"), 1);
        for (k, s) in kids(sl, "SegmentURL").enumerate() {
            if k >= MAX_PARTS { return Err("Media manifest has too many segments".into()); }
            t.parts.push(part(s.attribute("media").unwrap_or(""), s.attribute("mediaRange"), start.saturating_add(k as u64))?);
        }
    } else {
        t.parts.push(part("", None, 0)?); // the whole file at BaseURL (SegmentBase)
        t.whole = true;
    }
    // where this period's fragments go on the joined timeline: its start, less the media time it begins at
    let attr = |k: &str| ["SegmentTemplate", "SegmentList", "SegmentBase"].iter().flat_map(|n| [rep, set, period].into_iter().filter_map(move |x| el(x, n))).find_map(|e| e.attribute(k));
    let shift = start - num(attr("presentationTimeOffset"), 0.0) / num(attr("timescale"), 1.0f64).max(1.0);
    for p in t.parts.iter_mut().chain(t.init.iter_mut()) {
        p.shift = shift;
    }
    Ok(t)
}
