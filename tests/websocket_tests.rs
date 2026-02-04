//! Tests for WebSocket message protocol
//!
//! These tests verify the WebSocket message serialization and protocol handling.

use serde_json;
use janus::websocket::TerminalMessage;

#[test]
fn test_output_message_serialization() {
    let msg = TerminalMessage::Output {
        data: vec![72, 101, 108, 108, 111], // "Hello"
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    assert!(json.contains("\"type\":\"output\""));
    assert!(json.contains("\"data\""));

    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");
    match deserialized {
        TerminalMessage::Output { data } => {
            assert_eq!(data, vec![72, 101, 108, 108, 111]);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_input_message_serialization() {
    let msg = TerminalMessage::Input {
        data: vec![108, 115, 10], // "ls\n"
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    assert!(json.contains("\"type\":\"input\""));

    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");
    match deserialized {
        TerminalMessage::Input { data } => {
            assert_eq!(data, vec![108, 115, 10]);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_resize_message_serialization() {
    let msg = TerminalMessage::Resize { rows: 40, cols: 120 };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    assert!(json.contains("\"type\":\"resize\""));
    assert!(json.contains("\"rows\":40"));
    assert!(json.contains("\"cols\":120"));

    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");
    match deserialized {
        TerminalMessage::Resize { rows, cols } => {
            assert_eq!(rows, 40);
            assert_eq!(cols, 120);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_ping_pong_messages() {
    let ping = TerminalMessage::Ping;
    let ping_json = serde_json::to_string(&ping).expect("Failed to serialize");
    assert!(ping_json.contains("\"type\":\"ping\""));

    let pong = TerminalMessage::Pong;
    let pong_json = serde_json::to_string(&pong).expect("Failed to serialize");
    assert!(pong_json.contains("\"type\":\"pong\""));

    // Deserialize
    let _: TerminalMessage = serde_json::from_str(&ping_json).expect("Failed to deserialize ping");
    let _: TerminalMessage = serde_json::from_str(&pong_json).expect("Failed to deserialize pong");
}

#[test]
fn test_error_message_serialization() {
    let msg = TerminalMessage::Error {
        message: "Session not found".to_string(),
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    assert!(json.contains("\"type\":\"error\""));
    assert!(json.contains("Session not found"));

    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");
    match deserialized {
        TerminalMessage::Error { message } => {
            assert_eq!(message, "Session not found");
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_attached_message_serialization() {
    let msg = TerminalMessage::Attached {
        session_id: "test-session-123".to_string(),
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    assert!(json.contains("\"type\":\"attached\""));
    assert!(json.contains("test-session-123"));

    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");
    match deserialized {
        TerminalMessage::Attached { session_id } => {
            assert_eq!(session_id, "test-session-123");
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_empty_data_serialization() {
    let msg = TerminalMessage::Output { data: vec![] };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");

    match deserialized {
        TerminalMessage::Output { data } => {
            assert_eq!(data, Vec::<u8>::new());
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_large_data_serialization() {
    let large_data = vec![65u8; 65536]; // 64KB of 'A'
    let msg = TerminalMessage::Output { data: large_data.clone() };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");

    match deserialized {
        TerminalMessage::Output { data } => {
            assert_eq!(data.len(), 65536);
            assert_eq!(data, large_data);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_binary_data_with_nulls() {
    // Binary data with null bytes
    let binary_data = vec![0, 1, 2, 3, 0, 0, 255, 254];
    let msg = TerminalMessage::Output {
        data: binary_data.clone(),
    };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");

    match deserialized {
        TerminalMessage::Output { data } => {
            assert_eq!(data, binary_data);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_resize_boundary_values() {
    // Minimum values
    let msg_min = TerminalMessage::Resize { rows: 1, cols: 1 };
    let json_min = serde_json::to_string(&msg_min).expect("Failed to serialize");
    let _: TerminalMessage = serde_json::from_str(&json_min).expect("Failed to deserialize");

    // Maximum reasonable values
    let msg_max = TerminalMessage::Resize {
        rows: 999,
        cols: 999,
    };
    let json_max = serde_json::to_string(&msg_max).expect("Failed to serialize");
    let deserialized: TerminalMessage =
        serde_json::from_str(&json_max).expect("Failed to deserialize");

    match deserialized {
        TerminalMessage::Resize { rows, cols } => {
            assert_eq!(rows, 999);
            assert_eq!(cols, 999);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_message_roundtrip() {
    let messages = vec![
        TerminalMessage::Output {
            data: b"Hello, World!".to_vec(),
        },
        TerminalMessage::Input {
            data: b"ls -la\n".to_vec(),
        },
        TerminalMessage::Resize { rows: 24, cols: 80 },
        TerminalMessage::Ping,
        TerminalMessage::Pong,
        TerminalMessage::Error {
            message: "Test error".to_string(),
        },
        TerminalMessage::Attached {
            session_id: "test-123".to_string(),
        },
    ];

    for msg in messages {
        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        let _: TerminalMessage = serde_json::from_str(&json).expect("Failed to deserialize");
    }
}

#[test]
fn test_utf8_in_output() {
    let utf8_data = "Hello, 世界! 🚀".as_bytes().to_vec();
    let msg = TerminalMessage::Output { data: utf8_data };

    let json = serde_json::to_string(&msg).expect("Failed to serialize");
    let deserialized: TerminalMessage =
        serde_json::from_str(&json).expect("Failed to deserialize");

    match deserialized {
        TerminalMessage::Output { data } => {
            let text = String::from_utf8(data).expect("Invalid UTF-8");
            assert_eq!(text, "Hello, 世界! 🚀");
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_invalid_message_type() {
    let invalid_json = r#"{"type":"unknown","data":[1,2,3]}"#;
    let result: Result<TerminalMessage, _> = serde_json::from_str(invalid_json);
    assert!(result.is_err());
}

#[test]
fn test_missing_required_field() {
    // Resize without cols
    let invalid_json = r#"{"type":"resize","rows":24}"#;
    let result: Result<TerminalMessage, _> = serde_json::from_str(invalid_json);
    assert!(result.is_err());

    // Output without data
    let invalid_json = r#"{"type":"output"}"#;
    let result: Result<TerminalMessage, _> = serde_json::from_str(invalid_json);
    assert!(result.is_err());
}

#[test]
fn test_snake_case_serialization() {
    let msg = TerminalMessage::Attached {
        session_id: "test".to_string(),
    };
    let json = serde_json::to_string(&msg).expect("Failed to serialize");

    // Should use snake_case for session_id
    assert!(json.contains("session_id"));
    assert!(!json.contains("sessionId"));
}
