//! RFC2217 wire-level coverage against the in-process mock serial owner.

use std::time::Duration;

use ohmyserial::config::Config;
use ohmyserial::hub;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn read_exact_timeout(stream: &mut TcpStream, bytes: &mut [u8]) {
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(bytes))
        .await
        .expect("RFC2217 read timeout")
        .expect("RFC2217 read failed");
}

#[tokio::test]
async fn rfc2217_negotiates_and_round_trips_raw_serial_bytes() {
    let api_port = free_port().await;
    let rfc_port = free_port().await;
    let mut cfg = Config::default();
    cfg.real.path = "mock:rfc2217".into();
    cfg.clients.clear();
    cfg.api.bind = format!("127.0.0.1:{api_port}");
    cfg.log.mirror_console = false;
    cfg.rfc2217.enabled = true;
    cfg.rfc2217.bind = format!("127.0.0.1:{rfc_port}");
    cfg.rfc2217.can_read = true;
    cfg.rfc2217.can_write = true;
    cfg.validate().expect("valid RFC2217 config");

    let handle = hub::run_hub(cfg).await.expect("hub");
    let mut stream = TcpStream::connect(format!("127.0.0.1:{rfc_port}"))
        .await
        .expect("RFC2217 connect");

    let mut negotiation = [0_u8; 6];
    read_exact_timeout(&mut stream, &mut negotiation).await;
    assert_eq!(negotiation, [255, 251, 44, 255, 253, 44]);

    stream.write_all(b"ping\n").await.unwrap();
    let mut received = [0_u8; 5];
    read_exact_timeout(&mut stream, &mut received).await;
    assert_eq!(&received, b"ping\n");

    handle.shutdown();
}
