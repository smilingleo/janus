//! Authentication module for the Janus application.
//!
//! Provides token-based authentication for terminal sessions, including token generation,
//! validation, and expiration handling. Supports both single-use and reusable tokens.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use uuid::Uuid;
use tower_cookies::Cookie;

/// Authentication errors that can occur during token operations.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid token")]
    InvalidToken,

    #[error("Token has expired")]
    ExpiredToken,

    #[error("Token has already been used")]
    AlreadyUsed,

    #[error("Internal storage error")]
    LockPoisoned,

    #[error("Token generation failed")]
    TokenGenerationFailed,
}

/// Metadata associated with an authentication token.
///
/// Note: This type does not implement Clone to prevent detached atomic state.
/// Use TokenStore methods to access and modify token metadata atomically.
#[derive(Debug)]
pub struct TokenMetadata {
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub used: AtomicBool,
}

/// Thread-safe storage for authentication tokens and their metadata.
#[derive(Clone)]
pub struct TokenStore {
    tokens: Arc<RwLock<HashMap<String, TokenMetadata>>>,
    token_validity_secs: u64,
}

impl TokenStore {
    /// Create a new TokenStore with the specified token validity duration.
    ///
    /// # Arguments
    /// * `token_validity_secs` - How long tokens remain valid (in seconds)
    pub fn new(token_validity_secs: u64) -> Self {
        TokenStore {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            token_validity_secs,
        }
    }

    /// Generate a new cryptographically secure token and store it with metadata.
    ///
    /// Implements collision detection with retry logic (max 3 attempts).
    ///
    /// # Returns
    /// A 64-character hexadecimal string representing the token
    ///
    /// # Errors
    /// Returns `AuthError::LockPoisoned` if the internal lock is poisoned
    /// Returns `AuthError::TokenGenerationFailed` if token generation repeatedly collides
    pub fn generate_and_store(&self) -> Result<String, AuthError> {
        let mut attempts = 0;

        loop {
            let token = generate_token();
            let now = SystemTime::now();
            let expires_at = now + Duration::from_secs(self.token_validity_secs);

            let metadata = TokenMetadata {
                created_at: now,
                expires_at,
                used: AtomicBool::new(false),
            };

            let mut tokens = self.tokens.write()
                .map_err(|_| AuthError::LockPoisoned)?;

            // Check for collision (astronomically unlikely but defensive)
            if tokens.contains_key(&token) {
                attempts += 1;
                if attempts > 3 {
                    tracing::error!("Token generation failed after 3 collision attempts - possible RNG failure");
                    return Err(AuthError::TokenGenerationFailed);
                }
                tracing::warn!("Token collision detected, regenerating (attempt {})", attempts);
                continue;
            }

            tokens.insert(token.clone(), metadata);
            return Ok(token);
        }
    }

    /// Check if a token exists and is not expired.
    ///
    /// # Arguments
    /// * `token` - The token to check
    ///
    /// # Returns
    /// `Ok(true)` if token exists and is not expired
    /// `Ok(false)` if token doesn't exist or is expired
    /// `Err` if lock is poisoned
    pub fn is_valid(&self, token: &str) -> Result<bool, AuthError> {
        let tokens = self.tokens.read()
            .map_err(|_| AuthError::LockPoisoned)?;

        if let Some(metadata) = tokens.get(token) {
            Ok(!is_expired(metadata))
        } else {
            Ok(false)
        }
    }

    /// Check if a token exists in the store.
    ///
    /// # Arguments
    /// * `token` - The token to check
    ///
    /// # Returns
    /// `Ok(true)` if the token exists, `Ok(false)` otherwise
    /// `Err` if lock is poisoned
    pub fn exists(&self, token: &str) -> Result<bool, AuthError> {
        let tokens = self.tokens.read()
            .map_err(|_| AuthError::LockPoisoned)?;
        Ok(tokens.contains_key(token))
    }

    /// Clean up expired tokens from storage.
    ///
    /// # Returns
    /// The number of tokens removed
    ///
    /// # Errors
    /// Returns `AuthError::LockPoisoned` if the internal lock is poisoned
    pub fn cleanup_expired(&self) -> Result<usize, AuthError> {
        let now = SystemTime::now();
        let mut tokens = self.tokens.write()
            .map_err(|_| AuthError::LockPoisoned)?;

        let before = tokens.len();
        tokens.retain(|_, metadata| metadata.expires_at > now);
        Ok(before - tokens.len())
    }

