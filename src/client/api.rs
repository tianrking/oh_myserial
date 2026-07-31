//! HTTP + WebSocket control/data plane for agents.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Request;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN, SEC_WEBSOCKET_PROTOCOL};
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::broker::{Broker, WriteLockView};
use crate::client::static_ui::{static_handler, ui_embedded};

#[derive(Clone)]
pub struct ApiState {
    pub broker: Broker,
    /// Default client name for API writes when not specified.
    pub default_writer: String,
    /// Server-configured identity and permissions for WebSocket clients.
    pub ws_writer: String,
    pub history_on_ws_connect: usize,
    /// Resolved bearer secret. It is populated from ApiConfig::token_env and
    /// must never be logged or serialized.
    pub bearer_token: Option<String>,
    pub cors_origins: Vec<String>,
    pub can_read: bool,
    pub can_write: bool,
    pub ws_can_read: bool,
    pub ws_can_write: bool,
}

/// An API server with an explicit graceful-shutdown signal.
///
/// Dropping the handle also cancels the server. This matters to callers that
/// abort a supervisor task: upgraded WebSockets run independently of the HTTP
/// accept loop, so they must observe the same cancellation signal themselves.
pub struct ApiServerHandle {
    shutdown: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ApiServerHandle {
    /// Ask the HTTP accept loop and all upgraded WebSockets to stop.
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Gracefully stop the server and wait until the accept loop and upgraded
    /// connections have exited.
    pub async fn shutdown(mut self) -> Result<(), tokio::task::JoinError> {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            task.await?;
        }
        Ok(())
    }

    async fn wait(mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ApiServerHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
        // Detach the accept-loop task after signalling it. Aborting it here
        // would tear down the underlying HTTP connection before upgraded
        // WebSockets can flush their Close handshake.
        let _ = self.task.take();
    }
}

#[derive(Clone, Copy)]
struct HostGateState {
    listen_addr: SocketAddr,
}

pub async fn spawn_api_server(
    state: ApiState,
    bind: String,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let server = spawn_api_server_owned(state, bind).await?;
    // Compatibility adapter for existing hub supervisors. If they abort this
    // task, dropping `server` cancels upgraded WebSockets as well.
    Ok(tokio::spawn(server.wait()))
}

pub async fn spawn_api_server_owned(
    state: ApiState,
    bind: String,
) -> anyhow::Result<ApiServerHandle> {
    // Parse and bind before returning the task. A hub must never report ready
    // when its advertised control plane failed to start.
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid api.bind '{bind}': {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("api bind {bind} failed: {e}"))?;
    let listen_addr = listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("api bind {bind} has no local address: {e}"))?;

    let cors = build_cors(&state.cors_origins)?;
    let host_gate = HostGateState { listen_addr };
    let state = Arc::new(state);
    let shutdown = CancellationToken::new();

    let mut app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/clients", get(clients))
        .route("/v1/endpoints", get(endpoints))
        .route("/v1/write", post(write))
        .route("/v1/lock", post(lock).delete(unlock))
        // Unlimited concurrent agents/monitors share the same stream path.
        .route("/v1/stream", get(ws_stream))
        // React console embedded from web/dist (same origin as API).
        .fallback(static_handler)
        // Authentication applies to every /v1 route except health. Static UI
        // assets remain public and receive no bearer credential handling.
        .layer(middleware::from_fn_with_state(state.clone(), authorize_v1))
        .layer(TraceLayer::new_for_http())
        .layer(Extension(shutdown.clone()))
        .with_state(state);

    // With no configured origins there is no CORS layer at all: browser calls
    // are same-origin only. WebSocket Origin is checked separately below.
    if let Some(cors) = cors {
        app = app.layer(cors);
    }

    // This is deliberately outside CORS and authentication so every HTTP
    // request, including health, static assets, preflights, and WS upgrades,
    // checks the actual loopback listener authority. A bearer must not turn a
    // malicious Host into a trusted browser origin.
    app = app.layer(middleware::from_fn_with_state(host_gate, enforce_safe_host));

    let shutdown_signal = shutdown.clone();
    let task = tokio::spawn(async move {
        if ui_embedded() {
            tracing::info!("api+ui listening on http://{listen_addr}/  (WS /v1/stream, UI /)");
        } else {
            tracing::info!(
                "api listening on http://{listen_addr}  (WS /v1/stream; UI not embedded)"
            );
        }
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal.cancelled_owned())
            .await
        {
            tracing::error!("api server error: {e}");
        }
    });

    Ok(ApiServerHandle {
        shutdown,
        task: Some(task),
    })
}

