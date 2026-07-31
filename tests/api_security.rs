//! Live HTTP/WebSocket security regressions against the mock serial hub.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ohmyserial::config::Config;
use ohmyserial::hub;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HOST, ORIGIN, SEC_WEBSOCKET_PROTOCOL};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

const TOKEN: &str = "api-security-test-token";
const ALLOWED_ORIGIN: &str = "https://console.example.test";

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("parse response JSON: {error}; response={self:?}"))
    }
}

async fn unused_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("allocate API address");
    let addr = listener.local_addr().expect("ephemeral API address");
    drop(listener);
    addr
}

fn authenticated_config(api: SocketAddr, cors_origins: &[&str]) -> Config {
    static ENV_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    let env_name = format!(
        "OHMYSERIAL_API_SECURITY_TEST_TOKEN_{}_{}",
        std::process::id(),
        ENV_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    std::env::set_var(&env_name, TOKEN);

    let mut cfg = Config::default();
    cfg.real.path = "mock:api-security".into();
    cfg.clients.clear();
    cfg.api.bind = api.to_string();
    cfg.api.enabled = true;
    cfg.api.token_env = Some(env_name);
    cfg.api.cors_origins = cors_origins
        .iter()
        .map(|origin| (*origin).to_owned())
        .collect();
    cfg.api.can_read = true;
    cfg.api.can_write = true;
    cfg.log.mirror_console = false;
    cfg.validate().expect("valid authenticated test config");
    cfg
}

fn unauthenticated_loopback_config(api: SocketAddr) -> Config {
    let mut cfg = Config::default();
    cfg.real.path = "mock:api-security".into();
    cfg.clients.clear();
    cfg.api.bind = api.to_string();
    cfg.api.enabled = true;
    cfg.api.token_env = None;
    cfg.api.cors_origins.clear();
    cfg.api.can_read = true;
    cfg.api.can_write = true;
    cfg.log.mirror_console = false;
    cfg.validate()
        .expect("valid unauthenticated loopback test config");
    cfg
}

async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    http_request_with_host(addr, &addr.to_string(), method, path, headers, body).await
}

async fn http_request_with_host(
    addr: SocketAddr,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("connect to API");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write HTTP request");

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read HTTP response");
    let raw = String::from_utf8(raw).expect("HTTP response is UTF-8");
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response: {raw:?}"));
    let mut lines = head.lines();
    let status_line = lines.next().expect("HTTP status line");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code")
        .parse()
        .expect("numeric HTTP status");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();

    HttpResponse {
        status,
        headers,
        body: body.to_owned(),
    }
}

fn bearer_header() -> (&'static str, &'static str) {
    ("Authorization", "Bearer api-security-test-token")
}

fn websocket_request(
    addr: SocketAddr,
    origin: &str,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    websocket_request_with_host(addr, &addr.to_string(), origin, Some(token))
}

fn websocket_request_with_host(
    addr: SocketAddr,
    host: &str,
    origin: &str,
    token: Option<&str>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://{addr}/v1/stream")
        .into_client_request()
        .expect("WebSocket request");
    request.headers_mut().insert(
        HOST,
        HeaderValue::from_str(host).expect("valid Host header"),
    );
    request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(origin).expect("valid Origin header"),
    );
    if let Some(token) = token {
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&format!("bearer, {token}")).expect("valid WebSocket protocols"),
        );
    }
    request
}

async fn assert_ws_http_rejection(
    addr: SocketAddr,
    origin: &str,
    token: &str,
    expected: StatusCode,
) {
    match connect_async(websocket_request(addr, origin, token)).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), expected),
        Err(error) => panic!("expected HTTP {expected}, got WebSocket error: {error}"),
        Ok((mut socket, response)) => {
            let _ = socket.close(None).await;
            panic!(
                "expected HTTP {expected}, WebSocket upgraded with status {}",
                response.status()
            );
        }
    }
}

