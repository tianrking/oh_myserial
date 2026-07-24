//! Client adapters: TCP, WebSocket/HTTP API, Unix PTY.

mod api;
mod tcp;

#[cfg(unix)]
mod pty;

pub use api::{spawn_api_server, ApiState};
pub use tcp::spawn_tcp_listener;

#[cfg(unix)]
pub use pty::spawn_pty_client;
