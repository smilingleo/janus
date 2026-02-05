//! Embedded static file serving.
//!
//! Embeds the static/ directory at compile time and provides handlers to serve
//! these files via HTTP. This allows for single-binary deployment without
//! requiring external static file directories.

use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode, Uri},
    response::IntoResponse,
};
use rust_embed::RustEmbed;

/// Embedded static files from the static/ directory.
///
/// Files are embedded at compile time using the rust-embed crate.
/// This allows the binary to serve frontend assets without requiring
/// an external static directory at runtime.
#[derive(RustEmbed)]
#[folder = "static/"]
pub struct StaticAssets;

/// Serve an embedded static file.
///
/// This handler extracts the requested path, looks it up in the embedded
/// assets, and returns the file content with appropriate MIME type headers.
///
/// # Arguments
/// * `uri` - The requested URI path
///
/// # Returns
/// HTTP response with file contents or 404 if not found
pub async fn serve_static_file(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    // Handle root path - serve index.html
    let path = if path.is_empty() || path == "/" {
        "index.html"
    } else {
        path
    };

    tracing::debug!(path = %path, "Serving static file");

    match StaticAssets::get(path) {
        Some(content) => {
            // Determine MIME type from file extension
            let mime_type = mime_guess::from_path(path)
                .first_or_octet_stream()
                .as_ref()
                .to_string();

            // Build response with appropriate headers
            Response::builder()
                .status(StatusCode::OK)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(&mime_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
                )
                .header(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000"), // Cache for 1 year
                )
                .body(Body::from(content.data))
                .unwrap()
        }
        None => {
            // File not found - try serving index.html for SPA routing
            if path != "index.html" {
                tracing::debug!(path = %path, "File not found, falling back to index.html");
                match StaticAssets::get("index.html") {
                    Some(content) => {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(header::CONTENT_TYPE, HeaderValue::from_static("text/html"))
                            .header(
                                header::CACHE_CONTROL,
                                HeaderValue::from_static("no-cache"), // Don't cache index.html
                            )
                            .body(Body::from(content.data))
                            .unwrap()
                    }
                    None => {
                        tracing::warn!("index.html not found in embedded assets");
                        Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Body::from("404 Not Found"))
                            .unwrap()
                    }
                }
            } else {
                tracing::warn!("index.html not found in embedded assets");
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("404 Not Found"))
                    .unwrap()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Uri;

    #[tokio::test]
    async fn test_serve_root() {
        let uri = Uri::from_static("/");
        let response = serve_static_file(uri).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_serve_index_html() {
        let uri = Uri::from_static("/index.html");
        let response = serve_static_file(uri).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_serve_nonexistent_file() {
        let uri = Uri::from_static("/nonexistent.txt");
        let response = serve_static_file(uri).await.into_response();
        // Should fall back to index.html for SPA routing
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_list_embedded_files() {
        // Verify some expected files are embedded
        let files: Vec<_> = StaticAssets::iter().collect();
        println!("Embedded files: {:?}", files);
        assert!(!files.is_empty(), "No files were embedded");
    }
}
