//! Integration tests for end-to-end flows
//!
//! These tests verify complete user flows and interactions between components.

use std::time::Duration;
use janus::auth::TokenStore;
use janus::config::Config;
use janus::session::SessionManager;
use janus::session_logger::SessionLogger;

/// Test complete authentication flow
#[test]
fn test_authentication_flow() {
    let token_store = TokenStore::new(300);

    // 1. Generate token
    let token = token_store
        .generate_and_store("192.168.1.1".to_string())
        .expect("Failed to generate token");
    assert_eq!(token.len(), 64);

    // 2. Verify token exists
    assert!(token_store.exists(&token).expect("Check failed"));
    assert!(token_store.is_valid(&token).expect("Check failed"));

    // 3. Validate token (marks as used)
    assert!(token_store.validate_token(&token).is_ok());

    // 4. Token cannot be reused (validation should fail even though token still exists)
    assert!(token_store.validate_token(&token).is_err());

    // 5. Cleanup
    let removed = token_store.cleanup_expired().expect("Cleanup failed");
    assert_eq!(removed, 0); // Token not expired, just used
}

/// Test session lifecycle: create, list, delete
#[test]
fn test_session_lifecycle() {
    let manager = SessionManager::new(10);

    // 1. List sessions (should be empty)
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    assert_eq!(sessions.len(), 0);

    // 2. Create first session
    let session1 = manager
        .create_session(None, None)
        .expect("Failed to create session 1");
    assert_eq!(manager.session_count().unwrap(), 1);

    // 3. Create second session
    let session2 = manager
        .create_session(None, None)
        .expect("Failed to create session 2");
    assert_eq!(manager.session_count().unwrap(), 2);

    // 4. List sessions (should have 2)
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    assert_eq!(sessions.len(), 2);
    let ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
    assert!(ids.contains(&session1));
    assert!(ids.contains(&session2));

    // 5. Verify session metadata
    for session in &sessions {
        assert!(session.pty_rows > 0);
        assert!(session.pty_cols > 0);
        assert!(session.last_activity_secs_ago < 10);
    }

    // 6. Delete first session
    manager
        .delete_session(&session1)
        .expect("Failed to delete session 1");
    assert_eq!(manager.session_count().unwrap(), 1);

    // 7. Verify first session is gone
    assert!(manager.get_session(&session1).is_err());
    assert!(manager.get_session(&session2).is_ok());

    // 8. Delete second session
    manager
        .delete_session(&session2)
        .expect("Failed to delete session 2");
    assert_eq!(manager.session_count().unwrap(), 0);

    // 9. List sessions (should be empty again)
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    assert_eq!(sessions.len(), 0);
}

/// Test session limit enforcement
#[test]
fn test_session_limit_enforcement() {
    let manager = SessionManager::new(3);

    // Create sessions up to limit
    let session1 = manager.create_session(None, None).expect("Failed to create session 1");
    let session2 = manager.create_session(None, None).expect("Failed to create session 2");
    let session3 = manager.create_session(None, None).expect("Failed to create session 3");

    assert_eq!(manager.session_count().unwrap(), 3);

    // Attempt to exceed limit
    let result = manager.create_session(None, None);
    assert!(result.is_err());
    assert_eq!(manager.session_count().unwrap(), 3);

    // Delete one session
    manager.delete_session(&session1).expect("Failed to delete session");
    assert_eq!(manager.session_count().unwrap(), 2);

    // Should be able to create again
    let session4 = manager.create_session(None, None).expect("Failed to create session 4");
    assert_eq!(manager.session_count().unwrap(), 3);

    // Cleanup
    manager.delete_session(&session2).unwrap();
    manager.delete_session(&session3).unwrap();
    manager.delete_session(&session4).unwrap();
}