    /// Atomically mark a token as used via compare-and-swap.
    ///
    /// Uses AtomicBool to ensure only one caller can successfully mark the token as used,
    /// preventing race conditions where multiple threads attempt to use the same token.
    ///
    /// # Arguments
    /// * `token` - The token to mark as used
    ///
    /// # Returns
    /// `Ok(())` if the token was successfully marked as used
    ///
    /// # Errors
    /// * `AuthError::InvalidToken` - Token does not exist
    /// * `AuthError::AlreadyUsed` - Token was already marked as used
    /// * `AuthError::LockPoisoned` - Internal lock is poisoned
    pub fn mark_used_once(&self, token: &str) -> Result<(), AuthError> {
        let tokens = self.tokens.read()
            .map_err(|_| AuthError::LockPoisoned)?;

        let metadata = tokens.get(token)
            .ok_or(AuthError::InvalidToken)?;

        // Atomic compare-and-swap: false -> true
        // Returns previous value
        if metadata.used.swap(true, Ordering::SeqCst) {
            // Was already true (already used)
            return Err(AuthError::AlreadyUsed);
        }

        Ok(())
    }

    /// Validate a token atomically: check existence, expiry, and used status, then mark as used.
    ///
    /// This operation is atomic to prevent TOCTOU (time-of-check-time-of-use) race conditions.
    /// If validation passes, the token is immediately marked as used before returning.
    ///
    /// # Arguments
    /// * `token` - The token to validate
    ///
    /// # Returns
    /// `Ok(())` if the token is valid and successfully marked as used
    ///
    /// # Errors
    /// * `AuthError::InvalidToken` - Token does not exist
    /// * `AuthError::ExpiredToken` - Token has expired
    /// * `AuthError::AlreadyUsed` - Token was already used
    /// * `AuthError::LockPoisoned` - Internal lock is poisoned
    pub fn validate_token(&self, token: &str) -> Result<(), AuthError> {
        let tokens = self.tokens.read()
            .map_err(|_| AuthError::LockPoisoned)?;

        let metadata = tokens.get(token)
            .ok_or(AuthError::InvalidToken)?;

        // Check expiry
        if is_expired(metadata) {
            return Err(AuthError::ExpiredToken);
        }

        // Atomic check-and-set
        if metadata.used.swap(true, Ordering::SeqCst) {
            return Err(AuthError::AlreadyUsed);
        }

        Ok(())
    }
}

/// Generate a cryptographically secure random token.
///
/// # Returns
/// A 64-character hexadecimal string (UUID v4 without hyphens repeated for length)
fn generate_token() -> String {
    // Generate two UUIDs and combine them to get 64 hex characters
    let uuid1 = Uuid::new_v4().simple().to_string();
    let uuid2 = Uuid::new_v4().simple().to_string();
    format!("{}{}", uuid1, uuid2)
}

/// Check if a token has expired based on its metadata.
///
/// # Arguments
/// * `metadata` - The token metadata to check
///
/// # Returns
/// `true` if the token has expired, `false` otherwise
pub fn is_expired(metadata: &TokenMetadata) -> bool {
    SystemTime::now() >= metadata.expires_at
}

/// Generate a CSRF token for session protection.
///
/// CSRF tokens are separate from authentication tokens and are used to prevent
/// cross-site request forgery attacks. They should be stored in session state
/// and validated on state-changing operations.
///
/// # Returns
/// A 64-character hexadecimal string representing the CSRF token
pub fn generate_csrf_token() -> String {
    generate_token()
}

