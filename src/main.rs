//! Binary entry point for the Janus terminal application.
//!
//! This executable starts the web server and manages the application lifecycle.

use axum::{
    routing::{get, post, delete},
    Router,
    Json,
    extract::{State, WebSocketUpgrade, Path as AxumPath},
    http::{StatusCode, Request, HeaderMap},
    middleware::{self, Next},
    response::Response,
    body::Body,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, Duration};
use tower_cookies::Cookies;
use tower_http::trace::TraceLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use janus::auth::{build_session_cookie, generate_csrf_token, TokenStore};
use janus::config::Config;
use janus::middleware::{create_csrf_validator, SessionData};
use janus::notification::{IMessageSender, NotificationSender};
use janus::session::SessionManager;
use janus::session_logger::SessionLogger;
use janus::tls::{create_rustls_config, TlsConfig};
use tower_governor::{
    governor::GovernorConfigBuilder,
    GovernorLayer,
    key_extractor::GlobalKeyExtractor,
};

/// Application version from Cargo.toml
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Command-line arguments
#[derive(Parser, Debug)]
#[command(name = "janus")]
#[command(version = VERSION)]
#[command(about = "Web-based terminal with token-based authentication", long_about = None)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, value_name = "FILE", default_value = "config.toml")]
    config: PathBuf,

    /// Session log directory (overrides config file)
    #[arg(short = 'l', long = "log-dir", value_name = "DIR")]
    log_dir: Option<PathBuf>,
}

/// Server state containing start time for uptime tracking and auth components
#[derive(Clone)]
struct AppState {
    start_time: SystemTime,
    /// Unique server instance ID (generated on startup) to detect restarts
    instance_id: String,
    token_store: TokenStore,
    notification_sender: Arc<dyn NotificationSender>,
    config: Config,
    // Auth session storage: session_id -> SessionData (for CSRF tokens)
    sessions: Arc<std::sync::RwLock<std::collections::HashMap<String, SessionData>>>,
    // Terminal session manager: manages PTY-backed terminal sessions
    session_manager: Arc<SessionManager>,
}

/// Health check response
#[derive(Serialize, Deserialize)]
struct HealthResponse {
    status: String,
    version: String,
    uptime_secs: u64,
    /// Server instance ID (changes on restart)
    instance_id: String,
}

/// Token generation response
#[derive(Serialize, Deserialize)]
struct TokenGenerateResponse {
    success: bool,
    message: String,
}

/// Login request containing authentication token
#[derive(Serialize, Deserialize)]
struct LoginRequest {
    token: String,
}

/// Login response with CSRF token and session info
#[derive(Serialize, Deserialize)]
struct LoginResponse {
    success: bool,
    message: String,
    csrf_token: Option<String>,
    session_duration_secs: Option<u64>,
}

/// Request to create a new terminal session
#[derive(Serialize, Deserialize)]
struct CreateSessionRequest {
    /// Optional shell command (defaults to $SHELL or /bin/bash)
    shell_command: Option<String>,
    /// Initial terminal size (rows)
    rows: Option<u16>,
    /// Initial terminal size (columns)
    cols: Option<u16>,
}

/// Response after creating a session
#[derive(Serialize, Deserialize)]
struct CreateSessionResponse {
    success: bool,
    message: String,
    session_id: Option<String>,
}

/// Response for listing sessions
#[derive(Serialize, Deserialize)]
struct ListSessionsResponse {
    success: bool,
    sessions: Vec<janus::session::SessionInfo>,
}

/// Response for deleting a session
#[derive(Serialize, Deserialize)]
struct DeleteSessionResponse {
    success: bool,
    message: String,
}

/// Health check endpoint - returns server status and uptime
async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let uptime = state.start_time
        .elapsed()
        .unwrap_or_default()
        .as_secs();

    Json(HealthResponse {
        status: "ok".to_string(),
        version: VERSION.to_string(),
        uptime_secs: uptime,
        instance_id: state.instance_id.clone(),
    })
}

/// Token generation endpoint - generates auth token and sends via iMessage
///
/// This endpoint:
/// 1. Generates a cryptographically secure token
/// 2. Stores it with expiration metadata
/// 3. Sends it via the configured notification channel (iMessage)
/// 4. Returns success/failure status
///
/// Rate limited to 3 requests per minute to prevent abuse.
async fn generate_token(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<TokenGenerateResponse>) {
    // Generate and store token
    let token = match state.token_store.generate_and_store() {
        Ok(token) => {
            tracing::info!(
                token_length = token.len(),
                "Generated authentication token"
            );
            token
        }
        Err(e) => {
            tracing::error!(error = ?e, "Failed to generate token");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(TokenGenerateResponse {
                    success: false,
                    message: "Failed to generate token".to_string(),
                }),
            );
        }
    };

    // Send token via notification (don't block on failure - degraded mode)
    match state.notification_sender.send_token(&token).await {
        Ok(()) => {
            tracing::info!("Token sent successfully");
            (
                StatusCode::OK,
                Json(TokenGenerateResponse {
                    success: true,
                    message: "Token generated and sent successfully".to_string(),
                }),
            )
        }
        Err(e) => {
            tracing::error!(error = ?e, "Failed to send token notification");
            // Return success even if notification fails (degraded mode)
            // Token is still valid and can be used if user receives it through other means
            (
                StatusCode::OK,
                Json(TokenGenerateResponse {
                    success: true,
                    message: "Token generated but notification failed. Check server logs."
                        .to_string(),
                }),
            )
        }
    }
}

