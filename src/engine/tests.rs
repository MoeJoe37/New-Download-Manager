use super::*;
use super::ftp::{FtpCfg, FTP};
use super::job::{merge_cookies, VERIFY};
use super::names::typed_name;
use super::probe::same_file;
use crate::transport::auth::{digest, md5_hex};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;

use crate::test_support::serve;

#[test]
fn retries_wait_longer() {
    let w: Vec<u64> = (1..=9).map(|n| retry_wait(n).as_secs()).collect();
    assert_eq!(w, [15, 30, 60, 120, 240, 480, 600, 600, 600]);
}

#[test]
fn browser_file_is_checked() {
    // /file: a 1000-byte video; /app: the site's web page (what a service worker's address gives NDM);
    // /gone: an expired link
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut line = String::new();
            let mut r = BufReader::new(&s);
            r.read_line(&mut line).unwrap();
            while { let mut h = String::new(); r.read_line(&mut h).unwrap_or(0) > 2 } {}
            let head = match line.split(' ').nth(1).unwrap_or("") {
                "/file" => "HTTP/1.1 206 Partial\r\nContent-Type: video/mp4\r\nContent-Range: bytes 0-0/1000\r\nContent-Length: 1\r\n\r\nx",
                "/app" => "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 4\r\n\r\n<p/>",
                _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
            };
            let _ = (&s).write_all(head.as_bytes());
        }
    });
    let a = agent("", "");
    let check = |path: &str, mime: &str, size: u64, name: &str| same_file(&a, &format!("{base}{path}"), "", "", mime, size, name);
    assert!(check("/file", "video/mp4", 1000, "v.mp4").is_ok());
    assert!(check("/file", "", 0, "v.mp4").is_ok(), "nothing known from the browser");
    assert!(check("/file", "video/mp4", 1_000_000, "v.mp4").is_err(), "another size");
    assert!(check("/app", "video/mp4", 12_000_000, "v.mp4").is_err(), "a page for a video");
    assert!(check("/app", "", 0, "song.mp3").is_err(), "a page for an mp3");
    assert!(check("/app", "text/html", 0, "page.html").is_ok(), "a page is a page");
    assert!(check("/gone", "audio/mpeg", 0, "a.mp3").is_err(), "an error");
}

/// A server that serves at most `most` requests at once and answers 503 to the rest (`hang`: leaves
/// them unanswered, as some video sites do); returns its address and how many it turned away.
fn serve_limited(data: Arc<Vec<u8>>, most: usize, hang: bool) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::AtomicUsize;
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    let (active, refused) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let refused2 = refused.clone();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (data, active, refused) = (data.clone(), active.clone(), refused.clone());
            thread::spawn(move || loop {
                // keep-alive: one request after another on this connection
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut range = (0, data.len() - 1);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = strip_ci(&h, "range: bytes=") {
                        let (a, b) = v.trim().split_once('-').unwrap();
                        range = (a.parse().unwrap(), b.parse().unwrap_or(data.len() - 1).min(data.len() - 1));
                    }
                }
                let mut w = &s;
                if active.fetch_add(1, Relaxed) >= most {
                    active.fetch_sub(1, Relaxed);
                    refused.fetch_add(1, Relaxed);
                    if hang {
                        thread::sleep(Duration::from_secs(120));
                        return;
                    }
                    let _ = w.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
                    continue;
                }
                let (a, b) = range;
                let head = format!("HTTP/1.1 206 Partial\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\n\r\n", data.len(), b + 1 - a);
                let mut ok = w.write_all(head.as_bytes()).is_ok();
                for c in data[a..=b].chunks(64 * 1024) {
                    ok = ok && w.write_all(c).is_ok();
                    thread::sleep(Duration::from_millis(30));
                }
                active.fetch_sub(1, Relaxed);
                if !ok {
                    return;
                }
            });
        }
    });
    (base, refused2)
}

