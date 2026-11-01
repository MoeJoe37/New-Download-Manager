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
}
