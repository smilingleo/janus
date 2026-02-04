//! WebSocket module for terminal streaming.
//!
//! Handles WebSocket connections for real-time terminal I/O. Supports:
//! - Session authentication via cookies
//! - Bidirectional terminal streaming (PTY ↔ WebSocket)
//! - Control messages (resize, ping/pong)
//! - Backpressure handling

use axum::extract::ws::{Message, WebSocket};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during WebSocket operations
#[derive(Debug, Error)]
pub enum WebSocketError {
    #[error("WebSocket connection closed")]
    ConnectionClosed,

    #[error("Failed to read from PTY: {0}")]
    PtyReadFailed(String),

    #[error("Failed to write to PTY: {0}")]
    PtyWriteFailed(String),

    #[error("Failed to send WebSocket message: {0}")]
    SendFailed(String),

    #[error("Invalid message format: {0}")]
    InvalidMessage(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),
}

/// WebSocket message types for terminal communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalMessage {
    /// Terminal output data (PTY → client)
    Output { data: Vec<u8> },

    /// Terminal input data (client → PTY)
    Input { data: Vec<u8> },

    /// Resize terminal
    Resize { rows: u16, cols: u16 },

    /// Ping message for keepalive
    Ping,

    /// Pong response to ping
    Pong,

    /// Error message
    Error { message: String },

    /// Session attached successfully
    Attached { session_id: String },
}

impl TerminalMessage {
    /// Serialize message to JSON bytes
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, WebSocketError> {
        serde_json::to_vec(self)
            .map_err(|e| WebSocketError::InvalidMessage(format!("JSON serialization failed: {}", e)))
    }

    /// Deserialize message from JSON bytes
    pub fn from_json_bytes(data: &[u8]) -> Result<Self, WebSocketError> {
        serde_json::from_slice(data)
            .map_err(|e| WebSocketError::InvalidMessage(format!("JSON deserialization failed: {}", e)))
    }
}

/// WebSocket connection handler state
pub struct WebSocketHandler {
    /// Session ID this connection is attached to
    session_id: String,

    /// WebSocket connection
    socket: WebSocket,
}

impl WebSocketHandler {
    /// Create a new WebSocket handler
    pub fn new(session_id: String, socket: WebSocket) -> Self {
        WebSocketHandler {
            session_id,
            socket,
        }
    }

    /// Get the session ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Send a message to the client
    pub async fn send_message(&mut self, msg: TerminalMessage) -> Result<(), WebSocketError> {
        let json_bytes = msg.to_json_bytes()?;
        self.socket
            .send(Message::Text(String::from_utf8_lossy(&json_bytes).into_owned()))
            .await
            .map_err(|e| WebSocketError::SendFailed(e.to_string()))
    }

    /// Send binary output data to the client
    pub async fn send_output(&mut self, data: Vec<u8>) -> Result<(), WebSocketError> {
        self.send_message(TerminalMessage::Output { data }).await
    }

    /// Send error message to the client
    pub async fn send_error(&mut self, message: String) -> Result<(), WebSocketError> {
        self.send_message(TerminalMessage::Error { message }).await
    }

    /// Send pong response
    pub async fn send_pong(&mut self) -> Result<(), WebSocketError> {
        self.send_message(TerminalMessage::Pong).await
    }

    /// Receive and parse the next message from the client
    pub async fn receive_message(&mut self) -> Result<Option<TerminalMessage>, WebSocketError> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let msg = TerminalMessage::from_json_bytes(text.as_bytes())?;
                Ok(Some(msg))
            }
            Some(Ok(Message::Binary(data))) => {
                // Binary messages are treated as raw input
                Ok(Some(TerminalMessage::Input { data }))
            }
            Some(Ok(Message::Close(_))) => {
                tracing::info!(session_id = %self.session_id, "WebSocket close received");
                Ok(None)
            }
            Some(Ok(Message::Ping(_))) => {
                // Axum handles ping/pong automatically, but we can log it
                tracing::debug!(session_id = %self.session_id, "WebSocket ping received");
                Ok(Some(TerminalMessage::Ping))
            }
            Some(Ok(Message::Pong(_))) => {
                tracing::debug!(session_id = %self.session_id, "WebSocket pong received");
                Ok(Some(TerminalMessage::Pong))
            }
            Some(Err(e)) => {
                tracing::error!(
                    session_id = %self.session_id,
                    error = %e,
                    "WebSocket error"
                );
                Err(WebSocketError::SendFailed(e.to_string()))
            }
            None => {
                tracing::info!(session_id = %self.session_id, "WebSocket connection closed");
                Ok(None)
            }
        }
    }

    /// Close the WebSocket connection
    pub async fn close(self) -> Result<(), WebSocketError> {
        self.socket
            .close()
            .await
            .map_err(|e| WebSocketError::SendFailed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_message_serialization() {
        let msg = TerminalMessage::Output {
            data: b"hello world".to_vec(),
        };
        let bytes = msg.to_json_bytes().unwrap();
        let deserialized = TerminalMessage::from_json_bytes(&bytes).unwrap();

        match deserialized {
            TerminalMessage::Output { data } => {
                assert_eq!(data, b"hello world");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_terminal_message_resize() {
        let msg = TerminalMessage::Resize { rows: 30, cols: 120 };
        let bytes = msg.to_json_bytes().unwrap();
        let deserialized = TerminalMessage::from_json_bytes(&bytes).unwrap();

        match deserialized {
            TerminalMessage::Resize { rows, cols } => {
                assert_eq!(rows, 30);
                assert_eq!(cols, 120);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_terminal_message_types() {
        let messages = vec![
            TerminalMessage::Input {
                data: vec![1, 2, 3],
            },
            TerminalMessage::Ping,
            TerminalMessage::Pong,
            TerminalMessage::Error {
                message: "test error".to_string(),
            },
            TerminalMessage::Attached {
                session_id: "test-session".to_string(),
            },
        ];

        for msg in messages {
            let bytes = msg.to_json_bytes().unwrap();
            let _deserialized = TerminalMessage::from_json_bytes(&bytes).unwrap();
        }
    }
}
