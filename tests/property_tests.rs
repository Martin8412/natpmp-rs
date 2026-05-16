//! Property-based tests for the NAT-PMP client and qBittorrent client.
//!
//! Each test encodes a universal property derived from the RFC 6886 spec or
//! the qBittorrent Web API contract, then verifies it holds for all values in
//! the given domain using `proptest`.
//!
//! The mock infrastructure is the same loopback-socket pattern used in the
//! other integration test files.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::thread;

use proptest::prelude::*;
use natpmp::{NatpmpClient, Protocol};

// ── UDP mock helpers (copy of the pattern from natpmp_tests.rs) ───────────────

fn mock_udp(responses: Vec<Vec<u8>>) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let addr = socket.local_addr().unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        for resp in responses {
            let Ok((_, peer)) = socket.recv_from(&mut buf) else { return };
            let _ = socket.send_to(&resp, peer);
        }
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
    addr
}

fn udp_client(addr: SocketAddr) -> NatpmpClient {
    NatpmpClient::new_with_port(Ipv4Addr::LOCALHOST, addr.port(), None)
        .expect("create test client")
}

fn addr_response(result: u16, ip: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; 12];
    p[1] = 128;
    p[2..4].copy_from_slice(&result.to_be_bytes());
    p[4..8].copy_from_slice(&1u32.to_be_bytes());
    p[8..12].copy_from_slice(&ip.octets());
    p
}

fn map_response(opcode: u8, result: u16, priv_port: u16, pub_port: u16, lifetime: u32) -> Vec<u8> {
    let mut p = vec![0u8; 16];
    p[1] = opcode;
    p[2..4].copy_from_slice(&result.to_be_bytes());
    p[4..8].copy_from_slice(&1u32.to_be_bytes());
    p[8..10].copy_from_slice(&priv_port.to_be_bytes());
    p[10..12].copy_from_slice(&pub_port.to_be_bytes());
    p[12..16].copy_from_slice(&lifetime.to_be_bytes());
    p
}

/// Capture the first UDP datagram sent by the client, then respond.
fn capture_udp_request(response: Vec<u8>) -> (SocketAddr, std::sync::mpsc::Receiver<Vec<u8>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let addr = socket.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        let Ok((n, peer)) = socket.recv_from(&mut buf) else { return };
        let _ = tx.send(buf[..n].to_vec());
        let _ = socket.send_to(&response, peer);
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
    (addr, rx)
}

// ── HTTP mock helpers (copy of the pattern from qbt_tests.rs) ─────────────────

struct HttpResp {
    status: u16,
    cookie: Option<&'static str>,
    body: &'static str,
}

fn mock_http(responses: Vec<HttpResp>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies_clone = Arc::clone(&bodies);
    let queue: Arc<Mutex<VecDeque<HttpResp>>> = Arc::new(Mutex::new(VecDeque::from(responses)));

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
            let q = Arc::clone(&queue);
            let b = Arc::clone(&bodies_clone);
            thread::spawn(move || http_handler(stream, q, b));
        }
    });

    (format!("http://127.0.0.1:{port}"), bodies)
}

fn http_handler(
    stream: TcpStream,
    queue: Arc<Mutex<VecDeque<HttpResp>>>,
    bodies: Arc<Mutex<Vec<String>>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.starts_with("POST") && !line.starts_with("GET") {
            continue;
        }
        let mut content_length = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 { return; }
            if h == "\r\n" || h == "\n" { break; }
            if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body_bytes).is_err() { return; }
        bodies.lock().unwrap().push(String::from_utf8_lossy(&body_bytes).into_owned());

        let resp = match queue.lock().unwrap().pop_front() {
            Some(r) => r,
            None => return,
        };
        let reason = if resp.status == 200 { "OK" } else { "Forbidden" };
        let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason);
        if let Some(c) = resp.cookie {
            out.push_str(&format!("Set-Cookie: {c}\r\n"));
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            resp.body.len(), resp.body
        ));
        if writer.write_all(out.as_bytes()).is_err() { return; }
        let _ = writer.flush();
    }
}

// ── NAT-PMP properties ────────────────────────────────────────────────────────

