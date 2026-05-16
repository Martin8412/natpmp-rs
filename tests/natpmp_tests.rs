//! Integration tests for the NAT-PMP client library.
//!
//! All tests are black-box: they drive `NatpmpClient` through its public API
//! while a mock UDP server on a loopback socket provides hand-crafted
//! responses derived directly from RFC 6886.
//!
//! # Wire-format reference (RFC 6886)
//!
//! Public-address response (12 bytes):
//!   [0]    version = 0
//!   [1]    opcode  = 128 (0x80)
//!   [2..4] result code u16 big-endian
//!   [4..8] seconds since epoch u32 big-endian (ignored by client)
//!   [8..12] external IPv4
//!
//! Port-mapping response (16 bytes):
//!   [0]    version = 0
//!   [1]    opcode  = 129 (UDP) or 130 (TCP)
//!   [2..4] result code u16 big-endian
//!   [4..8] seconds since epoch
//!   [8..10]  private port u16 big-endian
//!   [10..12] public port  u16 big-endian
//!   [12..16] lifetime     u32 big-endian

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use natpmp::{NatpmpClient, Protocol};

// ---------------------------------------------------------------------------
// Mock-server helper
// ---------------------------------------------------------------------------

/// Binds a UDP socket on `127.0.0.1:0` and spawns a thread that sends each
/// datagram in `responses` in order whenever a datagram arrives from the
/// client.  Once the list is exhausted the thread reads and discards every
/// further datagram without replying (useful for retransmission tests).
///
/// Returns the bound `SocketAddr` and a `JoinHandle` for the server thread.
fn mock_server(responses: Vec<Vec<u8>>) -> (SocketAddr, JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind mock server");
    // Give the server enough time to outlast any test — 30 s is generous.
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .expect("set server read timeout");
    let addr = socket.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        let mut buf = [0u8; 256];
        for response in responses {
            // Wait for a datagram from the client.
            let (_, peer) = match socket.recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => return, // timeout / shutdown
            };
            socket
                .send_to(&response, peer)
                .expect("mock server send");
        }
        // Drain further datagrams silently (no replies).
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    });

    (addr, handle)
}

/// Like `mock_server`, but also forwards every incoming datagram to the returned
/// `Receiver` so tests can inspect the raw request bytes.
fn mock_server_capture(
    responses: Vec<Vec<u8>>,
) -> (SocketAddr, mpsc::Receiver<Vec<u8>>, JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind mock server");
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let addr = socket.local_addr().unwrap();
    let (tx, rx) = mpsc::sync_channel(32);

    let handle = thread::spawn(move || {
        let mut buf = [0u8; 256];
        for response in responses {
            let (n, peer) = match socket.recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => return,
            };
            tx.send(buf[..n].to_vec()).ok();
            socket.send_to(&response, peer).expect("mock server send");
        }
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    tx.send(buf[..n].to_vec()).ok();
                }
                Err(_) => return,
            }
        }
    });

    (addr, rx, handle)
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

/// Builds a public-address response (opcode 128, 12 bytes).
fn build_addr_response(result: u16, ip: Ipv4Addr) -> Vec<u8> {
    let mut pkt = vec![0u8; 12];
    pkt[0] = 0;   // version
    pkt[1] = 128; // opcode
    pkt[2..4].copy_from_slice(&result.to_be_bytes());
    pkt[4..8].copy_from_slice(&42u32.to_be_bytes()); // epoch — any value
    pkt[8..12].copy_from_slice(&ip.octets());
    pkt
}

/// Builds a port-mapping response (opcode 129=UDP or 130=TCP, 16 bytes).
fn build_map_response(
    opcode: u8,
    result: u16,
    private_port: u16,
    public_port: u16,
    lifetime: u32,
) -> Vec<u8> {
    let mut pkt = vec![0u8; 16];
    pkt[0] = 0; // version
    pkt[1] = opcode;
    pkt[2..4].copy_from_slice(&result.to_be_bytes());
    pkt[4..8].copy_from_slice(&42u32.to_be_bytes()); // epoch
    pkt[8..10].copy_from_slice(&private_port.to_be_bytes());
    pkt[10..12].copy_from_slice(&public_port.to_be_bytes());
    pkt[12..16].copy_from_slice(&lifetime.to_be_bytes());
    pkt
}