async fn enforce_safe_host(
    State(gate): State<HostGateState>,
    request: Request,
    next: Next,
) -> Response {
    if safe_loopback_host(request.headers(), gate.listen_addr) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "ok": false,
            "error": "Host header is not allowed for this loopback API"
        })),
    )
        .into_response()
}

fn safe_loopback_host(headers: &HeaderMap, listen_addr: SocketAddr) -> bool {
    let mut values = headers.get_all(HOST).iter();
    let Some(value) = values.next() else {
        return false;
    };
    // Multiple Host values are ambiguous and invalid for this trust boundary.
    if values.next().is_some() {
        return false;
    }
    let Ok(value) = value.to_str() else {
        return false;
    };
    if value.contains('@') {
        return false;
    }
    let Ok(authority) = value.parse::<Authority>() else {
        return false;
    };
    let port = match authority.port_u16() {
        Some(port) => port,
        None if listen_addr.port() == 80 => 80,
        None => return false,
    };
    if port != listen_addr.port() {
        return false;
    }

    let authority_host = authority.host();
    let host = authority_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(authority_host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback() || address == listen_addr.ip())
}

fn build_cors(origins: &[String]) -> anyhow::Result<Option<CorsLayer>> {
    if origins.is_empty() {
        return Ok(None);
    }
    let origins = origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(origin)
                .map_err(|e| anyhow::anyhow!("invalid api.cors_origins value '{origin}': {e}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([Method::GET, Method::POST, Method::DELETE])
            .allow_headers([AUTHORIZATION, CONTENT_TYPE]),
    ))
}

async fn authorize_v1(State(st): State<Arc<ApiState>>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    // Health intentionally stays public so supervisors can distinguish a live
    // service from an authentication/configuration failure.
    if path == "/v1/health" || !path.starts_with("/v1/") {
        return next.run(request).await;
    }
    let Some(expected) = st.bearer_token.as_deref() else {
        return next.run(request).await;
    };
    if request_bearer(request.headers()).is_some_and(|token| secure_eq(token, expected)) {
        next.run(request).await
    } else {
        unauthorized()
    }
}

fn request_bearer(headers: &HeaderMap) -> Option<&str> {
    authorization_bearer(headers).or_else(|| websocket_protocol_bearer(headers))
}

fn authorization_bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || token.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(token)
}

/// Browser WebSocket constructors cannot set Authorization. They may offer
/// protocols `["bearer", token]`; the server authenticates the second item and
/// selects only the non-secret `bearer` protocol in its response.
fn websocket_protocol_bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(SEC_WEBSOCKET_PROTOCOL)?.to_str().ok()?;
    let mut protocols = value.split(',').map(str::trim);
    while let Some(protocol) = protocols.next() {
        if protocol.eq_ignore_ascii_case("bearer") {
            let token = protocols.next()?;
            if !token.is_empty() && !token.chars().any(char::is_whitespace) {
                return Some(token);
            }
            return None;
        }
    }
    None
}

fn secure_eq(candidate: &str, expected: &str) -> bool {
    let candidate = candidate.as_bytes();
    let expected = expected.as_bytes();
    let mut difference = candidate.len() ^ expected.len();
    let length = candidate.len().max(expected.len());
    for index in 0..length {
        difference |= usize::from(
            candidate.get(index).copied().unwrap_or(0) ^ expected.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
        Json(serde_json::json!({
            "ok": false,
            "error": "missing or invalid bearer token"
        })),
    )
        .into_response()
}

fn permission_denied(permission: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "ok": false,
            "error": format!("API endpoint does not permit {permission}")
        })),
    )
        .into_response()
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "service": "ohmyserial" }))
}

async fn status(State(st): State<Arc<ApiState>>) -> Response {
    if !st.can_read {
        return permission_denied("read access");
    }
    Json(st.broker.snapshot()).into_response()
}

async fn clients(State(st): State<Arc<ApiState>>) -> Response {
    if !st.can_read {
        return permission_denied("read access");
    }
    let snap = st.broker.snapshot();
    Json(snap.clients).into_response()
}

