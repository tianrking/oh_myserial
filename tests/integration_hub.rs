//! End-to-end tests against mock serial + TCP + HTTP API.

use std::time::Duration;

use ohmyserial::config::{ClientConfig, Config};
use ohmyserial::hub;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(api_port: u16, tcp_port: u16) -> Config {
    let toml = format!(
        r#"
[real]
path = "mock:integration"
baud = 115200

[tx]
mode = "queue_by_line"
write_lock_ms = 2000

[api]
bind = "127.0.0.1:{api_port}"
enabled = true

[[clients]]
type = "tcp"
name = "tcp"
bind = "127.0.0.1:{tcp_port}"
can_write = true
can_read = true

[[clients]]
type = "websocket"
name = "agent"
history_bytes = 4096

[log]
mirror_console = false
format = "text"
"#
    );
    let mut cfg: Config = toml::from_str(&toml).expect("parse");
    cfg.expand_fanout().expect("expand");
    cfg.validate().expect("validate");
    cfg
}

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn http_get(url: &str) -> String {
    let url = url.strip_prefix("http://").expect("http");
    let (host_port, path) = url
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((url, "/".into()));
    let mut stream = TcpStream::connect(host_port).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    text.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

async fn http_post_json(url: &str, body: &str) -> String {
    let url = url.strip_prefix("http://").expect("http");
    let (host_port, path) = url
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((url, "/".into()));
    let mut stream = TcpStream::connect(host_port).await.unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    text.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

#[tokio::test]
async fn mock_loopback_tcp_and_status() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.expect("hub");

    // wait for listeners
    tokio::time::sleep(Duration::from_millis(100)).await;

    let health = http_get(&format!("http://127.0.0.1:{api_port}/v1/health")).await;
    assert!(health.contains("ok"), "health={health}");

    let status = http_get(&format!("http://127.0.0.1:{api_port}/v1/status")).await;
    assert!(status.contains("mock:integration"), "status={status}");
    assert!(
        status.contains("\"connected\":true") || status.contains("connected\": true"),
        "status={status}"
    );

    // TCP client receives mock loopback of HTTP write
    let mut tcp = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .expect("tcp connect");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/write"),
        r#"{"text":"ping-from-api","newline":true}"#,
    )
    .await;
    assert!(
        resp.contains("\"ok\":true") || resp.contains("ok\": true"),
        "write={resp}"
    );

    let mut buf = vec![0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut buf))
        .await
        .expect("timeout")
        .expect("read");
    let got = String::from_utf8_lossy(&buf[..n]);
    assert!(got.contains("ping-from-api"), "tcp got={got:?}");

    handle.shutdown();
}

#[tokio::test]
async fn prometheus_metrics_expose_safe_hub_counters() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.expect("hub");
    let metrics = http_get(&format!("http://127.0.0.1:{api_port}/v1/metrics")).await;
    assert!(metrics.contains("# TYPE ohmyserial_rx_bytes_total counter"));
    assert!(metrics.contains("ohmyserial_port_connected 1"));
    assert!(metrics.contains("ohmyserial_clients_connected"));
    assert!(
        !metrics.contains("mock:integration"),
        "device paths are not labels"
    );
    handle.shutdown();
}

#[tokio::test]
async fn write_lock_blocks_other_client_name() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.expect("hub");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let lock = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/lock"),
        r#"{"as_client":"owner"}"#,
    )
    .await;
    assert!(lock.contains("ok"), "lock={lock}");
    let lock_json: serde_json::Value = serde_json::from_str(&lock).expect("lock json");
    let lease_token = lock_json["lock"]["lease_token"]
        .as_str()
        .expect("lease token");

    let denied = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/write"),
        r#"{"text":"nope","as_client":"other","newline":true}"#,
    )
    .await;
    assert!(
        denied.contains("false") || denied.contains("error"),
        "denied={denied}"
    );

    let ok = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/write"),
        &serde_json::json!({
            "text": "yes",
            "as_client": "owner",
            "lease_token": lease_token,
            "newline": true
        })
        .to_string(),
    )
    .await;
    assert!(ok.contains("true"), "ok={ok}");

    handle.shutdown();
}

