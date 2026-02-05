//! Per-session logging module for terminal session audit trails.
//!
//! Provides non-blocking, channel-based logging for terminal sessions. Each session
//! gets its own log file with timestamped events (creation, I/O, resize, deletion).
//! Uses tokio channels to avoid blocking session I/O operations.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// Errors that can occur during session logging operations
#[derive(Debug, Error)]
pub enum SessionLoggerError {
    #[error("Failed to create log file: {0}")]
    FileCreationFailed(String),

    #[error("Failed to write log entry: {0}")]
    WriteFailed(String),

    #[error("Log channel closed")]
    ChannelClosed,
}

/// Types of log events that can be recorded
#[derive(Debug, Clone)]
pub enum LogEvent {
    /// Token requested
    TokenRequested {
        client_ip: String,
        user_agent: String,
    },
    /// Session created
    SessionCreated {
        session_id: String,
        shell_command: String,
        rows: u16,
        cols: u16,
        client_ip: String,
        user_agent: String,
    },
    /// WebSocket connection established
    WebSocketConnected {
        session_id: String,
        client_ip: String,
        user_agent: String,
    },
    /// Data received from client (input to PTY)
    Input { session_id: String, data: Vec<u8> },
    /// Data sent to client (output from PTY)
    Output { session_id: String, data: Vec<u8> },
    /// Terminal resized
    Resize {
        session_id: String,
        rows: u16,
        cols: u16,
    },
    /// Session deleted/terminated
    SessionDeleted { session_id: String },
}

/// Channel-based session logger
///
/// Provides non-blocking logging for terminal sessions. Messages are sent via
/// a channel to a background task that writes to log files.
#[derive(Clone)]
pub struct SessionLogger {
    /// Channel sender for log events
    sender: mpsc::UnboundedSender<LogEvent>,
}

impl SessionLogger {
    /// Create a new SessionLogger and spawn the background writer task
    ///
    /// # Arguments
    /// * `log_dir` - Directory where session log files will be stored
    ///
    /// # Returns
    /// A new SessionLogger instance
    ///
    /// # Errors
    /// Returns SessionLoggerError if the log directory cannot be created
    pub async fn new<P: AsRef<Path>>(log_dir: P) -> Result<Self, SessionLoggerError> {
        let log_dir = log_dir.as_ref().to_path_buf();

        // Create log directory if it doesn't exist
        tokio::fs::create_dir_all(&log_dir)
            .await
            .map_err(|e| SessionLoggerError::FileCreationFailed(e.to_string()))?;

        tracing::info!(log_dir = %log_dir.display(), "Initialized session logger");

        // Create channel for log events
        let (sender, receiver) = mpsc::unbounded_channel();

        // Spawn background task to process log events
        tokio::spawn(log_writer_task(log_dir, receiver));

        Ok(SessionLogger { sender })
    }

    /// Log a session event (non-blocking)
    ///
    /// # Arguments
    /// * `event` - The log event to record
    ///
    /// # Returns
    /// Ok(()) if the event was queued, Err if the channel is closed
    pub fn log(&self, event: LogEvent) -> Result<(), SessionLoggerError> {
        self.sender
            .send(event)
            .map_err(|_| SessionLoggerError::ChannelClosed)
    }

    /// Log token request (convenience method)
    pub fn log_token_requested(
        &self,
        client_ip: String,
        user_agent: String,
    ) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::TokenRequested {
            client_ip,
            user_agent,
        })
    }

    /// Log session creation (convenience method)
    pub fn log_session_created(
        &self,
        session_id: String,
        shell_command: String,
        rows: u16,
        cols: u16,
        client_ip: String,
        user_agent: String,
    ) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::SessionCreated {
            session_id,
            shell_command,
            rows,
            cols,
            client_ip,
            user_agent,
        })
    }

    /// Log WebSocket connection (convenience method)
    pub fn log_websocket_connected(
        &self,
        session_id: String,
        client_ip: String,
        user_agent: String,
    ) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::WebSocketConnected {
            session_id,
            client_ip,
            user_agent,
        })
    }

    /// Log input data (convenience method)
    pub fn log_input(&self, session_id: String, data: Vec<u8>) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::Input { session_id, data })
    }

    /// Log output data (convenience method)
    pub fn log_output(&self, session_id: String, data: Vec<u8>) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::Output { session_id, data })
    }

    /// Log terminal resize (convenience method)
    pub fn log_resize(
        &self,
        session_id: String,
        rows: u16,
        cols: u16,
    ) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::Resize {
            session_id,
            rows,
            cols,
        })
    }

    /// Log session deletion (convenience method)
    pub fn log_session_deleted(&self, session_id: String) -> Result<(), SessionLoggerError> {
        self.log(LogEvent::SessionDeleted { session_id })
    }
}

