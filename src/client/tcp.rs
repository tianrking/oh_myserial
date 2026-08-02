//! Raw TCP client adapter (bidirectional byte stream).

use std::sync::Arc;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::broker::{Broker, ClientRx};

pub async fn spawn_tcp_listener(
    broker: Broker,
    name: String,
    bind: String,
    can_read: bool,
    can_write: bool,
    atomic: bool,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    // Bind eagerly so run_hub can fail atomically instead of advertising a dead
    // endpoint while the listener task merely logs an error.
    let listener = TcpListener::bind(&bind)
        .await
        .map_err(|e| anyhow::anyhow!("tcp bind {bind} failed: {e}"))?;

    Ok(tokio::spawn(async move {
        tracing::info!("tcp client '{name}' listening on {bind}");
        broker
            .log()
            .event(&format!("tcp_listen name={name} bind={bind}"));

        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        tracing::info!("tcp '{name}' accept from {peer}");
                        let broker = broker.clone();
                        let name = name.clone();
                        connections.spawn(async move {
                            if let Err(e) = handle_conn(broker, name, stream, can_read, can_write, atomic).await {
                                tracing::debug!("tcp connection closed: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("tcp accept error: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                },
                completed = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(e)) = completed {
                        tracing::debug!("tcp connection task ended: {e}");
                    }
                }
            }
        }
    }))
}

async fn handle_conn(
    broker: Broker,
    name: String,
    stream: TcpStream,
    can_read: bool,
    can_write: bool,
    atomic: bool,
) -> anyhow::Result<()> {
    let peer = stream.peer_addr()?;
    let (id, mut from_broker) =
        broker.register_client(name, format!("tcp@{peer}"), can_read, can_write, None);
    let _registration = broker.client_registration(id);

    let (mut reader, mut writer) = stream.into_split();

    let mut buf = vec![0u8; 4096];
    loop {
        tokio::select! {
            outbound = from_broker.recv(), if can_read => {
                match outbound {
                    Some(data) => writer.write_all(&data).await?,
                    None => break,
                }
            }
            inbound = reader.read(&mut buf) => {
                let n = inbound?;
                if n == 0 {
                    break;
                }
                if !can_write {
                    writer
                        .write_all(b"!ohmyserial tx denied: client is read-only\n")
                        .await?;
                    continue;
                }
                let result = if atomic {
                    broker
                        .client_tx_atomic(id, Bytes::copy_from_slice(&buf[..n]))
                        .await
                } else {
                    broker
                        .client_tx(id, Bytes::copy_from_slice(&buf[..n]))
                        .await
                };
                if let Err(e) = result {
                    tracing::warn!("tcp tx denied: {e}");
                    writer
                        .write_all(format!("!ohmyserial tx denied: {e}\n").as_bytes())
                        .await?;
                }
            }
        }
    }
    Ok(())
}

/// Test helper: connect and exchange (not used in production path).
#[allow(dead_code)]
pub async fn pipe_channels(mut from_broker: ClientRx, to_client: Arc<tokio::sync::Mutex<Vec<u8>>>) {
    while let Some(data) = from_broker.recv().await {
        to_client.lock().await.extend_from_slice(&data);
    }
}
