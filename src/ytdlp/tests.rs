use super::*;
use super::formats::{cookie_header, MERGED, PAIR};
use super::install::{deno_asset, ytdlp_asset, fetch_deno, fetch_ffmpeg, fetch_ytdlp, listed_sum, only_sum, unpack_ffmpeg, unzip_one, version};
use super::playlist::{list_playlist, playlist_entries, PLAYLIST_LIMIT};

#[test]
fn flat_playlist_filters_invalid_and_duplicate_links() {
    use serde_json::json;
    let entry = json!({"title": "First", "url": "https://other.example/video?id=1"});
    let youtube = json!({"ie_key": "Youtube", "id": "v1234567890", "title": "Second"});
    let playlist = json!({"entries": [null, entry, entry,
        {"url": "file:///home/secret"}, {"url": "https://user:secret@other.example/video"},
        {"_type": "playlist", "url": "https://example.org/another-playlist"}, youtube]});
    let result = playlist_entries(&playlist).unwrap();
    assert_eq!(result.entries.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["First", "Second"]);
    assert_eq!(result.entries[0].url, "https://other.example/video?id=1");
    assert_eq!(result.entries[1].url, "https://www.youtube.com/watch?v=v1234567890");
    assert!(!result.limited);
    let entries: Vec<_> = (0..PLAYLIST_LIMIT + 1).map(|n| json!({"url": format!("https://example.org/{n}")})).collect();
    let capped = playlist_entries(&json!({"entries": entries})).unwrap();
    assert_eq!(capped.entries.len(), PLAYLIST_LIMIT);
    assert!(capped.limited);
    assert!(playlist_entries(&json!({"url": "https://example.org/video"})).is_err());

    let mut args = Vec::new();
    browser_args(&mut args, "").unwrap();
    assert!(args.is_empty());
    browser_args(&mut args, "firefox").unwrap();
    assert_eq!(args, ["--cookies-from-browser", "firefox"]);
    assert!(browser_args(&mut args, "unrecognized:profile").is_err());
}

#[test]
fn extractor_results_are_not_limited_to_known_sites() {
    use serde_json::json;
    let page = "https://media.example.org/podcast/episode";
    assert!(!is_video_page(page));
    assert!(wants(&Info { url: page.into(), ytdlp: true, ..Default::default() }));
    let info = video_info(json!({"_type": "playlist", "entries": [null,
        {"title": "An episode", "ext": "m4a", "format_id": "audio", "protocol": "https", "url": "https://cdn.example.org/signed?token=x"}
    ]})).unwrap();
    assert_eq!(video_name(&info), "An episode.m4a");
    let streams = picked(&info).unwrap();
    assert_eq!(streams.len(), 1);
    assert!(streams[0].direct);
    assert_eq!(streams[0].id, "audio");
    assert!(video_info(json!({"entries": []})).is_err());
    for extra in [json!({"fragments": []}), json!({"is_live": true}), json!({"downloader_options": {"impersonate": "chrome"}})] {
        let mut f = info.clone();
        f.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert!(!Stream::of(&f).unwrap().direct, "{extra}");
    }
    let args = base_args(page, page, "session=abc");
    assert!(args.iter().any(|a| a == "--ignore-config"));
    assert!(args.iter().any(|a| a == "Cookie: session=abc"));
}

#[test]
fn fallback_streams_download_together_and_stop_on_error() {
    use std::sync::atomic::AtomicUsize;
    let job = Job::new(Info { conns: 4, limit_kb: 100, ..Default::default() });
    let make = |id: &str| Stream { id: id.into(), url: String::new(), ext: "mp4".into(), size: 16, headers: vec![], direct: false };
    let mut streams = [make("video"), make("audio")];
    // One native stream fails before receiving bytes and switches to the fallback while its
    // companion is already downloading there. No network is needed for an invalid URL.
    streams[0].direct = true;
    streams[0].url = "invalid-url".into();
    let outs = [PathBuf::from("video"), PathBuf::from("audio")];
    let active = AtomicUsize::new(0);
    let fallback = |part: &Job, _: &Stream, _: &Path| {
        assert_eq!(part.info().conns, 4);
        assert_eq!(part.limiter.rate.load(Relaxed), 50 * 1024, "parallel streams share one download's limit");
        active.fetch_add(1, Relaxed);
        let start = std::time::Instant::now();
        while active.load(Relaxed) < 2 && start.elapsed().as_secs() < 3 { std::thread::yield_now(); }
        assert_eq!(active.load(Relaxed), 2, "both streams start concurrently");
        part.info().segs = vec![Seg { next: 16, stop: Some(16), ..Default::default() }];
        Ok(true)
    };
    let agent = crate::transport::client::agent("", "ndm-test");
    assert_eq!(fetch_streams(&job, &agent, &Limiter::default(), &streams, &outs, &fallback), Ok(true));
    assert_eq!(job.info().done(), 32);
    let failed = |part: &Job, stream: &Stream, _: &Path| {
        if stream.id == "audio" { return Err("audio failed".into()); }
        let start = std::time::Instant::now();
        while !part.stopping() && start.elapsed().as_secs() < 3 { std::thread::sleep(std::time::Duration::from_millis(10)); }
        assert!(part.stopping(), "failed companion stops the other stream");
        Ok(false)
    };
    assert_eq!(fetch_streams(&job, &agent, &Limiter::default(), &streams, &outs, &failed), Err("audio failed".into()));
}

