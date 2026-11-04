use super::*;

pub(in crate::app) fn human(n: u64) -> String {
    i18n::line(&human_os(n))
}

/// `human` in reading order, for a line it is part of.
pub(in crate::app) fn human_os(n: u64) -> String {
    let n = n as f64;
    match n {
        _ if n < 1024.0 => format!("{n} {}", tr_os("Bytes")),
        _ if n < 1024.0 * 1024.0 => format!("{:.2} {}", n / 1024.0, tr_os("KB")),
        _ if n < 1024.0 * 1024.0 * 1024.0 => format!("{:.2} {}", n / 1048576.0, tr_os("MB")),
        _ => format!("{:.2} {}", n / 1073741824.0, tr_os("GB")),
    }
}

pub(in crate::app) fn rate(bps: f64) -> String {
    if bps <= 0.0 { String::new() } else { i18n::line(&format!("{}/{}", human_os(bps as u64), tr_os("sec"))) }
}

/// NDM's time left (0x43f730): seconds, minutes and seconds, or hours and minutes ("%ld hour(s) %d min" in
/// its built-in English; the language files say "%d hours %d min").
pub(in crate::app) fn time_left(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, m) {
        _ if h > 10000 => String::new(),
        (0, 0) => i18n::trf("%d sec", &[&s]),
        (0, _) => i18n::trf("%d min %d sec", &[&m, &s]),
        _ if i18n::current().is_empty() => format!("{h} hour(s) {m} min"),
        _ => i18n::trf("%d hours %d min", &[&h, &m]),
    }
}

/// NDM's download-progress sizes (0x4a34c0): three truncated decimals with the locale's separator and two
/// spaces before the unit ("796.000  KB"); below 1 KB "N  Bytes"; 1000-1023 of a unit go to the next one.
pub(in crate::app) fn size3(n: u64) -> String {
    i18n::line(&size3_os(n))
}

/// `size3` in reading order, for a line it is part of.
pub(in crate::app) fn size3_os(n: u64) -> String {
    let (b, kb, mb, gb) = (n % 1024, n / 1024 % 1024, n / 1_048_576 % 1024, n / 1_073_741_824);
    let (whole, frac, unit) = if gb == 0 && mb < 1000 {
        if mb == 0 && kb < 1000 {
            if kb == 0 {
                return format!("{b}  {}", tr_os("Bytes"));
            }
            (kb, b * 1000 / 1024, "KB")
        } else {
            (mb, kb * 1000 / 1024, "MB")
        }
    } else {
        (gb, mb * 1000 / 1024, "GB")
    };
    format!("{whole}{}{frac:03}  {}", win::decimal_sep(), tr_os(unit))
}

/// A percentage as NDM prints it: whole part, separator, two truncated decimals.
pub(in crate::app) fn pct2(p: f64) -> String {
    let whole = p.trunc();
    format!("{}{}{:02}", whole as u64, win::decimal_sep(), ((p - whole) * 100.0).trunc() as u64)
}

/// A date and time as NDM's English prints it, or in the language file's format (its "%b %d %H:%M:%S %Y"
/// line: the languages shipped with NDM use numbers rather than English month names).
pub(in crate::app) fn date(ts: i64) -> String {
    use chrono::TimeZone;
    const EN: &str = "%b %d %H:%M:%S %Y";
    if ts == 0 {
        return String::new();
    }
    // a format chrono can't read would panic when printed
    let f = Some(tr_os(EN)).filter(|f| !chrono::format::StrftimeItems::new(f).any(|i| matches!(i, chrono::format::Item::Error))).unwrap_or_else(|| EN.into());
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.format(&f).to_string()).unwrap_or_default()
}
