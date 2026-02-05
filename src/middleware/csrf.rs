//! CSRF token validation middleware.
//!
//! Provides middleware to validate CSRF tokens on protected endpoints.
//! CSRF tokens are validated against the session data stored server-side.

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use crate::client_info::Fingerprint;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use tower_cookies::Cookies;

/// Session data stored server-side (must match main.rs SessionData)
#[derive(Debug, Clone)]
pub struct SessionData {
    pub csrf_token: String,
    pub created_at: SystemTime,
    pub last_activity: SystemTime,
    pub client_ip: String,
    pub fingerprint: Fingerprint,
}

/// Creates a CSRF validation middleware with the given sessions storage.
///
/// Returns a function that can be used with `middleware::from_fn`.
///
/// # Example
/// ```ignore
/// let sessions = Arc::new(RwLock::new(HashMap::new()));
/// let csrf_middleware = create_csrf_validator(sessions.clone());
///
/// Router::new()
///     .route("/protected", post(handler))
///     .layer(middleware::from_fn(csrf_middleware))
/// ```
pub fn create_csrf_validator(
    sessions: Arc<RwLock<HashMap<String, SessionData>>>,
) -> impl Fn(Cookies, HeaderMap, Request<Body>, Next) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Response, StatusCode>> + Send>,
> + Clone {
    move |cookies: Cookies, headers: HeaderMap, request: Request<Body>, next: Next| {
        let sessions = sessions.clone();
        Box::pin(async move {
            validate_csrf_impl(sessions, cookies, headers, request, next).await
        })
    }
}

