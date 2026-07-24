//! Embedded React console (from `web/dist`), served same-origin with the API.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "web/dist/"]
struct Assets;

/// Serve SPA assets. API routes under `/v1/*` must be registered before this fallback.
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Never hijack API
    if path.starts_with("v1/") || path == "v1" {
        return StatusCode::NOT_FOUND.into_response();
    }

    if path.is_empty() {
        return asset_response("index.html");
    }

    if Assets::get(path).is_some() {
        return asset_response(path);
    }

    // SPA fallback
    asset_response("index.html")
}

fn asset_response(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CACHE_CONTROL, cache_for(path)),
                ],
                file.data,
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            format!("static asset not found: {path} (rebuild web/dist)"),
        )
            .into_response(),
    }
}

fn cache_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Whether the binary includes a built UI.
pub fn ui_embedded() -> bool {
    Assets::get("index.html").is_some()
}
