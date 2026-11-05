use super::download::with_post;
use super::fmp4::{be32, boxes, find, join_files, mkbox, tfdt_of, traks, Bx};
use super::*;
use crate::test_support::{frag, init, mk};

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::Arc;

fn fake<'a>(
    files: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Result<(String, String), String> + 'a {
    move |u: &str| {
        files
            .iter()
            .find(|f| f.0 == u)
            .map(|f| (u.to_string(), f.1.to_string()))
            .ok_or(format!("404 {u}"))
    }
}

#[test]
fn hls_master_audio_keys_ranges() {
    let files = [
        ("http://h/m.m3u8", "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",DEFAULT=YES,URI=\"au/en.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS=\"avc1,mp4a\",AUDIO=\"a\"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000,AUDIO=\"a\",RESOLUTION=1920x1080\nhi/v.m3u8\n"),
        ("http://h/hi/v.m3u8", "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nseg1.m4s\n#EXT-X-KEY:METHOD=AES-128,URI=\"/k\"\n#EXT-X-BYTERANGE:100@0\n#EXTINF:4,\nall.m4s\n#EXT-X-BYTERANGE:50\nall.m4s\n#EXT-X-ENDLIST\n"),
        ("http://h/au/en.m3u8", "#EXTM3U\n#EXTINF:4,\na1.aac\n"),
    ];
    let t = plan(&fake(&files), "http://h/m.m3u8").unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!((t[0].ext.as_str(), t[1].ext.as_str()), ("mp4", "aac"));
    assert_eq!(t[0].init.as_ref().unwrap().url, "http://h/hi/init.mp4");
    let p = &t[0].parts;
    assert_eq!(
        (p[0].url.as_str(), p[0].key.is_none()),
        ("http://h/hi/seg1.m4s", true)
    );
    assert_eq!(p[1].range, Some((0, 99)));
    assert_eq!(p[2].range, Some((100, 149)));
    assert_eq!(
        p[2].key,
        Some(("http://h/k".into(), 9u128.to_be_bytes())),
        "IV = media sequence number"
    );
    assert_eq!(t[1].parts[0].url, "http://h/au/a1.aac");
    let drm = [(
        "http://h/d.m3u8",
        "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\nx.ts\n",
    )];
    assert_eq!(plan(&fake(&drm), "http://h/d.m3u8").unwrap_err(), PROTECTED);
}

#[test]
fn dash_periods_join() {
    let mpd = r#"<MPD mediaPresentationDuration="PT8S"><BaseURL>http://h/</BaseURL>
 <Period id="a" duration="PT4S"><AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
  <SegmentTemplate timescale="1000" duration="2000" initialization="a/init.mp4" media="a/$Number$.m4s"/></Representation></AdaptationSet></Period>
 <Period id="b"><AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
  <SegmentTemplate timescale="1000" duration="2000" presentationTimeOffset="90000" initialization="b/init.mp4" media="b/$Number$.m4s"/></Representation></AdaptationSet></Period></MPD>"#;
    let t = dash("http://h/x.mpd", mpd, 0.0).unwrap();
    assert_eq!(t.len(), 1);
    let urls: Vec<&str> = t[0].parts.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "http://h/a/1.m4s",
            "http://h/a/2.m4s",
            "http://h/b/init.mp4",
            "http://h/b/1.m4s",
            "http://h/b/2.m4s"
        ]
    );
    assert_eq!(
        (t[0].parts[0].shift, t[0].parts[3].shift),
        (0.0, 4.0 - 90.0)
    );
    // a period whose pieces carry on the same numbering: the shared piece is kept once
    let same = mpd
        .replace("b/init.mp4", "a/init.mp4")
        .replace("b/$Number$", "a/$Number$")
        .replace(
            r#"duration="2000" presentationTimeOffset"#,
            r#"duration="2000" startNumber="2" presentationTimeOffset"#,
        );
    let t = dash("http://h/x.mpd", &same, 0.0).unwrap();
    let urls: Vec<&str> = t[0].parts.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        urls,
        ["http://h/a/1.m4s", "http://h/a/2.m4s", "http://h/a/3.m4s"]
    );
}

