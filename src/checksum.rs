//! File Properties -> Checksum: a downloaded file's SHA-256, SHA-1 and MD5, worked out on a thread, and the
//! comparison with a checksum the site publishes.
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

pub struct Sums {
    pub sha256: String,
    pub sha1: String,
    pub md5: String,
}

/// A checksum being worked out: bytes read so far of `total`, then the result.
pub struct Run {
    pub done: AtomicU64,
    pub total: u64,
    pub result: Mutex<Option<Result<Sums, String>>>,
}

/// Starts reading `path` on its own thread (all three sums in one pass).
pub fn start(path: PathBuf) -> Arc<Run> {
    let total = std::fs::metadata(&path).map_or(0, |m| m.len());
    let run = Arc::new(Run { done: AtomicU64::new(0), total, result: Mutex::new(None) });
    let r = run.clone();
    std::thread::spawn(move || {
        let sums = (|| -> std::io::Result<Sums> {
            let mut f = std::fs::File::open(&path)?;
            let (mut a, mut b, mut c) = (Sha256::new(), Sha1::new(), Md5::new());
            let mut buf = vec![0; 1 << 20];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                a.update(&buf[..n]);
                b.update(&buf[..n]);
                c.update(&buf[..n]);
                r.done.fetch_add(n as u64, Relaxed);
            }
            let hex = |d: &[u8]| d.iter().map(|x| format!("{x:02x}")).collect::<String>();
            Ok(Sums { sha256: hex(&a.finalize()), sha1: hex(&b.finalize()), md5: hex(&c.finalize()) })
        })();
        *r.result.lock().unwrap() = Some(sums.map_err(|e| e.to_string()));
    });
    run
}

/// The checksum in `expected` (as sites publish it: the hex alone, "SHA256: hex", or `sha256sum`'s
/// "hex  file") compared with the file's: which kind it is and whether it matches. None: not a checksum.
pub fn compare(sums: &Sums, expected: &str) -> Option<(&'static str, bool)> {
    let hex = expected.split(|c: char| !c.is_ascii_hexdigit()).find(|t| matches!(t.len(), 32 | 40 | 64))?.to_ascii_lowercase();
    Some(match hex.len() {
        64 => ("SHA-256", hex == sums.sha256),
        40 => ("SHA-1", hex == sums.sha1),
        _ => ("MD5", hex == sums.md5),
    })
}

/// Where a site may publish a download's SHA-256: `<the file's address>.sha256`, else a sums file in its
/// folder. Addresses with a query (a script hands the file out) have none.
fn sidecars(url: &str) -> Vec<String> {
    let Ok(mut u) = url::Url::parse(url) else { return Vec::new() };
    if !matches!(u.scheme(), "http" | "https") || u.query().is_some() || u.path().ends_with('/') {
        return Vec::new();
    }
    u.set_fragment(None);
    let mut out = vec![format!("{u}.sha256")];
    out.extend(["SHA256SUMS", "sha256sums.txt"].iter().filter_map(|n| u.join(n).ok()).map(|x| x.to_string()));
    out
}

fn hex64(t: &str) -> bool {
    t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// The SHA-256 `text` gives `file`: its line in a sums list ("hex  name", "hex *name" or BSD's
/// "SHA256 (name) = hex"), or, in a file of its own (`own`), the one SHA-256 there.
fn sum_for(text: &str, file: &str, own: bool) -> Option<String> {
    let named = |n: &str| n.trim().trim_start_matches('*').rsplit(['/', '\\']).next() == Some(file);
    let listed = text.lines().find_map(|l| {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("SHA256 (") {
            let (name, hex) = rest.split_once(") = ")?;
            return (named(name) && hex64(hex.trim())).then(|| hex.trim().to_ascii_lowercase());
        }
        let (hex, name) = l.split_once(char::is_whitespace)?;
        (named(name) && hex64(hex)).then(|| hex.to_ascii_lowercase())
    });
    let only = || {
        let sums: Vec<&str> = text.split(|c: char| !c.is_ascii_hexdigit()).filter(|t| hex64(t)).collect();
        (sums.len() == 1).then(|| sums[0].to_ascii_lowercase())
    };
    listed.or_else(|| if own { only() } else { None })
}

/// The SHA-256 the site publishes beside the download at `url` for `file`, if it does (Options ->
/// Downloads -> "Check a downloaded file against the SHA-256 its site publishes").
pub fn published_sha256(agent: &crate::engine::Agent, url: &str, referrer: &str, cookies: &str, file: &str) -> Option<String> {
    for (k, at) in sidecars(url).iter().enumerate() {
        let Ok(r) = crate::engine::call(crate::engine::request(agent, at, referrer, cookies)) else { continue };
        if r.header("Content-Type").is_some_and(|t| t.to_ascii_lowercase().contains("html")) {
            continue; // a site's "not found" page
        }
        let mut text = String::new();
        if r.into_reader().take(256 << 10).read_to_string(&mut text).is_err() {
            continue;
        }
        if let Some(sum) = sum_for(&text, file, k == 0) {
            return Some(sum);
        }
    }
    None
}

/// A file's SHA-256 (hex).
pub fn sha256_of(path: &std::path::Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_and_compare() {
        let p = std::env::temp_dir().join(format!("ndm-test-sum-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let run = start(p.clone());
        let sums = loop {
            if let Some(r) = run.result.lock().unwrap().take() {
                break r.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let _ = std::fs::remove_file(&p);
        assert_eq!(sums.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sums.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(sums.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(compare(&sums, "SHA256: BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"), Some(("SHA-256", true)));
        assert_eq!(compare(&sums, "a9993e364706816aba3e25717850c26c9cd0d89d  abc.txt"), Some(("SHA-1", true)));
        assert_eq!(compare(&sums, "00000000000000000000000000000000"), Some(("MD5", false)));
        assert_eq!(compare(&sums, "not a checksum"), None);
    }

    /// A site's published SHA-256 beside the download: its own .sha256 file, or a sums list's line.
    #[test]
    fn published_sums() {
        let a = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sidecars("https://x.org/pub/app-1.0.zip"), ["https://x.org/pub/app-1.0.zip.sha256", "https://x.org/pub/SHA256SUMS", "https://x.org/pub/sha256sums.txt"]);
        assert!(sidecars("https://x.org/get.php?id=1").is_empty() && sidecars("ftp://x.org/a.zip").is_empty());
        assert_eq!(sum_for(&a.to_uppercase(), "app.zip", true).as_deref(), Some(a));
        assert_eq!(sum_for(&format!("{a}  other.zip\n"), "app.zip", true).as_deref(), Some(a), "a .sha256 of its own may name the file otherwise");
        let list = format!("{}  other.zip\n{a} *dist/app.zip\n", "0".repeat(64));
        assert_eq!(sum_for(&list, "app.zip", false).as_deref(), Some(a));
        assert_eq!(sum_for(&list, "missing.zip", false), None);
        assert_eq!(sum_for(&format!("SHA256 (app.zip) = {a}\n"), "app.zip", false).as_deref(), Some(a));
        let p = std::env::temp_dir().join(format!("ndm-test-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(sha256_of(&p).unwrap(), a);
        let _ = std::fs::remove_file(&p);
    }
}
