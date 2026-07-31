//! Trusted-core regressions exercised through the crate's public interfaces.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use ohmyserial::broker::{Broker, DeviceWrite, PortStatus};
use ohmyserial::config::Config;
use ohmyserial::hub;
use ohmyserial::observe::SessionLog;
use ohmyserial::policy::{Policy, SlowClientPolicy, TxMode};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

fn test_broker(connected: bool) -> (Broker, mpsc::Receiver<DeviceWrite>) {
    let policy = Policy {
        mode: TxMode::QueueByLine,
        primary: None,
        write_lock_ms: 2_000,
        write_timeout_ms: 2_000,
        max_frame_bytes: 1_024,
        max_write_bytes: 1_024,
        frame_delim: b'\n',
        slow_client: SlowClientPolicy::DropOldest,
        client_queue: 16,
        slow_block_ms: 100,
    };
    let port = PortStatus {
        path: "mock:trusted-core".into(),
        baud: 115_200,
        connected,
        detail: if connected { "open" } else { "disconnected" }.into(),
    };
    let split = Broker::new(policy, port, SessionLog::disabled(), 1_024, 32);
    (split.broker, split.serial_tx_rx)
}

async fn unused_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("allocate ephemeral port");
    let addr = listener.local_addr().expect("ephemeral address");
    drop(listener);
    addr
}

fn hub_config(api: SocketAddr, tcp: Option<SocketAddr>, log_file: Option<&Path>) -> Config {
    let tcp_client = tcp
        .map(|addr| {
            format!(
                r#"
[[clients]]
type = "tcp"
name = "trusted-tcp"
bind = "{addr}"
can_write = true
can_read = true
"#
            )
        })
        .unwrap_or_default();

    let text = format!(
        r#"
[real]
path = "mock:trusted-core"
baud = 115200

[tx]
mode = "queue_by_line"
write_lock_ms = 2000

[api]
bind = "{api}"
enabled = true

{tcp_client}

[log]
mirror_console = false
format = "text"
"#
    );
    let mut cfg: Config = toml::from_str(&text).expect("parse trusted-core config");
    cfg.log.file = log_file.map(Path::to_path_buf);
    cfg.expand_fanout().expect("expand fanout");
    cfg.validate().expect("validate trusted-core config");
    cfg
}

async fn http_json(addr: SocketAddr, method: &str, path: &str, body: Value) -> Value {
    let encoded = body.to_string();
    let mut stream = TcpStream::connect(addr).await.expect("connect to API");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{encoded}",
        encoded.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write HTTP request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read HTTP response");
    let response = String::from_utf8(response).expect("HTTP response is UTF-8");
    let payload = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response body");
    serde_json::from_str(payload)
        .unwrap_or_else(|error| panic!("parse JSON response: {error}; response={response:?}"))
}

fn assert_starting_but_not_ready(log_file: &Path) {
    let log = std::fs::read_to_string(log_file).expect("read startup session log");
    assert!(log.contains("hub_starting"), "startup log={log:?}");
    assert!(
        !log.contains("hub_ready"),
        "failed hub must not announce readiness; startup log={log:?}"
    );
}

#[tokio::test]
async fn disconnected_write_is_rejected_and_never_replayed_after_reconnect() {
    let (broker, mut serial_rx) = test_broker(false);
    let (client, _client_rx) = broker.register_client("writer", "trusted-test", false, true, None);

    let error = broker
        .client_tx_atomic(client, Bytes::from_static(b"stale-command"))
        .await
        .expect_err("a disconnected port must reject writes");
    assert!(error.contains("disconnected"), "error={error:?}");
    assert!(error.contains("not queued"), "error={error:?}");
    assert_eq!(broker.snapshot().stats.tx_denies, 1);
    assert!(
        serial_rx.try_recv().is_err(),
        "stale write entered TX queue"
    );

    broker.set_port_status(PortStatus {
        path: "mock:trusted-core".into(),
        baud: 115_200,
        connected: true,
        detail: "reconnected".into(),
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(75), serial_rx.recv())
            .await
            .is_err(),
        "a rejected write was replayed after reconnect"
    );

    broker
        .client_tx_atomic(client, Bytes::from_static(b"fresh-command"))
        .await
        .expect("fresh write after reconnect");
    let fresh = tokio::time::timeout(Duration::from_secs(1), serial_rx.recv())
        .await
        .expect("fresh write timeout")
        .expect("serial queue closed");
    assert_eq!(fresh.bytes(), &Bytes::from_static(b"fresh-command"));
}

#[tokio::test]
async fn http_hex_write_without_delimiter_is_one_atomic_unit() {
    let api = unused_addr().await;
    let cfg = hub_config(api, None, None);
    let handle = hub::run_hub(cfg).await.expect("start mock hub");
    let mut loopback = handle.broker.subscribe_rx();

    let response = http_json(api, "POST", "/v1/write", json!({"hex": "00 01 02 ff"})).await;
    assert_eq!(response["ok"], true, "response={response}");
    assert_eq!(response["bytes"], 4, "response={response}");

    let bytes = tokio::time::timeout(Duration::from_secs(2), loopback.recv())
        .await
        .expect("mock loopback timeout")
        .expect("mock loopback closed");
    assert_eq!(&bytes[..], &[0x00, 0x01, 0x02, 0xff]);
    handle.shutdown();
}

