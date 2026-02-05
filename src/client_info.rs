//! Client information extraction and validation.
//!
//! Provides utilities for extracting client IP addresses and browser fingerprints
//! from HTTP requests. Handles proxy headers (X-Forwarded-For) for ngrok/reverse proxy setups.

use axum::http::HeaderMap;
use std::net::IpAddr;
use std::str::FromStr;
use thiserror::Error;

/// Errors that can occur during client info extraction
#[derive(Debug, Error)]
pub enum ClientInfoError {
    #[error("Missing required header: {0}")]
    MissingHeader(String),

    #[error("Invalid IP address: {0}")]
    InvalidIpAddress(String),

    #[error("No IP address found in request")]
    NoIpFound,
}

/// Browser fingerprint for session validation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// User-Agent header
    pub user_agent: String,

    /// Accept header (content types)
    pub accept: String,

    /// Accept-Language header
    pub accept_language: String,

    /// Accept-Encoding header
    pub accept_encoding: String,
}

impl Fingerprint {
    /// Extract fingerprint from HTTP headers
    ///
    /// # Arguments
    /// * `headers` - HTTP request headers
    ///
    /// # Returns
    /// A Fingerprint struct with extracted headers (empty strings for missing headers)
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let user_agent = headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let accept = headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let accept_language = headers
            .get("accept-language")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let accept_encoding = headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        Fingerprint {
            user_agent,
            accept,
            accept_language,
            accept_encoding,
        }
    }

    /// Check if this fingerprint matches another one
    ///
    /// Returns true if all fields match exactly
    pub fn matches(&self, other: &Fingerprint) -> bool {
        self == other
    }

    /// Check if fingerprint appears valid (has at least User-Agent)
    pub fn is_valid(&self) -> bool {
        !self.user_agent.is_empty()
    }
}

/// Extract client IP address from HTTP headers
///
/// Handles both direct connections and proxy scenarios (ngrok, reverse proxies).
/// Checks headers in order of preference:
/// 1. X-Forwarded-For (first IP, set by proxies)
/// 2. X-Real-IP (set by some proxies)
/// 3. Falls back to connection IP if available
///
/// # Arguments
/// * `headers` - HTTP request headers
///
/// # Returns
/// The client's IP address as a string
///
/// # Errors
/// Returns ClientInfoError::NoIpFound if no valid IP can be extracted
pub fn extract_client_ip(headers: &HeaderMap) -> Result<String, ClientInfoError> {
    // Try X-Forwarded-For first (ngrok sets this)
    if let Some(xff) = headers.get("x-forwarded-for") {
        if let Ok(xff_str) = xff.to_str() {
            // X-Forwarded-For can contain multiple IPs: "client, proxy1, proxy2"
            // Take the first one (original client)
            let first_ip = xff_str.split(',').next().unwrap_or("").trim();
            if !first_ip.is_empty() {
                // Validate it's a proper IP address
                if IpAddr::from_str(first_ip).is_ok() {
                    return Ok(first_ip.to_string());
                }
            }
        }
    }

    // Try X-Real-IP next
    if let Some(real_ip) = headers.get("x-real-ip") {
        if let Ok(ip_str) = real_ip.to_str() {
            let ip_str = ip_str.trim();
            if IpAddr::from_str(ip_str).is_ok() {
                return Ok(ip_str.to_string());
            }
        }
    }

    // No valid IP found
    Err(ClientInfoError::NoIpFound)
}