/// List configured fan-out endpoints (virtual serial, TCP, WS, HTTP).
async fn endpoints(State(st): State<Arc<ApiState>>) -> Response {
    if !st.can_read {
        return permission_denied("read access");
    }
    let snap = st.broker.snapshot();
    Json(serde_json::json!({
        "real": snap.port,
        "endpoints": snap.endpoints,
        "connected_clients": snap.clients.len(),
    }))
    .into_response()
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
    /// Bearer credential returned by POST /v1/lock. Required while a lease is active.
    #[serde(default)]
    lease_token: Option<String>,
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

async fn write(State(st): State<Arc<ApiState>>, Json(body): Json<WriteBody>) -> Response {
    if !st.can_write {
        return permission_denied("write access");
    }
    let data = if let Some(hex) = body.hex {
        match parse_hex(&hex) {
            Ok(b) => b,
            Err(e) => {
                return Json(WriteResp {
                    ok: false,
                    error: Some(e),
                    bytes: 0,
                })
                .into_response();
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
        })
        .into_response();
    };

    let who = body.as_client.as_deref().unwrap_or(&st.default_writer);
    let n = data.len();
    match st
        .broker
        .api_write_confirmed_with_lease(who, Bytes::from(data), body.lease_token.as_deref())
        .await
    {
        Ok(()) => Json(WriteResp {
            ok: true,
            error: None,
            bytes: n,
        })
        .into_response(),
        Err(e) => Json(WriteResp {
            ok: false,
            error: Some(e),
            bytes: 0,
        })
        .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct LockBody {
    #[serde(default)]
    as_client: Option<String>,
    /// Existing bearer credential. Supplying it renews/releases that lease.
    #[serde(default)]
    lease_token: Option<String>,
}

async fn lock(State(st): State<Arc<ApiState>>, Json(body): Json<LockBody>) -> Response {
    if !st.can_write {
        return permission_denied("lease access");
    }
    let result = if let Some(token) = body.lease_token.as_deref() {
        st.broker.renew_lock(token)
    } else {
        let who = body.as_client.unwrap_or_else(|| st.default_writer.clone());
        st.broker.acquire_lock(&who)
    };
    match result {
        Ok(v) => Json(serde_json::json!({ "ok": true, "lock": v })).into_response(),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn unlock(State(st): State<Arc<ApiState>>, body: Option<Json<LockBody>>) -> Response {
    if !st.can_write {
        return permission_denied("lease access");
    }
    let lease_token = body.and_then(|Json(b)| b.lease_token);
    match st.broker.release_lock(lease_token.as_deref()) {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn ws_stream(
    ws: WebSocketUpgrade,
    State(st): State<Arc<ApiState>>,
    Extension(shutdown): Extension<CancellationToken>,
    headers: HeaderMap,
) -> Response {
    if !st.ws_can_read && !st.ws_can_write {
        return permission_denied("WebSocket read or write access");
    }
    if !websocket_origin_allowed(&headers, &st.cors_origins) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "ok": false,
                "error": "WebSocket origin is not allowed"
            })),
        )
            .into_response();
    }
    let upgrade = if websocket_protocol_bearer(&headers).is_some() {
        ws.protocols(["bearer"])
    } else {
        ws
    };
    upgrade
        .on_upgrade(move |socket| handle_ws(socket, st, shutdown))
        .into_response()
}

fn websocket_origin_allowed(headers: &HeaderMap, configured_origins: &[String]) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        // Non-browser agents commonly omit Origin. Bearer authentication still
        // applies independently when configured.
        return true;
    };
    if configured_origins
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(origin))
    {
        return true;
    }
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    origin_authority(origin).is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

fn origin_authority(origin: &str) -> Option<&str> {
    let (scheme, authority) = origin.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        return None;
    }
    Some(authority)
}