#[test]
fn segmentbase_periods_keep_their_shift() {
    // two periods, one whole file each, fetched in two byte ranges: each file's fragments get its own shift
    let dir = std::env::temp_dir().join(format!("ndm_sb_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let file = |m: &[u8]| {
        [
            mkbox(b"ftyp", b"iso6"),
            mkbox(b"moov", b""),
            mkbox(b"moof", b""),
            mkbox(b"mdat", m),
        ]
        .concat()
    };
    let (a, b) = (file(b"A"), file(b"B"));
    let pieces = [
        (&a[..10], "a", 1.0),
        (&a[10..], "a", 1.0),
        (&b[..10], "b", 9.0),
        (&b[10..], "b", 9.0),
    ];
    let parts: Vec<Part> = pieces
        .iter()
        .map(|(_, u, s)| Part {
            url: u.to_string(),
            range: None,
            key: None,
            seq: 0,
            shift: *s,
        })
        .collect();
    for (k, (bytes, ..)) in pieces.iter().enumerate() {
        fs::write(dir.join(k.to_string()), bytes).unwrap();
    }
    let f = |k: usize| dir.join(k.to_string());
    let out: Vec<(Vec<u8>, f64)> = timed(true, &parts, &f)
        .map(|(u, s)| (u.unwrap(), s))
        .collect();
    let _ = fs::remove_dir_all(&dir);
    let shift_of = |m: &[u8]| {
        out.iter()
            .find(|(u, _)| u.windows(5).any(|w| w == [b"mdat", m].concat()))
            .map(|x| x.1)
    };
    assert_eq!((shift_of(b"A"), shift_of(b"B")), (Some(1.0), Some(9.0)));
}

#[test]
fn retime_moves_fragments() {
    let mdhd = mkbox(
        b"mdhd",
        &[&[0u8; 12][..], &1000u32.to_be_bytes(), &[0; 4]].concat(),
    );
    let init = [
        mkbox(b"ftyp", b"iso6"),
        mkbox(
            b"moov",
            &mkbox(
                b"trak",
                &[mkbox(b"tkhd", &[0u8; 24]), mkbox(b"mdia", &mdhd)].concat(),
            ),
        ),
    ]
    .concat();
    let frag = |t: u32| {
        let trun = mkbox(
            b"trun",
            &[&[0, 0, 0, 1][..], &1u32.to_be_bytes(), &77i32.to_be_bytes()].concat(),
        );
        let traf = mkbox(
            b"traf",
            &[
                mkbox(b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]),
                mkbox(b"tfdt", &[&[0u8; 4][..], &t.to_be_bytes()].concat()),
                trun,
            ]
            .concat(),
        );
        [mkbox(b"moof", &traf), mkbox(b"mdat", b"x")].concat()
    };
    // period 1 starts at media time 90 s; period 2 (its own init) lands 5,000,000 s later
    let items: Vec<(Result<Vec<u8>, String>, f64)> = vec![
        (Ok(init.clone()), -86.0),
        (Ok(frag(90_000)), -86.0),
        (Ok(init), 5_000_004.0),
        (Ok(frag(2000)), 5_000_004.0),
    ];
    let base = first_time(items.clone().into_iter()).unwrap();
    assert_eq!(base, 4.0);
    let out = retime(items.into_iter(), base)
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .concat();
    let top = boxes(&out, 0, out.len());
    assert_eq!(
        top.iter().filter(|b| &b.3 == b"moov").count(),
        1,
        "later init pieces left out"
    );
    let moofs: Vec<Bx> = top.iter().filter(|b| &b.3 == b"moof").copied().collect();
    assert_eq!(tfdt_of(&out, moofs[0]), Some(0));
    assert_eq!(tfdt_of(&out, moofs[1]), Some(5_000_002_000));
    assert_eq!(
        out[find(&out, moofs[1], &[b"traf", b"tfdt"]).unwrap().1],
        1,
        "version 1 for a 64-bit time"
    );
    let off = |m: Bx| {
        let r = find(&out, m, &[b"traf", b"trun"]).unwrap();
        i32::from_be_bytes(out[r.1 + 8..r.1 + 12].try_into().unwrap())
    };
    assert_eq!(
        (off(moofs[0]), off(moofs[1])),
        (77, 81),
        "data offsets follow the bigger moof"
    );
}