/// Builds a new `NatpmpClient` aimed at the given loopback server address.
fn client_for(addr: SocketAddr) -> NatpmpClient {
    NatpmpClient::new_with_port(Ipv4Addr::LOCALHOST, addr.port(), None)
        .expect("create test client")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// get_public_address happy path: server returns opcode 128 with an IPv4.
#[test]
fn test_get_public_address_happy() {
    let expected_ip = Ipv4Addr::new(203, 0, 113, 7);
    let (addr, _srv) = mock_server(vec![build_addr_response(0, expected_ip)]);

    let client = client_for(addr);
    let ip = client.get_public_address().expect("get_public_address");
    assert_eq!(ip, expected_ip);
}

/// map_port UDP happy path: server returns opcode 129 with valid fields.
#[test]
fn test_map_port_udp_happy() {
    let pkt = build_map_response(129, 0, 4567, 8888, 3600);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    let mapping = client
        .map_port(4567, Protocol::Udp, 3600)
        .expect("map_port UDP");
    assert_eq!(mapping.private_port, 4567);
    assert_eq!(mapping.public_port, 8888);
    assert_eq!(mapping.lifetime, 3600);
}

/// map_port TCP happy path: server returns opcode 130 with valid fields.
#[test]
fn test_map_port_tcp_happy() {
    let pkt = build_map_response(130, 0, 9999, 7777, 1800);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    let mapping = client
        .map_port(9999, Protocol::Tcp, 1800)
        .expect("map_port TCP");
    assert_eq!(mapping.private_port, 9999);
    assert_eq!(mapping.public_port, 7777);
    assert_eq!(mapping.lifetime, 1800);
}

/// Server returns result code 1 (unsupported version) → client returns Err.
#[test]
fn test_error_code_1_unsupported_version() {
    let pkt = build_addr_response(1, Ipv4Addr::UNSPECIFIED);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.get_public_address().is_err());
}

/// Server returns result code 2 (not authorized) → client returns Err.
#[test]
fn test_error_code_2_not_authorized() {
    let pkt = build_map_response(129, 2, 0, 0, 0);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.map_port(1234, Protocol::Udp, 60).is_err());
}

/// Server returns result code 3 (network failure) → client returns Err.
#[test]
fn test_error_code_3_network_failure() {
    let pkt = build_map_response(130, 3, 0, 0, 0);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.map_port(1234, Protocol::Tcp, 60).is_err());
}

/// Server returns result code 4 (out of resources) → client returns Err.
#[test]
fn test_error_code_4_out_of_resources() {
    let pkt = build_map_response(129, 4, 0, 0, 0);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.map_port(1234, Protocol::Udp, 60).is_err());
}

/// Server returns result code 5 (unsupported opcode) → client returns Err.
#[test]
fn test_error_code_5_unsupported_opcode() {
    let pkt = build_addr_response(5, Ipv4Addr::UNSPECIFIED);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.get_public_address().is_err());
}