proptest! {
    /// RFC 6886 §3.5: any non-zero result code must be treated as an error,
    /// for all 65535 non-success values.
    #[test]
    fn prop_nonzero_result_code_always_errors(code in 1u16..=u16::MAX) {
        let addr = mock_udp(vec![addr_response(code, Ipv4Addr::UNSPECIFIED)]);
        let result = udp_client(addr).get_public_address();
        prop_assert!(result.is_err(),
            "result code {code} must produce Err, got Ok");
    }

    /// RFC 6886 §3.2: the external IP address in a valid public-address
    /// response (bytes 8–11) must be returned verbatim by `get_public_address`.
    #[test]
    fn prop_public_address_parsed_correctly(
        a in 0u8..=255, b in 0u8..=255, c in 0u8..=255, d in 0u8..=255
    ) {
        let ip = Ipv4Addr::new(a, b, c, d);
        let addr = mock_udp(vec![addr_response(0, ip)]);
        let parsed = udp_client(addr).get_public_address().expect("Ok");
        prop_assert_eq!(parsed, ip);
    }

    /// RFC 6886 §3.3: the private port sent in a mapping request (bytes 4–5,
    /// big-endian) must match the value returned in PortMapping.private_port.
    #[test]
    fn prop_private_port_round_trips(private_port in 1u16..=u16::MAX) {
        let resp = map_response(129, 0, private_port, 1234, 60);
        let addr = mock_udp(vec![resp]);
        let m = udp_client(addr).map_port(private_port, Protocol::Udp, 60).expect("Ok");
        prop_assert_eq!(m.private_port, private_port);
    }

    /// RFC 6886 §3.3: the public port in the response (bytes 10–11) must be
    /// returned verbatim in PortMapping.public_port.
    #[test]
    fn prop_public_port_round_trips(public_port in 1u16..=u16::MAX) {
        let resp = map_response(129, 0, 4000, public_port, 60);
        let addr = mock_udp(vec![resp]);
        let m = udp_client(addr).map_port(0, Protocol::Udp, 60).expect("Ok");
        prop_assert_eq!(m.public_port, public_port);
    }

    /// RFC 6886 §3.3: the lifetime in the response (bytes 12–15) must be
    /// returned verbatim in PortMapping.lifetime.
    #[test]
    fn prop_lifetime_round_trips(lifetime in 1u32..=u32::MAX) {
        let resp = map_response(129, 0, 4000, 5000, lifetime);
        let addr = mock_udp(vec![resp]);
        let m = udp_client(addr).map_port(0, Protocol::Udp, 60).expect("Ok");
        prop_assert_eq!(m.lifetime, lifetime);
    }

    /// RFC 6886 §3.3: the private port sent in the UDP request wire format
    /// (bytes 4–5, big-endian) must match `private_port`.
    #[test]
    fn prop_request_encodes_private_port(private_port in 0u16..=u16::MAX) {
        let resp = map_response(129, 0, private_port, 5000, 60);
        let (addr, rx) = capture_udp_request(resp);
        udp_client(addr).map_port(private_port, Protocol::Udp, 60).ok();
        let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        prop_assert!(req.len() >= 6);
        let encoded = u16::from_be_bytes([req[4], req[5]]);
        prop_assert_eq!(encoded, private_port,
            "private port must be encoded big-endian in bytes 4–5");
    }

    /// RFC 6886 §3.3: the lifetime sent in the request wire format (bytes
    /// 8–11, big-endian) must match the `lifetime` argument.
    #[test]
    fn prop_request_encodes_lifetime(lifetime in 1u32..=u32::MAX) {
        let resp = map_response(129, 0, 0, 5000, lifetime);
        let (addr, rx) = capture_udp_request(resp);
        udp_client(addr).map_port(0, Protocol::Udp, lifetime).ok();
        let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        prop_assert!(req.len() >= 12);
        let encoded = u32::from_be_bytes([req[8], req[9], req[10], req[11]]);
        prop_assert_eq!(encoded, lifetime,
            "lifetime must be encoded big-endian in bytes 8–11");
    }
}

// ── qBittorrent properties ────────────────────────────────────────────────────

proptest! {
    // Each case spawns a TcpListener thread that lives until the process exits.
    // Cap at 50 to avoid exhausting the OS thread limit on CI runners.
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// For every possible port value, the setPreferences request body must
    /// contain that port number as a decimal integer.
    #[test]
    fn prop_set_listen_port_encodes_any_port(port in 0u16..=u16::MAX) {
        let (url, bodies) = mock_http(vec![
            HttpResp { status: 200, cookie: Some("SID=tok; Path=/"), body: "Ok." },
            HttpResp { status: 200, cookie: None, body: "" },
        ]);
        let client = natpmp::qbt::Client::new(&url, "admin", "pass");
        client.set_listen_port(port).expect("set_listen_port");

        let captured = bodies.lock().unwrap();
        // Second request is the setPreferences call.
        prop_assert!(captured.len() >= 2,
            "expected login + setPreferences, got {} requests", captured.len());
        prop_assert!(captured[1].contains(&port.to_string()),
            "setPreferences body must contain {port}, got: {}", captured[1]);
    }
}