#[test]
fn dash_live_window() {
    let mpd = r#"<MPD type="dynamic" availabilityStartTime="2026-01-01T00:00:00Z" timeShiftBufferDepth="PT6S" minimumUpdatePeriod="PT2S">
 <Period start="PT0S"><AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
  <SegmentTemplate timescale="1000" duration="2000" startNumber="10" initialization="i.mp4" media="$Number$.m4s"/>
 </Representation></AdaptationSet></Period></MPD>"#;
    // 21 s in: pieces 0..=9 are finished; the 6 s window holds the last three
    let t = dash("http://h/l.mpd", mpd, 1_767_225_600.0 + 21.0).unwrap();
    assert_eq!(
        t[0].parts.iter().map(|p| p.seq).collect::<Vec<_>>(),
        [17, 18, 19]
    );
    assert_eq!(t[0].parts[2].url, "http://h/19.m4s");
    assert!(t[0].live && t[0].dash && t[0].target == 2.0 && t[0].src == "http://h/l.mpd");
    let t = dash("http://h/l.mpd", mpd, 1_767_225_600.0 + 1.0).unwrap();
    assert!(t[0].parts.is_empty(), "nothing finished yet");
}

/// A live DASH timeline: the second manifest adds a piece, the third is static and adds the last one.
#[test]
fn dash_live_follows_until_static() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    let fetches = Arc::new(AtomicUsize::new(0));
    let f2 = fetches.clone();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let fetches = f2.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line
                    .split(' ')
                    .nth(1)
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_string();
                let body = if path == "live.mpd" {
                    let n = fetches.fetch_add(1, Relaxed);
                    let (ty, t0) = match n {
                        0 => ("dynamic", 0),
                        1 => ("dynamic", 1000),
                        _ => ("static", 2000),
                    };
                    format!(
                        r#"<MPD type="{ty}" minimumUpdatePeriod="PT1S"><Period><AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1"><SegmentTemplate timescale="1000" initialization="init.mp4" media="seg$Time$.m4s"><SegmentTimeline><S t="{t0}" d="1000" r="2"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#
                    )
                } else {
                    path
                };
                let _ = (&s).write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });
    let dir = std::env::temp_dir().join(format!("ndm-test-dash-live-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(engine::Info {
        url: format!("{base}live.mpd"),
        folder: dir.display().to_string(),
        conns: 2,
        ..Default::default()
    });
    j.start(
        transport::client::agent("", ""),
        Arc::new(Limiter::default()),
    );
    while j.running() {
        thread::sleep(Duration::from_millis(20));
    }
    let i = j.info().clone();
    assert_eq!(i.status, Status::Complete);
    assert_eq!(i.name, "live.mp4");
    assert_eq!(
        fs::read_to_string(i.path()).unwrap(),
        "init.mp4seg0.m4sseg1000.m4sseg2000.m4sseg3000.m4sseg4000.m4s"
    );
    assert_eq!(fetches.load(Relaxed), 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dash_templates() {
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT0M9.5S">
 <BaseURL>media/</BaseURL>
 <Period>
  <AdaptationSet mimeType="video/mp4">
   <SegmentTemplate timescale="1000" initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/$Number%03d$.m4s" startNumber="5">
<SegmentTimeline><S t="0" d="4000" r="1"/><S d="1500"/></SegmentTimeline>
   </SegmentTemplate>
   <Representation id="v1" bandwidth="100"/>
   <Representation id="v2" bandwidth="900"/>
  </AdaptationSet>
  <AdaptationSet contentType="audio" mimeType="audio/mp4">
   <Representation id="a" bandwidth="64">
<SegmentTemplate duration="2" initialization="a-init.mp4" media="a-$Time$.m4s"/>
   </Representation>
  </AdaptationSet>
 </Period>
</MPD>"#;
    let t = plan(&fake(&[("http://h/x/s.mpd", mpd)]), "http://h/x/s.mpd").unwrap();
    assert_eq!(
        t[0].init.as_ref().unwrap().url,
        "http://h/x/media/v2/init.mp4"
    );
    let v: Vec<&str> = t[0].parts.iter().map(|p| p.url.as_str()).collect();
    assert_eq!(
        v,
        [
            "http://h/x/media/v2/005.m4s",
            "http://h/x/media/v2/006.m4s",
            "http://h/x/media/v2/007.m4s"
        ]
    );
    assert_eq!(t[1].parts.len(), 5, "ceil(9.5 s / 2 s)");
    assert_eq!(t[1].parts[1].url, "http://h/x/media/a-2.m4s");
    assert_eq!((t[0].ext.as_str(), t[1].ext.as_str()), ("mp4", "m4a"));
    assert_eq!(with_ext("My video.m3u8", "ts"), "My video.ts");
    assert_eq!(with_ext("Ep. 5", "mp4"), "Ep. 5.mp4");
}

#[test]
fn mux_interleaves_fragments() {
    let v = [
        init(1, 1000),
        frag(1, 0, b'v'),
        [frag(1, 2000, b'w'), frag(1, 4000, b'x')].concat(),
    ];
    let a = [init(1, 48000), frag(1, 0, b'a'), frag(1, 96000, b'b')];
    let out = std::env::temp_dir().join(format!("ndm-mux-{}.mp4", std::process::id()));
    assert!(mux_mp4(&mut v.into_iter().map(Ok), &mut a.into_iter().map(Ok), &out).unwrap());
    let d = fs::read(&out).unwrap();
    let _ = fs::remove_file(&out);
    let top = boxes(&d, 0, d.len());
    let moov = top.iter().find(|b| &b.3 == b"moov").copied().unwrap();
    let ids: Vec<u32> = traks(&d, moov).iter().map(|t| t.1).collect();
    assert_eq!(ids, [1, 2]);
    let mvex = find(&d, moov, &[b"mvex"]).unwrap();
    assert_eq!(boxes(&d, mvex.1, mvex.2).len(), 2, "two trex");
    let mvhd = find(&d, moov, &[b"mvhd"]).unwrap();
    assert_eq!(be32(&d, mvhd.2 - 4), 3);
    let order: Vec<u8> = top
        .iter()
        .filter(|b| &b.3 == b"mdat")
        .map(|b| d[b.1])
        .collect();
    assert_eq!(order, *b"vawbx");
    let tfhd_ids: Vec<u32> = top
        .iter()
        .filter(|b| &b.3 == b"moof")
        .map(|b| be32(&d, find(&d, *b, &[b"traf", b"tfhd"]).unwrap().1 + 4))
        .collect();
    assert_eq!(tfhd_ids, [1, 2, 1, 2, 1]);
    assert!(!top.iter().any(|b| &b.3 == b"styp"));
}

/// The bar during processing: the output growing towards the expected size, cleared when done.
#[test]
fn processing_shows_progress() {
    let dir = std::env::temp_dir().join(format!("ndm-post-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.bin");
    let job = Job::new(crate::engine::Info::default());
    let seen = Mutex::new(Vec::new());
    let r = with_post(
        &job,
        "Joining the pieces...",
        std::slice::from_ref(&out),
        1000,
        || {
            for k in 1..=4 {
                fs::write(&out, vec![0u8; k * 250]).unwrap();
                thread::sleep(Duration::from_millis(400));
                seen.lock().unwrap().push(job.info().post.clone());
            }
            7
        },
    );
    assert_eq!(r, 7);
    let seen = seen.into_inner().unwrap();
    assert!(
        seen.iter().all(|p| p
            .as_ref()
            .is_some_and(|(l, _)| l == "Joining the pieces...")),
        "{seen:?}"
    );
    let fr: Vec<f32> = seen.iter().map(|p| p.as_ref().unwrap().1).collect();
    assert!(
        fr[0] >= 0.2 && fr[0] <= 0.3 && fr[2] >= 0.7 && fr[3] <= 0.99,
        "{fr:?}"
    );
    assert!(job.info().post.is_none(), "cleared when done");
    let _ = fs::remove_dir_all(&dir);
}

/// yt-dlp's separate YouTube streams: whole fragmented-MP4 files (with a sidx), joined into one MP4.
#[test]
fn joins_whole_files() {
    let dir = std::env::temp_dir().join(format!("ndm-join-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let sidx = mk(b"sidx", &[&[0; 12]]);
    let v = [
        init(1, 1000),
        sidx.clone(),
        frag(1, 0, b'v'),
        frag(1, 2000, b'w'),
    ]
    .concat();
    let a = [init(1, 48000), sidx, frag(1, 0, b'a'), frag(1, 96000, b'b')].concat();
    let (vp, ap, out) = (
        dir.join("x.f137.mp4"),
        dir.join("x.f140.m4a"),
        dir.join("x.mp4"),
    );
    fs::write(&vp, v).unwrap();
    fs::write(&ap, a).unwrap();
    assert!(join_files(&vp, &ap, &out).unwrap());
    let d = fs::read(&out).unwrap();
    let top = boxes(&d, 0, d.len());
    let moov = top.iter().find(|b| &b.3 == b"moov").copied().unwrap();
    assert_eq!(traks(&d, moov).len(), 2);
    let order: Vec<u8> = top
        .iter()
        .filter(|b| &b.3 == b"mdat")
        .map(|b| d[b.1])
        .collect();
    assert_eq!(order, *b"vawb");
    // a webm with an mp4 isn't a pair
    assert!(!join_files(&vp, &dir.join("x.webm"), &out).unwrap());
    let _ = fs::remove_dir_all(&dir);
}

fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    let files = Arc::new(files);
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let files = files.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line
                    .split(' ')
                    .nth(1)
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_string();
                let mut w = &s;
                match files.iter().find(|f| f.0 == path) {
                    Some((_, body)) => {
                        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                        let _ = w.write_all(body);
                    }
                    None => {
                        let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            });
        }
    });
    base
}

