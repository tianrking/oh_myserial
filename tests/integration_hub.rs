//! End-to-end tests against mock serial + TCP + HTTP API.

use std::time::Duration;

use ohmyserial::config::Config;
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
    let cfg: Config = toml::from_str(&toml).expect("parse");
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
    let url = url
        .strip_prefix("http://")
        .expect("http");
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
    assert!(status.contains("\"connected\":true") || status.contains("connected\": true"), "status={status}");

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
    assert!(resp.contains("\"ok\":true") || resp.contains("ok\": true"), "write={resp}");

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
        r#"{"text":"yes","as_client":"owner","newline":true}"#,
    )
    .await;
    assert!(ok.contains("true"), "ok={ok}");

    handle.shutdown();
}
