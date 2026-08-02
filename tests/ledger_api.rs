//! End-to-end coverage for the canonical event ledger HTTP/WebSocket API.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use ohmyserial::broker::{Broker, DeviceWrite, PortStatus};
use ohmyserial::client::{spawn_api_server_owned, ApiServerHandle, ApiState};
use ohmyserial::ledger::{Ledger, LedgerOptions, MemoryOptions, StoreOptions};
use ohmyserial::observe::SessionLog;
use ohmyserial::policy::{Policy, SlowClientPolicy, TxMode};
use ohmyserial::workflow::{WorkflowLimits, WorkflowRunner};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

const ALLOWED_ORIGIN: &str = "https://console.example.test";

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("parse response JSON: {error}; response={self:?}"))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[derive(Clone)]
struct ApiAccess {
    bearer_token: Option<String>,
    cors_origins: Vec<String>,
    can_read: bool,
    can_write: bool,
    can_control: bool,
    ws_can_read: bool,
    ws_can_write: bool,
}

impl Default for ApiAccess {
    fn default() -> Self {
        Self {
            bearer_token: None,
            cors_origins: vec![ALLOWED_ORIGIN.to_owned()],
            can_read: true,
            can_write: true,
            can_control: false,
            ws_can_read: true,
            ws_can_write: true,
        }
    }
}

struct TestServer {
    addr: SocketAddr,
    broker: Broker,
    _serial_rx: mpsc::Receiver<DeviceWrite>,
    api: ApiServerHandle,
}

impl TestServer {
    async fn shutdown(self) {
        self.api.shutdown().await.expect("stop API server");
    }
}

fn test_policy() -> Policy {
    Policy {
        mode: TxMode::QueueByLine,
        primary: None,
        write_lock_ms: 10_000,
        write_timeout_ms: 2_000,
        max_frame_bytes: 4_096,
        max_write_bytes: 4_096,
        frame_delim: b'\n',
        slow_client: SlowClientPolicy::DropOldest,
        client_queue: 32,
        slow_block_ms: 100,
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

async fn start_server(ledger: Ledger, access: ApiAccess) -> TestServer {
    let port = PortStatus {
        path: "mock:ledger-api".into(),
        baud: 115_200,
        connected: true,
        detail: "open".into(),
    };
    let split = Broker::new_with_ledger(
        test_policy(),
        port,
        SessionLog::disabled(),
        4_096,
        32,
        ledger,
    );
    let addr = unused_addr().await;
    let api = spawn_api_server_owned(
        ApiState {
            broker: split.broker.clone(),
            workflow_runner: WorkflowRunner::new(WorkflowLimits::default())
                .expect("workflow limits"),
            default_writer: "api-test".into(),
            ws_writer: "ws-test".into(),
            history_on_ws_connect: 4_096,
            bearer_token: access.bearer_token,
            cors_origins: access.cors_origins,
            can_read: access.can_read,
            can_write: access.can_write,
            can_control: access.can_control,
            ws_can_read: access.ws_can_read,
            ws_can_write: access.ws_can_write,
        },
        addr.to_string(),
    )
    .await
    .expect("start API server");
    TestServer {
        addr,
        broker: split.broker,
        _serial_rx: split.serial_tx_rx,
        api,
    }
}

async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("connect to API");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
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
    let status = lines
        .next()
        .expect("HTTP status line")
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

fn websocket_request(
    addr: SocketAddr,
    path: &str,
    origin: Option<&str>,
    bearer_token: Option<&str>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let mut request = format!("ws://{addr}{path}")
        .into_client_request()
        .expect("WebSocket request");
    if let Some(origin) = origin {
        request.headers_mut().insert(
            ORIGIN,
            HeaderValue::from_str(origin).expect("valid Origin header"),
        );
    }
    if let Some(token) = bearer_token {
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&format!("bearer, {token}")).expect("valid WebSocket protocols"),
        );
    }
    request
}

