use super::browser_bridge::{handle, proxy_conn, Incoming};
use super::queue::quota_bytes;
use super::settings::Category;
use super::state::Saved;
use super::*;

#[test]
fn localhost_rejects_web_origins_and_ambiguous_messages() {
    let parse = |headers: &str| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        write!(client, "POST /add HTTP/1.1\r\nX-NDM: 1\r\n{headers}\r\n").unwrap();
        handle(&listener.accept().unwrap().0)
    };
    assert!(parse("Origin: https://unrelated.example\r\n").is_none());
    assert!(parse("Origin: chrome-extension://unrelated\r\n").is_none());
    assert!(parse("Content-Length: 0\r\nContent-Length: 0\r\n").is_none());
    assert!(parse("Transfer-Encoding: chunked\r\n").is_none());
    assert!(parse("Origin: chrome-extension://fjkomnlijmdmohmlbfgkepgdieleinoa\r\n").is_some());
    assert!(parse("Origin: moz-extension://824f4499-377a-4127-8cc0-c11758bf683a\r\n").is_some());
}

/// Changes Windows' real proxy setting for a moment: run by hand (`cargo test -- --ignored`).
#[test]
#[ignore]
#[cfg(not(target_os = "macos"))]
fn integration_restores_system_proxy() {
    let core = Core::load();
    core.settings().saved_proxy = None;
    let before = crate::win::system_proxy();
    core.apply_integration(true);
    let (on, server, _) = crate::win::system_proxy();
    assert!(
        on && server.starts_with(&format!("http=127.0.0.1:{PROXY_PORT}")),
        "{server}"
    );
    let p = core.settings().proxy_for("http://x.com/a");
    assert_eq!(
        p,
        if before.0 {
            core.settings().proxy_for("http://x.com/a")
        } else {
            String::new()
        },
        "NDM's own downloads skip its proxy"
    );
    core.shutdown();
    assert_eq!(crate::win::system_proxy(), before);
}

