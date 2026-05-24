//! Integration tests for the qBittorrent Web API client.
//!
//! A minimal TCP-based HTTP mock server is used — no external dependencies.
//! The mock speaks just enough HTTP/1.1 to satisfy ureq (keep-alive, correct
//! Content-Length, Set-Cookie).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use natpmp::qbt::Client;

// ---------------------------------------------------------------------------
// HTTP mock infrastructure
// ---------------------------------------------------------------------------

struct MockResponse {
    status: u16,
    cookie: Option<String>,
    body: String,
}

impl MockResponse {
    fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            cookie: None,
            body: body.into(),
        }
    }

    fn with_cookie(cookie: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            status: 200,
            cookie: Some(cookie.into()),
            body: body.into(),
        }
    }

    fn forbidden() -> Self {
        Self {
            status: 403,
            cookie: None,
            body: String::new(),
        }
    }
}

/// Spawns a mock HTTP/1.1 server that services requests in the order they
/// arrive and sends back the pre-programmed `responses` in order.
///
/// Returns `(base_url, captured)` where `captured` accumulates
/// `(request_path, request_body)` for every request received.
fn spawn_mock_http(responses: Vec<MockResponse>) -> (String, Arc<Mutex<Vec<(String, String)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock HTTP server");
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");

    let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    thread::spawn(move || {
        let mut responses: VecDeque<MockResponse> = VecDeque::from(responses);

        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

            // Two separate handles so BufReader and write path don't alias.
            let read_side = stream.try_clone().expect("clone stream");
            let mut reader = BufReader::new(read_side);
            let mut writer = stream;

            loop {
                if responses.is_empty() {
                    break;
                }
                let req = match read_http_request(&mut reader) {
                    Some(r) => r,
                    None => break, // connection closed / EOF
                };
                captured_clone.lock().unwrap().push(req);

                if let Some(resp) = responses.pop_front() {
                    write_http_response(&mut writer, &resp);
                }
            }

            if responses.is_empty() {
                break;
            }
        }
    });

    (url, captured)
}

/// Reads one HTTP/1.1 request from a buffered reader.
/// Returns `(path, body)` or `None` on EOF / error.
fn read_http_request(reader: &mut BufReader<TcpStream>) -> Option<(String, String)> {
    // Request line: "POST /path HTTP/1.1\r\n"
    let mut request_line = String::new();
    match reader.read_line(&mut request_line) {
        Ok(0) | Err(_) => return None,
        Ok(_) => {}
    }
    let request_line = request_line.trim();
    if request_line.is_empty() {
        return None;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    // Headers: read until blank line, capture Content-Length.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(val) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    // Body
    let body = if content_length > 0 {
        use std::io::Read;
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).is_err() {
            return None;
        }
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        String::new()
    };

    Some((path, body))
}

fn write_http_response(writer: &mut TcpStream, resp: &MockResponse) {
    let status_text = match resp.status {
        200 => "OK",
        403 => "Forbidden",
        _ => "Unknown",
    };
    let mut raw = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
        resp.status,
        status_text,
        resp.body.len()
    );
    if let Some(cookie) = &resp.cookie {
        raw.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    raw.push_str("Connection: keep-alive\r\n\r\n");
    raw.push_str(&resp.body);
    writer.write_all(raw.as_bytes()).ok();
    writer.flush().ok();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// First call with no cached session: client logs in then calls setPreferences.
#[test]
fn test_login_and_set_port() {
    let (url, captured) = spawn_mock_http(vec![
        MockResponse::with_cookie("SID=abc123; Path=/", "Ok."),
        MockResponse::ok(""),
    ]);

    let client = Client::new(&url, "admin", "secret");
    client.set_listen_port(8080).expect("set_listen_port");

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 2, "expected login + setPreferences");
    assert!(
        reqs[0].0.ends_with("/api/v2/auth/login"),
        "first request must be login, got: {}",
        reqs[0].0
    );
    assert!(
        reqs[1].0.ends_with("/api/v2/app/setPreferences"),
        "second request must be setPreferences, got: {}",
        reqs[1].0
    );
    assert!(
        reqs[1].1.contains("8080"),
        "setPreferences body must contain the port, got: {}",
        reqs[1].1
    );
}

/// Server returns "Fails." on login → client returns Err without calling setPreferences.
#[test]
fn test_bad_credentials() {
    let (url, captured) = spawn_mock_http(vec![MockResponse::ok("Fails.")]);

    let client = Client::new(&url, "admin", "wrongpass");
    assert!(
        client.set_listen_port(8080).is_err(),
        "bad credentials must return Err"
    );

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 1, "only login request should have been sent");
    assert!(reqs[0].0.ends_with("/api/v2/auth/login"));
}

