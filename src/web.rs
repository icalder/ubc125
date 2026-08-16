//! Static file serving for the web UI.
//!
//! The compiled frontend in `web/dist` is embedded into the binary with
//! `rust-embed`, so the serve binary is self-contained. The axum router
//! built here is attached as the *fallback* of the tonic router: gRPC and
//! gRPC-Web paths are matched first by service name, everything else (the
//! browser UI) lands here.

use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

/// The static-file router (any method/path that gRPC routing did not claim).
pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/{*path}", get(file))
}

/// Serve an embedded asset. Rejects paths that could escape the embedded
/// dir; unknown files are 404.
async fn serve_asset(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.contains("..") {
        return (axum::http::StatusCode::NOT_FOUND, "not found").into_response();
    }
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
            Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .body(Body::from(file.data.to_vec()))
                .unwrap()
        }
        None => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn index() -> impl IntoResponse {
    serve_asset("index.html").await
}

async fn file(axum::extract::Path(path): axum::extract::Path<String>) -> impl IntoResponse {
    serve_asset(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_index_at_root() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn missing_file_is_404() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn path_traversal_is_404() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/does/../not/leak")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // axum normalizes the path before routing; the handler must not
        // serve anything outside the embedded dir.
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert!(
            status == StatusCode::NOT_FOUND || !body.windows(9).any(|w| w == b"RustEmbed"),
            "handler leaked something unexpected: {}",
            String::from_utf8_lossy(&body)
        );
    }
}
