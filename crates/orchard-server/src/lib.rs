//! Embedded browser assets for the Orchard workspace server.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::Response;
use axum::routing::get;
use axum::Router;

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

/// Builds the UI router merged into [`orchard_workspace_host::WorkspaceHost`].
pub fn ui_router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/{*path}", get(asset_or_index))
}

async fn index() -> Response {
    asset_response("index.html").expect("index.html is checked by build.rs")
}

async fn asset_or_index(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(response) = asset_response(path) {
        return response;
    }
    if path.starts_with("w/")
        || (!path.starts_with("api/") && !path.starts_with("workspaces/") && !path.contains('.'))
    {
        return asset_response("index.html").expect("index.html is checked by build.rs");
    }
    response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        b"Not found",
    )
}

fn asset_response(path: &str) -> Option<Response> {
    let (_, bytes) = ASSETS.iter().find(|(candidate, _)| *candidate == path)?;
    let mut response = response(StatusCode::OK, content_type(path), bytes);
    let cache = if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(response)
}

fn response(status: StatusCode, content_type: &'static str, bytes: &'static [u8]) -> Response {
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

fn content_type(path: &str) -> &'static str {
    match Path::new(path).extension().and_then(|value| value.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn embeds_index_assets_and_spa_fallback() {
        let router = ui_router();
        let index = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(index.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let bytes = to_bytes(index.into_body(), usize::MAX).await.unwrap();
        assert!(bytes
            .windows(b"<title>Orchard</title>".len())
            .any(|part| part == b"<title>Orchard</title>"));

        let fallback = router
            .clone()
            .oneshot(Request::get("/settings").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(fallback.status(), StatusCode::OK);

        let dotted_resource = router
            .clone()
            .oneshot(
                Request::get("/w/workspace/channels/review.v1?ignored=query")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dotted_resource.status(), StatusCode::OK);

        let missing = router
            .oneshot(Request::get("/missing.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