#[tokio::test]
async fn tokenless_loopback_rejects_dns_rebinding_host_for_http_and_websocket() {
    let api = unused_addr().await;
    let cfg = unauthenticated_loopback_config(api);
    let handle = hub::run_hub(cfg)
        .await
        .expect("start tokenless loopback mock hub");

    let actual = http_request(api, "GET", "/v1/health", &[], "").await;
    assert_eq!(actual.status, 200, "actual authority={actual:?}");

    let localhost = format!("localhost:{}", api.port());
    let local = http_request_with_host(api, &localhost, "GET", "/v1/health", &[], "").await;
    assert_eq!(local.status, 200, "localhost authority={local:?}");

    let other_loopback = format!("127.0.0.2:{}", api.port());
    let loopback = http_request_with_host(api, &other_loopback, "GET", "/v1/health", &[], "").await;
    assert_eq!(loopback.status, 200, "loopback authority={loopback:?}");

    let malicious_host = format!("attacker.example.test:{}", api.port());
    let denied = http_request_with_host(api, &malicious_host, "GET", "/v1/health", &[], "").await;
    assert_eq!(denied.status, 403, "malicious Host={denied:?}");

    let wrong_port = http_request_with_host(api, "localhost:1", "GET", "/v1/health", &[], "").await;
    assert_eq!(wrong_port.status, 403, "wrong-port Host={wrong_port:?}");

    // Before the Host gate, matching a malicious Origin to the malicious Host
    // satisfied the WS same-origin check and allowed a DNS-rebinding upgrade.
    let malicious_origin = format!("http://{malicious_host}");
    match connect_async(websocket_request_with_host(
        api,
        &malicious_host,
        &malicious_origin,
        None,
    ))
    .await
    {
        Err(WsError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN)
        }
        Err(error) => panic!("expected HTTP 403, got WebSocket error: {error}"),
        Ok((mut socket, response)) => {
            let _ = socket.close(None).await;
            panic!(
                "DNS-rebinding WebSocket upgraded with status {}",
                response.status()
            );
        }
    }

    handle.shutdown();
}

#[tokio::test]
async fn bearer_protects_status_and_write_but_health_stays_public() {
    let api = unused_addr().await;
    let cfg = authenticated_config(api, &[]);
    let handle = hub::run_hub(cfg)
        .await
        .expect("start authenticated mock hub");
    let mut loopback = handle.broker.subscribe_rx();

    let health = http_request(api, "GET", "/v1/health", &[], "").await;
    assert_eq!(health.status, 200, "health={health:?}");

    let status_denied = http_request(api, "GET", "/v1/status", &[], "").await;
    assert_eq!(status_denied.status, 401, "response={status_denied:?}");
    assert_eq!(status_denied.header("www-authenticate"), Some("Bearer"));

    let status_ok = http_request(api, "GET", "/v1/status", &[bearer_header()], "").await;
    assert_eq!(status_ok.status, 200, "response={status_ok:?}");
    assert_eq!(status_ok.json()["port"]["path"], "mock:api-security");

    let malicious_host = format!("attacker.example.test:{}", api.port());
    let host_denied = http_request_with_host(
        api,
        &malicious_host,
        "GET",
        "/v1/status",
        &[bearer_header()],
        "",
    )
    .await;
    assert_eq!(
        host_denied.status, 403,
        "a valid bearer must not bypass the Host gate: {host_denied:?}"
    );

    let payload = json!({"hex": "00 01 02 ff"}).to_string();
    let write_denied = http_request(
        api,
        "POST",
        "/v1/write",
        &[("Content-Type", "application/json")],
        &payload,
    )
    .await;
    assert_eq!(write_denied.status, 401, "response={write_denied:?}");
    assert!(
        loopback.try_recv().is_err(),
        "unauthorized write reached device"
    );

    let write_ok = http_request(
        api,
        "POST",
        "/v1/write",
        &[bearer_header(), ("Content-Type", "application/json")],
        &payload,
    )
    .await;
    assert_eq!(write_ok.status, 200, "response={write_ok:?}");
    assert_eq!(write_ok.json()["ok"], true);
    let echoed = tokio::time::timeout(Duration::from_secs(2), loopback.recv())
        .await
        .expect("authorized write loopback timeout")
        .expect("mock loopback closed");
    assert_eq!(&echoed[..], &[0x00, 0x01, 0x02, 0xff]);

    handle.shutdown();
}

#[tokio::test]
async fn cors_preflight_allows_only_the_explicit_origin_without_wildcard() {
    let api = unused_addr().await;
    let cfg = authenticated_config(api, &[ALLOWED_ORIGIN]);
    let handle = hub::run_hub(cfg).await.expect("start CORS mock hub");

    let preflight_headers = [
        ("Origin", ALLOWED_ORIGIN),
        ("Access-Control-Request-Method", "POST"),
        (
            "Access-Control-Request-Headers",
            "authorization, content-type",
        ),
    ];
    let allowed = http_request(api, "OPTIONS", "/v1/write", &preflight_headers, "").await;
    assert_eq!(allowed.status, 200, "response={allowed:?}");
    assert_eq!(
        allowed.header("access-control-allow-origin"),
        Some(ALLOWED_ORIGIN),
        "response={allowed:?}"
    );
    assert_ne!(allowed.header("access-control-allow-origin"), Some("*"));

    let denied_headers = [
        ("Origin", "https://evil.example.test"),
        ("Access-Control-Request-Method", "POST"),
        (
            "Access-Control-Request-Headers",
            "authorization, content-type",
        ),
    ];
    let denied = http_request(api, "OPTIONS", "/v1/write", &denied_headers, "").await;
    assert!(
        denied.header("access-control-allow-origin").is_none(),
        "denied origin received CORS permission: {denied:?}"
    );

    handle.shutdown();
}