/// Login endpoint - validates token and creates authenticated session
///
/// This endpoint:
/// 1. Validates the provided token (existence, expiry, one-time use)
/// 2. Atomically marks the token as used (prevents reuse)
/// 3. Generates a session ID
/// 4. Sets a secure session cookie
/// 5. Generates and returns a CSRF token
///
/// Returns 200 OK with CSRF token on success
/// Returns 401 Unauthorized if token is invalid, expired, or already used
async fn login(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(request): Json<LoginRequest>,
) -> (StatusCode, Json<LoginResponse>) {
    tracing::info!(
        token_length = request.token.len(),
        "Login attempt"
    );

    // Validate token format (should be 64-char hex string)
    if request.token.len() != 64 || !request.token.chars().all(|c| c.is_ascii_hexdigit()) {
        tracing::warn!(
            token_length = request.token.len(),
            "Invalid token format"
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(LoginResponse {
                success: false,
                message: "Invalid token format".to_string(),
                csrf_token: None,
                session_duration_secs: None,
            }),
        );
    }

    // Validate token atomically (checks existence, expiry, marks as used)
    match state.token_store.validate_token(&request.token) {
        Ok(()) => {
            tracing::info!("Token validated successfully");

            // Generate session ID (reuse token generation for consistency)
            let session_id = uuid::Uuid::new_v4().simple().to_string();

            // Generate CSRF token
            let csrf_token = generate_csrf_token();

            // Store session server-side
            {
                let mut sessions = match state.sessions.write() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!(error = ?e, "Session storage lock poisoned");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(LoginResponse {
                                success: false,
                                message: "Internal server error".to_string(),
                                csrf_token: None,
                                session_duration_secs: None,
                            }),
                        );
                    }
                };
                let now = SystemTime::now();
                sessions.insert(
                    session_id.clone(),
                    SessionData {
                        csrf_token: csrf_token.clone(),
                        created_at: now,
                        last_activity: now,
                    },
                );
            }

            // Create session cookie
            let cookie = build_session_cookie(
                "session_id",
                session_id,
                state.config.idle_timeout_secs,
                state.config.use_https,
            );

            // Set cookie in response
            cookies.add(cookie);

            tracing::info!(
                session_duration_secs = state.config.idle_timeout_secs,
                "Login successful, session created"
            );

            (
                StatusCode::OK,
                Json(LoginResponse {
                    success: true,
                    message: "Login successful".to_string(),
                    csrf_token: Some(csrf_token),
                    session_duration_secs: Some(state.config.idle_timeout_secs),
                }),
            )
        }
        Err(e) => {
            let error_message = match e {
                janus::auth::AuthError::InvalidToken => {
                    tracing::warn!("Login failed: Invalid token");
                    "Invalid token"
                }
                janus::auth::AuthError::ExpiredToken => {
                    tracing::warn!("Login failed: Expired token");
                    "Token has expired"
                }
                janus::auth::AuthError::AlreadyUsed => {
                    tracing::warn!("Login failed: Token already used");
                    "Token has already been used"
                }
                _ => {
                    tracing::error!(error = ?e, "Login failed: Internal error");
                    "Authentication failed"
                }
            };

            (
                StatusCode::UNAUTHORIZED,
                Json(LoginResponse {
                    success: false,
                    message: error_message.to_string(),
                    csrf_token: None,
                    session_duration_secs: None,
                }),
            )
        }
    }
}

/// List all active terminal sessions
///
/// Returns a list of all active terminal sessions with their metadata.
/// Requires authentication (valid session cookie).
async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ListSessionsResponse>) {
    match state.session_manager.list_sessions() {
        Ok(sessions) => {
            tracing::debug!(count = sessions.len(), "Listed terminal sessions");
            (
                StatusCode::OK,
                Json(ListSessionsResponse {
                    success: true,
                    sessions,
                }),
            )
        }
        Err(e) => {
            tracing::error!(error = ?e, "Failed to list sessions");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ListSessionsResponse {
                    success: false,
                    sessions: vec![],
                }),
            )
        }
    }
}