/// Validate that a request comes from the same IP address
///
/// # Arguments
/// * `headers` - HTTP request headers
/// * `expected_ip` - The expected IP address
///
/// # Returns
/// Ok(()) if IP matches, Err otherwise
pub fn validate_client_ip(headers: &HeaderMap, expected_ip: &str) -> Result<(), ClientInfoError> {
    let actual_ip = extract_client_ip(headers)?;

    if actual_ip == expected_ip {
        Ok(())
    } else {
        Err(ClientInfoError::InvalidIpAddress(format!(
            "IP mismatch: expected {}, got {}",
            expected_ip, actual_ip
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn test_extract_client_ip_from_xff() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.1, 198.51.100.1"),
        );

        let ip = extract_client_ip(&headers).unwrap();
        assert_eq!(ip, "203.0.113.1");
    }

    #[test]
    fn test_extract_client_ip_from_xff_single() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));

        let ip = extract_client_ip(&headers).unwrap();
        assert_eq!(ip, "203.0.113.1");
    }

    #[test]
    fn test_extract_client_ip_from_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.1"));

        let ip = extract_client_ip(&headers).unwrap();
        assert_eq!(ip, "203.0.113.1");
    }

    #[test]
    fn test_extract_client_ip_xff_priority() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.1"));

        // X-Forwarded-For should take priority
        let ip = extract_client_ip(&headers).unwrap();
        assert_eq!(ip, "203.0.113.1");
    }

    #[test]
    fn test_extract_client_ip_no_headers() {
        let headers = HeaderMap::new();
        let result = extract_client_ip(&headers);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ClientInfoError::NoIpFound));
    }

    #[test]
    fn test_extract_client_ip_invalid_format() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));

        let result = extract_client_ip(&headers);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_client_ip_matches() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));

        let result = validate_client_ip(&headers, "203.0.113.1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_client_ip_mismatch() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));

        let result = validate_client_ip(&headers, "198.51.100.1");
        assert!(result.is_err());
        match result.unwrap_err() {
            ClientInfoError::InvalidIpAddress(msg) => {
                assert!(msg.contains("IP mismatch"));
                assert!(msg.contains("203.0.113.1"));
                assert!(msg.contains("198.51.100.1"));
            }
            _ => panic!("Expected InvalidIpAddress error"),
        }
    }

    #[test]
    fn test_fingerprint_from_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static("Mozilla/5.0"));
        headers.insert("accept", HeaderValue::from_static("text/html"));
        headers.insert("accept-language", HeaderValue::from_static("en-US"));
        headers.insert("accept-encoding", HeaderValue::from_static("gzip"));

        let fingerprint = Fingerprint::from_headers(&headers);
        assert_eq!(fingerprint.user_agent, "Mozilla/5.0");
        assert_eq!(fingerprint.accept, "text/html");
        assert_eq!(fingerprint.accept_language, "en-US");
        assert_eq!(fingerprint.accept_encoding, "gzip");
    }

    #[test]
    fn test_fingerprint_missing_headers() {
        let headers = HeaderMap::new();
        let fingerprint = Fingerprint::from_headers(&headers);

        // Should have empty strings, not crash
        assert_eq!(fingerprint.user_agent, "");
        assert_eq!(fingerprint.accept, "");
        assert_eq!(fingerprint.accept_language, "");
        assert_eq!(fingerprint.accept_encoding, "");
    }

    #[test]
    fn test_fingerprint_matches() {
        let fp1 = Fingerprint {
            user_agent: "Mozilla/5.0".to_string(),
            accept: "text/html".to_string(),
            accept_language: "en-US".to_string(),
            accept_encoding: "gzip".to_string(),
        };

        let fp2 = Fingerprint {
            user_agent: "Mozilla/5.0".to_string(),
            accept: "text/html".to_string(),
            accept_language: "en-US".to_string(),
            accept_encoding: "gzip".to_string(),
        };

        assert!(fp1.matches(&fp2));
    }

    #[test]
    fn test_fingerprint_mismatch() {
        let fp1 = Fingerprint {
            user_agent: "Mozilla/5.0".to_string(),
            accept: "text/html".to_string(),
            accept_language: "en-US".to_string(),
            accept_encoding: "gzip".to_string(),
        };

        let fp2 = Fingerprint {
            user_agent: "Chrome/90.0".to_string(),
            accept: "text/html".to_string(),
            accept_language: "en-US".to_string(),
            accept_encoding: "gzip".to_string(),
        };

        assert!(!fp1.matches(&fp2));
    }

    #[test]
    fn test_fingerprint_is_valid() {
        let valid_fp = Fingerprint {
            user_agent: "Mozilla/5.0".to_string(),
            accept: "text/html".to_string(),
            accept_language: "en-US".to_string(),
            accept_encoding: "gzip".to_string(),
        };
        assert!(valid_fp.is_valid());

        let invalid_fp = Fingerprint {
            user_agent: "".to_string(),
            accept: "text/html".to_string(),
            accept_language: "en-US".to_string(),
            accept_encoding: "gzip".to_string(),
        };
        assert!(!invalid_fp.is_valid());
    }

    #[test]
    fn test_extract_ipv6_address() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("2001:db8::1"),
        );

        let ip = extract_client_ip(&headers).unwrap();
        assert_eq!(ip, "2001:db8::1");
    }

    #[test]
    fn test_extract_client_ip_with_spaces() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static(" 203.0.113.1 , 198.51.100.1 "),
        );

        let ip = extract_client_ip(&headers).unwrap();
        assert_eq!(ip, "203.0.113.1");
    }
}