#[tokio::test]
async fn matching_owner_name_cannot_impersonate_a_lease_token() {
    let api = unused_addr().await;
    let cfg = hub_config(api, None, None);
    let handle = hub::run_hub(cfg).await.expect("start mock hub");
    let mut loopback = handle.broker.subscribe_rx();

    let acquired = http_json(api, "POST", "/v1/lock", json!({"as_client": "owner"})).await;
    let token = acquired["lock"]["lease_token"]
        .as_str()
        .expect("lease token")
        .to_owned();

    let spoofed = http_json(
        api,
        "POST",
        "/v1/write",
        json!({"hex": "aa", "as_client": "owner"}),
    )
    .await;
    assert_eq!(spoofed["ok"], false, "response={spoofed}");
    assert!(
        spoofed["error"]
            .as_str()
            .is_some_and(|error| error.contains("lease token")),
        "response={spoofed}"
    );
    assert!(loopback.try_recv().is_err(), "spoofed write reached device");
    assert_eq!(
        handle.broker.snapshot().lock_owner.as_deref(),
        Some("owner")
    );

    // The token is the authority; the display name is not.
    let authorized = http_json(
        api,
        "POST",
        "/v1/write",
        json!({
            "hex": "bb",
            "as_client": "different-display-name",
            "lease_token": token
        }),
    )
    .await;
    assert_eq!(authorized["ok"], true, "response={authorized}");
    let bytes = tokio::time::timeout(Duration::from_secs(2), loopback.recv())
        .await
        .expect("authorized loopback timeout")
        .expect("authorized loopback closed");
    assert_eq!(&bytes[..], &[0xbb]);
    handle.shutdown();
}

#[tokio::test]
async fn wrong_lease_token_cannot_renew_or_release() {
    let api = unused_addr().await;
    let cfg = hub_config(api, None, None);
    let handle = hub::run_hub(cfg).await.expect("start mock hub");

    let acquired = http_json(api, "POST", "/v1/lock", json!({"as_client": "owner"})).await;
    let token = acquired["lock"]["lease_token"]
        .as_str()
        .expect("lease token")
        .to_owned();

    let bad_renew = http_json(
        api,
        "POST",
        "/v1/lock",
        json!({"lease_token": "definitely-not-the-token"}),
    )
    .await;
    assert_eq!(bad_renew["ok"], false, "response={bad_renew}");
    assert_eq!(
        handle.broker.snapshot().lock_owner.as_deref(),
        Some("owner")
    );

    let bad_release = http_json(
        api,
        "DELETE",
        "/v1/lock",
        json!({"lease_token": "definitely-not-the-token"}),
    )
    .await;
    assert_eq!(bad_release["ok"], false, "response={bad_release}");
    assert_eq!(
        handle.broker.snapshot().lock_owner.as_deref(),
        Some("owner")
    );

    let renewed = http_json(api, "POST", "/v1/lock", json!({"lease_token": token})).await;
    assert_eq!(renewed["ok"], true, "response={renewed}");
    let token = renewed["lock"]["lease_token"]
        .as_str()
        .expect("renewed lease token")
        .to_owned();

    let released = http_json(api, "DELETE", "/v1/lock", json!({"lease_token": token})).await;
    assert_eq!(released["ok"], true, "response={released}");
    assert!(handle.broker.snapshot().lock_owner.is_none());
    handle.shutdown();
}

#[tokio::test]
async fn occupied_api_bind_returns_error_without_announcing_ready() {
    let occupied = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("occupy API port");
    let api = occupied.local_addr().expect("occupied API address");
    let temp = tempfile::tempdir().expect("startup log tempdir");
    let log_file = temp.path().join("api-bind.log");
    let cfg = hub_config(api, None, Some(&log_file));

    let error = match hub::run_hub(cfg).await {
        Ok(handle) => {
            handle.shutdown();
            panic!("occupied API bind unexpectedly started")
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("api bind"), "error={error:#}");
    assert_starting_but_not_ready(&log_file);
    drop(occupied);
}

#[tokio::test]
async fn occupied_tcp_bind_returns_error_without_announcing_ready() {
    let occupied = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("occupy TCP port");
    let tcp = occupied.local_addr().expect("occupied TCP address");
    let api = unused_addr().await;
    let temp = tempfile::tempdir().expect("startup log tempdir");
    let log_file = temp.path().join("tcp-bind.log");
    let cfg = hub_config(api, Some(tcp), Some(&log_file));

    let error = match hub::run_hub(cfg).await {
        Ok(handle) => {
            handle.shutdown();
            panic!("occupied TCP bind unexpectedly started")
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("tcp bind"), "error={error:#}");
    assert_starting_but_not_ready(&log_file);
    drop(occupied);
}