/// Create a new terminal session
///
/// Creates a new PTY-backed terminal session with the specified parameters.
/// Requires authentication (valid session cookie).
async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> (StatusCode, Json<CreateSessionResponse>) {
    // Validate PTY dimensions if provided
    if let Some(rows) = request.rows {
        if rows == 0 || rows > 999 {
            tracing::warn!(rows = rows, "Invalid PTY rows value");
            return (
                StatusCode::BAD_REQUEST,
                Json(CreateSessionResponse {
                    success: false,
                    message: "Invalid rows: must be between 1 and 999".to_string(),
                    session_id: None,
                }),
            );
        }
    }
    if let Some(cols) = request.cols {
        if cols == 0 || cols > 999 {
            tracing::warn!(cols = cols, "Invalid PTY cols value");
            return (
                StatusCode::BAD_REQUEST,
                Json(CreateSessionResponse {
                    success: false,
                    message: "Invalid cols: must be between 1 and 999".to_string(),
                    session_id: None,
                }),
            );
        }
    }

    // Build PtySize if dimensions provided
    let pty_size = if request.rows.is_some() || request.cols.is_some() {
        Some(portable_pty::PtySize {
            rows: request.rows.unwrap_or(24),
            cols: request.cols.unwrap_or(80),
            pixel_width: 0,
            pixel_height: 0,
        })
    } else {
        None
    };

    match state
        .session_manager
        .create_session(request.shell_command, pty_size)
    {
        Ok(session_id) => {
            tracing::info!(session_id = %session_id, "Created terminal session");
            (
                StatusCode::CREATED,
                Json(CreateSessionResponse {
                    success: true,
                    message: "Session created successfully".to_string(),
                    session_id: Some(session_id),
                }),
            )
        }
        Err(janus::session::SessionError::LimitReached(max)) => {
            tracing::warn!(max_sessions = max, "Session limit reached");
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(CreateSessionResponse {
                    success: false,
                    message: format!("Session limit reached (max: {})", max),
                    session_id: None,
                }),
            )
        }
        Err(e) => {
            tracing::error!(error = ?e, "Failed to create session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(CreateSessionResponse {
                    success: false,
                    message: "Failed to create session".to_string(),
                    session_id: None,
                }),
            )
        }
    }
}

/// Delete a terminal session
///
/// Terminates and cleans up the specified terminal session.
/// Requires authentication (valid session cookie).
async fn delete_session(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> (StatusCode, Json<DeleteSessionResponse>) {
    match state.session_manager.delete_session(&session_id) {
        Ok(()) => {
            tracing::info!(session_id = %session_id, "Deleted terminal session");
            (
                StatusCode::OK,
                Json(DeleteSessionResponse {
                    success: true,
                    message: "Session deleted successfully".to_string(),
                }),
            )
        }
        Err(janus::session::SessionError::NotFound(_)) => {
            tracing::warn!(
                session_id = %session_id,
                instance_id = %state.instance_id,
                "Session not found (may have been deleted or server restarted)"
            );
            (
                StatusCode::NOT_FOUND,
                Json(DeleteSessionResponse {
                    success: false,
                    message: "Session not found (may have been deleted or server restarted)"
                        .to_string(),
                }),
            )
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, error = ?e, "Failed to delete session");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(DeleteSessionResponse {
                    success: false,
                    message: "Failed to delete session".to_string(),
                }),
            )
        }
    }
}

