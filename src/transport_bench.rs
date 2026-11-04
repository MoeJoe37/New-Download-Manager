//! Local TLS fixtures for negotiated HTTPS and extra connections.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const CHUNK: usize = 48 * 1024;
static FILE: [u8; CHUNK] = [0x53; CHUNK];

async fn serve(socket: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, delay: Duration) {
    let range = format!("bytes 0-{}/{}", CHUNK - 1, CHUNK);
    if socket.get_ref().1.alpn_protocol() == Some(b"h2") {
        let Ok(mut h2) = h2::server::handshake::<_>(socket).await else {
            return;
        };
        let mut next = tokio::time::Instant::now();
        while let Some(Ok((request, mut respond))) = h2.accept().await {
            let wait = if request.uri().path() == "/warm" || delay.is_zero() {
                tokio::time::Instant::now()
            } else {
                next = next.max(tokio::time::Instant::now()) + delay;
                next
            };
            let response = http::Response::builder()
                .status(206)
                .header("content-range", &range)
                .header("content-length", CHUNK)
                .body(())
                .unwrap();
            tokio::spawn(async move {
                tokio::time::sleep_until(wait).await;
                if let Ok(mut stream) = respond.send_response(response, false) {
                    let _ = stream.send_data((&FILE[..]).into(), true);
                }
            });
        }
    } else {
        let mut socket = tokio::io::BufReader::new(socket);
        let head = format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {CHUNK}\r\nContent-Range: {range}\r\n\r\n");
        loop {
            let mut first = String::new();
            if !socket.read_line(&mut first).await.is_ok_and(|n| n > 0) {
                break;
            }
            loop {
                match socket.read_line(&mut String::new()).await {
                    Ok(n) if n > 2 => {}
                    Ok(2) => break,
                    _ => return,
                }
            }
            if !first.contains("/warm") {
                tokio::time::sleep(delay).await;
            }
            let stream = socket.get_mut();
            if stream.write_all(head.as_bytes()).await.is_err()
                || stream.write_all(&FILE).await.is_err()
            {
                break;
            }
        }
    }
}

fn with_server<T>(
    h2_only: bool,
    delay: Duration,
    run: impl FnOnce(&str, &reqwest::blocking::Client, &reqwest::blocking::Client) -> T,
) -> (T, usize) {
    let protocols: &[&[u8]] = if h2_only { &[b"h2"] } else { &[b"h2", b"http/1.1"] };
    let (tls, der) = crate::test_support::localhost_tls(protocols);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    listener.set_nonblocking(true).unwrap();
    let total = Arc::new(AtomicUsize::new(0));
    let accepted = total.clone();
    let (stop, mut quit) = tokio::sync::oneshot::channel();
    let server = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                while let Err(tokio::sync::oneshot::error::TryRecvError::Empty) = quit.try_recv() {
                    if let Ok(Ok((socket, _))) =
                        tokio::time::timeout(Duration::from_millis(20), listener.accept()).await
                    {
                        let (acceptor, accepted) = (acceptor.clone(), accepted.clone());
                        tokio::spawn(async move {
                            if let Ok(socket) = acceptor.accept(socket).await {
                                accepted.fetch_add(1, SeqCst);
                                serve(socket, delay).await;
                            }
                        });
                    }
                }
            });
    });
    let builder = || {
        client_builder().add_root_certificate(reqwest::Certificate::from_der(der.as_ref()).unwrap())
    };
    let (auto, forced) = (
        builder().build().unwrap(),
        builder().http1_only().build().unwrap(),
    );
    let result = run(&url, &auto, &forced);
    stop.send(()).unwrap();
    server.join().unwrap();
    (result, total.load(SeqCst))
}

fn range_request(agent: &Agent, url: &str, path: &str, extra: bool) -> Request {
    request(agent, &format!("{url}{path}"), "", "")
        .set("Range", &format!("bytes=0-{}", CHUNK - 1))
        .prefer_http1(extra)
}

fn check_response(response: Response, version: reqwest::Version) {
    assert!(matches!(&response, Response::Tls(r) if r.version() == version));
    assert_eq!(response.status(), 206);
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, FILE);
}

#[test]
fn extra_request_retries_with_http2_and_remembers_the_origin() {
    let ((), connections) = with_server(true, Duration::ZERO, |url, auto, http1| {
        let agent = crate::engine::agent("", "");
        let check = |extra, expected: &[bool]| {
            let offered = std::cell::RefCell::new(Vec::new());
            let response = call_with(range_request(&agent, url, "/warm", extra), |forced| {
                offered.borrow_mut().push(forced);
                Some(if forced { http1 } else { auto })
            })
            .unwrap();
            check_response(response, reqwest::Version::HTTP_2);
            assert_eq!(offered.into_inner(), expected);
        };
        check(false, &[false]);
        check(true, &[true, false]);
        check(true, &[false]);

        // An explicit user override still forces HTTP/1.1, even for an origin NDM learned.
        let agent = agent.with_http1_hosts("localhost");
        let result = call_with(range_request(&agent, url, "/forced", true), |forced| {
            assert!(forced);
            Some(http1)
        });
        assert!(result.is_err());
    });
    assert_eq!(connections, 1, "fallback reuses the negotiated connection");
}

fn measure(split: bool, delay: Duration) -> (Duration, usize) {
    with_server(false, delay, |url, auto, http1| {
        let agent = crate::engine::agent("", "");
        let read = |path: &str, extra| {
            let response = call_with(range_request(&agent, url, path, extra), |forced| {
                Some(if forced { http1 } else { auto })
            })
            .unwrap();
            check_response(
                response,
                if extra {
                    reqwest::Version::HTTP_11
                } else {
                    reqwest::Version::HTTP_2
                },
            );
        };
        read("/warm", false);
        let started = Instant::now();
        std::thread::scope(|scope| {
            for n in 0..4 {
                scope.spawn(move || read(&format!("/{n}"), split && n > 0));
            }
        });
        started.elapsed()
    })
}

#[test]
fn extra_requests_can_use_http1_without_downgrading_the_first() {
    let (_, connections) = measure(true, Duration::from_millis(24));
    assert!(connections > 1);
}

#[test]
#[ignore = "local TLS throughput comparison, run explicitly for measurements"]
fn http_protocol_benchmark() {
    for (workload, delay) in [
        ("unrestricted", Duration::ZERO),
        ("per-connection budget", Duration::from_millis(24)),
    ] {
        for split in [false, true] {
            let mut trials: Vec<_> = (0..5).map(|_| measure(split, delay)).collect();
            trials.sort_unstable();
            let (median, connections) = trials[2];
            println!("{workload}: {} {:.1} ms median, {connections} TLS connections (4 x 48 KiB, 5 trials)",
                if split { "HTTP/2 plus extra HTTP/1.1" } else { "HTTP/2 only" }, median.as_secs_f64() * 1000.0);
            if !split {
                assert_eq!(connections, 1);
            }
        }
    }
}