#[tokio::test]
async fn websocket_requires_allowed_origin_and_bearer_protocol_pair() {
    let api = unused_addr().await;
    let cfg = authenticated_config(api, &[ALLOWED_ORIGIN]);
    let handle = hub::run_hub(cfg)
        .await
        .expect("start WebSocket security mock hub");

    let (mut socket, response) = connect_async(websocket_request(api, ALLOWED_ORIGIN, TOKEN))
        .await
        .expect("allowed browser WebSocket handshake");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok()),
        Some("bearer"),
        "the server must select only the non-secret protocol"
    );
    socket.close(None).await.expect("close accepted WebSocket");

    assert_ws_http_rejection(api, ALLOWED_ORIGIN, "wrong-token", StatusCode::UNAUTHORIZED).await;
    assert_ws_http_rejection(
        api,
        "https://evil.example.test",
        TOKEN,
        StatusCode::FORBIDDEN,
    )
    .await;

    let malicious_host = format!("attacker.example.test:{}", api.port());
    match connect_async(websocket_request_with_host(
        api,
        &malicious_host,
        ALLOWED_ORIGIN,
        Some(TOKEN),
    ))
    .await
    {
        Err(WsError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN)
        }
        Err(error) => panic!("expected Host-gate HTTP 403, got WebSocket error: {error}"),
        Ok((mut socket, response)) => {
            let _ = socket.close(None).await;
            panic!(
                "valid bearer bypassed Host gate with status {}",
                response.status()
            );
        }
    }

    handle.shutdown();
}

#[tokio::test]
async fn websocket_binary_without_delimiter_is_one_atomic_device_write() {
    let api = unused_addr().await;
    let cfg = authenticated_config(api, &[ALLOWED_ORIGIN]);
    let handle = hub::run_hub(cfg).await.expect("start WebSocket mock hub");
    let mut loopback = handle.broker.subscribe_rx();
    let (mut socket, _) = connect_async(websocket_request(api, ALLOWED_ORIGIN, TOKEN))
        .await
        .expect("authenticated WebSocket handshake");

    let frame = vec![0x00, 0x01, 0x02, 0xff];
    socket
        .send(Message::Binary(frame.clone().into()))
        .await
        .expect("send delimiter-free binary frame");

    let echoed = tokio::time::timeout(Duration::from_secs(2), loopback.recv())
        .await
        .expect("WebSocket binary loopback timeout")
        .expect("mock loopback closed");
    assert_eq!(&echoed[..], frame.as_slice());

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
}

#[tokio::test]
async fn hub_shutdown_closes_websocket_unregisters_client_and_prevents_writes() {
    let api = unused_addr().await;
    let cfg = authenticated_config(api, &[ALLOWED_ORIGIN]);
    let handle = hub::run_hub(cfg)
        .await
        .expect("start WebSocket shutdown mock hub");
    let broker = handle.broker.clone();
    let mut loopback = broker.subscribe_rx();
    let (mut socket, _) = connect_async(websocket_request(api, ALLOWED_ORIGIN, TOKEN))
        .await
        .expect("authenticated WebSocket handshake");

    tokio::time::timeout(Duration::from_secs(2), async {
        while broker.snapshot().clients.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("WebSocket client was not registered");

    handle.shutdown();

    let mut saw_close = false;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(_))) => saw_close = true,
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Ok(message)) => panic!("unexpected frame during shutdown: {message:?}"),
                Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) | None => break,
                Some(Err(error)) => panic!("WebSocket shutdown failed: {error}"),
            }
        }
    })
    .await
    .expect("WebSocket did not reach EOF after hub shutdown");
    assert!(saw_close, "server did not send a WebSocket Close frame");

    tokio::time::timeout(Duration::from_secs(2), async {
        while !broker.snapshot().clients.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("WebSocket registration leaked after shutdown");

    assert!(
        socket
            .send(Message::Binary(vec![0xde, 0xad].into()))
            .await
            .is_err(),
        "closed WebSocket accepted a device write"
    );
    assert!(
        loopback.try_recv().is_err(),
        "a post-shutdown WebSocket write reached the device"
    );
    assert!(broker.snapshot().clients.is_empty());
}
