//! Raw TCP client adapter (bidirectional byte stream).

use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::broker::Broker;

pub fn spawn_tcp_listener(
    broker: Broker,
    name: String,
    bind: String,
    can_read: bool,
    can_write: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let listener = match TcpListener::bind(&bind).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("tcp bind {bind} failed: {e}");
                return;
            }
        };
        tracing::info!("tcp client '{name}' listening on {bind}");
        broker
            .log()
            .event(&format!("tcp_listen name={name} bind={bind}"));

        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    tracing::info!("tcp '{name}' accept from {peer}");
                    let broker = broker.clone();
                    let name = name.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_conn(broker, name, stream, can_read, can_write).await
                        {
                            tracing::debug!("tcp connection closed: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("tcp accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    })
}

async fn handle_conn(
    broker: Broker,
    name: String,
    stream: TcpStream,
    can_read: bool,
    can_write: bool,
) -> anyhow::Result<()> {
    let (id, mut from_broker) =
        broker.register_client(format!("{name}@{}", stream.peer_addr()?), "tcp", can_read, can_write, None);

    let (mut reader, mut writer) = stream.into_split();
    let broker_r = broker.clone();

    let write_task = tokio::spawn(async move {
        while let Some(data) = from_broker.recv().await {
            if writer.write_all(&data).await.is_err() {
                break;
            }
        }
    });

    let mut buf = vec![0u8; 4096];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if can_write {
            if let Err(e) = broker_r
                .client_tx(id, Bytes::copy_from_slice(&buf[..n]))
                .await
            {
                tracing::warn!("tcp tx denied: {e}");
                // surface error as text line to client
                let msg = format!("!ohmyserial tx denied: {e}\n");
                // cannot easily write to writer here; ignore
                let _ = msg;
            }
        }
    }

    write_task.abort();
    broker.unregister_client(id);
    Ok(())
}

/// Test helper: connect and exchange (not used in production path).
#[allow(dead_code)]
pub async fn pipe_channels(
    mut from_broker: mpsc::Receiver<Bytes>,
    to_client: Arc<tokio::sync::Mutex<Vec<u8>>>,
) {
    while let Some(data) = from_broker.recv().await {
        to_client.lock().await.extend_from_slice(&data);
    }
}