/// WebSocket handler for terminal streaming
///
/// Upgrades the HTTP connection to WebSocket and handles bidirectional
/// terminal I/O streaming. Requires authentication via session cookie.
async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    AxumPath(session_id): AxumPath<String>,
    cookies: Cookies,
) -> Result<Response, StatusCode> {
    // Check authentication - validate session cookie exists and is valid
    let auth_session_id = cookies
        .get("session_id")
        .map(|cookie| cookie.value().to_string())
        .ok_or_else(|| {
            tracing::warn!(
                terminal_session_id = %session_id,
                "WebSocket connection rejected: no session cookie"
            );
            StatusCode::UNAUTHORIZED
        })?;

    // Validate session exists in server storage (defense in depth)
    {
        let sessions = state.sessions.read().map_err(|_| {
            tracing::error!("Session storage lock poisoned");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        if !sessions.contains_key(&auth_session_id) {
            tracing::warn!(
                terminal_session_id = %session_id,
                auth_session_id = %auth_session_id,
                "WebSocket connection rejected: auth session not found in storage"
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    // Verify the terminal session exists
    if let Err(e) = state.session_manager.get_session(&session_id) {
        tracing::warn!(
            session_id = %session_id,
            instance_id = %state.instance_id,
            error = ?e,
            "WebSocket connection rejected: session not found (may have been deleted or server restarted)"
        );
        return Err(StatusCode::NOT_FOUND);
    }

    tracing::info!(
        session_id = %session_id,
        "WebSocket connection established"
    );

    // Upgrade to WebSocket and handle the connection
    Ok(ws.on_upgrade(move |socket| handle_websocket(socket, session_id, state)))
}

/// Handle an active WebSocket connection
///
/// This function manages the bidirectional streaming between the PTY and WebSocket.
/// It will be implemented in subsequent tasks to handle:
/// - PTY output → WebSocket (Task 3.3)
/// - WebSocket input → PTY (Task 3.4)
/// - Error handling and connection management (Task 3.5)
async fn handle_websocket(
    socket: axum::extract::ws::WebSocket,
    session_id: String,
    state: Arc<AppState>,
) {
    use janus::websocket::{WebSocketHandler, TerminalMessage};

    let mut handler = WebSocketHandler::new(session_id.clone(), socket);

    // Send attached confirmation
    if let Err(e) = handler
        .send_message(TerminalMessage::Attached {
            session_id: session_id.clone(),
        })
        .await
    {
        tracing::error!(
            session_id = %session_id,
            error = ?e,
            "Failed to send attached message"
        );
        return;
    }

    tracing::info!(session_id = %session_id, "WebSocket attached to session");

    // Get PTY reader for this session
    let pty_reader = match state.session_manager.get_pty_reader(&session_id) {
        Ok(reader) => reader,
        Err(e) => {
            tracing::error!(
                session_id = %session_id,
                error = ?e,
                "Failed to get PTY reader"
            );
            let _ = handler.send_error(format!("Failed to get PTY reader: {}", e)).await;
            return;
        }
    };

    // Create a channel for PTY output with backpressure
    let (pty_tx, mut pty_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

    // Spawn task to read from PTY and send to channel (Task 3.3)
    // PTY uses blocking I/O, so we use spawn_blocking
    let pty_session_id = session_id.clone();
    let pty_reader_task = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut reader = pty_reader;
        let mut buffer = vec![0u8; 8192]; // 8KB buffer

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    // PTY closed
                    tracing::info!(session_id = %pty_session_id, "PTY closed (EOF)");
                    break;
                }
                Ok(n) => {
                    let data = buffer[..n].to_vec();
                    tracing::trace!(
                        session_id = %pty_session_id,
                        bytes = n,
                        "Read from PTY"
                    );

                    // Send to channel with backpressure (blocking send)
                    if pty_tx.blocking_send(data).is_err() {
                        tracing::info!(
                            session_id = %pty_session_id,
                            "PTY reader channel closed, stopping"
                        );
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %pty_session_id,
                        error = %e,
                        "PTY read error"
                    );
                    break;
                }
            }
        }

        tracing::info!(session_id = %pty_session_id, "PTY reader task completed");
    });

    // Create a channel for PTY input (WebSocket → PTY) - Task 3.4
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

    // Get PTY writer once at the beginning (can only be called once!)
    let pty_writer = match state.session_manager.get_pty_writer(&session_id) {
        Ok(writer) => writer,
        Err(e) => {
            tracing::error!(
                session_id = %session_id,
                error = ?e,
                "Failed to get PTY writer"
            );
            let _ = handler.send_error(format!("Failed to get PTY writer: {}", e)).await;
            return;
        }
    };

    // Spawn task to write to PTY from channel (Task 3.4)
    // PTY uses blocking I/O, so we use spawn_blocking
    let input_session_id = session_id.clone();
    let input_writer_task = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut writer = pty_writer;

        while let Some(data) = input_rx.blocking_recv() {
            let data_len = data.len();

            match writer.write_all(&data) {
                Ok(()) => {
                    tracing::trace!(
                        session_id = %input_session_id,
                        bytes = data_len,
                        "Wrote to PTY"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %input_session_id,
                        error = %e,
                        "Failed to write to PTY"
                    );
                    break;
                }
            }
        }

        tracing::info!(session_id = %input_session_id, "PTY writer task completed");
    });

    // Main event loop: handle bidirectional streaming
    loop {
        tokio::select! {
            // PTY output → WebSocket (Task 3.3)
            result = pty_rx.recv() => {
                match result {
                    Some(data) => {
                        if let Err(e) = handler.send_output(data).await {
                            tracing::error!(
                                session_id = %session_id,
                                error = ?e,
                                "Failed to send PTY output to WebSocket"
                            );
                            break;
                        }
                        // Update activity timestamp
                        let _ = state.session_manager.touch_session(&session_id);
                    }
                    None => {
                        // PTY closed (shell exited)
                        tracing::info!(
                            session_id = %session_id,
                            "PTY channel closed, shell has exited"
                        );
                        let _ = handler.send_error("Session ended".to_string()).await;
                        break;
                    }
                }
            }

            // WebSocket input (Task 3.4 will implement PTY writing)
            result = handler.receive_message() => {
                match result {
                    Ok(Some(msg)) => {
                        match msg {
                            TerminalMessage::Ping => {
                                if let Err(e) = handler.send_pong().await {
                                    tracing::error!(
                                        session_id = %session_id,
                                        error = ?e,
                                        "Failed to send pong"
                                    );
                                    break;
                                }
                            }
                            TerminalMessage::Resize { rows, cols } => {
                                // Validate dimensions
                                if rows == 0 || rows > 999 || cols == 0 || cols > 999 {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        rows = rows,
                                        cols = cols,
                                        "Invalid resize dimensions"
                                    );
                                    let _ = handler.send_error(format!(
                                        "Invalid dimensions: rows and cols must be between 1 and 999"
                                    )).await;
                                    continue;
                                }

                                let pty_size = portable_pty::PtySize {
                                    rows,
                                    cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                };
                                if let Err(e) = state.session_manager.resize_pty(&session_id, pty_size) {
                                    tracing::error!(
                                        session_id = %session_id,
                                        error = ?e,
                                        "Failed to resize PTY"
                                    );
                                    let _ = handler.send_error(format!("Failed to resize PTY: {}", e)).await;
                                }
                            }
                            TerminalMessage::Input { data } => {
                                // Validate input size (max 64KB per message to prevent abuse)
                                if data.len() > 64 * 1024 {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        bytes = data.len(),
                                        "Input data too large, rejecting"
                                    );
                                    let _ = handler.send_error(
                                        "Input too large (max 64KB per message)".to_string()
                                    ).await;
                                    continue;
                                }

                                // Task 3.4: Write input to PTY via channel
                                tracing::trace!(
                                    session_id = %session_id,
                                    bytes = data.len(),
                                    "Sending input to PTY"
                                );

                                if input_tx.send(data).await.is_err() {
                                    tracing::error!(
                                        session_id = %session_id,
                                        "PTY writer channel closed"
                                    );
                                    break;
                                }

                                // Update activity timestamp
                                let _ = state.session_manager.touch_session(&session_id);
                            }
                            _ => {
                                tracing::debug!(session_id = %session_id, "Ignoring message type");
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::info!(session_id = %session_id, "WebSocket connection closed by client");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(
                            session_id = %session_id,
                            error = ?e,
                            "WebSocket error"
                        );
                        break;
                    }
                }
            }
        }
    }

    // Cleanup: abort background tasks
    pty_reader_task.abort();
    input_writer_task.abort();

    // Update session activity on disconnect
    let _ = state.session_manager.touch_session(&session_id);

    tracing::info!(session_id = %session_id, "WebSocket handler completed");
}

