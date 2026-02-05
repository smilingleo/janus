//! Configuration module for the Janus application.
//!
//! Handles loading and parsing configuration from TOML files, environment variables,
//! and command-line arguments. Provides validated configuration structs for use
//! throughout the application.

use crate::rate_limit::RateLimitConfig;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Validation error: {0}")]
    ValidationError(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationConfig {
    #[serde(rename = "imessage")]
    IMessage { phone_number: String },
    // Future: Slack, WhatsApp, etc.
}

#[derive(Debug, Clone, Copy)]
pub enum ErrorPolicy {
    FailFast,   // Server won't start if component fails
    Degraded,   // Continue without failed component
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_bind_address")]
    pub bind_address: String,

    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    #[serde(default = "default_token_validity_secs")]
    pub token_validity_secs: u64,

    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,

    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,

    #[serde(default = "default_session_log_dir")]
    pub session_log_dir: PathBuf,

    pub notification: NotificationConfig,

    #[serde(default = "default_use_https")]
    pub use_https: bool,

    #[serde(default)]
    pub tls_cert_path: Option<PathBuf>,

    #[serde(default)]
    pub tls_key_path: Option<PathBuf>,

    #[serde(default = "default_tls_auto_generate")]
    pub tls_auto_generate: bool,

    #[serde(default)]
    pub allowed_origins: Vec<String>,

    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

// Default value functions
fn default_bind_address() -> String {
    "127.0.0.1:8080".to_string()
}

fn default_idle_timeout_secs() -> u64 {
    3600
}

fn default_token_validity_secs() -> u64 {
    3600
}

fn default_max_sessions() -> usize {
    4
}

fn default_log_dir() -> PathBuf {
    PathBuf::from(".janus/logs")
}

fn default_session_log_dir() -> PathBuf {
    PathBuf::from(".janus/session-logs")
}

fn default_use_https() -> bool {
    false
}

fn default_tls_auto_generate() -> bool {
    true
}

impl Config {
    /// Create a configuration with sensible defaults.
    ///
    /// Note: This will panic if the default configuration is invalid,
    /// which should never happen unless defaults are misconfigured.
    pub fn with_defaults() -> Self {
        let mut config = Config {
            bind_address: default_bind_address(),
            idle_timeout_secs: default_idle_timeout_secs(),
            token_validity_secs: default_token_validity_secs(),
            max_sessions: default_max_sessions(),
            log_dir: default_log_dir(),
            session_log_dir: default_session_log_dir(),
            notification: NotificationConfig::IMessage {
                phone_number: "+1234567890".to_string(),
            },
            use_https: default_use_https(),
            tls_cert_path: None,
            tls_key_path: None,
            tls_auto_generate: default_tls_auto_generate(),
            allowed_origins: Vec::new(),
            rate_limit: RateLimitConfig::default(),
        };

        // Expand tilde in paths
        config.log_dir = expand_tilde(&config.log_dir);
        config.session_log_dir = expand_tilde(&config.session_log_dir);

        // Validate defaults (should never fail)
        config.validate().expect("Default configuration is invalid");

        config
    }

    /// Load configuration from a TOML file and validate it.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let mut config: Config = toml::from_str(&contents)?;

        // Expand tilde in log_dir path
        config.log_dir = expand_tilde(&config.log_dir);

        // Validate the configuration
        config.validate()?;

        Ok(config)
    }

    /// Validate the configuration values.
    fn validate(&self) -> Result<(), ConfigError> {
        // Validate bind_address is localhost-only
        if !is_localhost_address(&self.bind_address) {
            return Err(ConfigError::ValidationError(format!(
                "bind_address must be localhost (127.0.0.1 or [::1]), got: '{}'",
                self.bind_address
            )));
        }

        // Validate idle_timeout_secs
        if self.idle_timeout_secs == 0 {
            return Err(ConfigError::ValidationError(
                "idle_timeout_secs must be greater than 0".to_string(),
            ));
        }

        // Validate token_validity_secs
        if self.token_validity_secs == 0 {
            return Err(ConfigError::ValidationError(
                "token_validity_secs must be greater than 0".to_string(),
            ));
        }

        // Validate max_sessions
        if self.max_sessions == 0 {
            return Err(ConfigError::ValidationError(
                "max_sessions must be greater than 0".to_string(),
            ));
        }

        // Validate notification configuration
        match &self.notification {
            NotificationConfig::IMessage { phone_number } => {
                if !is_valid_phone_number(phone_number) {
                    return Err(ConfigError::ValidationError(format!(
                        "Invalid phone number format: '{}'. Phone number must start with + and contain only digits after",
                        phone_number
                    )));
                }
            }
        }

        // Validate allowed_origins configuration
        if !self.allowed_origins.is_empty() && !self.use_https {
            return Err(ConfigError::ValidationError(
                "HTTPS must be enabled when allowed_origins is configured".to_string()
            ));
        }

        // Validate that public origins use HTTPS
        for origin in &self.allowed_origins {
            if !origin.starts_with("https://") && !origin.starts_with("http://127.0.0.1:") && !origin.starts_with("http://localhost:") {
                return Err(ConfigError::ValidationError(
                    format!("Public origin must use HTTPS: {}", origin)
                ));
            }

            // Validate wildcard patterns
            if origin.contains('*') {
                let wildcard_count = origin.matches('*').count();
                if wildcard_count != 1 {
                    return Err(ConfigError::ValidationError(
                        format!("Origin pattern must contain exactly one wildcard (*): {}", origin)
                    ));
                }

                // Wildcard should be in the subdomain part (after https:// and before the next dot or slash)
                if !origin.starts_with("https://*.") && !origin.starts_with("http://*.") {
                    return Err(ConfigError::ValidationError(
                        format!("Wildcard (*) must be in subdomain position (e.g., https://*.example.com): {}", origin)
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Expand tilde (~) in a path to the user's home directory.
fn expand_tilde(path: &Path) -> PathBuf {
    if let Some(path_str) = path.to_str() {
        if path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path_str[2..]);
            }
        } else if path_str == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }
    }
    path.to_path_buf()
}

/// Validate that an address string is localhost-only.
/// Accepts: 127.0.0.1:port, localhost:port, [::1]:port
fn is_localhost_address(addr: &str) -> bool {
    addr.starts_with("127.0.0.1:")
        || addr.starts_with("localhost:")
        || addr.starts_with("[::1]:")
}

/// Validate phone number format: must start with + and contain 4-20 digits after.
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
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_configuration() {
        let config = Config::with_defaults();
        assert_eq!(config.bind_address, "127.0.0.1:8080");
        assert_eq!(config.idle_timeout_secs, 3600);
        assert_eq!(config.token_validity_secs, 3600);
        assert_eq!(config.max_sessions, 4);
        assert_eq!(config.use_https, false);
    }

    #[test]
    fn test_valid_phone_number() {
        assert!(is_valid_phone_number("+1234567890")); // 10 digits
        assert!(is_valid_phone_number("+1234")); // Minimum 4 digits
        assert!(is_valid_phone_number("+919876543210")); // 12 digits
        assert!(is_valid_phone_number("+12345678901234567890")); // Maximum 20 digits
    }

    #[test]
    fn test_invalid_phone_number() {
        assert!(!is_valid_phone_number("1234567890")); // Missing +
        assert!(!is_valid_phone_number("+")); // No digits
        assert!(!is_valid_phone_number("+123")); // Too few digits (< 4)
        assert!(!is_valid_phone_number("+1")); // Too few digits
        assert!(!is_valid_phone_number("+123456789012345678901")); // Too many digits (> 20)
        assert!(!is_valid_phone_number("+123abc")); // Contains letters
        assert!(!is_valid_phone_number("+123-456-7890")); // Contains dashes
        assert!(!is_valid_phone_number("")); // Empty string
    }

    #[test]
    fn test_from_file_valid_config() {
        let config_content = r#"
bind_address = "127.0.0.1:9090"
idle_timeout_secs = 7200
max_sessions = 8
token_validity_secs = 1800
log_dir = "/tmp/janus/logs"

[notification.imessage]
phone_number = "+1234567890"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Config::from_file(temp_file.path()).unwrap();
        assert_eq!(config.bind_address, "127.0.0.1:9090");
        assert_eq!(config.idle_timeout_secs, 7200);
        assert_eq!(config.token_validity_secs, 1800);
        assert_eq!(config.max_sessions, 8);
        assert_eq!(config.log_dir, PathBuf::from("/tmp/janus/logs"));
    }

    #[test]
    fn test_invalid_phone_number_validation() {
        let config_content = r#"
bind_address = "127.0.0.1:8080"
token_validity_secs = 3600
log_dir = "/tmp/logs"

[notification.imessage]
phone_number = "1234567890"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::from_file(temp_file.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::ValidationError(msg) => {
                assert!(msg.contains("Invalid phone number format"));
            }
            _ => panic!("Expected ValidationError"),
        }
    }

    #[test]
    fn test_invalid_timeout_values() {
        let config_content = r#"
bind_address = "127.0.0.1:8080"
idle_timeout_secs = 0
token_validity_secs = 3600
log_dir = "/tmp/logs"

[notification.imessage]
phone_number = "+1234567890"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::from_file(temp_file.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::ValidationError(msg) => {
                assert!(msg.contains("idle_timeout_secs must be greater than 0"));
            }
            _ => panic!("Expected ValidationError"),
        }
    }

    #[test]
    fn test_invalid_max_sessions() {
        let config_content = r#"
bind_address = "127.0.0.1:8080"
max_sessions = 0
token_validity_secs = 3600
log_dir = "/tmp/logs"

[notification.imessage]
phone_number = "+1234567890"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::from_file(temp_file.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::ValidationError(msg) => {
                assert!(msg.contains("max_sessions must be greater than 0"));
            }
            _ => panic!("Expected ValidationError"),
        }
    }

    #[test]
    fn test_tilde_expansion() {
        let path = Path::new("~/test/path");
        let expanded = expand_tilde(path);

        // Verify tilde was expanded (should not start with ~/)
        let expanded_str = expanded.to_str().unwrap();
        assert!(!expanded_str.starts_with("~/"));

        // Verify it's an absolute path now
        assert!(expanded.is_absolute() || cfg!(windows));
    }

    #[test]
    fn test_tilde_expansion_home_only() {
        let path = Path::new("~");
        let expanded = expand_tilde(path);

        // Should expand to home directory
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home);
        }
    }

    #[test]
    fn test_no_tilde_expansion() {
        let path = Path::new("/absolute/path");
        let expanded = expand_tilde(path);
        assert_eq!(expanded, path);
    }

    #[test]
    fn test_non_localhost_bind_address_rejected() {
        let config_content = r#"
bind_address = "0.0.0.0:8080"
token_validity_secs = 3600
log_dir = "/tmp/logs"

[notification.imessage]
phone_number = "+1234567890"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::from_file(temp_file.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::ValidationError(msg) => {
                assert!(msg.contains("bind_address must be localhost"));
            }
            _ => panic!("Expected ValidationError"),
        }
    }

    #[test]
    fn test_config_with_defaults() {
        let config_content = r#"
[notification.imessage]
phone_number = "+1234567890"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(config_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let config = Config::from_file(temp_file.path()).unwrap();

        // Should use defaults for missing fields
        assert_eq!(config.bind_address, "127.0.0.1:8080");
        assert_eq!(config.idle_timeout_secs, 3600);
        assert_eq!(config.token_validity_secs, 3600);
        assert_eq!(config.max_sessions, 4);
        assert_eq!(config.use_https, false);
    }
}