async fn handle_ws(socket: WebSocket, st: Arc<ApiState>, shutdown: CancellationToken) {
    let (mut sink, mut stream) = socket.split();
    // Each WebSocket connection is an independent fan-out subscriber.
    let conn_kind = format!("websocket@{}", uuid::Uuid::new_v4());
    let (id, mut from_broker) = st.broker.register_client(
        st.ws_writer.clone(),
        conn_kind,
        st.ws_can_read,
        st.ws_can_write,
        None,
    );
    let _registration = st.broker.client_registration(id);

    // Send history first (binary).
    if st.ws_can_read && st.history_on_ws_connect > 0 {
        let hist = st.broker.history_bytes();
        if !hist.is_empty() {
            let take = hist.len().min(st.history_on_ws_connect);
            let slice = hist.slice(hist.len() - take..);
            let sent = tokio::select! {
                biased;
                _ = shutdown.cancelled() => false,
                result = sink.send(Message::Binary(slice.to_vec().into())) => result.is_ok(),
            };
            if !sent {
                if shutdown.is_cancelled() {
                    close_ws(&mut sink, &mut stream).await;
                }
                return;
            }
        }
    }

    let cancelled = 'connected: loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                break 'connected true;
            }
            outbound = from_broker.recv(), if st.ws_can_read => {
                match outbound {
                    Some(data) => {
                        let sent = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => false,
                            result = sink.send(Message::Binary(data.to_vec().into())) => result.is_ok(),
                        };
                        if !sent {
                            break 'connected shutdown.is_cancelled();
                        }
                    }
                    None => break 'connected false,
                }
            }
            inbound = stream.next() => {
                let Some(Ok(msg)) = inbound else { break 'connected false };
                match msg {
                    Message::Binary(b) => {
                        if !st.ws_can_write {
                            if !send_ws_error(
                                &mut sink,
                                "client is read-only",
                                &shutdown,
                            )
                            .await
                            {
                                break 'connected shutdown.is_cancelled();
                            }
                            continue;
                        }
                        let result = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => {
                                break 'connected true;
                            },
                            result = st.broker.client_tx_atomic(id, Bytes::from(b.to_vec())) => result,
                        };
                        if let Err(e) = result {
                            tracing::warn!("ws tx denied: {e}");
                            if !send_ws_error(&mut sink, &e, &shutdown).await {
                                break 'connected shutdown.is_cancelled();
                            }
                        }
                    }
                    Message::Text(t) => {
                        if !st.ws_can_write {
                            if !send_ws_error(
                                &mut sink,
                                "client is read-only",
                                &shutdown,
                            )
                            .await
                            {
                                break 'connected shutdown.is_cancelled();
                            }
                            continue;
                        }
                        let mut s = t.to_string();
                        if !s.ends_with('\n') {
                            s.push('\n');
                        }
                        let result = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => {
                                break 'connected true;
                            },
                            result = st.broker.client_tx(id, Bytes::from(s.into_bytes())) => result,
                        };
                        if let Err(e) = result {
                            tracing::warn!("ws tx denied: {e}");
                            if !send_ws_error(&mut sink, &e, &shutdown).await {
                                break 'connected shutdown.is_cancelled();
                            }
                        }
                    }
                    Message::Close(_) => break 'connected false,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
        }
    };
    if cancelled {
        close_ws(&mut sink, &mut stream).await;
    }
}

async fn close_ws(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
) {
    let timeout = std::time::Duration::from_millis(500);
    if !matches!(
        tokio::time::timeout(timeout, sink.send(Message::Close(None))).await,
        Ok(Ok(()))
    ) {
        return;
    }
    // Give the peer a bounded opportunity to acknowledge the Close frame so
    // Windows does not turn an otherwise clean shutdown into a TCP reset.
    let _ = tokio::time::timeout(timeout, async {
        while let Some(message) = stream.next().await {
            match message {
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
}

async fn send_ws_error(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    error: &str,
    shutdown: &CancellationToken,
) -> bool {
    let payload = serde_json::json!({
        "type": "ohmyserial.error",
        "ok": false,
        "error": error,
    })
    .to_string();
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => false,
        result = sink.send(Message::Text(payload.into())) => result.is_ok(),
    }
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !clean.len().is_multiple_of(2) {
        return Err("hex length must be even".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| format!("bad hex: {e}")))
        .collect()
}

#[allow(dead_code)]
pub type LockView = WriteLockView;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_authorization_is_strict() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        assert_eq!(authorization_bearer(&headers), Some("secret-token"));
        assert!(secure_eq(request_bearer(&headers).unwrap(), "secret-token"));
        assert!(!secure_eq(
            request_bearer(&headers).unwrap(),
            "secret-tokee"
        ));

        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic secret-token"),
        );
        assert_eq!(authorization_bearer(&headers), None);
    }

    #[test]
    fn browser_websocket_bearer_uses_protocol_pair() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("chat, bearer, browser-token"),
        );
        assert_eq!(websocket_protocol_bearer(&headers), Some("browser-token"));

        headers.insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("bearer"));
        assert_eq!(websocket_protocol_bearer(&headers), None);
    }

    #[test]
    fn websocket_origin_defaults_to_same_host() {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:8787"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:8787"));
        assert!(websocket_origin_allowed(&headers, &[]));

        headers.insert(ORIGIN, HeaderValue::from_static("https://console.example"));
        assert!(!websocket_origin_allowed(&headers, &[]));
        assert!(websocket_origin_allowed(
            &headers,
            &["https://console.example".into()]
        ));
    }

    #[test]
    fn host_gate_accepts_only_loopback_on_the_listener_port() {
        let listen: SocketAddr = "127.0.0.1:8787".parse().unwrap();
        for host in [
            "127.0.0.1:8787",
            "127.0.0.2:8787",
            "localhost:8787",
            "[::1]:8787",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(HOST, HeaderValue::from_str(host).unwrap());
            assert!(safe_loopback_host(&headers, listen), "host={host}");
        }

        for host in [
            "attacker.example:8787",
            "localhost:8788",
            "127.0.0.1.evil:8787",
            "user@localhost:8787",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(HOST, HeaderValue::from_str(host).unwrap());
            assert!(!safe_loopback_host(&headers, listen), "host={host}");
        }
    }
}