/// Test idle timeout mechanism
#[test]
fn test_idle_timeout_mechanism() {
    let manager = SessionManager::new(10);

    // Create sessions
    let active_session = manager
        .create_session(None, None)
        .expect("Failed to create active session");

    let idle_session = manager
        .create_session(None, None)
        .expect("Failed to create idle session");

    assert_eq!(manager.session_count().unwrap(), 2);

    // Wait for sessions to become idle
    std::thread::sleep(Duration::from_secs(2));

    // Touch active session to prevent timeout
    manager
        .touch_session(&active_session)
        .expect("Failed to touch session");

    // Cleanup with 1 second timeout
    let cleaned = manager
        .cleanup_idle_sessions(1)
        .expect("Failed to cleanup idle sessions");

    // Should have cleaned up only the idle session
    assert_eq!(cleaned.len(), 1);
    assert!(cleaned.contains(&idle_session));
    assert_eq!(manager.session_count().unwrap(), 1);

    // Verify active session still exists
    assert!(manager.get_session(&active_session).is_ok());
    assert!(manager.get_session(&idle_session).is_err());

    // Cleanup
    manager.delete_session(&active_session).unwrap();
}

/// Test process reaping after exit
#[test]
fn test_process_reaping() {
    let manager = SessionManager::new(10);

    // Create sessions with commands that exit immediately
    let dead_session1 = manager
        .create_session(Some("/usr/bin/true".to_string()), None)
        .expect("Failed to create dead session 1");

    let dead_session2 = manager
        .create_session(Some("/usr/bin/false".to_string()), None)
        .expect("Failed to create dead session 2");

    // Create a long-running session
    let alive_session = manager
        .create_session(None, None)
        .expect("Failed to create alive session");

    assert_eq!(manager.session_count().unwrap(), 3);

    // Wait for short-lived sessions to exit
    std::thread::sleep(Duration::from_millis(500));

    // Reap dead sessions
    let reaped = manager
        .reap_dead_sessions()
        .expect("Failed to reap dead sessions");

    // Should have reaped exactly 2 sessions
    assert_eq!(reaped.len(), 2);
    assert!(reaped.contains(&dead_session1));
    assert!(reaped.contains(&dead_session2));
    assert_eq!(manager.session_count().unwrap(), 1);

    // Verify only alive session remains
    assert!(manager.get_session(&alive_session).is_ok());
    assert!(manager.get_session(&dead_session1).is_err());
    assert!(manager.get_session(&dead_session2).is_err());

    // Cleanup
    manager.delete_session(&alive_session).unwrap();
}

/// Test token expiration
#[test]
fn test_token_expiration() {
    let token_store = TokenStore::new(1); // 1 second expiry

    // Generate token
    let token = token_store
        .generate_and_store("192.168.1.1".to_string())
        .expect("Failed to generate token");

    // Token should be valid initially
    assert!(token_store.is_valid(&token).expect("Check failed"));

    // Wait for expiration
    std::thread::sleep(Duration::from_secs(2));

    // Token should be expired
    assert!(!token_store.is_valid(&token).expect("Check failed"));

    // Validation should fail
    let result = token_store.validate_token(&token);
    assert!(result.is_err());

    // Cleanup should remove expired token
    let removed = token_store.cleanup_expired().expect("Cleanup failed");
    assert_eq!(removed, 1);

    // Token should no longer exist
    assert!(!token_store.exists(&token).expect("Check failed"));
}

