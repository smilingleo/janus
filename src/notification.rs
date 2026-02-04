//! Notification module for the Janus application.
//!
//! Provides a trait-based notification system for sending authentication tokens
//! to users. Currently supports iMessage via AppleScript with input sanitization.

use async_trait::async_trait;
use std::process::Command;
use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during notification operations
#[derive(Debug, Error)]
pub enum NotificationError {
    #[error("Failed to send notification: {0}")]
    SendFailed(String),

    #[error("Notification timeout after {0} seconds")]
    Timeout(u64),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

/// Trait for sending notifications to users
#[async_trait]
pub trait NotificationSender: Send + Sync {
    /// Send a notification with the given token to the configured recipient
    ///
    /// # Arguments
    /// * `token` - The authentication token to send
    ///
    /// # Returns
    /// Ok(()) on success, NotificationError on failure
    async fn send_token(&self, token: &str) -> Result<(), NotificationError>;
}

/// iMessage notification sender using AppleScript
///
/// Sends authentication tokens via iMessage to a configured phone number.
/// Uses input sanitization to prevent AppleScript injection attacks.
#[derive(Debug)]
pub struct IMessageSender {
    phone_number: String,
    timeout_secs: u64,
}

impl IMessageSender {
    /// Create a new iMessage sender
    ///
    /// # Arguments
    /// * `phone_number` - The phone number to send messages to (format: +1234567890)
    /// * `timeout_secs` - Timeout in seconds for the AppleScript command (default: 10)
    ///
    /// # Returns
    /// A new IMessageSender instance
    ///
    /// # Errors
    /// Returns NotificationError::InvalidInput if phone number format is invalid
    pub fn new(phone_number: String, timeout_secs: u64) -> Result<Self, NotificationError> {
        // Validate phone number format
        if !is_valid_phone_number(&phone_number) {
            return Err(NotificationError::InvalidInput(format!(
                "Invalid phone number format: '{}'. Must start with + and contain only digits.",
                phone_number
            )));
        }

        Ok(IMessageSender {
            phone_number,
            timeout_secs,
        })
    }

    /// Build the AppleScript command to send an iMessage
    ///
    /// # Arguments
    /// * `sanitized_token` - The sanitized token (alphanumeric only)
    /// * `sanitized_phone` - The sanitized phone number (+ and digits only)
    ///
    /// # Returns
    /// The AppleScript command as a string
    fn build_applescript(&self, sanitized_token: &str, sanitized_phone: &str) -> String {
        format!(
            r#"tell application "Messages"
    set targetService to 1st account whose service type = iMessage
    set targetBuddy to participant "{}" of targetService
    send "Your Janus authentication token: {}" to targetBuddy
end tell"#,
            sanitized_phone, sanitized_token
        )
    }
}

#[async_trait]
impl NotificationSender for IMessageSender {
    async fn send_token(&self, token: &str) -> Result<(), NotificationError> {
        // Sanitize inputs to prevent AppleScript injection
        let sanitized_token = sanitize_token(token)?;
        let sanitized_phone = sanitize_phone_number(&self.phone_number)?;

        tracing::info!(
            phone_number = %sanitized_phone,
            token_length = sanitized_token.len(),
            "Sending authentication token via iMessage"
        );

        // Build AppleScript command
        let script = self.build_applescript(&sanitized_token, &sanitized_phone);

        // Execute AppleScript with timeout
        let handle = tokio::task::spawn_blocking(move || {
            Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()
        });

        let result = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            handle,
        )
        .await;

        match result {
            Ok(Ok(Ok(output))) => {
                if output.status.success() {
                    tracing::info!("iMessage sent successfully");
                    Ok(())
                } else {
                    let error_msg = String::from_utf8_lossy(&output.stderr);
                    tracing::error!(error = %error_msg, "Failed to send iMessage");
                    Err(NotificationError::SendFailed(error_msg.to_string()))
                }
            }
            Ok(Ok(Err(e))) => {
                tracing::error!(error = %e, "Failed to spawn osascript process");
                Err(NotificationError::SendFailed(e.to_string()))
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "Task join error");
                Err(NotificationError::SendFailed(e.to_string()))
            }
            Err(_) => {
                // Timeout occurred - the subprocess may still be running
                // Note: spawn_blocking tasks cannot be aborted, but at least we won't wait
                tracing::error!(
                    timeout_secs = self.timeout_secs,
                    "iMessage send timeout - subprocess may still be running"
                );
                Err(NotificationError::Timeout(self.timeout_secs))
            }
        }
    }
}

