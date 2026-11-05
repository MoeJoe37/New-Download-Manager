use crate::engine::{CHANGED, Duration, Response, Seg};

/// An inclusive, concrete byte interval. Unsatisfied and unknown-size ranges cannot be split.
pub(crate) fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (span, total) = value.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let (first, last, total) = (first.parse().ok()?, last.parse().ok()?, total.parse().ok()?);
    (first <= last && last < total).then_some((first, last, total))
}

pub(crate) fn validate_http_response(r: &Response, range: Option<(u64, Option<u64>)>, expected: Option<u64>) -> Result<(), String> {
    if r.header("Content-Encoding").is_some_and(|value| !value.eq_ignore_ascii_case("identity")) {
        return Err("Server returned an encoded body for a byte download".into());
    }
    let length = r.header("Content-Length").map(|n| n.trim().parse::<u64>()).transpose().map_err(|_| "Invalid Content-Length")?;
    match (r.status(), range) {
        (206, Some((start, end))) => {
            let (first, last, total) = r.header("Content-Range").and_then(content_range).ok_or("Invalid Content-Range")?;
            if expected.is_some_and(|size| total != size) {
                return Err(CHANGED.into());
            }
            if first != start || last != end.unwrap_or(total - 1).min(total - 1) || length.is_some_and(|n| n != last - first + 1) {
                return Err("Content-Range does not match the requested bytes".into());
            }
            Ok(())
        }
        (200, _) if expected.zip(length).is_some_and(|(old, new)| old != new) => Err(CHANGED.into()),
        (200, _) => Ok(()),
        _ => Err(format!("Unexpected HTTP {} response", r.status())),
    }
}

pub(crate) fn permanent_error(error: &str) -> bool {
    let code = error.strip_prefix("HTTP ").and_then(|text| text.split_whitespace().next()).and_then(|code| code.parse::<u16>().ok());
    code.is_some_and(|code| (400..500).contains(&code) && !matches!(code, 408 | 425 | 429))
        || error == CHANGED
        || error == "server ignored the Range request"
        || error == "Invalid Content-Range"
        || error == "Content-Range does not match the requested bytes"
        || error == "Invalid proxy configuration"
}

pub(crate) fn retry_delay(failures: u32) -> Duration {
    use std::hash::{BuildHasher, RandomState};
    let jitter = RandomState::new().hash_one(failures) % 500;
    Duration::from_millis((1000 * (1u64 << failures.min(5)) + jitter).min(30_000))
}

pub(super) fn valid_layout(segs: &[Seg], size: Option<u64>) -> bool {
    if segs.len() == 1 && segs[0].stop.is_none() {
        return segs[0].start == 0 && size.is_none();
    }
    let mut ordered: Vec<_> = segs.iter().collect();
    ordered.sort_unstable_by_key(|segment| segment.start);
    let mut end = 0;
    for segment in ordered {
        let Some(stop) = segment.stop else { return false };
        if segment.start != end || segment.next < end || segment.next > stop {
            return false;
        }
        end = stop;
    }
    size.is_none_or(|size| size == end)
}