/// Server first replies with a wrong opcode (129 when 130 is expected), then
/// with the correct one.  The client must silently discard the stale reply and
/// succeed on the second datagram — RFC 6886 §3.1.
///
/// Because the client keeps waiting inside the same timeout window, the server
/// must send both packets for the same "request arrival".  We implement this by
/// having the mock server send both replies when it receives the first request.
#[test]
fn test_stale_opcode_discarded() {
    // Server sends opcode 129 (wrong, client expects 130 for TCP), then 130.
    let wrong = build_map_response(129, 0, 1234, 5678, 3600);
    let correct = build_map_response(130, 0, 1234, 5678, 3600);

    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind mock server");
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let addr = socket.local_addr().unwrap();

    let handle = thread::spawn(move || {
        let mut buf = [0u8; 256];
        // Wait for first datagram from client.
        let (_, peer) = socket.recv_from(&mut buf).expect("recv");
        // Send wrong opcode first, then correct one — back to back.
        socket.send_to(&wrong, peer).expect("send wrong");
        socket.send_to(&correct, peer).expect("send correct");
        // Drain the rest silently.
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let client = client_for(addr);
    let mapping = client
        .map_port(1234, Protocol::Tcp, 3600)
        .expect("should succeed after discarding stale opcode");
    assert_eq!(mapping.public_port, 5678);
    drop(handle);
}

/// Response with version byte ≠ 0 → client returns Err (RFC 6886 §2).
#[test]
fn test_wrong_version_in_response() {
    let mut pkt = build_addr_response(0, Ipv4Addr::new(1, 2, 3, 4));
    pkt[0] = 1; // corrupt the version byte

    // The mock server needs to reply to every attempt (9 total) because the
    // client will keep retransmitting after receiving an invalid response.
    // We send 9 bad replies to cover all attempts.
    let responses = vec![pkt.clone(); 9];
    let (addr, _srv) = mock_server(responses);

    let client = client_for(addr);
    assert!(client.get_public_address().is_err());
}

/// Lifetime = 0 in mapping response → client returns Err (gateway refused).
#[test]
fn test_lifetime_zero_returns_error() {
    let pkt = build_map_response(129, 0, 1234, 5678, 0); // lifetime = 0
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.map_port(1234, Protocol::Udp, 60).is_err());
}

/// Public port = 0 in mapping response → client returns Err (gateway refused).
#[test]
fn test_public_port_zero_returns_error() {
    let pkt = build_map_response(129, 0, 1234, 0, 3600); // public_port = 0
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    assert!(client.map_port(1234, Protocol::Udp, 60).is_err());
}

/// When client sends private_port=1234, the server echoes it; client reports
/// the same value in `PortMapping.private_port`.
#[test]
fn test_private_port_echo() {
    let pkt = build_map_response(129, 0, 1234, 6000, 3600);
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    let mapping = client
        .map_port(1234, Protocol::Udp, 3600)
        .expect("map_port");
    assert_eq!(mapping.private_port, 1234);
}

/// When client sends private_port=0, the server assigns 9000; client reports
/// 9000 in `PortMapping.private_port`.
#[test]
fn test_zero_private_port_server_assigns() {
    let pkt = build_map_response(129, 0, 9000, 9000, 3600); // server assigns 9000
    let (addr, _srv) = mock_server(vec![pkt]);

    let client = client_for(addr);
    let mapping = client
        .map_port(0, Protocol::Udp, 3600)
        .expect("map_port with port=0");
    assert_eq!(mapping.private_port, 9000);
}

/// Retransmission: server ignores first 3 requests, responds on the 4th.
/// The client must retransmit and eventually succeed (RFC 6886 §3.1).
#[test]
fn test_retransmission_succeeds_after_initial_silence() {
    let expected_ip = Ipv4Addr::new(198, 51, 100, 42);

    // The mock server discards the first 3 datagrams, then replies.
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind mock server");
    // 5 s is enough to cover attempts 1–4 (250 + 500 + 1000 ms ≈ 1.75 s wait)
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let addr = socket.local_addr().unwrap();

    let handle = thread::spawn(move || {
        let mut buf = [0u8; 256];
        // Discard first 3 datagrams.
        for _ in 0..3 {
            match socket.recv_from(&mut buf) {
                Ok(_) => {} // discard
                Err(_) => return,
            }
        }
        // Respond to the 4th.
        let (_, peer) = socket.recv_from(&mut buf).expect("recv 4th");
        let response = build_addr_response(0, expected_ip);
        socket.send_to(&response, peer).expect("send response");
        // Drain the rest.
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let client = client_for(addr);
    let ip = client.get_public_address().expect("should succeed after retransmits");
    assert_eq!(ip, expected_ip);
    drop(handle);
}

// ---------------------------------------------------------------------------
// Request wire-format tests (RFC 6886 §3.2 / §3.3)
// ---------------------------------------------------------------------------

/// Public-address request must be exactly 2 bytes: version=0, opcode=0.
#[test]
fn test_request_format_public_address() {
    let (addr, rx, _srv) =
        mock_server_capture(vec![build_addr_response(0, Ipv4Addr::new(1, 2, 3, 4))]);
    let client = client_for(addr);
    client.get_public_address().expect("get_public_address");

    let req = rx.recv().expect("no request captured");
    assert_eq!(req.len(), 2, "public-address request must be 2 bytes");
    assert_eq!(req[0], 0, "version must be 0");
    assert_eq!(req[1], 0, "opcode must be 0");
}

/// UDP mapping request: 12 bytes, opcode=1, private_port and lifetime big-endian.
#[test]
fn test_request_format_map_port_udp() {
    let (addr, rx, _srv) =
        mock_server_capture(vec![build_map_response(129, 0, 1234, 5000, 60)]);
    let client = client_for(addr);
    client.map_port(1234, Protocol::Udp, 60).expect("map_port");

    let req = rx.recv().expect("no request captured");
    assert_eq!(req.len(), 12, "mapping request must be 12 bytes");
    assert_eq!(req[0], 0, "version must be 0");
    assert_eq!(req[1], 1, "opcode must be 1 for UDP");
    assert_eq!(&req[2..4], &[0, 0], "reserved bytes must be 0");
    assert_eq!(
        u16::from_be_bytes([req[4], req[5]]),
        1234,
        "private_port at bytes 4-5"
    );
    assert_eq!(
        u16::from_be_bytes([req[6], req[7]]),
        0,
        "suggested public port must be 0 (let server assign)"
    );
    assert_eq!(
        u32::from_be_bytes([req[8], req[9], req[10], req[11]]),
        60,
        "lifetime at bytes 8-11"
    );
}

/// TCP mapping request: same layout as UDP but opcode=2.
#[test]
fn test_request_format_map_port_tcp() {
    let (addr, rx, _srv) =
        mock_server_capture(vec![build_map_response(130, 0, 4321, 5000, 120)]);
    let client = client_for(addr);
    client.map_port(4321, Protocol::Tcp, 120).expect("map_port");

    let req = rx.recv().expect("no request captured");
    assert_eq!(req[1], 2, "opcode must be 2 for TCP");
    assert_eq!(
        u16::from_be_bytes([req[4], req[5]]),
        4321,
        "private_port at bytes 4-5"
    );
    assert_eq!(
        u32::from_be_bytes([req[8], req[9], req[10], req[11]]),
        120,
        "lifetime at bytes 8-11"
    );
}

// ---------------------------------------------------------------------------
// Truncated-response tests
// ---------------------------------------------------------------------------

/// A response shorter than 12 bytes for a public-address reply is rejected (RFC 6886 §3.2).
#[test]
fn test_truncated_addr_response() {
    // 8 bytes: valid header but no IP address field.
    let truncated = vec![0u8, 128, 0, 0, 0, 0, 0, 0];
    let (addr, _srv) = mock_server(vec![truncated]);
    let client = client_for(addr);
    assert!(
        client.get_public_address().is_err(),
        "truncated public-address response must be an error"
    );
}

/// A response shorter than 16 bytes for a port-mapping reply is rejected (RFC 6886 §3.3).
#[test]
fn test_truncated_map_response() {
    // 12 bytes: valid header + private port, but public port and lifetime are missing.
    let truncated = vec![0u8, 129, 0, 0, 0, 0, 0, 0, 0, 42, 0, 17];
    let (addr, _srv) = mock_server(vec![truncated]);
    let client = client_for(addr);
    assert!(
        client.map_port(42, Protocol::Udp, 60).is_err(),
        "truncated mapping response must be an error"
    );
}

// ---------------------------------------------------------------------------
// Miscellaneous correctness tests
// ---------------------------------------------------------------------------

/// Any non-zero result code — including values beyond the five named ones — must
/// be treated as an error.
#[test]
fn test_unknown_result_code() {
    let pkt = build_addr_response(99, Ipv4Addr::UNSPECIFIED);
    let (addr, _srv) = mock_server(vec![pkt]);
    let client = client_for(addr);
    assert!(
        client.get_public_address().is_err(),
        "unknown result code must be an error"
    );
}

/// A single `NatpmpClient` handles sequential calls correctly — map TCP, then
/// reuse the returned private_port for the UDP mapping, on the same socket.
/// This mirrors the renewal loop in main.rs.
#[test]
fn test_sequential_tcp_then_udp_on_same_client() {
    let tcp_resp = build_map_response(130, 0, 5000, 6000, 60);
    let udp_resp = build_map_response(129, 0, 5000, 6000, 60);
    let (addr, _srv) = mock_server(vec![tcp_resp, udp_resp]);

    let client = client_for(addr);
    let tcp = client.map_port(0, Protocol::Tcp, 60).expect("TCP map");
    let udp = client
        .map_port(tcp.private_port, Protocol::Udp, 60)
        .expect("UDP map");

    assert_eq!(tcp.public_port, 6000);
    assert_eq!(udp.public_port, 6000);
    assert_eq!(
        tcp.private_port, udp.private_port,
        "both mappings must share the same private port"
    );
}

// ── Request wire-format verification ─────────────────────────────────────────

/// Binds a one-shot mock server, returns its address and a channel receiver
/// that yields the first datagram bytes the server receives.  The server
/// responds with `response` and then drains silently.
fn capture_first_request(response: Vec<u8>) -> (SocketAddr, std::sync::mpsc::Receiver<Vec<u8>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind capture server");
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

/// `get_public_address` must send version=0, opcode=0 (RFC 6886 §3.2).
#[test]
fn test_request_get_public_address_wire_format() {
    let (addr, rx) =
        capture_first_request(build_addr_response(0, Ipv4Addr::new(1, 2, 3, 4)));
    client_for(addr).get_public_address().unwrap();

    let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(req[0], 0, "version must be 0");
    assert_eq!(req[1], 0, "opcode must be 0 (public-address request)");
}

/// `map_port(..., Protocol::Udp, ...)` must send opcode=1 with the private
/// port in bytes 4–5, suggested public port 0 in bytes 6–7, and the lifetime
/// in bytes 8–11, all big-endian (RFC 6886 §3.3).
#[test]
fn test_request_map_port_udp_wire_format() {
    let (addr, rx) =
        capture_first_request(build_map_response(129, 0, 1234, 5678, 60));
    client_for(addr).map_port(1234, Protocol::Udp, 60).unwrap();

    let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(req.len() >= 12, "request must be at least 12 bytes");
    assert_eq!(req[0], 0, "version must be 0");
    assert_eq!(req[1], 1, "opcode must be 1 (UDP map request)");
    assert_eq!(
        u16::from_be_bytes([req[4], req[5]]),
        1234,
        "private port big-endian in bytes 4–5"
    );
    assert_eq!(
        u16::from_be_bytes([req[6], req[7]]),
        0,
        "suggested public port must be 0 (let server assign)"
    );
    assert_eq!(
        u32::from_be_bytes([req[8], req[9], req[10], req[11]]),
        60,
        "lifetime big-endian in bytes 8–11"
    );
}

/// `map_port(..., Protocol::Tcp, ...)` must send opcode=2 (RFC 6886 §3.3).
#[test]
fn test_request_map_port_tcp_wire_format() {
    let (addr, rx) =
        capture_first_request(build_map_response(130, 0, 9999, 5678, 120));
    client_for(addr).map_port(9999, Protocol::Tcp, 120).unwrap();

    let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(req[1], 2, "opcode must be 2 (TCP map request)");
    assert_eq!(
        u16::from_be_bytes([req[4], req[5]]),
        9999,
        "private port big-endian in bytes 4–5"
    );
    assert_eq!(
        u32::from_be_bytes([req[8], req[9], req[10], req[11]]),
        120,
        "lifetime big-endian in bytes 8–11"
    );
}

// ── Truncated and malformed responses ────────────────────────────────────────

/// A datagram shorter than 4 bytes is treated as a failed attempt and the
/// client retransmits (RFC 6886 §3.1).  The test verifies that after the runt
/// the client retransmits and succeeds when the second reply is valid.
#[test]
fn test_too_short_response_triggers_retransmit() {
    let expected = Ipv4Addr::new(10, 0, 0, 1);
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let addr = socket.local_addr().unwrap();

    thread::spawn(move || {
        let mut buf = [0u8; 256];
        let Ok((_, peer)) = socket.recv_from(&mut buf) else { return };
        // 3-byte runt — too short to parse.
        socket.send_to(&[0u8, 128u8, 0u8], peer).unwrap();
        // Valid reply to the retransmit.
        let Ok((_, peer)) = socket.recv_from(&mut buf) else { return };
        socket
            .send_to(&build_addr_response(0, expected), peer)
            .unwrap();
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let ip = client_for(addr)
        .get_public_address()
        .expect("should succeed after runt");
    assert_eq!(ip, expected);
}

/// A response with the right version and opcode but fewer than the required
/// minimum bytes (4 ≤ n < 12 for public-address) must be an immediate error.
#[test]
fn test_truncated_response_returns_error() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let addr = socket.local_addr().unwrap();

    thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            let Ok((_, peer)) = socket.recv_from(&mut buf) else { return };
            // 8 bytes: correct version + opcode, result=0, partial epoch — no IP.
            let _ = socket.send_to(&[0u8, 128u8, 0u8, 0u8, 0u8, 0u8, 0u8, 42u8], peer);
        }
    });

    assert!(
        client_for(addr).get_public_address().is_err(),
        "truncated response must produce an error"
    );
}

// ── Unknown / future result codes ─────────────────────────────────────────────

/// Any non-zero result code — including values not defined in RFC 6886 — must
/// be treated as an error.
#[test]
fn test_unknown_result_code_is_error() {
    // Code 6 is not assigned in RFC 6886.
    let pkt = build_addr_response(6, Ipv4Addr::UNSPECIFIED);
    let (addr, _srv) = mock_server(vec![pkt]);
    assert!(client_for(addr).get_public_address().is_err());
}

// ── Sequential calls on the same client ───────────────────────────────────────

/// Mirrors the main-loop pattern: map TCP first, feed the returned private_port
/// into a UDP mapping on the same NatpmpClient.  Verifies the socket is reusable
/// across calls and that port values are threaded through correctly.
#[test]
fn test_sequential_tcp_then_udp_mapping() {
    let tcp_pkt = build_map_response(130, 0, 12345, 54321, 60);
    let udp_pkt = build_map_response(129, 0, 12345, 54321, 60);
    let (addr, _srv) = mock_server(vec![tcp_pkt, udp_pkt]);

    let client = client_for(addr);
    let tcp = client.map_port(0, Protocol::Tcp, 60).expect("TCP mapping");
    let udp = client
        .map_port(tcp.private_port, Protocol::Udp, 60)
        .expect("UDP mapping");

    assert_eq!(tcp.private_port, udp.private_port);
    assert_eq!(tcp.public_port, udp.public_port);
}

// ── ConnectionRefused handling ────────────────────────────────────────────────

/// When no server is bound at the gateway port, the loopback stack returns an
/// ICMP port-unreachable which the OS delivers as `ConnectionRefused` on a
/// connected UDP socket.  The client must bail immediately with a helpful
/// message — not silently retry 9 times over ~128 s.
///
/// Timing enforcement: the result must arrive within 5 s.  With the correct
/// guard the call returns in < 1 s.  If `ConnectionRefused` were mistakenly
/// treated as a timeout (the `is_timeout → true` mutation), the client would
/// exhaust all 9 attempts (~128 s total) and the deadline would fire first.
#[test]
fn test_connection_refused_bails_immediately_with_helpful_message() {
    // Grab a free loopback port and immediately release it.
    let tmp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = tmp.local_addr().unwrap().port();
    drop(tmp);

    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let client = NatpmpClient::new_with_port(Ipv4Addr::LOCALHOST, port, None)
            .expect("create client");
        let _ = tx.send(client.get_public_address());
    });

    let result = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("ConnectionRefused must not be retried — client must bail immediately, not after 128 s");

    assert!(result.is_err(), "ConnectionRefused must produce Err");

    // Guard mutation `e.kind() == ConnectionRefused with false` would route the
    // error to the generic bail("recv from gateway...") arm instead, causing
    // this assertion to fail.
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("not a NAT-PMP server") || msg.contains("VPN"),
        "expected helpful NAT-PMP / VPN message, got: {msg}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────

/// No-response timeout test: server never replies → `map_port` returns `Err`.
/// The full retry sequence takes ~128 s (9 attempts, 250→64000 ms backoff).
/// Marked `#[ignore]` so it does not block the normal test suite.
#[test]
#[ignore]
fn test_no_response_returns_error() {
    // Bind a port so that packets are accepted (no ICMP port-unreachable) but
    // never reply to any of them.
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind mock server");
    let addr = socket.local_addr().unwrap();

    // Server thread: receive and discard every datagram, forever.
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match socket.recv_from(&mut buf) {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let client = client_for(addr);
    // This will take ~128 s before returning Err.
    assert!(
        client.map_port(1234, Protocol::Udp, 60).is_err(),
        "expected Err after all retransmit attempts exhausted"
    );
}
