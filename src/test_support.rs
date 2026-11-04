//! Shared fixtures for download and media tests.
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Tiny HTTP server: paths starting with /r honour Range, others always send 200 + full body.
pub(crate) fn serve(data: Arc<Vec<u8>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let data = data.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    let prefix = "range: bytes=";
                    if h.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix)) {
                        let v = &h[prefix.len()..];
                        let (a, b) = v.trim().split_once('-').unwrap();
                        let a: usize = a.parse().unwrap();
                        let b = b.parse().unwrap_or(data.len() - 1).min(data.len() - 1);
                        range = Some((a, b));
                    }
                }
                let mut w = &s;
                let (head, body) = match range.filter(|_| path.starts_with("/r")) {
                    Some((a, b)) => (format!("HTTP/1.1 206 Partial\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nLast-Modified: Sun, 06 Nov 1994 08:49:37 GMT\r\n\r\n", data.len(), b + 1 - a), &data[a..=b]),
                    None => (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()), &data[..]),
                };
                let _ = w.write_all(head.as_bytes());
                // "/rslow..." paths trickle ~1.3 MB/s per connection, so extra connections pay off
                let step = if path.starts_with("/rslow") { 64 * 1024 } else { body.len().max(1) };
                for c in body.chunks(step) {
                    if w.write_all(c).is_err() {
                        break;
                    }
                    if step < body.len() {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            });
        }
    });
    base
}

pub(crate) fn mk(t: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let body = parts.concat();
    [&((body.len() + 8) as u32).to_be_bytes()[..], t, &body].concat()
}
pub(crate) fn init(id: u32, scale: u32) -> Vec<u8> {
    let tkhd = mk(b"tkhd", &[&[0; 12], &id.to_be_bytes(), &[0; 4]]);
    let mdhd = mk(b"mdhd", &[&[0; 12], &scale.to_be_bytes(), &[0; 4]]);
    let trak = mk(b"trak", &[&tkhd, &mk(b"mdia", &[&mdhd])]);
    let mvhd = mk(b"mvhd", &[&[0; 96], &(id + 1).to_be_bytes()]);
    let trex = mk(b"trex", &[&[0; 4], &id.to_be_bytes(), &[0; 12]]);
    [mk(b"ftyp", &[b"iso6"]), mk(b"moov", &[&mvhd, &trak, &mk(b"mvex", &[&trex])])].concat()
}
pub(crate) fn frag(id: u32, t: u32, payload: u8) -> Vec<u8> {
    let traf = mk(b"traf", &[&mk(b"tfhd", &[&[0; 4], &id.to_be_bytes()]), &mk(b"tfdt", &[&[0; 4], &t.to_be_bytes()])]);
    [mk(b"styp", &[b"msdh"]), mk(b"moof", &[&traf]), mk(b"mdat", &[&[payload]])].concat()
}

pub(crate) fn localhost_tls(alpn: &[&[u8]]) -> (rustls::ServerConfig, rustls::pki_types::CertificateDer<'static>) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let der = certificate.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
    let mut tls = rustls::ServerConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key.into())
        .unwrap();
    tls.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    (tls, der)
}