#[test]
fn integration_proxy() {
    // a server with a page and a file; the proxy passes the page through and takes the file
    let srv = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = srv.local_addr().unwrap().port();
    thread::spawn(move || {
        for s in srv.incoming().flatten() {
            let mut r = BufReader::new(&s);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).unwrap() <= 2 {
                    break;
                }
            }
            let (ty, body) = if line.contains("/a.zip") {
                ("application/zip", "zipdata")
            } else {
                ("text/html", "<html>hi</html>")
            };
            let _ = (&s).write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    let core = Core::load();
    let px = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = px.local_addr().unwrap();
    let ask = |path: &str| {
        let c = std::net::TcpStream::connect(addr).unwrap();
        (&c).write_all(format!("GET http://127.0.0.1:{port}{path} HTTP/1.1\r\nHost: x\r\nUser-Agent: curl/8\r\nProxy-Connection: keep-alive\r\n\r\n").as_bytes()).unwrap();
        let inc = proxy_conn(&core, px.accept().unwrap().0);
        let mut got = String::new();
        BufReader::new(&c).read_to_string(&mut got).unwrap();
        (inc, got)
    };
    let (inc, got) = ask("/page.html");
    assert!(
        inc.is_none() && got.starts_with("HTTP/1.1 200") && got.ends_with("<html>hi</html>"),
        "{got}"
    );
    let (inc, got) = ask("/a.zip");
    assert!(got.starts_with("HTTP/1.0 204"), "{got}");
    assert_eq!(inc.unwrap().url, format!("http://127.0.0.1:{port}/a.zip"));
}

#[test]
fn site_logins() {
    let mut s = Settings::default();
    s.logins.push(Login {
        site: "https://files.example.com/".into(),
        user: "u".into(),
        pass: "p w".into(),
    });
    assert_eq!(
        s.with_login("http://files.example.com/a.zip"),
        "http://u:p%20w@files.example.com/a.zip"
    );
    assert_eq!(
        s.with_login("http://cdn.files.example.com/a.zip"),
        "http://u:p%20w@cdn.files.example.com/a.zip"
    );
    assert_eq!(
        s.with_login("http://other.com/a.zip"),
        "http://other.com/a.zip"
    );
    assert_eq!(
        s.with_login("http://x:y@files.example.com/a.zip"),
        "http://x:y@files.example.com/a.zip"
    );
}

/// A finished file whose name didn't say what it is (YouTube's "watch", a "get.php"): named, filed and
/// moved by its contents; a known one is left alone.
#[test]
fn finished_files_are_typed() {
    let dir = std::env::temp_dir().join(format!("ndm-classify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = Core::load();
    core.settings().base_dir = dir.display().to_string();
    core.settings().cat_dirs.clear();
    let job = |name: &str, bytes: &[u8]| {
        std::fs::write(dir.join(name), bytes).unwrap();
        engine::Job::new(engine::Info {
            name: name.into(),
            folder: dir.display().to_string(),
            category: GENERAL.into(),
            url: "https://x.com/watch?v=1".into(),
            ..Default::default()
        })
    };
    let mp4 = b"\0\0\0\x18ftypisom\0\0\0\0rest";
    let j = job("watch", mp4);
    assert!(core.classify(&j));
    let i = j.info().clone();
    assert_eq!(
        (i.name.as_str(), i.category.as_str()),
        ("watch.mp4", "Video")
    );
    assert_eq!(Path::new(&i.folder), dir.join("Video"));
    assert_eq!(std::fs::read(dir.join("Video/watch.mp4")).unwrap(), mp4);
    let j = job("get.php", b"%PDF-1.7 ...");
    assert!(core.classify(&j));
    assert_eq!(j.info().name, "get.pdf");
    let j = job("notes.zip", b"PK\x03\x04");
    assert!(!core.classify(&j), "a known type stays");
    let j = job("readme", b"plain text");
    assert!(!core.classify(&j), "unknown contents stay");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn categories_and_capture() {
    let mut s = Settings::default();
    assert_eq!(s.category_for("a.ZIP", ""), "Compressed");
    assert_eq!(
        s.category_for("movie.mkv", "http://x.com/movie.mkv"),
        "Video"
    );
    assert_eq!(s.category_for("noext", ""), GENERAL);
    s.categories.insert(
        0,
        Category {
            name: "Work".into(),
            exts: "zip".into(),
            sites_on: true,
            sites: "*.corp.com intranet".into(),
            ..Default::default()
        },
    );
    assert_eq!(
        s.category_for("a.zip", "https://files.corp.com/a.zip"),
        "Work"
    );
    assert_eq!(
        s.category_for("a.zip", "https://other.com/a.zip"),
        "Compressed"
    );
    let core = Core::load();
    let inc = |url: &str, mime: &str| Incoming {
        url: url.into(),
        mime: mime.into(),
        ..Default::default()
    };
    assert!(core.wants(&inc("https://x.com/a.zip", "")));
    assert!(!core.wants(&inc("https://x.com/a.html", "")));
    assert!(core.wants(&inc("https://x.com/get?id=1", "application/octet-stream")));
    assert!(!core.wants(&inc("https://x.com/get?id=1", "text/html")));
    assert!(!core.wants(&inc("blob:https://x.com/1", "")));
    core.settings().skip_urls = vec!["https://x.com/dl/*".into()];
    assert!(!core.wants(&inc("https://x.com/dl/a.zip", "")));
    assert!(
        core.wants(&Incoming {
            force: true,
            ..inc("https://x.com/dl/a.zip", "")
        }),
        "the context menu still downloads"
    );
    let edge_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 Edg/130.0";
    let chrome_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
    assert_eq!(
        (
            browser_kind(edge_ua),
            browser_kind(chrome_ua),
            browser_kind("Microsoft Edge")
        ),
        ("edge", "chrome", "edge")
    );
    core.settings().browsers = vec![
        Browser {
            name: "Microsoft Edge".into(),
            exe: r"C:\x\msedge.exe".into(),
            on: false,
        },
        Browser {
            name: "Google Chrome".into(),
            exe: r"C:\x\chrome.exe".into(),
            on: true,
        },
    ];
    assert!(!core.captures(edge_ua) && core.captures(chrome_ua) && core.captures("curl/8"));
}

#[test]
fn queues() {
    // a state file from before queues: its schedule becomes the main queue's
    let old = r#"{"settings":{"max_active":5,"sched":{"start_at":"03:00","on_startup":true}},"items":[{"id":1,"queued":true,"status":"Complete"}],"next_id":2}"#;
    let core = Core::from_saved(
        serde_json::from_str(old).unwrap(),
        std::env::temp_dir().join("ndm-test-unused.json"),
    );
    {
        let s = core.settings();
        assert_eq!(s.queues.len(), 2);
        assert!(
            s.queues[1].sync && s.queues[1].builtin(),
            "NDM's Synchronization queue is added"
        );
        assert_eq!(
            (
                s.queues[0].id,
                s.queues[0].max_active,
                s.queues[0].sched.start_at.as_str()
            ),
            (MAIN_QUEUE, 5, "03:00")
        );
    }
    assert!(core.queue_running(MAIN_QUEUE)); // "start on startup"
    core.tick_queue(); // only a finished file left: the queue finishes
    assert!(!core.queue_running(MAIN_QUEUE));
    assert_eq!(*core.queue_finished.lock().unwrap(), vec![MAIN_QUEUE]);

    let q = core.add_queue("");
    assert_eq!(core.settings().queue(q).unwrap().name, "Queue # 2");
    let mut i = core.new_info("http://x/a.bin");
    i.status = Status::Error("x".into());
    let id = i.id;
    let job = core.add(i);
    core.settings()
        .queues
        .iter_mut()
        .find(|queue| queue.id == q)
        .unwrap()
        .limit_kb = 50;
    core.start_queue(q);
    core.set_queue(&[id], Some(q));
    assert!(job.info().queued);
    assert_eq!(job.limiter.rate.load(Relaxed), 50 * 1024);
    core.set_job_limit(&job, 100);
    assert_eq!(job.limiter.rate.load(Relaxed), 50 * 1024);
    core.set_queue(&[id], None);
    assert_eq!(job.limiter.rate.load(Relaxed), 100 * 1024);
    core.set_queue(&[id], Some(q));
    core.delete_queue(MAIN_QUEUE); // not allowed
    core.delete_queue(1);
    core.delete_queue(q);
    assert_eq!(core.settings().queues.len(), 2);
    let i = core.find(id).unwrap().info().clone();
    assert!(!i.queued && i.queue == MAIN_QUEUE);

    // synchronization: a finished file stays queued; each pass checks it with the server and the
    // queue waits for the check, then starts again after "Start again every"
    // Keep the probe pending until the test observes it. Closing the listener releases the request.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut i = core.new_info(&format!("http://{}/a.bin", listener.local_addr().unwrap()));
    (i.status, i.size) = (Status::Complete, Some(1));
    let id = i.id;
    core.add(i);
    core.set_queue(&[id], Some(1));
    assert!(core.find(id).unwrap().info().queued);
    {
        let mut s = core.settings();
        let sc = &mut s.queues[1].sched;
        (sc.again_on, sc.again_h, sc.again_m) = (true, 0, 1);
    }
    core.start_queue(1);
    core.tick_queue();
    assert!(core.queue_running(1) && core.checking.lock().unwrap().contains(&id));
    drop(listener);
    let deadline = Instant::now() + Duration::from_secs(5);
    while core.checking.lock().unwrap().contains(&id) {
        assert!(
            Instant::now() < deadline,
            "synchronization probe did not finish"
        );
        thread::sleep(Duration::from_millis(20));
    }
    core.tick_queue();
    assert!(!core.queue_running(1));
    assert!(core.again.lock().unwrap().contains_key(&1));
}

#[test]
fn removed_view_options_preserve_settings() {
    // Old size/theme keys are ignored; the remaining style and custom scales still round-trip.
    for ndm_style in [false, true] {
        let legacy = serde_json::json!({
            "classic": true,
            "small_buttons": true,
            "ndm_style": ndm_style,
            "scale_toolbar": 0.75,
            "scale_tree": 1.25,
            "scale_text": 1.5,
            "tray_icon": 2,
            "tb_skin": "Sunset",
            "tb_layout": [6, -1, 0],
            "base_dir": "/downloads"
        });
        let settings: Settings = serde_json::from_value(legacy).unwrap();
        assert_eq!(settings.ndm_style, ndm_style);
        assert_eq!(settings.scale_toolbar, 0.75);
        assert_eq!(settings.scale_tree, 1.25);
        assert_eq!(settings.scale_text, 1.5);
        assert_eq!(settings.tray_icon, 2);
        assert_eq!(settings.tb_skin, "Sunset");
        assert_eq!(settings.tb_layout, [6, -1, 0]);
        assert_eq!(settings.base_dir, "/downloads");
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("classic").is_none());
        assert!(saved.get("small_buttons").is_none());
        let restored: Settings = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), saved);
    }
}

#[test]
fn old_state_keeps_behaviour() {
    // what 1.0.4 saved: none of 1.1's fields
    let s: Settings = serde_json::from_str(
        r#"{"proxy_mode":3,"proxy_host":"px","proxy_port":"3128","proxy_https":false,
        "adv":{"socks_on":true,"socks_host":"sx","socks_port":"1080","socks5":true}}"#,
    )
    .unwrap();
    assert_eq!(s.proxy_for("http://a.com/f"), "http://px:3128");
    assert_eq!(s.proxy_for("https://a.com/f"), "socks5h://sx:1080");
    // ftp not ticked goes by SOCKS, as the Advanced dialog's note says
    assert_eq!(s.proxy_for("ftp://a.com/f"), "socks5h://sx:1080");
    assert!(!s.proxy_ftp && !s.multi_net);
}