/// End to end through the engine: an HLS playlist whose last piece is AES-128 encrypted
/// (NIST SP 800-38A F.2.1 vector).
#[test]
fn hls_download_decrypts_and_joins() {
    let hex = |s: &str| {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect::<Vec<u8>>()
    };
    let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
    let plain = hex("6bc1bee22e409f96e93d7e117393172a");
    let list = "#EXTM3U\n#EXTINF:2,\ns1.ts\n#EXTINF:2,\ns2.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXTINF:2,\ns3.ts\n#EXT-X-ENDLIST\n";
    let base = serve(vec![
        ("v.m3u8", list.into()),
        ("s1.ts", vec![1; 70_000]),
        ("s2.ts", vec![2; 5]),
        ("s3.ts", hex("7649abac8119b246cee98e9b12e9197d")),
        ("k.bin", key),
    ]);
    let dir = std::env::temp_dir().join(format!("ndm-test-hls-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let j = Job::new(engine::Info {
        url: format!("{base}v.m3u8"),
        folder: dir.display().to_string(),
        conns: 2,
        ..Default::default()
    });
    j.start(
        transport::client::agent("", ""),
        Arc::new(Limiter::default()),
    );
    while j.running() {
        thread::sleep(Duration::from_millis(20));
    }
    let i = j.info().clone();
    assert_eq!(i.status, Status::Complete);
    assert_eq!(i.name, "v.ts");
    assert_eq!(
        fs::read(i.path()).unwrap(),
        [vec![1; 70_000], vec![2; 5], plain].concat()
    );
    assert!(!i.part().exists(), "pieces removed");
    let _ = fs::remove_dir_all(&dir);
}