/// Graceful shutdown signal handler
///
/// Listens for SIGTERM (Unix) or Ctrl+C and initiates graceful shutdown.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received Ctrl+C signal");
        },
        _ = terminate => {
            tracing::info!("Received SIGTERM signal");
        },
    }

    tracing::info!("Starting graceful shutdown");
}

/// Initialize structured logging with tracing-subscriber
fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,janus=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// Check if running as root (EUID = 0) and exit if so
fn check_not_root() {
    #[cfg(unix)]
    {
        // Use nix crate to get effective user ID
        use nix::unistd::Uid;

        if Uid::effective().is_root() {
            tracing::error!("SECURITY: Refusing to run as root. Please run as a normal user.");
            std::process::exit(1);
        }
    }
}

/// Middleware to validate Origin/Host headers to prevent CSRF attacks
/// Supports both localhost and configured public origins (e.g., ngrok URLs)
async fn validate_origin(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Allow GET requests (idempotent)
    if request.method() == axum::http::Method::GET {
        return Ok(next.run(request).await);
    }

    // For POST/PUT/DELETE, validate Origin or Referer
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let referer = headers.get("referer").and_then(|v| v.to_str().ok());
    let host = headers.get("host").and_then(|v| v.to_str().ok());

    // Check if origin is allowed
    let is_allowed_origin = origin
        .map(|o| is_allowed_origin_str(o, &state.config))
        .unwrap_or(false);

    let is_allowed_referer = referer
        .map(|r| is_allowed_origin_str(r, &state.config))
        .unwrap_or(false);

    // Require either allowed origin or allowed referer for state-changing operations
    if is_allowed_origin || is_allowed_referer {
        Ok(next.run(request).await)
    } else {
        tracing::warn!(
            origin = ?origin,
            referer = ?referer,
            host = ?host,
            "Rejected request with invalid origin/referer"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

/// Check if an origin string is allowed based on configuration
fn is_allowed_origin_str(origin: &str, config: &Config) -> bool {
    // Localhost origins (HTTP and HTTPS variants)
    if origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("https://127.0.0.1:")
        || origin.starts_with("http://localhost:")
        || origin.starts_with("https://localhost:")
        || origin.starts_with("http://[::1]:")
        || origin.starts_with("https://[::1]:")
    {
        return true;
    }

    // Configured public origins (supports wildcards like https://*.ngrok-free.app)
    config.allowed_origins.iter().any(|allowed| {
        if allowed.contains('*') {
            // Wildcard matching
            match_origin_pattern(origin, allowed)
        } else {
            // Exact match
            origin == allowed
        }
    })
}

/// Match an origin against a pattern with wildcards
/// Supports patterns like "https://*.example.com" or "https://*.ngrok-free.app"
fn match_origin_pattern(origin: &str, pattern: &str) -> bool {
    // Split pattern into parts around the wildcard
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.len() != 2 {
        // Pattern must have exactly one wildcard
        return false;
    }

    let prefix = parts[0];
    let suffix = parts[1];

    // Origin must start with prefix and end with suffix
    if !origin.starts_with(prefix) || !suffix.is_empty() && !origin.ends_with(suffix) {
        return false;
    }

    // The wildcard part should only match subdomain characters (no slashes or other URL parts)
    // Extract the wildcard-matched portion
    let wildcard_start = prefix.len();
    let wildcard_end = if suffix.is_empty() {
        origin.len()
    } else {
        origin.len() - suffix.len()
    };

    if wildcard_start > wildcard_end {
        return false;
    }

    let wildcard_part = &origin[wildcard_start..wildcard_end];

    // Wildcard should only match valid subdomain characters (no /, :, ?, #, etc.)
    // This prevents matching across URL boundaries
    wildcard_part.chars().all(|c| {
        c.is_alphanumeric() || c == '-' || c == '.'
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_origin_pattern_ngrok() {
        // Ngrok free tier pattern
        assert!(match_origin_pattern(
            "https://a06b-158-178-224-177.ngrok-free.app",
            "https://*.ngrok-free.app"
        ));

        assert!(match_origin_pattern(
            "https://xyz-123.ngrok-free.app",
            "https://*.ngrok-free.app"
        ));
    }

    #[test]
    fn test_match_origin_pattern_exact_domain() {
        // Should match exact subdomain
        assert!(match_origin_pattern(
            "https://myapp.example.com",
            "https://*.example.com"
        ));

        assert!(match_origin_pattern(
            "https://test.example.com",
            "https://*.example.com"
        ));
    }

    #[test]
    fn test_match_origin_pattern_rejects_wrong_domain() {
        // Should not match different domains
        assert!(!match_origin_pattern(
            "https://evil.com",
            "https://*.ngrok-free.app"
        ));

        assert!(!match_origin_pattern(
            "https://ngrok-free.app.evil.com",
            "https://*.ngrok-free.app"
        ));
    }

    #[test]
    fn test_match_origin_pattern_rejects_url_tricks() {
        // Should not match if wildcard would match across URL boundaries
        assert!(!match_origin_pattern(
            "https://evil.com/path?x=.ngrok-free.app",
            "https://*.ngrok-free.app"
        ));
    }

    #[test]
    fn test_match_origin_pattern_with_port() {
        // Should work with ports
        assert!(match_origin_pattern(
            "https://test-123.ngrok-free.app:443",
            "https://*.ngrok-free.app:443"
        ));
    }
}

#[tokio::main]
async fn main() {
    // Parse command-line arguments
    let cli = Cli::parse();

    // Initialize structured logging before any other operations
    init_tracing();

    // Security check: refuse to run as root
    check_not_root();

    // Load configuration from file, or use defaults if file doesn't exist
    let mut config = if cli.config.exists() {
        match Config::from_file(&cli.config) {
            Ok(config) => {
                tracing::info!(
                    config_file = %cli.config.display(),
                    "Loaded configuration from file"
                );
                config
            }
            Err(e) => {
                tracing::error!(
                    config_file = %cli.config.display(),
                    error = ?e,
                    "Failed to load configuration file, exiting"
                );
                std::process::exit(1);
            }
        }
    } else {
        tracing::warn!(
            config_file = %cli.config.display(),
            "Configuration file not found, using defaults"
        );
        Config::with_defaults()
    };

    // Override session_log_dir if provided via CLI
    if let Some(log_dir) = cli.log_dir {
        // Expand tilde in the provided path
        let expanded_path = if let Some(path_str) = log_dir.to_str() {
            if path_str.starts_with("~/") {
                if let Some(home) = dirs::home_dir() {
                    home.join(&path_str[2..])
                } else {
                    log_dir
                }
            } else if path_str == "~" {
                dirs::home_dir().unwrap_or(log_dir)
            } else {
                log_dir
            }
        } else {
            log_dir
        };

        tracing::info!(
            log_dir = %expanded_path.display(),
            "Using session log directory from command line"
        );
        config.session_log_dir = expanded_path;
    }

    let bind_address = config.bind_address.clone();

    // Initialize TokenStore
    let token_store = TokenStore::new(config.token_validity_secs);

    // Initialize notification sender
    let notification_sender: Arc<dyn NotificationSender> = match &config.notification {
        janus::config::NotificationConfig::IMessage { phone_number } => {
            match IMessageSender::new(phone_number.clone(), 10) {
                Ok(sender) => {
                    tracing::info!(
                        phone_number = %phone_number,
                        "Initialized iMessage notification sender"
                    );
                    Arc::new(sender)
                }
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        phone_number = %phone_number,
                        "Failed to initialize iMessage sender"
                    );
                    std::process::exit(1);
                }
            }
        }
    };

    // Initialize auth session storage
    let sessions = Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));

    // Initialize session logger
    let session_log_dir = config.session_log_dir.clone();
    let session_logger = match SessionLogger::new(&session_log_dir).await {
        Ok(logger) => {
            tracing::info!(
                log_dir = %session_log_dir.display(),
                "Initialized session logger"
            );
            Some(logger)
        }
        Err(e) => {
            tracing::error!(
                error = ?e,
                log_dir = %session_log_dir.display(),
                "Failed to initialize session logger, logging disabled"
            );
            None
        }
    };

    // Initialize terminal session manager
    let session_manager = if let Some(logger) = session_logger {
        Arc::new(SessionManager::with_logger(config.max_sessions, logger))
    } else {
        Arc::new(SessionManager::new(config.max_sessions))
    };
    tracing::info!(
        max_sessions = config.max_sessions,
        "Initialized terminal session manager"
    );

    // Generate unique server instance ID (for restart detection)
    let instance_id = uuid::Uuid::new_v4().simple().to_string();
    tracing::info!(
        instance_id = %instance_id,
        "Generated server instance ID"
    );

    // Create application state with server start time and auth components
    let state = Arc::new(AppState {
        start_time: SystemTime::now(),
        instance_id,
        token_store: token_store.clone(),
        notification_sender,
        config: config.clone(),
        sessions: sessions.clone(),
        session_manager: session_manager.clone(),
    });

    // Create rate limiting layer for token generation and login
    // Use GlobalKeyExtractor which works reliably for all scenarios (localhost HTTP/HTTPS, ngrok, etc.)
    // This applies rate limiting globally rather than per-IP, which is sufficient for most use cases
    let rate_limit_config = config.rate_limit.clone();
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(rate_limit_config.replenish_interval_secs())
            .burst_size(rate_limit_config.requests_per_period)
            .key_extractor(GlobalKeyExtractor)
            .finish()
            .unwrap(),
    );

    tracing::info!(
        requests_per_period = rate_limit_config.requests_per_period,
        period_secs = rate_limit_config.period_secs,
        "Rate limiting configured (global)"
    );

    let governor_limiter = GovernorLayer {
        config: governor_conf,
    };

    // Spawn background task for token cleanup (keep handle for graceful shutdown)
    let token_cleanup_task = {
        let store_clone = token_store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600)); // Every hour
            loop {
                interval.tick().await;
                match store_clone.cleanup_expired() {
                    Ok(removed) => {
                        if removed > 0 {
                            tracing::info!(removed_tokens = removed, "Cleaned up expired tokens");
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "Failed to cleanup expired tokens");
                    }
                }
            }
        })
    };

    // Spawn background task for dead session reaping (zombie process cleanup)
    let session_reaping_task = {
        let manager_clone = session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30)); // Every 30 seconds
            loop {
                interval.tick().await;
                match manager_clone.reap_dead_sessions() {
                    Ok(reaped) => {
                        if !reaped.is_empty() {
                            tracing::info!(
                                count = reaped.len(),
                                session_ids = ?reaped,
                                "Reaped dead sessions"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "Failed to reap dead sessions");
                    }
                }
            }
        })
    };

    // Spawn background task for idle session cleanup
    let idle_cleanup_task = {
        let manager_clone = session_manager.clone();
        let idle_timeout = config.idle_timeout_secs;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60)); // Every 60 seconds
            loop {
                interval.tick().await;
                match manager_clone.cleanup_idle_sessions(idle_timeout) {
                    Ok(cleaned) => {
                        if !cleaned.is_empty() {
                            tracing::info!(
                                count = cleaned.len(),
                                session_ids = ?cleaned,
                                timeout_secs = idle_timeout,
                                "Cleaned up idle sessions"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "Failed to cleanup idle sessions");
                    }
                }
            }
        })
    };

    // Spawn background task for auth session cleanup
    let auth_session_cleanup_task = {
        let sessions_clone = sessions.clone();
        let idle_timeout = config.idle_timeout_secs;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // Every 5 minutes
            loop {
                interval.tick().await;
                let mut sessions = match sessions_clone.write() {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::error!(error = ?e, "Auth session storage lock poisoned");
                        continue;
                    }
                };

                let now = SystemTime::now();
                let before = sessions.len();

                // Remove sessions that haven't been active within the timeout period
                sessions.retain(|_, data| {
                    now.duration_since(data.last_activity)
                        .map(|d| d.as_secs() < idle_timeout)
                        .unwrap_or(false)
                });

                let removed = before - sessions.len();
                if removed > 0 {
                    tracing::info!(
                        removed,
                        timeout_secs = idle_timeout,
                        "Cleaned up expired auth sessions"
                    );
                }
            }
        })
    };

    // Create CSRF validator middleware
    let csrf_validator = create_csrf_validator(sessions.clone());

    // Create protected routes (require authentication and CSRF)
    let protected_routes = Router::new()
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/:id", delete(delete_session))
        .route("/api/sessions/:id/ws", get(websocket_handler))
        .layer(middleware::from_fn(csrf_validator)) // CSRF validation
        .layer(middleware::from_fn_with_state(state.clone(), validate_origin)); // Origin validation

    // Create public routes (no authentication required, but origin validation for POST)
    let public_auth_routes = Router::new()
        .route("/api/auth/login", post(login))
        .layer(governor_limiter.clone()) // Rate limit login endpoint
        .layer(middleware::from_fn_with_state(state.clone(), validate_origin)); // Origin validation

    // Create fully public routes (no auth, no origin validation)
    let public_routes = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/token/generate", post(generate_token))
        .layer(governor_limiter); // Rate limit token generation

    // Combine all routes with global middleware
    let app = Router::new()
        .merge(protected_routes)
        .merge(public_auth_routes)
        .merge(public_routes)
        .layer(RequestBodyLimitLayer::new(1024 * 10)) // 10KB max request body
        .layer(tower_cookies::CookieManagerLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
        // Serve static files (frontend)
        .fallback_service(
            tower_http::services::ServeDir::new("static")
                .not_found_service(tower_http::services::ServeFile::new("static/index.html"))
        );

    // Log startup message with structured fields
    tracing::info!(
        bind_address = %bind_address,
        version = VERSION,
        https_enabled = config.use_https,
        "Janus - Gateway Guardian API starting"
    );

    // Log security configuration if public exposure is enabled
    if !config.allowed_origins.is_empty() {
        tracing::warn!("Public exposure enabled - ensure security features are working");
        tracing::info!(
            https = config.use_https,
            allowed_origins = ?config.allowed_origins,
            rate_limit_per_period = config.rate_limit.requests_per_period,
            rate_limit_period_secs = config.rate_limit.period_secs,
            token_validity_secs = config.token_validity_secs,
            "Security configuration"
        );
    }

    // Start the server with or without HTTPS
    if config.use_https {
        // Build TLS configuration
        let tls_config = TlsConfig {
            cert_path: config.tls_cert_path.clone(),
            key_path: config.tls_key_path.clone(),
            auto_generate: config.tls_auto_generate,
        };

        let rustls_config = match create_rustls_config(&tls_config) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::error!(error = %e, "Failed to create TLS configuration");
                std::process::exit(1);
            }
        };

        // Bind and serve with HTTPS
        let addr: std::net::SocketAddr = bind_address.parse()
            .expect("Failed to parse bind address");

        match axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await
        {
            Ok(_) => {},
            Err(e) => {
                tracing::error!(error = %e, "HTTPS server error");
                std::process::exit(1);
            }
        }
    } else {
        // Bind to address from config
        let listener = match tokio::net::TcpListener::bind(&bind_address).await {
            Ok(listener) => listener,
            Err(e) => {
                tracing::error!(
                    bind_address = %bind_address,
                    error = %e,
                    "Failed to bind to address"
                );
                std::process::exit(1);
            }
        };

        // Start the server with graceful shutdown (HTTP)
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            tracing::error!(error = %e, "HTTP server error");
            std::process::exit(1);
        }
    }

    // Graceful shutdown: stop background tasks
    tracing::info!("Server stopped, cleaning up background tasks");
    token_cleanup_task.abort();
    session_reaping_task.abort();
    idle_cleanup_task.abort();
    auth_session_cleanup_task.abort();

    // Graceful shutdown: cleanup all terminal sessions (kills PTY processes)
    tracing::info!("Cleaning up terminal sessions");
    match session_manager.list_sessions() {
        Ok(sessions_list) => {
            let session_count = sessions_list.len();
            for session_info in &sessions_list {
                if let Err(e) = session_manager.delete_session(&session_info.id) {
                    tracing::error!(
                        session_id = %session_info.id,
                        error = ?e,
                        "Failed to delete session during shutdown"
                    );
                } else {
                    tracing::debug!(
                        session_id = %session_info.id,
                        "Session cleaned up"
                    );
                }
            }
            tracing::info!(
                count = session_count,
                "Terminal sessions cleaned up"
            );
        }
        Err(e) => {
            tracing::error!(error = ?e, "Failed to list sessions during shutdown");
        }
    }

    tracing::info!("Graceful shutdown complete");
}