#[test]
fn lanes_only_for_direct() {
    let mut s = Settings {
        multi_net: true,
        ..Default::default()
    };
    assert!(s.use_lanes("http://a.com/f") && !s.use_lanes("ftp://a.com/f"));
    (s.proxy_mode, s.proxy_host) = (3, "px".into());
    assert!(!s.use_lanes("http://a.com/f"));
    s.multi_net = false;
    (s.proxy_mode, s.proxy_host) = (0, String::new());
    assert!(!s.use_lanes("http://a.com/f"));
}

#[test]
fn ftp_routes() {
    let mut s = Settings {
        proxy_mode: 3,
        proxy_host: "px".into(),
        proxy_port: "3128".into(),
        ..Default::default()
    };
    assert_eq!(s.proxy_for("ftp://a.com/f"), "");
    s.proxy_ftp = true;
    assert_eq!(s.proxy_for("ftp://a.com/f"), "http://px:3128");
    s.adv = AdvProxy {
        ftp_on: true,
        ftp_host: "fx".into(),
        ftp_port: "2121".into(),
        ..Default::default()
    };
    assert_eq!(s.proxy_for("ftp://a.com/f"), "ftp://fx:2121"); // the FTP proxy comes first
    (s.proxy_bypass_on, s.proxy_bypass) = (true, "a.com".into());
    assert_eq!(s.proxy_for("ftp://a.com/f"), "");
}