/// A live playlist (no ENDLIST) at an address without .m3u8, known only by its Content-Type: the
/// first fetch lists two pieces, the next adds a third and ends the stream.
#[test]
fn live_playlist_by_content_type() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    let fetches = Arc::new(AtomicUsize::new(0));
    let f2 = fetches.clone();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let fetches = f2.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                while r.read_line(&mut String::new()).unwrap_or(0) > 2 {}
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let (ct, body) = if path == "/play" {
                    let n = fetches.fetch_add(1, Relaxed);
                    let mut b = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:5\n#EXTINF:1,\ns5.ts\n#EXTINF:1,\ns6.ts\n".to_string();
                    if n >= 4 {
                        // fetches: probe, the engine's first request, the plan, one live poll
                        b += "#EXTINF:1,\ns7.ts\n#EXT-X-ENDLIST\n";
                    }
                    ("application/vnd.apple.mpegurl", b.into_bytes())
                } else {
                    (
                        "video/mp2t",
                        path.trim_start_matches('/').repeat(3).into_bytes(),
                    )
                };
                let mut w = &s;
                let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = w.write_all(&body);
            });
        }
    });
    let dir = std::env::temp_dir().join(format!("ndm-test-live-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let p = engine::probe::probe(
        &transport::client::agent("", ""),
        &format!("{base}play"),
        "",
        "",
    )
    .unwrap();
    assert!(p.stream && p.name == "play.mp4", "{}", p.name);
    let j = Job::new(engine::Info {
        url: format!("{base}play"),
        folder: dir.display().to_string(),
        conns: 2,
        ..Default::default()
    });
    j.start(
        transport::client::agent("", ""),
        Arc::new(Limiter::default()),
    );
    while j.running() {
        thread::sleep(Duration::from_millis(20));
    }
    let i = j.info().clone();
    assert_eq!(i.status, Status::Complete);
    assert!(i.stream);
    assert_eq!(i.name, "play.ts", "not MPEG-TS data, so kept as it is");
    assert_eq!(
        fs::read_to_string(i.path()).unwrap(),
        "s5.tss5.tss5.tss6.tss6.tss6.tss7.tss7.tss7.ts"
    );
    assert!(
        fetches.load(Relaxed) >= 5,
        "the live playlist was fetched again"
    );
    let _ = fs::remove_dir_all(&dir);
}