async fn assert_ws_rejected(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
    expected: StatusCode,
) {
    match connect_async(request).await {
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

fn memory_ledger(max_events: usize) -> Ledger {
    Ledger::memory(MemoryOptions {
        max_events,
        max_bytes: 1024 * 1024,
    })
    .expect("valid memory ledger")
}

#[tokio::test]
async fn status_query_filters_and_pagination_preserve_canonical_bytes() {
    let ledger = memory_ledger(64);
    let server = start_server(ledger.clone(), ApiAccess::default()).await;

    server
        .broker
        .record_control(Some("alice".into()), "phase", Some("one".into()))
        .expect("append first control");
    server
        .broker
        .on_device_rx(Bytes::from_static(&[0x00, 0xff]));
    server
        .broker
        .record_control(Some("bob".into()), "phase", Some("two".into()))
        .expect("append second control");

    let status = http_request(server.addr, "GET", "/v1/events/status", &[], "").await;
    assert_eq!(status.status, 200, "response={status:?}");
    let status_json = status.json();
    assert_eq!(status_json["newest_seq"], 3);
    assert_eq!(status_json["oldest_available_seq"], 1);
    assert_eq!(status_json["retained_events"], 3);
    assert_eq!(status_json["persistence"], "disabled");

    let first = http_request(
        server.addr,
        "GET",
        "/v1/events?after_seq=0&limit=2",
        &[],
        "",
    )
    .await;
    assert_eq!(first.status, 200, "response={first:?}");
    let first = first.json();
    assert_eq!(first["page"]["events"].as_array().unwrap().len(), 2);
    assert_eq!(first["page"]["next_after_seq"], 2);
    assert_eq!(first["page"]["has_more"], true);

    let second = http_request(
        server.addr,
        "GET",
        "/v1/events?after_seq=2&limit=2",
        &[],
        "",
    )
    .await
    .json();
    assert_eq!(second["page"]["events"].as_array().unwrap().len(), 1);
    assert_eq!(second["page"]["events"][0]["seq"], 3);
    assert_eq!(second["page"]["has_more"], false);

    let actor = http_request(
        server.addr,
        "GET",
        "/v1/events?type=control&actor=alice&connection_epoch=1",
        &[],
        "",
    )
    .await
    .json();
    assert_eq!(actor["page"]["events"].as_array().unwrap().len(), 1);
    assert_eq!(actor["page"]["events"][0]["payload"]["actor"], "alice");

    let bytes = http_request(
        server.addr,
        "GET",
        "/v1/events?type=rx&contains_hex=00ff",
        &[],
        "",
    )
    .await
    .json();
    assert_eq!(bytes["page"]["events"].as_array().unwrap().len(), 1);
    assert_eq!(bytes["page"]["events"][0]["type"], "rx");
    assert_eq!(bytes["page"]["events"][0]["payload"]["data_base64"], "AP8=");
    assert_eq!(bytes["page"]["events"][0]["payload"]["len"], 2);

    let combined = http_request(server.addr, "GET", "/v1/events?type=rx,control", &[], "")
        .await
        .json();
    assert_eq!(combined["page"]["events"].as_array().unwrap().len(), 3);

    let invalid = http_request(server.addr, "GET", "/v1/events?type=unknown", &[], "").await;
    assert_eq!(invalid.status, 400, "response={invalid:?}");

    server.shutdown().await;
}

#[tokio::test]
async fn evicted_memory_cursor_and_export_return_gone() {
    let ledger = memory_ledger(2);
    let server = start_server(ledger, ApiAccess::default()).await;
    for index in 0..3 {
        server
            .broker
            .record_control(None, "evict", Some(index.to_string()))
            .expect("append event");
    }

    let query = http_request(server.addr, "GET", "/v1/events?after_seq=0", &[], "").await;
    assert_eq!(query.status, 410, "response={query:?}");
    let query = query.json();
    assert_eq!(query["page"]["incomplete"], true);
    assert_eq!(query["page"]["missing_through_seq"], 1);
    assert_eq!(query["page"]["oldest_available_seq"], 2);

    let export = http_request(server.addr, "GET", "/v1/events/export", &[], "").await;
    assert_eq!(export.status, 410, "response={export:?}");
    assert!(export.body.contains("in-memory ledger was evicted"));

    server.shutdown().await;
}

#[tokio::test]
async fn persisted_query_checkpoints_full_history_and_export_is_ndjson() {
    let directory = tempfile::tempdir().expect("ledger directory");
    let ledger = Ledger::open(LedgerOptions {
        memory: MemoryOptions {
            max_events: 2,
            max_bytes: 1024 * 1024,
        },
        stream_capacity: 32,
        store: Some(StoreOptions {
            directory: directory.path().to_path_buf(),
            segment_max_bytes: 1024 * 1024,
            segment_max_events: 100,
            flush_every_events: 1,
            fsync_on_flush: false,
        }),
        session_id: None,
    })
    .expect("persistent ledger");
    let server = start_server(ledger, ApiAccess::default()).await;
    for index in 0..4 {
        server
            .broker
            .record_control(Some("persist".into()), "step", Some(index.to_string()))
            .expect("append persistent event");
    }

    let query = http_request(server.addr, "GET", "/v1/events?after_seq=0", &[], "").await;
    assert_eq!(query.status, 200, "response={query:?}");
    let query = query.json();
    let events = query["page"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events.first().unwrap()["seq"], 1);
    assert_eq!(events.last().unwrap()["seq"], 4);
    assert_eq!(query["page"]["incomplete"], false);

    let export = http_request(server.addr, "GET", "/v1/events/export", &[], "").await;
    assert_eq!(export.status, 200, "response={export:?}");
    assert_eq!(
        export.header("content-type"),
        Some("application/x-ndjson; charset=utf-8")
    );
    let lines = export
        .body
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid NDJSON event"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["schema"], "ohmyserial.event");
    assert_eq!(lines[3]["seq"], 4);

    server.shutdown().await;
}

#[tokio::test]
async fn event_websocket_filters_live_events_honors_upper_bound_and_raw_stream_stays_binary() {
    let ledger = memory_ledger(64);
    let server = start_server(ledger.clone(), ApiAccess::default()).await;
    let through_seq = ledger.status().newest_seq + 2;
    let path = format!(
        "/v1/events/stream?after_seq={}&through_seq={through_seq}&type=rx",
        ledger.status().newest_seq
    );
    let (mut events, response) = connect_async(websocket_request(
        server.addr,
        &path,
        Some(ALLOWED_ORIGIN),
        None,
    ))
    .await
    .expect("connect event WebSocket");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

    server
        .broker
        .record_control(Some("filtered".into()), "not-rx", None)
        .expect("append filtered control");
    server
        .broker
        .on_device_rx(Bytes::from_static(&[0x10, 0x20]));
    server.broker.on_device_rx(Bytes::from_static(&[0x30]));

    let event = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("event WebSocket frame timeout")
        .expect("event WebSocket remains open")
        .expect("valid event WebSocket frame");
    let Message::Text(text) = event else {
        panic!("event stream must use JSON text frames, got {event:?}");
    };
    let event: Value = serde_json::from_str(text.as_str()).expect("event JSON");
    assert_eq!(event["type"], "rx");
    assert_eq!(event["seq"], through_seq);
    assert_eq!(event["payload"]["data_base64"], "ECA=");

    let close = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("bounded stream close timeout")
        .expect("bounded stream close frame")
        .expect("valid bounded stream close frame");
    assert!(matches!(close, Message::Close(_)), "frame={close:?}");

    let (mut raw, _) = connect_async(websocket_request(
        server.addr,
        "/v1/stream",
        Some(ALLOWED_ORIGIN),
        None,
    ))
    .await
    .expect("connect raw WebSocket");
    let raw_frame = tokio::time::timeout(Duration::from_secs(2), raw.next())
        .await
        .expect("raw history timeout")
        .expect("raw WebSocket remains open")
        .expect("valid raw frame");
    let Message::Binary(raw_bytes) = raw_frame else {
        panic!("raw stream must remain binary, got {raw_frame:?}");
    };
    assert_eq!(raw_bytes.as_ref(), &[0x10, 0x20, 0x30]);
    raw.close(None).await.expect("close raw WebSocket");

    assert_ws_rejected(
        websocket_request(
            server.addr,
            "/v1/events/stream",
            Some("https://evil.example.test"),
            None,
        ),
        StatusCode::FORBIDDEN,
    )
    .await;

    server.shutdown().await;
}

#[tokio::test]
async fn event_api_enforces_permissions_and_never_serializes_bearer_or_lease_tokens() {
    const API_TOKEN: &str = "event-api-super-secret";
    let access = ApiAccess {
        bearer_token: Some(API_TOKEN.into()),
        ..ApiAccess::default()
    };
    let server = start_server(memory_ledger(64), access).await;

    let unauthenticated = http_request(server.addr, "GET", "/v1/events/status", &[], "").await;
    assert_eq!(unauthenticated.status, 401, "response={unauthenticated:?}");
    assert_ws_rejected(
        websocket_request(server.addr, "/v1/events/stream", Some(ALLOWED_ORIGIN), None),
        StatusCode::UNAUTHORIZED,
    )
    .await;

    let authorization = format!("Bearer {API_TOKEN}");
    let lock = http_request(
        server.addr,
        "POST",
        "/v1/lock",
        &[
            ("Authorization", authorization.as_str()),
            ("Content-Type", "application/json"),
        ],
        &json!({"as_client": "lease-owner"}).to_string(),
    )
    .await;
    assert_eq!(lock.status, 200, "response={lock:?}");
    let lease_token = lock.json()["lock"]["lease_token"]
        .as_str()
        .expect("lease token")
        .to_owned();

    let evidence = http_request(
        server.addr,
        "GET",
        "/v1/events?type=control&actor=lease-owner",
        &[("Authorization", authorization.as_str())],
        "",
    )
    .await;
    assert_eq!(evidence.status, 200, "response={evidence:?}");
    assert!(evidence.body.contains("lease_acquired"));
    assert!(!evidence.body.contains(API_TOKEN), "response={evidence:?}");
    assert!(
        !evidence.body.contains(&lease_token),
        "lease token leaked into event evidence: {evidence:?}"
    );

    let (mut stream, response) = connect_async(websocket_request(
        server.addr,
        "/v1/events/stream?type=control&actor=lease-owner",
        Some(ALLOWED_ORIGIN),
        Some(API_TOKEN),
    ))
    .await
    .expect("authenticated event WebSocket");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok()),
        Some("bearer")
    );
    let frame = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("authenticated event timeout")
        .expect("authenticated event stream remains open")
        .expect("valid authenticated event");
    let Message::Text(text) = frame else {
        panic!("event stream must use text frames, got {frame:?}");
    };
    assert!(!text.contains(API_TOKEN));
    assert!(!text.contains(&lease_token));
    stream.close(None).await.expect("close event WebSocket");
    server.shutdown().await;

    let denied_access = ApiAccess {
        can_read: false,
        ..ApiAccess::default()
    };
    let denied = start_server(memory_ledger(8), denied_access).await;
    let response = http_request(denied.addr, "GET", "/v1/events/status", &[], "").await;
    assert_eq!(response.status, 403, "response={response:?}");
    assert_ws_rejected(
        websocket_request(denied.addr, "/v1/events/stream", Some(ALLOWED_ORIGIN), None),
        StatusCode::FORBIDDEN,
    )
    .await;
    denied.shutdown().await;
}