/// Sanitize a token to contain only alphanumeric characters
///
/// # Arguments
/// * `token` - The token to sanitize
///
/// # Returns
/// The sanitized token containing only alphanumeric characters
///
/// # Errors
/// Returns NotificationError::InvalidInput if token contains no valid characters
fn sanitize_token(token: &str) -> Result<String, NotificationError> {
    let sanitized: String = token.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

    if sanitized.is_empty() {
        return Err(NotificationError::InvalidInput(
            "Token contains no valid alphanumeric characters".to_string(),
        ));
    }

    Ok(sanitized)
}

/// Sanitize a phone number to contain only + and digits
///
/// # Arguments
/// * `phone` - The phone number to sanitize
///
/// # Returns
/// The sanitized phone number
///
/// # Errors
/// Returns NotificationError::InvalidInput if phone number is invalid after sanitization
fn sanitize_phone_number(phone: &str) -> Result<String, NotificationError> {
    let sanitized: String = phone
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();

    if !is_valid_phone_number(&sanitized) {
        return Err(NotificationError::InvalidInput(format!(
            "Invalid phone number after sanitization: '{}'",
            sanitized
        )));
    }

    Ok(sanitized)
}

/// Validate phone number format
///
/// # Arguments
/// * `phone` - The phone number to validate
///
/// # Returns
/// true if valid, false otherwise
fn is_valid_phone_number(phone: &str) -> bool {
    if !phone.starts_with('+') {
        return false;
    }

    let digits = &phone[1..];
    let digit_count = digits.len();

    // Phone numbers should be between 4 and 20 digits (excluding country code +)
    if digit_count < 4 || digit_count > 20 {
        return false;
    }

    digits.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_token_valid() {
        let token = "abc123XYZ";
        let sanitized = sanitize_token(token).unwrap();
        assert_eq!(sanitized, "abc123XYZ");
    }

    #[test]
    fn test_sanitize_token_with_special_chars() {
        let token = "abc-123_XYZ!@#";
        let sanitized = sanitize_token(token).unwrap();
        assert_eq!(sanitized, "abc123XYZ");
    }

    #[test]
    fn test_sanitize_token_empty() {
        let token = "!@#$%^&*()";
        let result = sanitize_token(token);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), NotificationError::InvalidInput(_)));
    }

    #[test]
    fn test_sanitize_phone_number_valid() {
        let phone = "+1234567890";
        let sanitized = sanitize_phone_number(phone).unwrap();
        assert_eq!(sanitized, "+1234567890");
    }

    #[test]
    fn test_sanitize_phone_number_with_dashes() {
        let phone = "+123-456-7890";
        let sanitized = sanitize_phone_number(phone).unwrap();
        assert_eq!(sanitized, "+1234567890");
    }

    #[test]
    fn test_sanitize_phone_number_invalid() {
        let phone = "1234567890"; // Missing +
        let result = sanitize_phone_number(phone);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_valid_phone_number() {
        assert!(is_valid_phone_number("+1234567890"));
        assert!(is_valid_phone_number("+123456789012345")); // 15 digits
        assert!(!is_valid_phone_number("1234567890")); // Missing +
        assert!(!is_valid_phone_number("+123")); // Too short
        assert!(!is_valid_phone_number("+123456789012345678901")); // Too long (21 digits)
    }

    #[test]
    fn test_imessage_sender_creation_valid() {
        let sender = IMessageSender::new("+1234567890".to_string(), 10);
        assert!(sender.is_ok());
    }

    #[test]
    fn test_imessage_sender_creation_invalid_phone() {
        let sender = IMessageSender::new("invalid".to_string(), 10);
        assert!(sender.is_err());
        assert!(matches!(sender.unwrap_err(), NotificationError::InvalidInput(_)));
    }

    #[test]
    fn test_build_applescript() {
        let sender = IMessageSender::new("+1234567890".to_string(), 10).unwrap();
        let script = sender.build_applescript("abc123", "+1234567890");

        assert!(script.contains("Messages"));
        assert!(script.contains("+1234567890"));
        assert!(script.contains("abc123"));
        assert!(script.contains("Your Janus authentication token"));
    }
}