/// The second call reuses the cached SID cookie and skips the login round-trip.
#[test]
fn test_cookie_is_cached() {
    let (url, captured) = spawn_mock_http(vec![
        MockResponse::with_cookie("SID=abc123; Path=/", "Ok."), // login
        MockResponse::ok(""),                                   // setPreferences(8080)
        MockResponse::ok(""),                                   // setPreferences(9090) — no login
    ]);

    let client = Client::new(&url, "admin", "secret");
    client.set_listen_port(8080).expect("first call");
    client.set_listen_port(9090).expect("second call");

    let reqs = captured.lock().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "only one login; two setPreferences calls expected"
    );
    assert!(reqs[0].0.ends_with("/api/v2/auth/login"), "initial login");
    assert!(reqs[1].0.ends_with("/api/v2/app/setPreferences"));
    assert!(reqs[2].0.ends_with("/api/v2/app/setPreferences"));
    assert!(
        reqs[2].1.contains("9090"),
        "second setPreferences must carry the new port, got: {}",
        reqs[2].1
    );
}

/// A non-403 HTTP error from setPreferences must propagate as Err without
/// triggering a re-login.  This verifies that `is_auth_error` only matches 403.
///
/// The first call succeeds so that the session cookie is cached.  The second
/// call hits the `is_auth_error` branch (cached cookie path); a 500 response
/// must return Err immediately.  If `is_auth_error` incorrectly returns true
/// for a 500, the client would attempt a fourth re-login request — caught by
/// the request-count assertion.
#[test]
fn test_non_403_server_error_propagates_without_reauth() {
    let (url, captured) = spawn_mock_http(vec![
        MockResponse::with_cookie("SID=tok; Path=/", "Ok."), // call 1: login
        MockResponse::ok(""), // call 1: setPreferences(1111) → OK (cookie cached)
        MockResponse {
            status: 500,
            cookie: None,
            body: "Internal Server Error".into(),
        }, // call 2: setPreferences(2222) → 500
        MockResponse::with_cookie("SID=tok2; Path=/", "Ok."), // only reached if is_auth_error wrongly fires re-login
    ]);

    let client = Client::new(&url, "admin", "secret");
    client
        .set_listen_port(1111)
        .expect("first call must succeed");
    assert!(
        client.set_listen_port(2222).is_err(),
        "500 from setPreferences must return Err"
    );

    let reqs = captured.lock().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "non-403 error must not trigger re-login; got {} requests (expected 3: login + setPreferences(1111) + setPreferences(2222))",
        reqs.len()
    );
    assert!(
        reqs[2].0.ends_with("/api/v2/app/setPreferences"),
        "request 3 must be setPreferences"
    );
}

/// A 403 from setPreferences clears the cached cookie, triggers re-login, and
/// retries the setPreferences call with the new session.
#[test]
fn test_reauthentication_on_403() {
    let (url, captured) = spawn_mock_http(vec![
        MockResponse::with_cookie("SID=first; Path=/", "Ok."), // initial login
        MockResponse::ok(""),                                  // setPreferences(1111) → ok
        MockResponse::forbidden(),                             // setPreferences(2222) → 403
        MockResponse::with_cookie("SID=second; Path=/", "Ok."), // re-login
        MockResponse::ok(""),                                  // setPreferences(2222) → ok
    ]);

    let client = Client::new(&url, "admin", "secret");
    client.set_listen_port(1111).expect("first set_listen_port");
    client
        .set_listen_port(2222)
        .expect("second set_listen_port should succeed after re-auth");

    let reqs = captured.lock().unwrap();
    assert_eq!(reqs.len(), 5, "login + pref + pref(403) + re-login + pref");
    assert!(reqs[0].0.ends_with("/api/v2/auth/login"), "initial login");
    assert!(
        reqs[1].0.ends_with("/api/v2/app/setPreferences"),
        "setPreferences(1111)"
    );
    assert!(
        reqs[2].0.ends_with("/api/v2/app/setPreferences"),
        "setPreferences(2222) → 403"
    );
    assert!(
        reqs[3].0.ends_with("/api/v2/auth/login"),
        "re-login after 403"
    );
    assert!(
        reqs[4].0.ends_with("/api/v2/app/setPreferences"),
        "setPreferences(2222) after re-auth"
    );
    assert!(
        reqs[4].1.contains("2222"),
        "final setPreferences must carry port 2222, got: {}",
        reqs[4].1
    );
}