#[tokio::test]
async fn workflow_api_runs_linear_expect_and_is_idempotent_without_token_leak() {
    let server = start_server(memory_ledger(64), ApiAccess::default()).await;
    let body = json!({
        "request_id": "workflow-api-1",
        "workflow": {
            "id": "wait-for-ok",
            "steps": [
                {"op": "lease"},
                {"op": "expect", "pattern": {"text": "OK"}, "timeout_ms": 1000, "capture": "reply"}
            ]
        }
    })
    .to_string();
    let body_for_request = body.clone();
    let addr = server.addr;
    let request = tokio::spawn(async move {
        http_request(
            addr,
            "POST",
            "/v1/workflows/run",
            &[("Content-Type", "application/json")],
            &body_for_request,
        )
        .await
    });
    tokio::task::yield_now().await;
    for _ in 0..100 {
        if server.broker.ledger().status().newest_seq > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    server.broker.on_device_rx(Bytes::from_static(b"O"));
    server.broker.on_device_rx(Bytes::from_static(b"K"));
    let response = request.await.unwrap();
    assert_eq!(response.status, 200, "response={response:?}");
    assert_eq!(response.json()["result"]["status"], "succeeded");
    assert!(response.json()["result"]["actor"]
        .as_str()
        .unwrap()
        .starts_with("workflow:"));
    assert!(!response.body.contains("lease_token"));

    let replay = http_request(
        server.addr,
        "POST",
        "/v1/workflows/run",
        &[("Content-Type", "application/json")],
        &body,
    )
    .await;
    assert_eq!(replay.status, 200, "response={replay:?}");
    assert_eq!(replay.json()["result"], response.json()["result"]);
    server.shutdown().await;
}

#[tokio::test]
async fn control_api_requires_capability_lease_and_owner_ack() {
    let access = ApiAccess {
        can_control: true,
        ..ApiAccess::default()
    };
    let server = start_server(memory_ledger(64), access).await;
    let (control_tx, mut control_rx) = mpsc::channel(1);
    server.broker.attach_serial_control(control_tx);

    let lock = http_request(
        server.addr,
        "POST",
        "/v1/lock",
        &[("Content-Type", "application/json")],
        &json!({"as_client": "control-agent"}).to_string(),
    )
    .await;
    let lease_token = lock.json()["lock"]["lease_token"]
        .as_str()
        .expect("lease token")
        .to_owned();
    let body = json!({
        "op": "dtr",
        "level": true,
        "as_client": "control-agent",
        "lease_token": lease_token,
    })
    .to_string();
    let request = {
        let addr = server.addr;
        tokio::spawn(async move {
            http_request(
                addr,
                "POST",
                "/v1/control",
                &[("Content-Type", "application/json")],
                &body,
            )
            .await
        })
    };
    let command = control_rx.recv().await.expect("owner control command");
    let acknowledgement = match command {
        ohmyserial::broker::SerialControl::Command {
            command: ohmyserial::broker::ControlCommand::Dtr(true),
            acknowledgement,
        } => acknowledgement,
        _ => panic!("unexpected control command"),
    };
    acknowledgement.send(Ok(())).unwrap();
    let response = request.await.unwrap();
    assert_eq!(response.status, 200, "response={response:?}");
    assert_eq!(response.json()["ok"], true);
    assert!(!response.body.contains("lease_token"));
    server.shutdown().await;
}