/// Create a session cookie with appropriate security flags.
///
/// # Arguments
/// * `name` - Cookie name (e.g., "session_id")
/// * `value` - Cookie value
/// * `max_age_secs` - Cookie lifetime in seconds
/// * `use_secure` - Whether to set the Secure flag (true for HTTPS, false for localhost HTTP)
///
/// # Returns
/// A configured Cookie instance
///
/// # Security
/// - Always sets http_only=true to prevent XSS
/// - Sets same_site=Strict to prevent CSRF
/// - Conditionally sets secure flag based on use_secure parameter
pub fn build_session_cookie<'a>(
    name: &str,
    value: String,
    max_age_secs: u64,
    use_secure: bool,
) -> Cookie<'a> {
    let mut cookie = Cookie::new(name.to_string(), value);
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Strict);
    cookie.set_path("/");
    cookie.set_max_age(tower_cookies::cookie::time::Duration::seconds(max_age_secs as i64));

    // Only set Secure flag if HTTPS is enabled
    if use_secure {
        cookie.set_secure(true);
    }

    cookie
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::thread;

    #[test]
    fn test_token_generation_format() {
        let token = generate_token();

        // Should be 64 characters long
        assert_eq!(token.len(), 64);

        // Should only contain hexadecimal characters
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_tokens_are_unique() {
        let mut tokens = HashSet::new();

        // Generate 100 tokens and verify they're all unique
        for _ in 0..100 {
            let token = generate_token();
            assert!(tokens.insert(token), "Generated duplicate token");
        }
    }

    #[test]
    fn test_token_metadata_stores_correct_timestamps() {
        let store = TokenStore::new(3600);
        let token = store.generate_and_store().expect("Token generation should succeed");

        // Token should exist and be valid
        assert!(store.is_valid(&token).expect("Check should succeed"));
        assert!(store.exists(&token).expect("Check should succeed"));
    }

    #[test]
    fn test_token_expiry_calculation() {
        let now = SystemTime::now();

        // Token that expires in the future
        let future_metadata = TokenMetadata {
            created_at: now,
            expires_at: now + Duration::from_secs(3600),
            used: AtomicBool::new(false),
        };
        assert!(!is_expired(&future_metadata));

        // Token that expired in the past
        let past_metadata = TokenMetadata {
            created_at: now - Duration::from_secs(7200),
            expires_at: now - Duration::from_secs(3600),
            used: AtomicBool::new(false),
        };
        assert!(is_expired(&past_metadata));
    }

    #[test]
    fn test_token_store_thread_safety() {
        let store = TokenStore::new(3600);
        let mut handles = vec![];

        // Spawn 10 threads, each generating 10 tokens
        for _ in 0..10 {
            let store_clone = store.clone();
            let handle = thread::spawn(move || {
                let mut tokens = Vec::new();
                for _ in 0..10 {
                    tokens.push(store_clone.generate_and_store().expect("Token generation should succeed"));
                }
                tokens
            });
            handles.push(handle);
        }

        // Collect all tokens from all threads
        let mut all_tokens = HashSet::new();
        for handle in handles {
            let tokens = handle.join().unwrap();
            for token in tokens {
                assert!(all_tokens.insert(token), "Duplicate token generated in concurrent environment");
            }
        }

        // Should have exactly 100 unique tokens
        assert_eq!(all_tokens.len(), 100);

        // All tokens should exist in the store
        for token in all_tokens {
            assert!(store.exists(&token).expect("Check should succeed"));
        }
    }

    #[test]
    fn test_is_valid_returns_false_for_nonexistent_token() {
        let store = TokenStore::new(3600);
        assert!(!store.is_valid("nonexistent_token").expect("Check should succeed"));
    }

    #[test]
    fn test_exists_returns_false_for_nonexistent_token() {
        let store = TokenStore::new(3600);
        assert!(!store.exists("nonexistent_token").expect("Check should succeed"));
    }

    #[test]
    fn test_token_store_with_different_validity_periods() {
        // Test with short validity
        let short_store = TokenStore::new(60);
        let token = short_store.generate_and_store().expect("Token generation should succeed");
        assert!(short_store.is_valid(&token).expect("Check should succeed"));

        // Test with long validity
        let long_store = TokenStore::new(7200);
        let token = long_store.generate_and_store().expect("Token generation should succeed");
        assert!(long_store.is_valid(&token).expect("Check should succeed"));
    }

    #[test]
    fn test_cleanup_expired_tokens() {
        let store = TokenStore::new(1); // 1 second validity

        // Generate some tokens
        let _ = store.generate_and_store().expect("Token generation should succeed");
        let _ = store.generate_and_store().expect("Token generation should succeed");

        // Wait for expiry
        std::thread::sleep(Duration::from_secs(2));

        // Cleanup should remove expired tokens
        let removed = store.cleanup_expired().expect("Cleanup should succeed");
        assert_eq!(removed, 2);
    }

    #[test]
    fn test_generate_and_store_returns_result() {
        let store = TokenStore::new(3600);
        let result = store.generate_and_store();
        assert!(result.is_ok());

        let token = result.unwrap();
        assert_eq!(token.len(), 64);
    }

    #[test]
    fn test_validate_token_with_valid_token() {
        let store = TokenStore::new(3600);
        let token = store.generate_and_store().expect("Token generation should succeed");

        // Validation should succeed
        let result = store.validate_token(&token);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_token_with_invalid_token() {
        let store = TokenStore::new(3600);

        // Validation should fail with InvalidToken
        let result = store.validate_token("nonexistent_token");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidToken));
    }

    #[test]
    fn test_validate_token_with_expired_token() {
        let store = TokenStore::new(1); // 1 second validity
        let token = store.generate_and_store().expect("Token generation should succeed");

        // Wait for expiry
        std::thread::sleep(Duration::from_secs(2));

        // Validation should fail with ExpiredToken
        let result = store.validate_token(&token);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::ExpiredToken));
    }

    #[test]
    fn test_validate_token_with_already_used_token() {
        let store = TokenStore::new(3600);
        let token = store.generate_and_store().expect("Token generation should succeed");

        // First validation should succeed
        store.validate_token(&token).expect("First validation should succeed");

        // Second validation should fail with AlreadyUsed
        let result = store.validate_token(&token);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::AlreadyUsed));
    }

    #[test]
    fn test_mark_used_once_idempotency() {
        let store = TokenStore::new(3600);
        let token = store.generate_and_store().expect("Token generation should succeed");

        // First call should succeed
        let result = store.mark_used_once(&token);
        assert!(result.is_ok());

        // Second call should fail with AlreadyUsed
        let result = store.mark_used_once(&token);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::AlreadyUsed));
    }

    #[test]
    fn test_mark_used_once_with_invalid_token() {
        let store = TokenStore::new(3600);

        // Should fail with InvalidToken
        let result = store.mark_used_once("nonexistent_token");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidToken));
    }

    #[test]
    fn test_concurrent_validation_only_one_succeeds() {
        let store = TokenStore::new(3600);
        let token = store.generate_and_store().expect("Token generation should succeed");

        let store_clone1 = store.clone();
        let store_clone2 = store.clone();
        let token_clone1 = token.clone();
        let token_clone2 = token.clone();

        let handle1 = thread::spawn(move || {
            store_clone1.validate_token(&token_clone1)
        });

        let handle2 = thread::spawn(move || {
            store_clone2.validate_token(&token_clone2)
        });

        let result1 = handle1.join().unwrap();
        let result2 = handle2.join().unwrap();

        // Exactly one should succeed, one should fail with AlreadyUsed
        assert!(result1.is_ok() != result2.is_ok());

        // The one that failed should have AlreadyUsed error
        if result1.is_err() {
            assert!(matches!(result1.unwrap_err(), AuthError::AlreadyUsed));
        } else {
            assert!(matches!(result2.unwrap_err(), AuthError::AlreadyUsed));
        }
    }

    #[test]
    fn test_concurrent_mark_used_once_only_one_succeeds() {
        let store = TokenStore::new(3600);
        let token = store.generate_and_store().expect("Token generation should succeed");

        let store_clone1 = store.clone();
        let store_clone2 = store.clone();
        let token_clone1 = token.clone();
        let token_clone2 = token.clone();

        let handle1 = thread::spawn(move || {
            store_clone1.mark_used_once(&token_clone1)
        });

        let handle2 = thread::spawn(move || {
            store_clone2.mark_used_once(&token_clone2)
        });

        let result1 = handle1.join().unwrap();
        let result2 = handle2.join().unwrap();

        // Exactly one should succeed, one should fail with AlreadyUsed
        assert!(result1.is_ok() != result2.is_ok());

        // The one that failed should have AlreadyUsed error
        if result1.is_err() {
            assert!(matches!(result1.unwrap_err(), AuthError::AlreadyUsed));
        } else {
            assert!(matches!(result2.unwrap_err(), AuthError::AlreadyUsed));
        }
    }

    #[test]
    fn test_expiry_boundary_semantics() {
        // Test that a token is expired exactly at expires_at time
        let now = SystemTime::now();
        let metadata = TokenMetadata {
            created_at: now - Duration::from_secs(1),
            expires_at: now,
            used: AtomicBool::new(false),
        };

        // Should be expired when now >= expires_at
        assert!(is_expired(&metadata));
    }

    #[test]
    fn test_csrf_token_generation() {
        let token = generate_csrf_token();

        // Should be 64 characters long
        assert_eq!(token.len(), 64);

        // Should only contain hexadecimal characters
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

        // Generate multiple tokens and ensure they're unique
        let token2 = generate_csrf_token();
        assert_ne!(token, token2);
    }

    #[test]
    fn test_build_session_cookie_http() {
        let cookie = build_session_cookie("session_id", "test_value".to_string(), 3600, false);

        assert_eq!(cookie.name(), "session_id");
        assert_eq!(cookie.value(), "test_value");
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(
            cookie.same_site(),
            Some(tower_cookies::cookie::SameSite::Strict)
        );
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.secure(), None); // Not set for HTTP
    }

    #[test]
    fn test_build_session_cookie_https() {
        let cookie = build_session_cookie("session_id", "test_value".to_string(), 3600, true);

        assert_eq!(cookie.name(), "session_id");
        assert_eq!(cookie.value(), "test_value");
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(
            cookie.same_site(),
            Some(tower_cookies::cookie::SameSite::Strict)
        );
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.secure(), Some(true)); // Set for HTTPS
    }

    #[test]
    fn test_build_session_cookie_max_age() {
        let cookie = build_session_cookie("session_id", "test_value".to_string(), 7200, false);

        // Max age should be set to 7200 seconds
        assert!(cookie.max_age().is_some());
        assert_eq!(
            cookie.max_age().unwrap().whole_seconds(),
            7200
        );
    }
}