/// Internal implementation of CSRF validation.
async fn validate_csrf_impl(
    sessions: Arc<RwLock<HashMap<String, SessionData>>>,
    cookies: Cookies,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Skip CSRF validation for GET requests (idempotent)
    if request.method() == Method::GET {
        return Ok(next.run(request).await);
    }

    // Get session cookie
    let session_id = cookies
        .get("session_id")
        .map(|c| c.value().to_string())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Get CSRF token from X-CSRF-Token header
    let csrf_header = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            tracing::warn!(
                session_id = %session_id,
                "CSRF validation failed: missing X-CSRF-Token header"
            );
            StatusCode::FORBIDDEN
        })?;

    // Extract current client IP and fingerprint
    let current_ip = crate::client_info::extract_client_ip(&headers).map_err(|e| {
        tracing::warn!(
            session_id = %session_id,
            error = ?e,
            "Failed to extract client IP"
        );
        StatusCode::UNAUTHORIZED
    })?;

    let current_fingerprint = Fingerprint::from_headers(&headers);

    // Validate against stored session
    let is_valid = {
        let sessions_guard = sessions.read().map_err(|_| {
            tracing::error!("Session storage lock poisoned");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let session_data = sessions_guard.get(&session_id).ok_or_else(|| {
            tracing::warn!(
                session_id = %session_id,
                "CSRF validation failed: session not found in storage"
            );
            StatusCode::UNAUTHORIZED
        })?;

        // Validate client IP
        if session_data.client_ip != current_ip {
            tracing::warn!(
                session_id = %session_id,
                expected_ip = %session_data.client_ip,
                actual_ip = %current_ip,
                "Security: IP address mismatch detected"
            );
            return Err(StatusCode::FORBIDDEN);
        }

        // Validate fingerprint
        if !session_data.fingerprint.matches(&current_fingerprint) {
            tracing::warn!(
                session_id = %session_id,
                "Security: Browser fingerprint mismatch detected"
            );
            return Err(StatusCode::FORBIDDEN);
        }

        // Constant-time comparison to prevent timing attacks
        use subtle::ConstantTimeEq;
        let valid: bool = session_data
            .csrf_token
            .as_bytes()
            .ct_eq(csrf_header.as_bytes())
            .into();

        valid
    }; // sessions_guard dropped here

    if is_valid {
        Ok(next.run(request).await)
    } else {
        tracing::warn!(
            session_id = %session_id,
            "CSRF validation failed: token mismatch"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Method, StatusCode},
        middleware,
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt;
    use tower_cookies::CookieManagerLayer;

    async fn test_handler() -> &'static str {
        "success"
    }

    fn create_test_app(sessions: Arc<RwLock<HashMap<String, SessionData>>>) -> Router {
        let csrf_validator = create_csrf_validator(sessions);

        Router::new()
            .route("/protected", post(test_handler))
            .route("/get-endpoint", get(test_handler))
            .layer(middleware::from_fn(csrf_validator))
            .layer(CookieManagerLayer::new())
    }

    #[tokio::test]
    async fn test_csrf_get_request_bypasses_validation() {
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        let app = create_test_app(sessions);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/get-endpoint")
                    .method(Method::GET)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_csrf_missing_session_cookie() {
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        let app = create_test_app(sessions);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .method(Method::POST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_csrf_missing_header() {
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        sessions.write().unwrap().insert(
            "test-session".to_string(),
            SessionData {
                csrf_token: "test-token".to_string(),
                created_at: SystemTime::now(),
                last_activity: SystemTime::now(),
                client_ip: "203.0.113.1".to_string(),
                fingerprint: Fingerprint {
                    user_agent: "Mozilla/5.0".to_string(),
                    accept: "text/html".to_string(),
                    accept_language: "en-US".to_string(),
                    accept_encoding: "gzip".to_string(),
                },
            },
        );

        let app = create_test_app(sessions);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .method(Method::POST)
                    .header("Cookie", "session_id=test-session")
                    .header("X-Forwarded-For", "203.0.113.1")
                    .header("User-Agent", "Mozilla/5.0")
                    .header("Accept", "text/html")
                    .header("Accept-Language", "en-US")
                    .header("Accept-Encoding", "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_csrf_invalid_token() {
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        sessions.write().unwrap().insert(
            "test-session".to_string(),
            SessionData {
                csrf_token: "correct-token".to_string(),
                created_at: SystemTime::now(),
                last_activity: SystemTime::now(),
                client_ip: "203.0.113.1".to_string(),
                fingerprint: Fingerprint {
                    user_agent: "Mozilla/5.0".to_string(),
                    accept: "text/html".to_string(),
                    accept_language: "en-US".to_string(),
                    accept_encoding: "gzip".to_string(),
                },
            },
        );

        let app = create_test_app(sessions);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .method(Method::POST)
                    .header("Cookie", "session_id=test-session")
                    .header("X-CSRF-Token", "wrong-token")
                    .header("X-Forwarded-For", "203.0.113.1")
                    .header("User-Agent", "Mozilla/5.0")
                    .header("Accept", "text/html")
                    .header("Accept-Language", "en-US")
                    .header("Accept-Encoding", "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_csrf_valid_token() {
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        sessions.write().unwrap().insert(
            "test-session".to_string(),
            SessionData {
                csrf_token: "correct-token".to_string(),
                created_at: SystemTime::now(),
                last_activity: SystemTime::now(),
                client_ip: "203.0.113.1".to_string(),
                fingerprint: Fingerprint {
                    user_agent: "Mozilla/5.0".to_string(),
                    accept: "text/html".to_string(),
                    accept_language: "en-US".to_string(),
                    accept_encoding: "gzip".to_string(),
                },
            },
        );

        let app = create_test_app(sessions);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .method(Method::POST)
                    .header("Cookie", "session_id=test-session")
                    .header("X-CSRF-Token", "correct-token")
                    .header("X-Forwarded-For", "203.0.113.1")
                    .header("User-Agent", "Mozilla/5.0")
                    .header("Accept", "text/html")
                    .header("Accept-Language", "en-US")
                    .header("Accept-Encoding", "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_csrf_nonexistent_session() {
        let sessions = Arc::new(RwLock::new(HashMap::new()));
        let app = create_test_app(sessions);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .method(Method::POST)
                    .header("Cookie", "session_id=nonexistent")
                    .header("X-CSRF-Token", "some-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