#[test]
fn proxies() {
    let mut s = Settings {
        proxy_mode: 3,
        proxy_host: "px".into(),
        proxy_port: "3128".into(),
        proxy_user: "u@x".into(),
        proxy_pass: "p".into(),
        ..Default::default()
    };
    assert_eq!(s.proxy_for("http://a.com/f"), "http://u%40x:p@px:3128");
    s.proxy_https = false;
    assert_eq!(s.proxy_for("https://a.com/f"), "");
    s.adv = AdvProxy {
        socks_on: true,
        socks_host: "sx".into(),
        socks_port: "1080".into(),
        socks5: true,
        ..Default::default()
    };
    assert_eq!(s.proxy_for("https://a.com/f"), "socks5h://sx:1080"); // socks for the unticked protocol
    s.adv.local_dns = true;
    assert_eq!(s.proxy_for("https://a.com/f"), "socks5://sx:1080");
    s.adv.socks5 = false;
    assert_eq!(s.proxy_for("https://a.com/f"), "socks4://sx:1080");
    (s.adv.socks5, s.adv.local_dns) = (true, false);
    s.adv.https_on = true;
    s.adv.https_host = "hx:8443".into();
    assert_eq!(s.proxy_for("https://a.com/f"), "http://hx:8443");
    (s.proxy_bypass_on, s.proxy_bypass) = (true, "*.corp.com 10.".into());
    assert_eq!(s.proxy_for("http://files.corp.com/f"), "");
    assert_eq!(s.proxy_for("http://10.1.2.3/f"), "");
    s.proxy_mode = 0;
    assert_eq!(s.proxy_for("http://a.com/f"), "");

    // automatic configuration script, evaluated by WinHTTP (Windows) or QuickJS
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    s.pac_url = format!("http://{}/proxy.pac", l.local_addr().unwrap());
    thread::spawn(move || {
        for c in l.incoming().flatten() {
            use std::io::{Read, Write};
            let mut c = c;
            let _ = c.read(&mut [0; 4096]);
            let pac = r#"function FindProxyForURL(url, host) { return host == "d.com" ? "DIRECT" : "PROXY 10.1.2.3:8080"; }"#;
            let _ = c.write_all(
                format!(
                    "HTTP/1.1 200 OK
Content-Type: application/x-ns-proxy-autoconfig
Content-Length: {}
Connection: close

{pac}",
                    pac.len()
                )
                .as_bytes(),
            );
        }
    });
    s.proxy_mode = 2;
    assert_eq!(s.proxy_for("http://a.com/f"), "http://10.1.2.3:8080");
    assert_eq!(s.proxy_for("http://d.com/f"), "");
}