#[test]
fn keeps_to_a_servers_connection_limit() {
    let data: Arc<Vec<u8>> = Arc::new((0..3 * 1024 * 1024).map(|i: u32| (i * 13 % 251) as u8).collect());
    let (base, refused) = serve_limited(data.clone(), 2, false);
    let dir = std::env::temp_dir().join(format!("ndm-test-limit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(Info { conns: 8, ..info(format!("{base}f.bin"), &dir) });
    let t = Instant::now();
    run(&j);
    let (status, segs) = { let i = j.info(); (i.status.clone(), i.segs.iter().map(|s| (s.start, s.next, s.stop, s.owned, s.info.clone())).collect::<Vec<_>>()) };
    assert_eq!(status, Status::Complete, "{segs:?}");
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    // the connections the server turned away end at once instead of retrying for a minute
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    assert!(refused.load(Relaxed) <= 12, "{} requests refused", refused.load(Relaxed));
    let _ = fs::remove_dir_all(&dir);
}

/// A server that leaves the connections over its limit unanswered: they give up after NO_ANSWER while
/// the others receive, instead of waiting out the read timeout again and again (a download that seemed
/// stuck).
#[test]
fn keeps_to_a_server_that_leaves_connections_hanging() {
    let data: Arc<Vec<u8>> = Arc::new((0..3 * 1024 * 1024).map(|i: u32| (i * 7 % 251) as u8).collect());
    let (base, _) = serve_limited(data.clone(), 1, true);
    let dir = std::env::temp_dir().join(format!("ndm-test-hang-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(Info { conns: 8, ..info(format!("{base}f.bin"), &dir) });
    let t = Instant::now();
    run(&j);
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    assert!(t.elapsed() < Duration::from_secs(25), "{:?}", t.elapsed());
    let _ = fs::remove_dir_all(&dir);
}

fn run(job: &Arc<Job>) {
    job.start(agent("", ""), Arc::new(Limiter::default()));
    while job.running() {
        thread::sleep(Duration::from_millis(20));
    }
}

fn info(url: String, dir: &Path) -> Info {
    Info { url, folder: dir.display().to_string(), conns: 4, ..Default::default() }
}

/// The finished file copied out of a temporary directory on another drive: in place, the temporary
/// copy gone, and no processing left showing.
#[test]
fn file_types() {
    assert_eq!(mime_ext("video/mp4; codecs=avc1"), Some("mp4"));
    assert_eq!(mime_ext("text/html"), None);
    assert_eq!(typed_name("watch", Some("mp4")), "watch.mp4");
    assert_eq!(typed_name("download.php", Some("zip")), "download.zip");
    assert_eq!(typed_name("setup.exe", Some("zip")), "setup.exe", "a real extension stays");
    assert_eq!(typed_name("v 1.2 final", Some("pdf")), "v 1.2 final.pdf");
    assert_eq!(typed_name("a.zip", None), "a.zip");
    let dir = std::env::temp_dir().join(format!("ndm-sniff-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    for (bytes, want) in [
        (b"\0\0\0\x18ftypisom\0\0\0\0".to_vec(), Some("mp4")),
        (b"\0\0\0\x18ftypM4A \0\0\0\0".to_vec(), Some("m4a")),
        ([&[0x1A, 0x45, 0xDF, 0xA3][..], b"\x9fB\x82\x84webm"].concat(), Some("webm")),
        (b"ID3\x04\0".to_vec(), Some("mp3")),
        (b"%PDF-1.7".to_vec(), Some("pdf")),
        (b"PK\x03\x04rest".to_vec(), Some("zip")),
        (b"MZ\x90\0".to_vec(), Some("exe")),
        (b"just some text".to_vec(), None),
    ] {
        let p = dir.join("f");
        fs::write(&p, &bytes).unwrap();
        assert_eq!(sniff(&p), want, "{bytes:?}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn copy_out_moves_the_file() {
    let dir = std::env::temp_dir().join(format!("ndm-copyout-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let (from, to) = (dir.join("a.part"), dir.join("a.bin"));
    let data: Vec<u8> = (0..9_000_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(&from, &data).unwrap();
    let j = Job::new(Info::default());
    j.copy_out(&from, &to).unwrap();
    assert_eq!(fs::read(&to).unwrap(), data);
    assert!(!from.exists() && !dir.join("a.bin.ndm-copy").exists());
    assert!(j.info().post.is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn downloads_resumes_and_splits() {
    let data: Arc<Vec<u8>> = Arc::new((0..3 * 1024 * 1024 + 123).map(|i: u32| (i * 7 % 251) as u8).collect());
    let base = serve(data.clone());
    let dir = std::env::temp_dir().join(format!("ndm-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);

    // one connection would take ~2.4 s, so the engine ramps 1 -> 2 after a second
    let j = Job::new(info(format!("{base}rslow1.bin"), &dir));
    j.set_date.store(true, Relaxed);
    run(&j);
    assert_eq!(j.info().status, Status::Complete);
    assert!(j.info().segs.len() >= 2, "{}", j.info().segs.len());
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let t = fs::metadata(j.info().path()).unwrap().modified().unwrap();
    assert_eq!(t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), 784111777, "server's Last-Modified");

    let j = Job::new(info(format!("{base}plain.bin"), &dir));
    run(&j);
    assert_eq!(j.info().status, Status::Complete);
    assert!(!j.info().ranges);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);

    // pause mid-way, round-trip through JSON (as on restart), resume
    let j = Job::new(Info { limit_kb: 1024, ..info(format!("{base}r2.bin"), &dir) });
    j.start(agent("", ""), Arc::new(Limiter::default()));
    thread::sleep(Duration::from_millis(800));
    j.stop();
    while j.running() {
        thread::sleep(Duration::from_millis(20));
    }
    let saved = serde_json::to_string(&*j.info()).unwrap();
    let mut i: Info = serde_json::from_str(&saved).unwrap();
    assert!(i.status == Status::Stopped && i.done() > 0 && i.done() < data.len() as u64, "{:?} {}", i.status, i.done());
    i.limit_kb = 0;
    let j = Job::new(i);
    run(&j);
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
#[cfg(windows)]
fn ie_cache_not_modified() {
    use std::io::{BufRead, BufReader, Write};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Networking::WinInet::{CommitUrlCacheEntryW, CreateUrlCacheEntryW, DeleteUrlCacheEntryW, NORMAL_CACHE_ENTRY};
    // a server that answers 304 when asked If-Modified-Since, else sends "fresh"
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/cached.txt", l.local_addr().unwrap());
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (mut ims, mut r) = (false, BufReader::new(&s));
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                ims |= line.to_ascii_lowercase().starts_with("if-modified-since:");
                line.clear();
            }
            let reply = if ims { "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_string() } else { "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfresh".to_string() };
            let _ = (&s).write_all(reply.as_bytes());
        }
    });
    let dir = std::env::temp_dir().join(format!("ndm-test-iec-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (wu, ext) = (w(&url), w("txt"));
    // WinINet only commits files it handed out itself (CreateUrlCacheEntryW)
    let mut name = [0u16; 260];
    let made = unsafe { CreateUrlCacheEntryW(wu.as_ptr(), 0, ext.as_ptr(), name.as_mut_ptr(), 0) };
    let cache_file = std::path::PathBuf::from(String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(0)]));
    let t = FILETIME { dwLowDateTime: 0xd53e_8000, dwHighDateTime: 0x01d9_0000 }; // some time in 2022
    let ok = made != 0
        && fs::write(&cache_file, "cached copy").is_ok()
        && unsafe { CommitUrlCacheEntryW(wu.as_ptr(), name.as_ptr(), FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }, t, NORMAL_CACHE_ENTRY, std::ptr::null(), 0, ext.as_ptr(), std::ptr::null()) } != 0;
    if !ok {
        eprintln!("ie_cache_not_modified skipped: CommitUrlCacheEntryW failed ({})", std::io::Error::last_os_error());
        return; // no WinINet cache on this machine (e.g. a service account): nothing to look up
    }
    let found = crate::win::url_cache(&url);
    let out = dir.join("out");
    let j = Job::new(Info { ie_cache: true, name: "f.txt".into(), ..info(url.clone(), &out) });
    run(&j);
    let plain = Job::new(Info { name: "g.txt".into(), ..info(url.clone(), &out) });
    run(&plain);
    unsafe { DeleteUrlCacheEntryW(wu.as_ptr()) };
    let (_, since) = found.expect("the entry just committed");
    assert!(since.ends_with(" GMT") && since.contains("2022"), "{since}");
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read_to_string(j.info().path()).unwrap(), "cached copy", "304: the cached file is used");
    assert_eq!(fs::read_to_string(plain.info().path()).unwrap(), "fresh", "without the option the server's copy");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rejects_invalid_ranges_and_preserves_partial_when_etag_changes() {
    for value in ["bytes 5-4/6", "bytes 0-6/6", "bytes */6", "items 0-1/6", "bytes 0-1/*"] {
        assert_eq!(content_range(value), None, "{value}");
    }
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/video.mp4?signature=abc", server.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        let mut request = String::new();
        let mut reader = BufReader::new(&socket);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() <= 2 { break; }
            request.push_str(&line);
        }
        assert!(request.to_ascii_lowercase().contains("if-range: \"old\""));
        socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/4\r\nETag: \"new\"\r\n\r\nXYZ!").unwrap();
    });
    let dir = std::env::temp_dir().join(format!("ndm-test-etag-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let info = Info { name: "video.mp4".into(), size: Some(4), ranges: true, probed: true, etag: "\"old\"".into(),
        segs: vec![Seg { next: 2, stop: Some(4), ..Default::default() }], ..info(url, &dir) };
    fs::write(info.part(), b"AB\0\0").unwrap();
    let job = Job::new(info);
    run(&job);
    assert_eq!(job.info().status, Status::Error(CHANGED.into()));
    let current = job.info();
    assert!(!current.path().exists());
    assert_eq!(current.done(), 2);
    assert_eq!(fs::read(current.part()).unwrap(), b"AB\0\0");
    drop(current);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn initial_full_response_is_downloaded_once_without_splitting() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/video", server.local_addr().unwrap());
    let data = [0, 0, 0, 16, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0, 0, 0, 0];
    let handle = thread::spawn(move || {
        let (mut socket, _) = server.accept().unwrap();
        let mut reader = BufReader::new(&socket);
        while reader.read_line(&mut String::new()).unwrap() > 2 {}
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 16\r\n\r\n").unwrap();
        socket.write_all(&data).unwrap();
    });
    let dir = std::env::temp_dir().join(format!("ndm-test-full-range-{}", std::process::id()));
    let job = Job::new(Info { conns: 8, ..info(url, &dir) });
    run(&job);
    let current = job.info();
    assert_eq!(current.status, Status::Complete);
    assert!(!current.ranges);
    assert_eq!(current.segs.len(), 1);
    assert_eq!(fs::read(current.path()).unwrap(), data);
    drop(current);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn resume_detects_changed_file() {
    let a: Arc<Vec<u8>> = Arc::new((0..2 * 1024 * 1024).map(|i: u32| (i % 251) as u8).collect());
    let b: Arc<Vec<u8>> = Arc::new(a.iter().map(|x| x ^ 0xff).collect());
    let (ua, ub) = (serve(a), serve(b.clone()));
    let dir = std::env::temp_dir().join(format!("ndm-test-chg-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(Info { limit_kb: 512, conns: 1, ..info(format!("{ua}r.bin"), &dir) });
    j.start(agent("", ""), Arc::new(Limiter::default()));
    thread::sleep(Duration::from_millis(600));
    j.stop();
    while j.running() {
        thread::sleep(Duration::from_millis(20));
    }
    let mut i = j.info().clone();
    assert!(i.done() > VERIFY);
    (i.url, i.limit_kb) = (format!("{ub}r.bin"), 0); // same size, different bytes
    let j = Job::new(i);
    run(&j);
    assert_eq!(j.info().status, Status::Error(CHANGED.into()));
    assert!(j.ended.load(Relaxed), "a failed run fires the Download failed sound");
    run(&j);
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *b);
    let _ = fs::remove_dir_all(&dir);
}

/// Tiny passive-mode FTP server for one file, honouring REST.
fn ftp_serve(data: Arc<Vec<u8>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ftp://{}/dir/f.bin", l.local_addr().unwrap());
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let data = data.clone();
            thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                let _ = w.write_all(b"220-hello\r\n220 ready\r\n");
                let (mut rest, mut pasv, mut port) = (0, None, None);
                let mut line = String::new();
                while r.read_line(&mut line).unwrap_or(0) > 0 {
                    let l = line.trim().to_string();
                    line.clear();
                    let (cmd, arg) = l.split_once(' ').unwrap_or((&l, ""));
                    let reply = match cmd {
                        "USER" => "331 password please".into(),
                        "SIZE" => format!("213 {}", data.len()),
                        "PASV" => {
                            let p = TcpListener::bind("127.0.0.1:0").unwrap();
                            let port = p.local_addr().unwrap().port();
                            pasv = Some(p);
                            format!("227 Entering Passive Mode (127,0,0,1,{},{}).", port / 256, port % 256)
                        }
                        "PORT" => {
                            let n: Vec<u16> = arg.split(',').map(|x| x.parse().unwrap()).collect();
                            port = Some(format!("{}.{}.{}.{}:{}", n[0], n[1], n[2], n[3], n[4] * 256 + n[5]));
                            "200 port ok".into()
                        }
                        "REST" => {
                            rest = arg.parse().unwrap();
                            "350 restarting".into()
                        }
                        "RETR" => {
                            let _ = w.write_all(b"150 sending\r\n");
                            let mut d = match pasv.take() {
                                Some(p) => p.accept().unwrap().0,
                                None => std::net::TcpStream::connect(port.take().unwrap()).unwrap(),
                            };
                            for c in data[rest..].chunks(64 * 1024) {
                                if d.write_all(c).is_err() {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(20));
                            }
                            "226 done".into()
                        }
                        _ => "200 ok".into(),
                    };
                    let _ = w.write_all(format!("{reply}\r\n").as_bytes());
                }
            });
        }
    });
    url
}

/// Range-aware HTTP server trickling ~1.3 MB/s per connection (User-Agent "slow": ~130 KB/s); logs
/// each request's User-Agent.
fn serve_ua(data: Arc<Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/f.bin", l.local_addr().unwrap());
    let log = Arc::new(Mutex::new(vec![]));
    let log2 = log.clone();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (data, log) = (data.clone(), log2.clone());
            thread::spawn(move || {
                let mut r = BufReader::new(&s);
                let (mut range, mut line, mut pause) = ((0, data.len() - 1), String::new(), 50);
                while r.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    if let Some(v) = strip_ci(&line, "user-agent: ") {
                        if v.trim() == "slow" {
                            pause = 500; // a network a tenth as fast
                        }
                        log.lock().unwrap().push(v.trim().to_string());
                    } else if let Some(v) = strip_ci(&line, "range: bytes=") {
                        let (a, b) = v.trim().split_once('-').unwrap();
                        range = (a.parse().unwrap(), b.parse().unwrap_or(data.len() - 1).min(data.len() - 1));
                    }
                    line.clear();
                }
                let (a, b) = range;
                let mut w = &s;
                let _ = w.write_all(format!("HTTP/1.1 206 Partial\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\n\r\n", data.len(), b + 1 - a).as_bytes());
                for c in data[a..=b].chunks(64 * 1024) {
                    if w.write_all(c).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(pause));
                }
            });
        }
    });
    (url, log)
}