/// Background task that processes log events and writes to files
async fn log_writer_task(log_dir: PathBuf, mut receiver: mpsc::UnboundedReceiver<LogEvent>) {
    // Map of session_id -> open file handle
    let mut log_files: std::collections::HashMap<String, File> = std::collections::HashMap::new();

    while let Some(event) = receiver.recv().await {
        if let Err(e) = handle_log_event(&log_dir, &mut log_files, event).await {
            tracing::error!(error = ?e, "Failed to handle log event");
        }
    }

    // Channel closed, flush and close all log files
    for (session_id, mut file) in log_files.drain() {
        if let Err(e) = file.flush().await {
            tracing::error!(
                session_id = %session_id,
                error = %e,
                "Failed to flush log file on shutdown"
            );
        }
    }

    tracing::info!("Session logger task shutting down");
}

/// Handle a single log event
async fn handle_log_event(
    log_dir: &Path,
    log_files: &mut std::collections::HashMap<String, File>,
    event: LogEvent,
) -> Result<(), SessionLoggerError> {
    match event {
        LogEvent::TokenRequested {
            client_ip,
            user_agent,
        } => {
            // Log to a central audit file
            let audit_path = log_dir.join("token-requests.log");
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&audit_path)
                .await
                .map_err(|e| SessionLoggerError::FileCreationFailed(e.to_string()))?;

            let timestamp = chrono::Utc::now().to_rfc3339();
            let entry = format!(
                "[{}] TOKEN_REQUESTED: ip={}, user_agent={}\n",
                timestamp, client_ip, user_agent
            );
            file.write_all(entry.as_bytes())
                .await
                .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
            file.flush()
                .await
                .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;

            tracing::debug!(
                client_ip = %client_ip,
                user_agent = %user_agent,
                "Logged token request"
            );
        }
        LogEvent::SessionCreated {
            session_id,
            shell_command,
            rows,
            cols,
            client_ip,
            user_agent,
        } => {
            // Create log file for this session
            let log_path = log_dir.join(format!("{}.log", session_id));
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .await
                .map_err(|e| SessionLoggerError::FileCreationFailed(e.to_string()))?;

            // Write session creation header with security info
            let timestamp = chrono::Utc::now().to_rfc3339();
            let header = format!(
                "[{}] SESSION_CREATED: shell={}, size={}x{}, client_ip={}, user_agent={}\n",
                timestamp, shell_command, cols, rows, client_ip, user_agent
            );
            file.write_all(header.as_bytes())
                .await
                .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;

            // Store file handle for future writes
            log_files.insert(session_id.clone(), file);

            tracing::debug!(
                session_id = %session_id,
                log_path = %log_path.display(),
                client_ip = %client_ip,
                "Created session log file"
            );
        }
        LogEvent::WebSocketConnected {
            session_id,
            client_ip,
            user_agent,
        } => {
            if let Some(file) = log_files.get_mut(&session_id) {
                let timestamp = chrono::Utc::now().to_rfc3339();
                let entry = format!(
                    "[{}] WEBSOCKET_CONNECTED: client_ip={}, user_agent={}\n",
                    timestamp, client_ip, user_agent
                );
                file.write_all(entry.as_bytes())
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
            }
        }
        LogEvent::Input { session_id, data } => {
            if let Some(file) = log_files.get_mut(&session_id) {
                let timestamp = chrono::Utc::now().to_rfc3339();
                let entry = format!("[{}] INPUT: {} bytes\n", timestamp, data.len());
                file.write_all(entry.as_bytes())
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
                // Write hex dump of input data
                file.write_all(format!("  {}\n", hex::encode(&data)).as_bytes())
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
            }
        }
        LogEvent::Output { session_id, data } => {
            if let Some(file) = log_files.get_mut(&session_id) {
                let timestamp = chrono::Utc::now().to_rfc3339();
                let entry = format!("[{}] OUTPUT: {} bytes\n", timestamp, data.len());
                file.write_all(entry.as_bytes())
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
                // For output, also write the actual text (if printable)
                if let Ok(text) = String::from_utf8(data.clone()) {
                    file.write_all(format!("  {}\n", text.escape_default()).as_bytes())
                        .await
                        .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
                } else {
                    file.write_all(format!("  {}\n", hex::encode(&data)).as_bytes())
                        .await
                        .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
                }
            }
        }
        LogEvent::Resize {
            session_id,
            rows,
            cols,
        } => {
            if let Some(file) = log_files.get_mut(&session_id) {
                let timestamp = chrono::Utc::now().to_rfc3339();
                let entry = format!("[{}] RESIZE: {}x{}\n", timestamp, cols, rows);
                file.write_all(entry.as_bytes())
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
            }
        }
        LogEvent::SessionDeleted { session_id } => {
            if let Some(mut file) = log_files.remove(&session_id) {
                let timestamp = chrono::Utc::now().to_rfc3339();
                let entry = format!("[{}] SESSION_DELETED\n", timestamp);
                file.write_all(entry.as_bytes())
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;
                file.flush()
                    .await
                    .map_err(|e| SessionLoggerError::WriteFailed(e.to_string()))?;

                tracing::debug!(session_id = %session_id, "Closed session log file");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_session_logger_creation() {
        let log_dir = tempdir().unwrap();
        let logger = SessionLogger::new(log_dir.path()).await.unwrap();

        // Log a session creation event
        logger
            .log_session_created(
                "test-session-1".to_string(),
                "/bin/bash".to_string(),
                24,
                80,
                "192.168.1.1".to_string(),
                "Mozilla/5.0".to_string(),
            )
            .unwrap();

        // Give the background task time to write
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify log file was created
        let log_path = log_dir.path().join("test-session-1.log");
        assert!(log_path.exists());

        // Read log file and verify content
        let content = tokio::fs::read_to_string(&log_path).await.unwrap();
        assert!(content.contains("SESSION_CREATED"));
        assert!(content.contains("shell=/bin/bash"));
        assert!(content.contains("size=80x24"));
        assert!(content.contains("client_ip=192.168.1.1"));
        assert!(content.contains("user_agent=Mozilla/5.0"));
    }

    #[tokio::test]
    async fn test_session_logger_io() {
        let log_dir = tempdir().unwrap();
        let logger = SessionLogger::new(log_dir.path()).await.unwrap();

        let session_id = "test-session-2".to_string();

        // Log session creation
        logger
            .log_session_created(
                session_id.clone(),
                "/bin/bash".to_string(),
                24,
                80,
                "192.168.1.1".to_string(),
                "Mozilla/5.0".to_string(),
            )
            .unwrap();

        // Log input
        logger
            .log_input(session_id.clone(), b"ls -la\n".to_vec())
            .unwrap();

        // Log output
        logger
            .log_output(session_id.clone(), b"total 0\n".to_vec())
            .unwrap();

        // Log resize
        logger.log_resize(session_id.clone(), 30, 100).unwrap();

        // Log deletion
        logger.log_session_deleted(session_id.clone()).unwrap();

        // Give the background task time to write
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Read log file and verify all events
        let log_path = log_dir.path().join("test-session-2.log");
        let content = tokio::fs::read_to_string(&log_path).await.unwrap();

        assert!(content.contains("SESSION_CREATED"));
        assert!(content.contains("INPUT"));
        assert!(content.contains("OUTPUT"));
        assert!(content.contains("RESIZE: 100x30"));
        assert!(content.contains("SESSION_DELETED"));
    }

    #[tokio::test]
    async fn test_session_logger_multiple_sessions() {
        let log_dir = tempdir().unwrap();
        let logger = SessionLogger::new(log_dir.path()).await.unwrap();

        // Create multiple sessions
        logger
            .log_session_created(
                "session-1".to_string(),
                "/bin/bash".to_string(),
                24,
                80,
                "192.168.1.1".to_string(),
                "Mozilla/5.0".to_string(),
            )
            .unwrap();
        logger
            .log_session_created(
                "session-2".to_string(),
                "/bin/zsh".to_string(),
                30,
                120,
                "192.168.1.2".to_string(),
                "Chrome/90.0".to_string(),
            )
            .unwrap();

        // Log to both sessions
        logger
            .log_input("session-1".to_string(), b"echo test\n".to_vec())
            .unwrap();
        logger
            .log_input("session-2".to_string(), b"pwd\n".to_vec())
            .unwrap();

        // Give the background task time to write
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify both log files exist and have correct content
        let log1 = tokio::fs::read_to_string(log_dir.path().join("session-1.log"))
            .await
            .unwrap();
        let log2 = tokio::fs::read_to_string(log_dir.path().join("session-2.log"))
            .await
            .unwrap();

        assert!(log1.contains("shell=/bin/bash"));
        assert!(log1.contains("INPUT"));
        assert!(log1.contains(hex::encode(b"echo test\n").as_str()));
        assert!(log2.contains("shell=/bin/zsh"));
        assert!(log2.contains("INPUT"));
        assert!(log2.contains(hex::encode(b"pwd\n").as_str()));
    }
}