#[test]
fn download_limits() {
    assert_eq!(quota_bytes(100), 100 * 1024 * 1024 / 100 * 98);
    let core = Core::from_saved(
        Saved::default(),
        std::env::temp_dir().join("ndm-test-unused.json"),
    );
    {
        let mut s = core.settings();
        (s.quota_on, s.quota_mb, s.quota_hours) = (true, 1, 5);
    }
    core.tick_quota(); // window starts at 0 bytes
    core.limiter.received.store(quota_bytes(1) - 1, Relaxed);
    core.tick_quota();
    assert!(!core.quota_blocked());
    core.limiter.received.store(quota_bytes(1), Relaxed);
    core.tick_quota();
    assert!(core.quota_blocked());
    assert!(core
        .quota_msg
        .lock()
        .unwrap()
        .as_deref()
        .is_some_and(|m| m.contains("you downloaded 0 MB")));
    // the window ends: downloads come back and counting starts over
    core.quota.lock().unwrap().start.as_mut().unwrap().0 -= chrono::Duration::hours(6);
    core.tick_quota();
    assert!(!core.quota_blocked());
    core.tick_quota();
    assert!(!core.quota_blocked());
    // switching the limit off lifts a stop
    core.limiter.received.store(10 * quota_bytes(1), Relaxed);
    core.tick_quota();
    assert!(core.quota_blocked());
    core.settings().quota_on = false;
    core.tick_quota();
    assert!(!core.quota_blocked());
}