fn lanes_run(lanes: Vec<(String, Agent)>, tag: &str) -> (Arc<Job>, Arc<Vec<u8>>, Arc<Mutex<Vec<String>>>, PathBuf) {
    lanes_run_then(lanes, tag, None)
}

/// As `lanes_run`; after a second the download's networks become `later`, as the core sets them
/// when a network comes or goes.
/// A download over `lanes`, which become `later` after `after` (if given).
fn lanes_run_then(lanes: Vec<(String, Agent)>, tag: &str, later: Option<(Duration, Vec<(String, Agent)>)>) -> (Arc<Job>, Arc<Vec<u8>>, Arc<Mutex<Vec<String>>>, PathBuf) {
    let data: Arc<Vec<u8>> = Arc::new((0..4 * 1024 * 1024 + 3).map(|i: u32| (i * 17 % 249) as u8).collect());
    let (url, log) = serve_ua(data.clone());
    let dir = std::env::temp_dir().join(format!("ndm-test-lanes-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(Info { conns: 4, ..info(url, &dir) });
    *j.lanes.lock().unwrap() = lanes;
    j.start(agent("", "main"), Arc::new(Limiter::default()));
    let (start, until) = (Instant::now(), Instant::now() + Duration::from_secs(60));
    let mut later = later;
    while j.running() && Instant::now() < until {
        if later.as_ref().is_some_and(|(after, _)| start.elapsed() >= *after) {
            *j.lanes.lock().unwrap() = later.take().unwrap().1;
        }
        thread::sleep(Duration::from_millis(20));
    }
    (j, data, log, dir)
}

#[test]
fn segments_use_every_lane() {
    let (j, data, log, dir) = lanes_run(vec![("a".into(), agent("", "lane-a")), ("b".into(), agent("", "lane-b"))], "every");
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let uas = log.lock().unwrap();
    assert!(uas.iter().any(|u| u == "lane-a") && uas.iter().any(|u| u == "lane-b") && !uas.iter().any(|u| u == "main"), "{uas:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lane_joins_mid_download() {
    // "b" is plugged in while "a" ramps up (four connections finish the file in about a second)
    let both = vec![("a".into(), agent("", "lane-a")), ("b".into(), agent("", "lane-b"))];
    let (j, data, log, dir) = lanes_run_then(vec![("a".into(), agent("", "lane-a"))], "join", Some((Duration::from_millis(300), both)));
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    assert!(log.lock().unwrap().iter().any(|u| u == "lane-b"), "the network that joined got connections");
    assert!(j.net_bytes.lock().unwrap().get("b").is_some_and(|n| *n > 0));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn lane_leaves_mid_download() {
    // "b" is unplugged after a second: its parts go to "a", and the download completes without an error
    let both = vec![("a".into(), agent("", "lane-a")), ("b".into(), agent("", "lane-b"))];
    let (j, data, _, dir) = lanes_run_then(both, "leave", Some((Duration::from_secs(1), vec![("a".into(), agent("", "lane-a"))])));
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn slow_lane_doesnt_hold_up_the_end() {
    // a network a tenth as fast would take ~8 s for its quarter alone: the fast one takes the bigger share
    // (how much bigger depends on how busy the machine is), and the end comes when it would on the fast
    // network alone
    let t = Instant::now();
    let (j, data, _, dir) = lanes_run(vec![("fast".into(), agent("", "lane-a")), ("slow".into(), agent("", "slow"))], "slow");
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let bytes = j.net_bytes.lock().unwrap().clone();
    let (fast, slow) = (bytes.get("fast").copied().unwrap_or(0), bytes.get("slow").copied().unwrap_or(0));
    assert!(slow > 0 && fast > slow, "fast {fast}, slow {slow}");
    assert!(t.elapsed() < Duration::from_secs(8), "took {:?}", t.elapsed());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dead_lane_doesnt_fail_download() {
    // port 9 refuses: every connection on the "dead" network fails at once
    let (j, data, _, dir) = lanes_run(vec![("ok".into(), agent("", "")), ("dead".into(), agent("socks5://127.0.0.1:9", ""))], "dead");
    assert!(!j.running(), "finished within 60 s");
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let _ = fs::remove_dir_all(&dir);
}

/// `FTP` is global and tests run in parallel: every FTP test holds this.
static FTP_TESTS: Mutex<()> = Mutex::new(());

fn ftp_route(route: String) {
    *FTP.lock().unwrap() = FtpCfg { pasv: true, route: Some(Arc::new(move |_: &str| route.clone())) };
}

#[test]
fn ftp_downloads_and_resumes() {
    let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    *FTP.lock().unwrap() = FtpCfg { pasv: true, route: None };
    let data: Arc<Vec<u8>> = Arc::new((0..2 * 1024 * 1024 + 77).map(|i: u32| (i * 13 % 241) as u8).collect());
    let url = ftp_serve(data.clone());
    let dir = std::env::temp_dir().join(format!("ndm-test-ftp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(Info { limit_kb: 512, ..info(url, &dir) });
    j.start(agent("", ""), Arc::new(Limiter::default()));
    thread::sleep(Duration::from_millis(700));
    j.stop();
    while j.running() {
        thread::sleep(Duration::from_millis(20));
    }
    let mut i = j.info().clone();
    assert!(i.ranges && i.name == "f.bin" && i.done() > VERIFY && i.done() < data.len() as u64, "{:?} {}", i.status, i.done());
    i.limit_kb = 0;
    // resume in active mode through an "FTP proxy" (the test server, reached as user@host)
    let addr = i.url.split('/').nth(2).unwrap().to_string();
    let proxy = format!("ftp://{addr}");
    *FTP.lock().unwrap() = FtpCfg { pasv: false, route: Some(Arc::new(move |_: &str| proxy.clone())) };
    i.url = i.url.replace(&addr, "files.example");
    let j = Job::new(i);
    run(&j);
    *FTP.lock().unwrap() = FtpCfg { pasv: true, route: None };
    assert_eq!(j.info().status, Status::Complete);
    assert_eq!(fs::read(j.info().path()).unwrap(), *data);
    let _ = fs::remove_dir_all(&dir);
}

/// An HTTP proxy that fetches ftp:// addresses: records each request head and answers from `data`,
/// with 206 + Content-Range when `honour_range`, else 200 + the whole file.
fn fake_http_proxy(data: Arc<Vec<u8>>, honour_range: bool) -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(vec![]));
    let heads2 = heads.clone();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (data, heads) = (data.clone(), heads2.clone());
            thread::spawn(move || {
                let mut r = BufReader::new(&s);
                let mut head = String::new();
                while r.read_line(&mut head).unwrap_or(0) > 0 && !head.ends_with("\r\n\r\n") {}
                let from: usize = head.split("Range: bytes=").nth(1).and_then(|v| v.split('-').next()).and_then(|v| v.parse().ok()).unwrap_or(0);
                heads.lock().unwrap().push(head);
                let mut w = &s;
                let (h, body) = if honour_range {
                    (format!("HTTP/1.0 206 Partial\r\nContent-Range: bytes {from}-{}/{}\r\nContent-Length: {}\r\n\r\n", data.len() - 1, data.len(), data.len() - from), &data[from..])
                } else {
                    (format!("HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()), &data[..])
                };
                let _ = w.write_all(h.as_bytes());
                for c in body.chunks(64 * 1024) {
                    if w.write_all(c).is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            });
        }
    });
    (port, heads)
}

fn ftp_via(route: String, url: &str, data: &[u8], tag: &str) -> Info {
    let dir = std::env::temp_dir().join(format!("ndm-test-ftp-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    ftp_route(route);
    let j = Job::new(Info { conns: 4, ..info(url.into(), &dir) });
    run(&j);
    *FTP.lock().unwrap() = FtpCfg { pasv: true, route: None };
    let i = j.info().clone();
    assert_eq!(i.status, Status::Complete);
    assert_eq!(fs::read(i.path()).unwrap(), data);
    let _ = fs::remove_dir_all(&dir);
    i
}

#[test]
fn ftp_through_http_proxy() {
    let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let data: Arc<Vec<u8>> = Arc::new((0..1024 * 1024 + 5).map(|i: u32| (i * 7 % 251) as u8).collect());
    let (port, heads) = fake_http_proxy(data.clone(), true);
    let i = ftp_via(format!("http://u:p@127.0.0.1:{port}"), "ftp://h.example/f.bin", &data, "http");
    assert!(i.ranges);
    let first = &heads.lock().unwrap()[0];
    assert!(first.starts_with("GET ftp://h.example/f.bin HTTP/1.0\r\n"), "{first}");
    assert!(first.contains("\r\nRange: bytes=0-\r\n") && first.contains("\r\nProxy-Authorization: Basic dTpw\r\n"), "{first}");
}

#[test]
fn ftp_http_proxy_without_ranges() {
    let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let data: Arc<Vec<u8>> = Arc::new((0..1024 * 1024 + 5).map(|i: u32| (i * 11 % 251) as u8).collect());
    let (port, heads) = fake_http_proxy(data.clone(), false);
    let i = ftp_via(format!("http://127.0.0.1:{port}"), "ftp://h.example/f.bin", &data, "norange");
    assert!(!i.ranges);
    assert_eq!(heads.lock().unwrap().len(), 1, "one connection");
}

#[test]
fn ftp_through_socks() {
    let _one = FTP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let data: Arc<Vec<u8>> = Arc::new((0..1024 * 1024 + 9).map(|i: u32| (i * 3 % 253) as u8).collect());
    let url = ftp_serve(data.clone());
    let i = ftp_via(crate::net::local_proxy(""), &url, &data, "socks");
    assert!(i.ranges);
}


#[test]
fn steal_keeps_segments_contiguous() {
    let j = Job::new(Info { ranges: true, segs: vec![Seg { stop: Some(10 * MIN_SEG), owned: true, ..Default::default() }], ..Default::default() });
    let k = j.steal("").unwrap();
    let i = j.info();
    assert_eq!(i.segs[0].stop, Some(i.segs[k].start));
    assert_eq!(i.segs[k].stop, Some(10 * MIN_SEG));
}

#[test]
fn names() {
    assert_eq!(disposition_name(r#"attachment; filename="a b.zip""#).unwrap(), "a b.zip");
    assert_eq!(disposition_name("attachment; filename=x.bin; filename*=UTF-8''%E2%82%AC.txt").unwrap(), "€.txt");
    assert_eq!(url_name("https://h.com/dir/my%20file.iso?x=1"), "my file.iso");
    assert_eq!(url_name("https://h.com"), "");
    assert_eq!(sanitize("a:b?.."), "a_b_");
    assert_eq!(merge_cookies("a=1; b=2", &["b=3".into(), "c=4".into()]), "a=1; b=3; c=4");
    // RFC 2617 section 3.5 example
    assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
    let h = digest(r#"realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#, "Mufasa", "Circle Of Life", "GET", "/dir/index.html");
    assert!(h.contains(r#"realm="testrealm@host.com""#) && h.contains("qop=auth, nc=00000001") && h.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#), "{h}");
    let ha1 = md5_hex("Mufasa:testrealm@host.com:Circle Of Life");
    let cn = h.split("cnonce=\"").nth(1).unwrap().split('"').next().unwrap();
    let want = md5_hex(&format!("{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:00000001:{cn}:auth:{}", md5_hex("GET:/dir/index.html")));
    assert!(h.contains(&format!(r#"response="{want}""#)));
    assert_eq!(ha1, "939e7578ed9e3c518a452acee763bce9");
}