#[test]
fn failed_merge_keeps_sources_and_reports_error() {
    let dir = std::env::temp_dir().join(format!("ndm-merge-failure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (video, audio, dest) = (dir.join("v.mp4"), dir.join("a.m4a"), dir.join("out.mp4"));
    std::fs::write(&video, b"not an MP4").unwrap();
    std::fs::write(&audio, b"not an M4A").unwrap();
    assert!(join(&Job::new(Info::default()), &video, &audio, &dest, None).is_err());
    assert!(video.exists() && audio.exists() && !dest.exists());
    let saved = save_extraction(&serde_json::json!({"cookies": "secret"}), &dir.join("state.json")).unwrap();
    let path = saved.path().to_path_buf();
    assert!(path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    drop(saved);
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn silent_downloader_stops() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("ndm-silent-ytdlp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("yt-dlp");
    std::fs::write(&bin, b"#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let job = Job::new(Info::default());
    let start = std::time::Instant::now();
    let result = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            job.stop();
        });
        fetch_video(&job, &bin, &[], &[], "v", "unused", 0, &Limiter::default())
    });
    assert_eq!(result, Ok(false));
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn video_pages() {
    assert!(is_video_page("https://www.youtube.com/watch?v=abc"));
    assert!(is_video_page("https://youtu.be/abc"));
    assert!(is_video_page("https://vimeo.com/12345"));
    assert!(!is_video_page("https://notyoutube.com/watch"));
    assert!(!is_video_page("https://video.twimg.com/a/b.mp4"));
    assert!(!is_video_page("https://cdn.vimeo.com/file.mp4"), "a file is downloaded by NDM itself");
    assert!(!is_video_page("ftp://youtube.com/x"));
    assert!(is_video_page("https://www.tiktok.com/@a/video/123"));
    assert!(is_video_page("https://www.threads.net/@a/post/xyz"));
    assert!(!is_video_page("https://i.imgur.com/abc.gif"), "a picture on a video site is a file");
    assert!(!is_video_page("https://www.linkedin.com/dms/report.pdf"));
    assert_eq!(last_error("[info] x\nERROR: [youtube] abc: Video unavailable\n"), "[youtube] abc: Video unavailable");
    let sums = "aa  yt-dlp\n0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa  yt-dlp_macos\n";
    assert_eq!(listed_sum(sums, "yt-dlp_macos").as_deref(), Some("0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa"));
    assert_eq!(listed_sum(sums, "yt-dlp"), None);
    let sum = "e890975bc2549807bbcc0b584ffafda72cc7c93349dd99c80483dd784f6c4609";
    assert_eq!(only_sum(&format!("{sum}  deno-x86_64-unknown-linux-gnu.zip\n")).as_deref(), Some(sum));
    let ps = format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\nPath      : D:\\a\\deno\\deno-x86_64-pc-windows-msvc.zip\r\n", sum.to_uppercase());
    assert_eq!(only_sum(&ps).as_deref(), Some(sum), "PowerShell's Get-FileHash, as Deno's Windows releases have it");
    // YouTube gets no cookies, other sites do
    assert!(!base_args("https://www.youtube.com/watch?v=a", "", "SID=1").iter().any(|a| a.starts_with("Cookie")));
    assert!(base_args("https://www.instagram.com/p/a", "", "s=1").iter().any(|a| a == "Cookie: s=1"));
    assert_eq!(template(Path::new("/d/100% fun.mp4")), "/d/100%% fun.mp4");
    assert_eq!(cookie_header("tt_chain_token=abc; Domain=.tiktok.com; Path=/; Secure; Expires=1790000000; ttwid=1%7Cx; Domain=.tiktok.com; Path=/"), "tt_chain_token=abc; ttwid=1%7Cx");
    assert_eq!(cookie_header(""), "");
    assert!(last_error("ERROR: [generic] Unable to download webpage: HTTP Error 404\n").contains("[generic]"));
    assert!(last_error("ERROR: Unsupported URL: https://www.tiktok.com/foryou\n").starts_with("There is no video"));
}

/// A zip as Deno's release has it: one program, deflated (or stored), maybe in a folder.
pub(super) fn zip_of(name: &str, data: &[u8], deflate: bool) -> Vec<u8> {
    use std::io::Write;
    let body = if deflate {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    } else {
        data.to_vec()
    };
    let method: u16 = if deflate { 8 } else { 0 };
    let n = name.len() as u16;
    let mut z = Vec::new();
    z.extend([0x50, 0x4b, 3, 4, 20, 0, 0, 0]);
    z.extend(method.to_le_bytes());
    z.extend([0u8; 8]); // time, date, crc
    z.extend((body.len() as u32).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend(n.to_le_bytes());
    z.extend(0u16.to_le_bytes());
    z.extend(name.as_bytes());
    z.extend(&body);
    let cd = z.len() as u32;
    z.extend([0x50, 0x4b, 1, 2, 20, 0, 20, 0, 0, 0]);
    z.extend(method.to_le_bytes());
    z.extend([0u8; 8]);
    z.extend((body.len() as u32).to_le_bytes());
    z.extend((data.len() as u32).to_le_bytes());
    z.extend(n.to_le_bytes());
    z.extend([0u8; 12]); // extra, comment, disk, attributes
    z.extend(0u32.to_le_bytes()); // local header offset
    z.extend(name.as_bytes());
    let cdlen = z.len() as u32 - cd;
    z.extend([0x50, 0x4b, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend(cdlen.to_le_bytes());
    z.extend(cd.to_le_bytes());
    z.extend([0, 0]);
    z
}

#[test]
fn unzips_deno() {
    let prog = b"#!/bin/sh\necho deno 9.9.9\n".repeat(50);
    assert_eq!(unzip_one(&zip_of("deno", &prog, true), "deno").unwrap(), prog);
    assert_eq!(unzip_one(&zip_of("bin/deno.exe", &prog, false), "deno.exe").unwrap(), prog);
    assert!(unzip_one(&zip_of("other", &prog, true), "deno").is_err());
    assert!(unzip_one(b"not a zip at all, just some bytes here", "deno").is_err());
}

/// Set yt-dlp against a local stand-in for GitHub's newest-release downloads: files checked against
/// the listed SHA-256, a damaged one refused, up-to-date copies left alone.
#[cfg(unix)]
#[test]
fn installs_and_checks() {
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, BufReader, Write};
    let hex = |d: &[u8]| Sha256::digest(d).iter().map(|b| format!("{b:02x}")).collect::<String>();
    let ytdlp = b"#!/bin/sh\necho 2099.01.02\n".to_vec();
    let deno_zip = zip_of("deno", b"#!/bin/sh\necho deno 9.9.9\n", true);
    let sums = std::sync::Arc::new(std::sync::Mutex::new(format!("{}  {}\n", hex(&ytdlp), ytdlp_asset())));
    let served = std::sync::Arc::new(AtomicU64::new(0));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/latest/download", listener.local_addr().unwrap());
    let (s2, n2) = (sums.clone(), served.clone());
    let (yt, dz, dsum) = (ytdlp.clone(), deno_zip.clone(), format!("\r\nAlgorithm : SHA256\r\nHash      : {}\r\n", hex(&deno_zip).to_uppercase()));
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut c = c;
            let body: Option<Vec<u8>> = if path.ends_with("/SHA2-256SUMS") {
                Some(s2.lock().unwrap().clone().into_bytes())
            } else if path.ends_with(&format!("/{}", ytdlp_asset())) {
                n2.fetch_add(1, Relaxed);
                Some(yt.clone())
            } else if path.ends_with(&format!("/{}.sha256sum", deno_asset())) {
                Some(dsum.clone().into_bytes())
            } else if path.ends_with(&format!("/{}", deno_asset())) {
                n2.fetch_add(1, Relaxed);
                Some(dz.clone())
            } else {
                None
            };
            let (head, body) = match body {
                Some(b) => ("200 OK", b),
                None => ("404 Not Found", vec![]),
            };
            let _ = write!(c, "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = c.write_all(&body);
        }
    });
    let dir = std::env::temp_dir().join(format!("ndm-test-ytdlp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let agent = ureq::AgentBuilder::new().build();
    let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
    // a damaged download is refused and leaves nothing behind
    *sums.lock().unwrap() = format!("{}  {}\n", "0".repeat(64), ytdlp_asset());
    assert!(fetch_ytdlp(&agent, &base, &dir, &got, &total).unwrap_err().contains("damaged"));
    assert!(!dir.join("yt-dlp").exists() && !dir.join("yt-dlp.part").exists());
    *sums.lock().unwrap() = format!("{}  {}\n", hex(&ytdlp), ytdlp_asset());
    fetch_ytdlp(&agent, &base, &dir, &got, &total).unwrap();
    fetch_deno(&agent, &base, &dir, &got, &total).unwrap();
    assert_eq!(std::fs::read(dir.join("yt-dlp")).unwrap(), ytdlp);
    assert!(runs(&dir.join("yt-dlp")) && runs(&dir.join("deno")), "installed programs run");
    assert_eq!(version(&dir.join("yt-dlp")), "2099.01.02");
    assert_eq!(total.load(Relaxed), (2 * ytdlp.len() + deno_zip.len()) as u64, "the damaged try, then both");
    assert!(!dir.read_dir().unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".part")));
    // up to date: neither is downloaded again
    fetch_ytdlp(&agent, &base, &dir, &got, &total).unwrap();
    fetch_deno(&agent, &base, &dir, &got, &total).unwrap();
    assert_eq!(served.load(Relaxed), 3, "the damaged try, yt-dlp and Deno, nothing more");
    let _ = std::fs::remove_dir_all(&dir);
}
/// The real yt-dlp (NDM_TEST_YTDLP=<its path>) on a site with separate video and audio streams, as
/// YouTube has them, without ffmpeg: video, then audio, joined by NDM into one MP4.
#[test]
#[ignore = "needs the real yt-dlp: NDM_TEST_YTDLP=/path/to/yt-dlp cargo test real_ytdlp -- --ignored"]
fn real_ytdlp_separate_streams() {
    use crate::test_support::{frag, init};
    use std::io::{BufRead, BufReader, Write};
    let bin = std::env::var("NDM_TEST_YTDLP").expect("NDM_TEST_YTDLP");
    let home = std::env::temp_dir().join(format!("ndm-real-ytdlp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let tools = home.join("NewDownloadManager/tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::copy(&bin, tools.join(exe_file("yt-dlp"))).unwrap();
    // Also find the isolated executable through PATH: macOS stores NDM's configuration under
    // Application Support rather than XDG_CONFIG_HOME/APPDATA. No ffmpeg is on this PATH.
    std::env::set_var("XDG_CONFIG_HOME", &home);
    std::env::set_var("APPDATA", &home);
    std::env::set_var("PATH", &tools);
    let seg = |kind: &str| format!(r#"<SegmentList timescale="1000" duration="2000"><Initialization sourceURL="{kind}init.mp4"/><SegmentURL media="{kind}1.m4s"/><SegmentURL media="{kind}2.m4s"/></SegmentList>"#);
    let mpd = format!(
        r#"<?xml version="1.0"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT4S" minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-live:2011"><Period>
<AdaptationSet mimeType="video/mp4" contentType="video"><Representation id="v1" codecs="avc1.4d401f" width="640" height="360" bandwidth="500000">{}</Representation></AdaptationSet>
<AdaptationSet mimeType="audio/mp4" contentType="audio" lang="en"><Representation id="a1" codecs="mp4a.40.2" audioSamplingRate="48000" bandwidth="128000">{}</Representation></AdaptationSet>
</Period></MPD>"#,
        seg("v"),
        seg("a")
    );
    let mut files: Vec<(String, Vec<u8>, &str)> = vec![
        ("clip.mpd".into(), mpd.into_bytes(), "application/dash+xml"),
        ("clip.m3u8".into(), b"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"vinit.mp4\"\n#EXTINF:2,\nv1.m4s\n#EXTINF:2,\nv2.m4s\n#EXT-X-ENDLIST\n".to_vec(), "application/vnd.apple.mpegurl"),
        ("vinit.mp4".into(), init(1, 1000), "video/mp4"),
        ("v1.m4s".into(), frag(1, 0, b'v'), "video/mp4"),
        ("v2.m4s".into(), frag(1, 2000, b'w'), "video/mp4"),
        ("ainit.mp4".into(), init(1, 48000), "audio/mp4"),
        ("a1.m4s".into(), frag(1, 0, b'a'), "audio/mp4"),
        ("a2.m4s".into(), frag(1, 96000, b'b'), "audio/mp4"),
        ("movie.mp4".into(), [init(1, 1000), frag(1, 0, b'm')].concat(), "video/mp4"),
    ];
    let movie = [init(1, 1000), frag(1, 0, b'm')].concat();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
    let rss = format!(r#"<?xml version="1.0"?><rss version="2.0"><channel><title>NDM test playlist</title>
      <item><title>First</title><enclosure url="{base}movie.mp4" type="video/mp4"/></item>
      <item><title>Second</title><enclosure url="{base}clip.m3u8" type="application/vnd.apple.mpegurl"/></item>
    </channel></rss>"#);
    files.push(("feed.xml".into(), rss.into_bytes(), "application/rss+xml"));
    let files = std::sync::Arc::new(files);
    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let maximum = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peak = maximum.clone();
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            let (files, active, maximum) = (files.clone(), active.clone(), maximum.clone());
            std::thread::spawn(move || {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
            let path = line.split_whitespace().nth(1).unwrap_or("").trim_start_matches('/').split('?').next().unwrap_or("").to_string();
            let fragment = path.ends_with(".m4s");
            if fragment {
                let live = active.fetch_add(1, Relaxed) + 1;
                maximum.fetch_max(live, Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            let mut c = c;
            match files.iter().find(|f| f.0 == path) {
                Some((_, body, ct)) => {
                    let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = c.write_all(body);
                }
                None => {
                    let _ = write!(c, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
            if fragment { active.fetch_sub(1, Relaxed); }
            });
        }
    });
    assert!(ffmpeg().is_none(), "ffmpeg hidden");
    let dir = home.join("Video");
    let job = Job::new(crate::engine::Info { url: format!("{base}clip.mpd"), folder: dir.display().to_string(), name: "clip.mp4".into(), conns: 4, ..Default::default() });
    assert_eq!(run(&job, &crate::transport::client::agent("", "ndm-test"), &Limiter::default()), Ok(true));
    let d = std::fs::read(dir.join("clip.mp4")).unwrap();
    let top = crate::stream::fmp4::boxes(&d, 0, d.len());
    let moov = top.iter().find(|b| &b.3 == b"moov").copied().expect("one MP4");
    assert!(crate::stream::fmp4::find(&d, moov, &[b"trak"]).is_some());
    let traks = crate::stream::fmp4::boxes(&d, moov.1, moov.2).iter().filter(|b| &b.3 == b"trak").count();
    assert_eq!(traks, 2, "video and audio in one file");
    let order: Vec<u8> = top.iter().filter(|b| &b.3 == b"mdat").map(|b| d[b.1]).collect();
    assert_eq!(order, *b"vawb");
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(left, ["clip.mp4"], "the separate streams are cleaned up");
    assert_eq!(job.info().size, Some(d.len() as u64));
    assert!(peak.load(Relaxed) > 2, "DASH fragments and companion streams fetched concurrently");
    // HLS uses the same concurrent fragment path, preserving the assembled fragment order.
    peak.store(0, Relaxed);
    let job = Job::new(Info { url: format!("{base}clip.m3u8"), folder: dir.display().to_string(), name: "hls.mp4".into(), conns: 4, ..Default::default() });
    assert_eq!(run(&job, &crate::transport::client::agent("", "ndm-test"), &Limiter::default()), Ok(true));
    assert_eq!(std::fs::read(dir.join("hls.mp4")).unwrap(), [init(1, 1000), frag(1, 0, b'v'), frag(1, 2000, b'w')].concat());
    assert!(peak.load(Relaxed) > 1, "HLS fragments fetched concurrently");
    // a site with one file that has both: no separate streams, so that file
    let job = Job::new(crate::engine::Info { url: format!("{base}movie.mp4"), folder: dir.display().to_string(), name: "movie.mp4".into(), ..Default::default() });
    assert_eq!(run(&job, &crate::transport::client::agent("", "ndm-test"), &Limiter::default()), Ok(true));
    assert_eq!(std::fs::read(dir.join("movie.mp4")).unwrap(), movie);
    // Listing uses the real flat extractor, then the selected entry is extracted on its own.
    let agent = crate::transport::client::agent("", "ndm-test");
    let lookup = Job::new(Info::default());
    let playlist = list_playlist(&lookup, &agent, &format!("{base}feed.xml"), "").unwrap();
    assert_eq!(playlist.entries.len(), 2);
    assert_eq!(playlist.entries[0].title, "First");
    assert_eq!(playlist.entries[1].url, format!("{base}clip.m3u8"));
    let selected = Job::new(Info {
        id: 73, url: playlist.entries[0].url.clone(), folder: dir.display().to_string(),
        ytdlp: true, yt: crate::engine::YtOpts { playlist_entry: true, ..Default::default() }, ..Default::default()
    });
    assert_eq!(run(&selected, &agent, &Limiter::default()), Ok(true));
    assert!(selected.info().name.contains("[NDM 73]"));
    assert_eq!(std::fs::read(selected.info().path()).unwrap(), movie);
    // and yt-dlp's own error when there is nothing
    let job = Job::new(crate::engine::Info { url: format!("{base}gone.mp4"), folder: dir.display().to_string(), name: "gone.mp4".into(), ..Default::default() });
    let e = run(&job, &crate::transport::client::agent("", "ndm-test"), &Limiter::default()).unwrap_err();
    assert!(e.contains("404"), "{e}");
    // An offline merge retry uses the completed tracks before starting the extractor.
    let retry = Job::new(Info { id: 42, url: "http://127.0.0.1:1/offline".into(),
        folder: dir.display().to_string(), name: "retry.mp4".into(), ..Default::default() });
    let (video, audio) = (dir.join("retry.ndm-video.mp4"), dir.join("retry.ndm-audio.m4a"));
    std::fs::write(&video, [init(1, 1000), frag(1, 0, b'v')].concat()).unwrap();
    std::fs::write(&audio, [init(1, 48000), frag(1, 0, b'a')].concat()).unwrap();
    let dest = retry.info().path();
    record_tracks(&retry, &dest, &video, &audio).unwrap();
    assert_eq!(run(&retry, &crate::transport::client::agent("", "ndm-test"), &Limiter::default()), Ok(true));
    assert!(dest.is_file() && !video.exists() && !audio.exists() && !receipt_path(&dest).exists());
    let _ = std::fs::remove_dir_all(&home);
}
/// File Info's choices with the real yt-dlp and ffmpeg on a page with a video, its poster and its
/// subtitles: the sound alone as an MP3, the subtitles and the thumbnail next to it.
#[test]
#[ignore = "needs the real yt-dlp and ffmpeg: NDM_TEST_YTDLP=/path/to/yt-dlp cargo test real_ytdlp -- --ignored"]
fn real_ytdlp_audio_and_extras() {
    use std::io::{BufRead, BufReader, Write};
    let bin = std::env::var("NDM_TEST_YTDLP").expect("NDM_TEST_YTDLP");
    let system_ff = crate::media::toolchain().expect("FFmpeg and ffprobe must be installed");
    let home = std::env::temp_dir().join(format!("ndm-real-ytdlp-audio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let tools = home.join("NewDownloadManager/tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::copy(&bin, tools.join(exe_file("yt-dlp"))).unwrap();
    let clip = home.join("clip.mp4");
    assert!(command(&system_ff)
        .args(["-v", "error", "-f", "lavfi", "-i", "color=black:size=1920x1080:rate=10", "-f", "lavfi", "-i", "sine=frequency=440", "-t", "2", "-c:v", "mpeg4", "-c:a", "aac", "-shortest"])
        .arg(&clip)
        .status()
        .unwrap()
        .success());
    let jpeg = home.join("poster.jpg");
    assert!(command(&system_ff).args(["-v", "error", "-f", "lavfi", "-i", "color=red:size=64x48", "-frames:v", "1"]).arg(&jpeg).status().unwrap().success());
    std::env::set_var("XDG_CONFIG_HOME", &home);
    std::env::set_var("APPDATA", &home);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let ff_dir = std::env::split_paths(&path).find(|d| d.join(exe_file("ffmpeg")).is_file()).expect("ffmpeg's folder");
    std::env::set_var("PATH", std::env::join_paths([tools.clone(), ff_dir]).unwrap());
    let page = b"<html><head><title>Clip</title></head><body><video poster=\"poster.jpg\" controls><source src=\"clip.mp4\" type=\"video/mp4\"><track kind=\"subtitles\" srclang=\"en\" src=\"clip.en.vtt\"></video></body></html>".to_vec();
    let media = home.join("720.m3u8");
    assert!(command(&system_ff).args(["-v", "error", "-i"]).arg(&clip)
        .args(["-vf", "scale=1280:720", "-c:v", "libx264", "-threads", "1", "-c:a", "copy", "-f", "hls", "-hls_playlist_type", "vod", "-hls_segment_filename"])
        .arg(home.join("720-%d.ts")).arg(&media).status().unwrap().success());
    let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\"\n720.m3u8\n";
    let files: Vec<(&str, Vec<u8>, &str)> = vec![
        ("page.html", page, "text/html"),
        ("master.m3u8", master.to_vec(), "application/vnd.apple.mpegurl"),
        ("720.m3u8", std::fs::read(&media).unwrap(), "application/vnd.apple.mpegurl"),
        ("720-0.ts", std::fs::read(home.join("720-0.ts")).unwrap(), "video/mp2t"),
        ("clip.mp4", std::fs::read(&clip).unwrap(), "video/mp4"),
        ("poster.jpg", std::fs::read(&jpeg).unwrap(), "image/jpeg"),
        ("clip.en.vtt", b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n".to_vec(), "text/vtt"),
    ];
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
    let files = std::sync::Arc::new(files);
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            let files = files.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line.split_whitespace().nth(1).unwrap_or("").trim_start_matches('/').split('?').next().unwrap_or("").to_string();
                let mut c = c;
                match files.iter().find(|f| f.0 == path) {
                    Some((_, body, ct)) => {
                        let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        let _ = c.write_all(body);
                    }
                    None => {
                        let _ = write!(c, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            });
        }
    });
    let dir = home.join("Music");
    let yt = crate::engine::YtOpts { quality: "mp3".into(), subs: true, thumb: true, ..Default::default() };
    let job = Job::new(Info { url: format!("{base}page.html"), folder: dir.display().to_string(), name: "song.mp3".into(), ytdlp: true, yt, ..Default::default() });
    assert_eq!(run(&job, &crate::transport::client::agent("", "ndm-test"), &Limiter::default()), Ok(true));
    let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    left.sort();
    assert_eq!(left, ["song.en.vtt", "song.jpg", "song.mp3"]);
    let probe = command(&beside_or_path("ffprobe")).args(["-v", "error", "-show_entries", "stream=codec_name,codec_type", "-of", "csv=p=0"]).arg(dir.join("song.mp3")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "mp3,audio");
    assert_eq!(job.info().size, Some(std::fs::metadata(dir.join("song.mp3")).unwrap().len()));
    let videos = home.join("Video");
    let video_job = |page, name, quality: &str| Job::new(Info {
        url: format!("{base}{page}"), folder: videos.display().to_string(), name,
        ytdlp: true, yt: crate::engine::YtOpts { quality: quality.into(), ..Default::default() }, ..Default::default()
    });
    let agent = crate::transport::client::agent("", "ndm-test");
    let unknown = video_job("page.html", "unknown.mp4".into(), "720");
    assert_eq!(run(&unknown, &agent, &Limiter::default()).unwrap_err(),
        "NDM could not verify a video format at or below 720p. Choose Best to download without a resolution limit.");
    assert!(!unknown.info().path().exists());

    let best = video_job("page.html", "best.mp4".into(), "");
    assert_eq!(run(&best, &agent, &Limiter::default()), Ok(true));
    assert_eq!(std::fs::read(best.info().path()).unwrap(), std::fs::read(&clip).unwrap());

    let known = video_job("master.m3u8", "known.mp4".into(), "720");
    assert_eq!(run(&known, &agent, &Limiter::default()), Ok(true));
    let height = command(&beside_or_path("ffprobe"))
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=height", "-of", "csv=p=0"])
        .arg(known.info().path()).output().unwrap();
    assert!(height.status.success());
    assert_eq!(String::from_utf8_lossy(&height.stdout).lines().next(), Some("720"));
    let _ = std::fs::remove_dir_all(&home);
}
/// Deno's real release zip (NDM_TEST_DENO_ZIP=<path>, NDM_TEST_DENO_SHA256=<the program's SHA-256>).
#[test]
#[ignore = "needs Deno's release zip"]
fn real_deno_zip() {
    use sha2::{Digest, Sha256};
    let zip = std::fs::read(std::env::var("NDM_TEST_DENO_ZIP").unwrap()).unwrap();
    let name = std::env::var("NDM_TEST_DENO_NAME").unwrap_or("deno".into());
    let prog = unzip_one(&zip, &name).unwrap();
    let sum = Sha256::digest(&prog).iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(sum, std::env::var("NDM_TEST_DENO_SHA256").unwrap());
}
/// The video and audio streams fetched by NDM's engine at once, each over several connections; the
/// parent's bar only moves forward against a total that stays put.
#[test]
fn ndm_fetches_both_streams() {
    use std::sync::Arc;
    let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 251) as u8).collect();
    let base = crate::test_support::serve(Arc::new(data.clone()));
    let dir = std::env::temp_dir().join(format!("ndm-direct-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let job = Job::new(Info { conns: 4, folder: dir.display().to_string(), name: "x.mp4".into(), ..Default::default() });
    let stream = |id: &str, path: &str, ext: &str| Stream { id: id.into(), url: format!("{base}{path}"), ext: ext.into(), size: data.len() as u64, headers: vec![("User-Agent".into(), "ndm-test/1".into())], direct: true };
    let streams = [stream("137", "rslow-v.mp4", "mp4"), stream("140", "r-a.m4a", "m4a")];
    let outs = [dir.join("x.ndm-video.mp4"), dir.join("x.ndm-audio.m4a")];
    let agent = crate::transport::client::agent("", "ndm-test");
    let done = std::sync::atomic::AtomicBool::new(false);
    let (ok, samples) = std::thread::scope(|sc| {
        let watch = sc.spawn(|| {
            let mut v = Vec::new();
            while !done.load(Relaxed) {
                let i = job.info();
                v.push((i.done(), i.size, i.segs.iter().filter(|s| s.owned).count()));
                drop(i);
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            v
        });
        let ok = fetch_direct(&job, &agent, &Limiter::default(), &streams, &outs);
        done.store(true, Relaxed);
        (ok, watch.join().unwrap())
    });
    assert_eq!(ok, Ok(true));
    for out in &outs {
        assert_eq!(std::fs::read(out).unwrap(), data, "{out:?}");
    }
    assert!(samples.windows(2).all(|w| w[1].0 >= w[0].0), "the bar never goes back");
    let total = 2 * data.len() as u64;
    // The observer may run before fetch_streams has initialized the total. Once known it stays put.
    assert!(samples.iter().any(|s| s.1 == Some(total)));
    assert!(samples.iter().skip_while(|s| s.1.is_none()).all(|s| s.1 == Some(total)), "the total stays put: {:?}", samples.iter().map(|s| s.1).collect::<Vec<_>>());
    assert!(samples.iter().map(|s| s.2).max().unwrap() > 2, "several connections at once");
    assert_eq!(job.info().done(), total);
    let _ = std::fs::remove_dir_all(&dir);
}
/// A page through the real yt-dlp (NDM_TEST_YTDLP), as the app downloads it: NDM_TEST_PAGE, into a
/// temporary folder; ffmpeg on PATH unless NDM_TEST_NO_FFMPEG.
#[cfg(unix)]
#[test]
#[ignore = "needs the real yt-dlp and a page: NDM_TEST_YTDLP=... NDM_TEST_PAGE=... cargo test real_ytdlp_page -- --ignored"]
fn real_ytdlp_page() {
    let bin = std::env::var("NDM_TEST_YTDLP").expect("NDM_TEST_YTDLP");
    let home = std::env::temp_dir().join(format!("ndm-page-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let tools = home.join("NewDownloadManager/tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::copy(&bin, tools.join("yt-dlp")).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &home);
    if std::env::var("NDM_TEST_NO_FFMPEG").is_ok() {
        std::env::set_var("PATH", "/nonexistent");
    }
    let dir = home.join("Video");
    let job = Job::new(crate::engine::Info { url: std::env::var("NDM_TEST_PAGE").unwrap(), folder: dir.display().to_string(), conns: 4, ..Default::default() });
    let t = std::time::Instant::now();
    let r = run(&job, &crate::transport::client::agent("", "ndm-test"), &Limiter::default());
    eprintln!("{r:?} in {:?}: {:?}", t.elapsed(), std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| (e.file_name(), e.metadata().map(|m| m.len()).unwrap_or(0))).collect::<Vec<_>>()));
    assert_eq!(r, Ok(true));
}
#[test]
fn formats_per_site() {
    let tiktok = formats("https://www.tiktok.com/@a/video/1", false);
    assert!(tiktok.starts_with("b[vcodec^=h264]/"), "{tiktok}");
    assert!(formats("https://m.douyin.com/video/1", true).starts_with("b[vcodec^=h264]/"));
    assert_eq!(formats("https://www.youtube.com/watch?v=x", false), PAIR);
    assert_eq!(formats("https://www.youtube.com/watch?v=x", true), MERGED);
    assert_eq!(formats("https://vimeo.com/1", true), MERGED);
    assert_eq!(formats("https://vimeo.com/1", false), format!("b/{PAIR}"));
}
/// File Info's Quality: a strict height cap, audio-only selection, and the corresponding filename.
#[test]
fn quality_choices() {
    assert_eq!(chosen("https://vimeo.com/1", true, ""), MERGED);
    assert_eq!(chosen("https://vimeo.com/1", true, "720"), "bv*[height<=720]+ba/b[height<=720]/bv[height<=720]");
    let yt = chosen("https://www.youtube.com/watch?v=x", false, "1080");
    assert!(yt.starts_with("bv*[height<=1080][acodec=none][ext=mp4]+ba[vcodec=none][ext=m4a]/"), "{yt}");
    assert!(!yt.contains("/b/"), "{yt}");
    let tiktok = chosen("https://www.tiktok.com/@a/video/1", true, "480");
    assert!(tiktok.starts_with("b[height<=480][vcodec^=h264]/"), "{tiktok}");
    assert_eq!(chosen("https://www.youtube.com/watch?v=x", true, "mp3"), "ba[ext=m4a]/ba/b");
    assert_eq!(chosen("https://www.youtube.com/watch?v=x", true, "bogus"), MERGED);
    assert_eq!(named("Song.mp4", "mp3"), "Song.mp3");
    assert_eq!(named("Song.webm", "m4a"), "Song.m4a");
    assert_eq!(named("Song.mp3", ""), "Song.mp4");
    assert_eq!(named("Song.webm", "720"), "Song.webm");
}

/// TikTok's streams are on tiktok.com with no file type in the address: one yt-dlp found is fetched
/// as it is, not looked up again (which went on forever, the download stuck at "Connecting...").
#[test]
fn found_streams_stay_plain() {
    let cdn = "https://v16-webapp-prime.tiktok.com/video/tos/useast2a/tos-useast2a-pve-0068/oQ/?a=1988&mime_type=video_mp4&tk=tt_chain_token";
    assert!(is_video_page(cdn), "on the site, no file type");
    assert!(wants(&Info { url: cdn.into(), ..Default::default() }));
    assert!(!wants(&Info { url: cdn.into(), plain: true, ..Default::default() }));
    assert!(!wants(&Info { url: cdn.into(), plain: true, ytdlp: true, ..Default::default() }));
}
/// Stop ends a yt-dlp look-up at once instead of waiting for it.
#[cfg(unix)]
#[test]
fn lookup_stops() {
    let job = Job::new(Info::default());
    job.stop();
    let mut c = Command::new("sleep");
    c.arg("30");
    let t = std::time::Instant::now();
    assert!(output_unless_stopped(&job, c).unwrap().is_none());
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
    let mut c = Command::new("echo");
    c.arg("hi");
    let out = output_unless_stopped(&Job::new(Info::default()), c).unwrap().unwrap();
    assert_eq!(out.stdout, b"hi\n");
}
/// The real ffmpeg download (Downloads -> Set yt-dlp) for this system, from its real release, into a
/// tools folder of its own, with no other ffmpeg around: it is found and runs.
#[test]
#[ignore = "downloads ffmpeg (100+ MB): cargo test real_ffmpeg -- --ignored"]
fn real_ffmpeg_install() {
    let home = std::env::temp_dir().join(format!("ndm-real-ffmpeg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::env::set_var("XDG_CONFIG_HOME", &home);
    std::env::set_var("APPDATA", &home);
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", home.join("none"));
    assert!(ffmpeg().is_none(), "no other ffmpeg");
    let dir = tools_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let (got, total) = (AtomicU64::new(0), AtomicU64::new(0));
    fetch_ffmpeg(&crate::transport::client::agent("", "ndm-test"), &dir, &got, &total).unwrap();
    assert!(got.load(Relaxed) > 10_000_000, "{} bytes", got.load(Relaxed));
    let ff = ffmpeg().expect("NDM's ffmpeg is found");
    assert!(ff.starts_with(&dir), "{ff:?}");
    let out = command(&ff).arg("-version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("ffmpeg version"), "{text}");
    eprintln!("{}", text.lines().next().unwrap_or(""));
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert!(left.iter().all(|f| !f.ends_with(".part")), "{left:?}");
    // a second time: a working ffmpeg stays as it is, nothing is downloaded
    let again = AtomicU64::new(0);
    fetch_ffmpeg(&crate::transport::client::agent("", "ndm-test"), &dir, &again, &total).unwrap();
    assert_eq!(again.load(Relaxed), 0);
    let _ = std::fs::remove_dir_all(&home);
}
/// A real ffmpeg release file already on disk (NDM_TEST_FFMPEG_ARCHIVE=<path>, NDM_TEST_FFMPEG_FILE=<its
/// release name>) unpacked as Set yt-dlp does it.
#[test]
#[ignore = "needs an ffmpeg release file"]
fn real_ffmpeg_unpack() {
    let archive = PathBuf::from(std::env::var("NDM_TEST_FFMPEG_ARCHIVE").unwrap());
    let file = std::env::var("NDM_TEST_FFMPEG_FILE").unwrap();
    let dir = std::env::temp_dir().join(format!("ndm-ffmpeg-unpack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = std::time::Instant::now();
    unpack_ffmpeg(&archive, &file, &dir).unwrap();
    let out = command(&dir.join(exe_file("ffmpeg"))).arg("-version").output().unwrap();
    eprintln!("{} in {:?}", String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or(""), t.elapsed());
    assert!(dir.join(exe_file("ffprobe")).is_file());
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(left.len(), 2, "{left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
