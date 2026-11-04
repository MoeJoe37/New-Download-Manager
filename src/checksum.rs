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

const MAX_MANIFEST: u64 = 1024 * 1024;

fn read_manifest(reader: impl Read) -> Result<String, String> {
    let mut text = String::new();
    reader.take(MAX_MANIFEST + 1).read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_MANIFEST {
        return Err("Checksum lists must be at most 1 MiB.".into());
    }
    Ok(text)
}

pub fn from_file(path: &std::path::Path, name: &str) -> Result<String, String> {
    if !path.is_file() { return Err("Choose a regular checksum file.".into()); }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let text = read_manifest(file)?;
    if is_sha256(text.trim()) {
        return Ok(text.trim().to_ascii_lowercase());
    }
    matching_sha256(&text, name)
}

pub fn from_publisher(agent: &crate::engine::Agent, url: &str, name: &str) -> Result<String, String> {
    let url = publisher_url(url).ok_or("Enter the publisher's HTTPS checksum-list address.")?;
    let req = crate::transport::request(agent, url.as_str(), "", "").https_only();
    let response = crate::transport::call(req)?;
    matching_sha256(&read_manifest(response.into_reader())?, name)
}

pub fn publisher_url(text: &str) -> Option<url::Url> {
    let url = url::Url::parse(text.trim()).ok()?;
    (url.scheme() == "https" && url.host_str().is_some()
        && url.username().is_empty() && url.password().is_none()).then_some(url)
}

fn is_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// GNU sha256sum and BSD SHA256 listings. A publisher result must explicitly name the file.
fn matching_sha256(text: &str, name: &str) -> Result<String, String> {
    let mut found = None;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let (hash, filename) = if let Some((file, hash)) = line.strip_prefix("SHA256 (").and_then(|s| s.rsplit_once(") = ")) {
            (hash.trim(), file)
        } else if let Some((hash, file)) = line.split_once(char::is_whitespace) {
            let file = file.trim_start();
            (hash, file.strip_prefix('*').unwrap_or(file))
        } else {
            continue;
        };
        let filename = filename.strip_prefix("./").unwrap_or(filename);
        if !is_sha256(hash) || filename != name {
            continue;
        }
        let hash = hash.to_ascii_lowercase();
        if found.as_ref().is_some_and(|previous| previous != &hash) {
            return Err("The checksum list contains conflicting hashes for this filename.".into());
        }
        found = Some(hash);
    }
    found.ok_or_else(|| "The checksum list has no SHA-256 entry for this exact filename.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_lists_require_an_unambiguous_exact_filename() {
        let one = "a".repeat(64);
        let two = "b".repeat(64);
        let list = format!("{two}  app-old.zip\n{one} *./NDM إصدار.zip\nSHA256 (NDM إصدار.zip) = {}\n", one.to_uppercase());
        assert_eq!(matching_sha256(&list, "NDM إصدار.zip"), Ok(one.clone()));
        assert!(matching_sha256(&list, "ndm إصدار.zip").is_err());
        assert!(matching_sha256(&one, "NDM إصدار.zip").is_err());
        assert!(matching_sha256(&format!("{list}{two}  NDM إصدار.zip"), "NDM إصدار.zip").is_err());
        assert!(publisher_url("http://example.org/checksums").is_none());
        assert!(publisher_url("https://user:pass@example.org/checksums").is_none());
        assert!(publisher_url("https://example.org/checksums").is_some());
        assert!(read_manifest(std::io::repeat(b'a').take(MAX_MANIFEST + 1)).is_err());

        let file = std::env::temp_dir().join(format!("ndm-checksum-list-{}", std::process::id()));
        std::fs::write(&file, list).unwrap();
        assert_eq!(from_file(&file, "NDM إصدار.zip"), Ok(one.clone()));
        std::fs::write(&file, &one).unwrap();
        assert_eq!(from_file(&file, "explicit.zip"), Ok(one));
        std::fs::remove_file(file).unwrap();
    }

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
}
