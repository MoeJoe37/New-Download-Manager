//! Local TLS comparison of automatic HTTP/2 and an exact-host HTTP/1.1 override.
use super::*;
use rustls::pki_types::PrivatePkcs8KeyDer;
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

fn measure(http1: bool, delay: Duration) -> (Duration, usize) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let der = certificate.cert.der().clone();
    let mut tls = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::ring::default_provider().into(),
    )
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![der.clone()],
        PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into(),
    )
    .unwrap();
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
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
    let agent = crate::engine::agent("", "").with_http1_hosts(if http1 { "localhost" } else { "" });
    let read = |path: &str| {
        let req = request(&agent, &format!("{url}{path}"), "", "")
            .set("Range", &format!("bytes=0-{}", CHUNK - 1));
        let response = call_with(req, |http1_only| {
            Some(if http1_only { &forced } else { &auto })
        })
        .unwrap();
        let version = if http1 {
            reqwest::Version::HTTP_11
        } else {
            reqwest::Version::HTTP_2
        };
        assert!(matches!(&response, Response::Tls(r) if r.version() == version));
        assert_eq!(response.status(), 206);
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, FILE);
    };
    read("/warm");
    let started = Instant::now();
    std::thread::scope(|scope| {
        for n in 0..4 {
            scope.spawn(move || read(&format!("/{n}")));
        }
    });
    let elapsed = started.elapsed();
    stop.send(()).unwrap();
    server.join().unwrap();
    (elapsed, total.load(SeqCst))
}

#[test]
#[ignore = "local TLS throughput comparison, run explicitly for measurements"]
fn http_protocol_benchmark() {
    for (workload, delay) in [
        ("unrestricted", Duration::ZERO),
        ("per-connection budget", Duration::from_millis(24)),
    ] {
        for http1 in [false, true] {
            let mut trials: Vec<_> = (0..5).map(|_| measure(http1, delay)).collect();
            trials.sort_unstable();
            let (median, connections) = trials[2];
            println!("{workload}: {} {:.1} ms median, {connections} TLS connections (4 x 48 KiB, 5 trials)",
                if http1 { "HTTP/1.1" } else { "HTTP/2" }, median.as_secs_f64() * 1000.0);
            if !http1 {
                assert_eq!(connections, 1);
            }
        }
    }
}