#[tokio::test]
async fn http_hex_is_one_atomic_write_without_a_delimiter() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.expect("hub");

    let mut tcp = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .expect("tcp connect");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/write"),
        r#"{"hex":"000102ff"}"#,
    )
    .await;
    assert!(resp.contains("\"ok\":true"), "write={resp}");

    let mut got = [0_u8; 4];
    tokio::time::timeout(Duration::from_secs(2), tcp.read_exact(&mut got))
        .await
        .expect("timeout")
        .expect("read exact");
    assert_eq!(got, [0x00, 0x01, 0x02, 0xff]);

    handle.shutdown();
}

#[tokio::test]
async fn empty_hex_write_is_rejected_before_touching_the_device() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.unwrap();
    let mut tcp = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();

    let response = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/write"),
        r#"{"hex":""}"#,
    )
    .await;
    assert!(
        response.contains("payload must not be empty"),
        "response={response}"
    );
    let mut byte = [0_u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(150), tcp.read(&mut byte))
            .await
            .is_err(),
        "an empty API write must not produce a loopback frame"
    );
    handle.shutdown();
}

#[tokio::test]
async fn embedded_console_and_static_assets_are_served_by_the_api() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.unwrap();

    let index = http_get(&format!("http://127.0.0.1:{api_port}/")).await;
    assert!(index.contains("<div id=\"root\"></div>"), "index={index}");
    let asset = index
        .split("src=\"/")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .expect("embedded script asset");
    let script = http_get(&format!("http://127.0.0.1:{api_port}/{asset}")).await;
    assert!(script.contains("ohmyserial") || script.contains("useState"));

    handle.shutdown();
}

#[tokio::test]
async fn hub_start_fails_when_api_bind_is_occupied() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", api_port))
        .await
        .expect("occupy api port");
    let mut cfg = test_config(api_port, tcp_port);
    let temp = tempfile::tempdir().unwrap();
    let log_path = temp.path().join("startup.log");
    cfg.log.file = Some(log_path.clone());
    cfg.log.mirror_console = false;

    let err = match hub::run_hub(cfg).await {
        Ok(handle) => {
            handle.shutdown();
            panic!("occupied API bind must fail");
        }
        Err(err) => err,
    };
    assert!(err.to_string().contains("api bind"), "error={err:#}");
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(
        !log.contains("serial_open"),
        "serial opened before binds: {log}"
    );
    assert!(
        !log.contains("hub_ready"),
        "hub reported ready after failure: {log}"
    );
    drop(occupied);
}

