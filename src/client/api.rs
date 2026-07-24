//! HTTP + WebSocket control/data plane for agents.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::broker::{Broker, WriteLockView};

#[derive(Clone)]
pub struct ApiState {
    pub broker: Broker,
    /// Default client name for API writes when not specified.
    pub default_writer: String,
    pub history_on_ws_connect: usize,
}

pub fn spawn_api_server(state: ApiState, bind: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // CORS: allow local Vite/Vercel static UIs to call this hub.
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let app = Router::new()
            .route("/v1/health", get(health))
            .route("/v1/status", get(status))
            .route("/v1/clients", get(clients))
            .route("/v1/endpoints", get(endpoints))
            .route("/v1/write", post(write))
            .route("/v1/lock", post(lock).delete(unlock))
            // Unlimited concurrent agents/monitors share the same stream path.
            .route("/v1/stream", get(ws_stream))
            .layer(cors)
            .layer(TraceLayer::new_for_http())
            .with_state(Arc::new(state));

        let addr: SocketAddr = match bind.parse() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("invalid api.bind '{bind}': {e}");
                return;
            }
        };

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("api bind {bind} failed: {e}");
                return;
            }
        };
        tracing::info!("api listening on http://{bind}  (WS /v1/stream)");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("api server error: {e}");
        }
    })
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "service": "ohmyserial" }))
}

async fn status(State(st): State<Arc<ApiState>>) -> impl IntoResponse {
    Json(st.broker.snapshot())
}

async fn clients(State(st): State<Arc<ApiState>>) -> impl IntoResponse {
    let snap = st.broker.snapshot();
    Json(snap.clients)
}

/// List configured fan-out endpoints (virtual serial, TCP, WS, HTTP).
async fn endpoints(State(st): State<Arc<ApiState>>) -> impl IntoResponse {
    let snap = st.broker.snapshot();
    Json(serde_json::json!({
        "real": snap.port,
        "endpoints": snap.endpoints,
        "connected_clients": snap.clients.len(),
    }))
}

#[derive(Debug, Deserialize)]
struct WriteBody {
    /// UTF-8 text to send. If both text and hex set, hex wins.
    #[serde(default)]
    text: Option<String>,
    /// Hex string without spaces, or with spaces.
    #[serde(default)]
    hex: Option<String>,
    /// Act as this client name for lock/policy checks.
    #[serde(default)]
    as_client: Option<String>,
    /// Append newline if missing (default true for text).
    #[serde(default = "default_true")]
    newline: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct WriteResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    bytes: usize,
}

async fn write(
    State(st): State<Arc<ApiState>>,
    Json(body): Json<WriteBody>,
) -> impl IntoResponse {
    let data = if let Some(hex) = body.hex {
        match parse_hex(&hex) {
            Ok(b) => b,
            Err(e) => {
                return Json(WriteResp {
                    ok: false,
                    error: Some(e),
                    bytes: 0,
                });
            }
        }
    } else if let Some(mut text) = body.text {
        if body.newline && !text.ends_with('\n') {
            text.push('\n');
        }
        text.into_bytes()
    } else {
        return Json(WriteResp {
            ok: false,
            error: Some("provide text or hex".into()),
            bytes: 0,
        });
    };

    let who = body
        .as_client
        .as_deref()
        .unwrap_or(&st.default_writer);
    let n = data.len();
    match st.broker.api_write(who, Bytes::from(data)).await {
        Ok(()) => Json(WriteResp {
            ok: true,
            error: None,
            bytes: n,
        }),
        Err(e) => Json(WriteResp {
            ok: false,
            error: Some(e),
            bytes: 0,
        }),
    }
}

#[derive(Debug, Deserialize)]
struct LockBody {
    #[serde(default)]
    as_client: Option<String>,
}

async fn lock(
    State(st): State<Arc<ApiState>>,
    Json(body): Json<LockBody>,
) -> impl IntoResponse {
    let who = body
        .as_client
        .unwrap_or_else(|| st.default_writer.clone());
    match st.broker.acquire_lock(&who) {
        Ok(v) => Json(serde_json::json!({ "ok": true, "lock": v })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

async fn unlock(
    State(st): State<Arc<ApiState>>,
    body: Option<Json<LockBody>>,
) -> impl IntoResponse {
    let who = body
        .and_then(|Json(b)| b.as_client)
        .unwrap_or_else(|| st.default_writer.clone());
    match st.broker.release_lock(Some(&who)) {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

async fn ws_stream(
    ws: WebSocketUpgrade,
    State(st): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, st))
}

async fn handle_ws(socket: WebSocket, st: Arc<ApiState>) {
    let (mut sink, mut stream) = socket.split();
    // Each WebSocket connection is an independent fan-out subscriber.
    let conn_name = format!("ws-{}", uuid::Uuid::new_v4());
    let (id, mut from_broker) =
        st.broker
            .register_client(conn_name, "websocket", true, true, None);

    // Send history first (binary).
    if st.history_on_ws_connect > 0 {
        let hist = st.broker.history_bytes();
        if !hist.is_empty() {
            let take = hist.len().min(st.history_on_ws_connect);
            let slice = hist.slice(hist.len() - take..);
            let _ = sink.send(Message::Binary(slice.to_vec().into())).await;
        }
    }

    let broker_out = st.broker.clone();
    let out = tokio::spawn(async move {
        while let Some(data) = from_broker.recv().await {
            if sink
                .send(Message::Binary(data.to_vec().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Binary(b) => {
                if let Err(e) = st.broker.client_tx(id, Bytes::from(b.to_vec())).await {
                    tracing::warn!("ws tx denied: {e}");
                }
            }
            Message::Text(t) => {
                let mut s = t.to_string();
                if !s.ends_with('\n') {
                    s.push('\n');
                }
                if let Err(e) = st.broker.client_tx(id, Bytes::from(s.into_bytes())).await {
                    tracing::warn!("ws tx denied: {e}");
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    out.abort();
    broker_out.unregister_client(id);
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 {
        return Err("hex length must be even".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| format!("bad hex: {e}"))
        })
        .collect()
}

#[allow(dead_code)]
pub type LockView = WriteLockView;
