//! Client adapters: TCP, WebSocket/HTTP API, static UI, Unix PTY.

mod api;
mod rfc2217;
mod static_ui;
mod tcp;

#[cfg(unix)]
mod pty;

pub use api::{spawn_api_server, spawn_api_server_owned, ApiServerHandle, ApiState};
pub use rfc2217::spawn_rfc2217_listener;
pub use static_ui::ui_embedded;
pub use tcp::spawn_tcp_listener;

#[cfg(unix)]
pub use pty::{prepare_pty_client, spawn_pty_client, PreparedPtyClient};