#[tokio::test]
async fn multi_tcp_clients_all_receive_rx() {
    // One real mock port → one TCP bind → many concurrent TCP clients all get RX.
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let toml = format!(
        r#"
[real]
path = "mock:multi"

[tx]
mode = "queue_by_line"

[api]
bind = "127.0.0.1:{api_port}"
enabled = true

[fanout]
tcp_count = 1
tcp_host = "127.0.0.1"
tcp_base_port = {tcp_port}
tcp_name_prefix = "t"

[log]
mirror_console = false
format = "text"
"#
    );
    let mut cfg: Config = toml::from_str(&toml).unwrap();
    cfg.expand_fanout().unwrap();
    cfg.validate().unwrap();
    let handle = hub::run_hub(cfg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    let mut a = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();
    let mut b = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let _ = http_post_json(
        &format!("http://127.0.0.1:{api_port}/v1/write"),
        r#"{"text":"broadcast-all","newline":true}"#,
    )
    .await;

    let mut buf_a = vec![0u8; 256];
    let mut buf_b = vec![0u8; 256];
    let na = tokio::time::timeout(Duration::from_secs(2), a.read(&mut buf_a))
        .await
        .unwrap()
        .unwrap();
    let nb = tokio::time::timeout(Duration::from_secs(2), b.read(&mut buf_b))
        .await
        .unwrap()
        .unwrap();
    let sa = String::from_utf8_lossy(&buf_a[..na]);
    let sb = String::from_utf8_lossy(&buf_b[..nb]);
    assert!(sa.contains("broadcast-all"), "a={sa:?}");
    assert!(sb.contains("broadcast-all"), "b={sb:?}");

    let eps = http_get(&format!("http://127.0.0.1:{api_port}/v1/endpoints")).await;
    assert!(eps.contains("endpoints"), "eps={eps}");

    handle.shutdown();
}

#[tokio::test]
async fn raw_tcp_ingress_reaches_mock_and_fans_out_exact_bytes() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let cfg = test_config(api_port, tcp_port);
    let handle = hub::run_hub(cfg).await.unwrap();

    let mut origin = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();
    let mut observer = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let expected = b"tcp-origin\n";
    origin.write_all(expected).await.unwrap();

    let mut origin_echo = [0_u8; 11];
    let mut observer_echo = [0_u8; 11];
    tokio::time::timeout(Duration::from_secs(2), origin.read_exact(&mut origin_echo))
        .await
        .expect("origin echo timeout")
        .expect("origin echo read");
    tokio::time::timeout(
        Duration::from_secs(2),
        observer.read_exact(&mut observer_echo),
    )
    .await
    .expect("observer echo timeout")
    .expect("observer echo read");

    assert_eq!(&origin_echo, expected);
    assert_eq!(&observer_echo, expected);
    handle.shutdown();
}

#[tokio::test]
async fn raw_tcp_atomic_mode_accepts_binary_without_delimiter() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let mut cfg = test_config(api_port, tcp_port);
    match &mut cfg.clients[0] {
        ClientConfig::Tcp { raw, .. } => *raw = true,
        other => panic!("expected tcp client, got {other:?}"),
    }
    let handle = hub::run_hub(cfg).await.unwrap();

    let mut origin = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();
    let mut observer = TcpStream::connect(format!("127.0.0.1:{tcp_port}"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let expected = [0x00, 0x01, 0x02, 0xff];
    origin.write_all(&expected).await.unwrap();
    let mut origin_echo = [0_u8; 4];
    let mut observer_echo = [0_u8; 4];
    tokio::time::timeout(Duration::from_secs(2), origin.read_exact(&mut origin_echo))
        .await
        .expect("origin echo timeout")
        .expect("origin echo read");
    tokio::time::timeout(
        Duration::from_secs(2),
        observer.read_exact(&mut observer_echo),
    )
    .await
    .expect("observer echo timeout")
    .expect("observer echo read");
    assert_eq!(origin_echo, expected);
    assert_eq!(observer_echo, expected);
    handle.shutdown();
}

#[cfg(unix)]
#[tokio::test]
async fn pty_prepare_failure_aborts_startup_before_serial_open() {
    let api_port = free_port().await;
    let tcp_port = free_port().await;
    let temp = tempfile::tempdir().unwrap();
    let link = temp.path().join("must-not-replace");
    let log_path = temp.path().join("startup.log");
    std::fs::write(&link, b"keep-me").unwrap();

    let mut cfg = test_config(api_port, tcp_port);
    cfg.log.file = Some(log_path.clone());
    cfg.log.mirror_console = false;
    cfg.clients.push(ohmyserial::config::ClientConfig::Pty {
        name: "broken-pty".into(),
        link: link.clone(),
        can_write: true,
        can_read: true,
    });
    cfg.validate().unwrap();

    let error = match hub::run_hub(cfg).await {
        Ok(handle) => {
            handle.shutdown();
            panic!("regular-file PTY link must make startup fail");
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("non-symlink"), "{error:#}");
    assert_eq!(std::fs::read(&link).unwrap(), b"keep-me");
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(
        !log.contains("serial_open"),
        "serial opened before PTY setup: {log}"
    );
    assert!(
        !log.contains("hub_ready"),
        "failed startup reported ready: {log}"
    );
}