/// Test concurrent session operations
#[test]
fn test_concurrent_session_operations() {
    use std::sync::Arc;
    use std::thread;

    let manager = Arc::new(SessionManager::new(20));
    let mut handles = vec![];

    // Spawn multiple threads creating sessions
    for i in 0..5 {
        let manager_clone = Arc::clone(&manager);
        let handle = thread::spawn(move || {
            let session_id = manager_clone
                .create_session(None, None)
                .expect(&format!("Thread {} failed to create session", i));
            // Small delay
            thread::sleep(Duration::from_millis(50));
            manager_clone
                .delete_session(&session_id)
                .expect(&format!("Thread {} failed to delete session", i));
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // All sessions should be cleaned up
    assert_eq!(manager.session_count().unwrap(), 0);
}

/// Test session activity tracking
#[test]
fn test_session_activity_tracking() {
    let manager = SessionManager::new(10);

    let session_id = manager
        .create_session(None, None)
        .expect("Failed to create session");

    // Get initial activity time
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    let initial_age = sessions[0].last_activity_secs_ago;
    assert!(initial_age < 1);

    // Wait a bit
    std::thread::sleep(Duration::from_secs(1));

    // Activity should have aged
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    let aged = sessions[0].last_activity_secs_ago;
    assert!(aged >= 1);

    // Touch session
    manager
        .touch_session(&session_id)
        .expect("Failed to touch session");

    // Activity should be reset
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    let reset_age = sessions[0].last_activity_secs_ago;
    assert!(reset_age < 1);

    // Cleanup
    manager.delete_session(&session_id).unwrap();
}

/// Test PTY resize functionality
#[test]
fn test_pty_resize() {
    use portable_pty::PtySize;

    let manager = SessionManager::new(10);

    let session_id = manager
        .create_session(None, Some(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }))
        .expect("Failed to create session");

    // Verify initial size
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    assert_eq!(sessions[0].pty_rows, 24);
    assert_eq!(sessions[0].pty_cols, 80);

    // Resize
    let new_size = PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    };
    manager
        .resize_pty(&session_id, new_size)
        .expect("Failed to resize PTY");

    // Verify new size
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    assert_eq!(sessions[0].pty_rows, 40);
    assert_eq!(sessions[0].pty_cols, 120);

    // Cleanup
    manager.delete_session(&session_id).unwrap();
}

/// Test configuration creation with defaults
#[test]
fn test_config_with_defaults() {
    let config = Config::with_defaults();

    // Verify defaults are sane
    assert_eq!(config.max_sessions, 10);
    assert_eq!(config.idle_timeout_secs, 3600);
    assert_eq!(config.token_validity_secs, 300);
    assert_eq!(config.use_https, false);
    assert!(config.bind_address.starts_with("127.0.0.1:"));

    // Verify rate limit defaults
    assert_eq!(config.rate_limit.requests_per_period, 3);
    assert_eq!(config.rate_limit.period_secs, 60);
}

/// Test session logger initialization and logging
#[tokio::test]
async fn test_session_logger() {
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let log_dir = temp_dir.path();

    // Create logger
    let logger = SessionLogger::new(log_dir)
        .await
        .expect("Failed to create logger");

    // Log events
    logger.log_session_created(
        "test-session-1".to_string(),
        "/bin/bash".to_string(),
        24,
        80,
    );
    logger.log_input("test-session-1".to_string(), b"echo test\n".to_vec());
    logger.log_output("test-session-1".to_string(), b"test\n".to_vec());
    logger.log_resize("test-session-1".to_string(), 40, 120);
    logger.log_session_deleted("test-session-1".to_string());

    // Give logger time to write
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify log file was created
    let log_file = log_dir.join("test-session-1.log");
    assert!(log_file.exists());

    // Read and verify contents
    let contents = std::fs::read_to_string(&log_file).expect("Failed to read log file");
    assert!(contents.contains("SessionCreated"));
    assert!(contents.contains("Input"));
    assert!(contents.contains("Output"));
    assert!(contents.contains("Resize"));
    assert!(contents.contains("SessionDeleted"));
}

/// Test rate limit configuration via Config
#[test]
fn test_rate_limit_config() {
    let config = Config::with_defaults();

    // Verify replenish interval calculation
    let interval = config.rate_limit.replenish_interval_secs();
    assert_eq!(interval, 20); // 60 / 3 = 20 seconds per token
}

/// Test multiple session creation with custom shells
#[test]
fn test_custom_shell_commands() {
    let manager = SessionManager::new(10);

    // Create session with default shell
    let default_session = manager
        .create_session(None, None)
        .expect("Failed to create default session");

    // Create session with custom shell (short-lived command)
    let custom_session = manager
        .create_session(Some("/usr/bin/true".to_string()), None)
        .expect("Failed to create custom session");

    assert_eq!(manager.session_count().unwrap(), 2);

    // Wait for custom session to exit
    std::thread::sleep(Duration::from_millis(500));

    // Reap dead sessions
    let reaped = manager.reap_dead_sessions().expect("Failed to reap");
    assert_eq!(reaped.len(), 1);
    assert!(reaped.contains(&custom_session));

    // Only default session should remain
    assert_eq!(manager.session_count().unwrap(), 1);

    // Cleanup
    manager.delete_session(&default_session).unwrap();
}
