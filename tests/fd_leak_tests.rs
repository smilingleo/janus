//! Integration tests for file descriptor leak detection
//!
//! These tests verify that PTY operations don't leak file descriptors by
//! checking the number of open file descriptors before and after operations.

use std::time::Duration;
use janus::session::SessionManager;

/// Get the count of open file descriptors for the current process using /dev/fd
///
/// This is faster than using lsof and works on both macOS and Linux
fn get_open_fd_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        let pid = std::process::id();
        let fd_dir = format!("/proc/{}/fd", pid);
        std::fs::read_dir(&fd_dir)
            .expect("Failed to read /proc fd directory")
            .count()
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, we can count entries in /dev/fd
        std::fs::read_dir("/dev/fd")
            .expect("Failed to read /dev/fd directory")
            .count()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Fallback: return a dummy value to skip tests on unsupported platforms
        0
    }
}

/// Test that creating and deleting sessions doesn't leak file descriptors
#[test]
fn test_session_creation_deletion_no_fd_leak() {
    let manager = SessionManager::new(10);

    // Baseline FD count
    let baseline_fds = get_open_fd_count();
    println!("Baseline FD count: {}", baseline_fds);

    // Create sessions
    let mut session_ids = Vec::new();
    for i in 0..3 {
        let session_id = manager
            .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
            .expect(&format!("Failed to create session {}", i));
        session_ids.push(session_id);
    }

    let after_create_fds = get_open_fd_count();
    println!("FD count after creating 3 sessions: {}", after_create_fds);

    // FDs should have increased (PTY master/slave pairs + other handles)
    assert!(
        after_create_fds > baseline_fds,
        "FD count should increase after creating sessions"
    );

    // Delete all sessions
    for session_id in session_ids {
        manager
            .delete_session(&session_id)
            .expect("Failed to delete session");
    }

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(300));

    let after_delete_fds = get_open_fd_count();
    println!("FD count after deleting all sessions: {}", after_delete_fds);

    // FD count should return close to baseline (allow small variance for runtime artifacts)
    let fd_leak = after_delete_fds.saturating_sub(baseline_fds);
    assert!(
        fd_leak <= 5,
        "Significant FD leak detected: {} FDs remain after cleanup (baseline: {}, current: {})",
        fd_leak, baseline_fds, after_delete_fds
    );
}

/// Test that reaping dead sessions properly closes file descriptors
#[test]
fn test_reap_dead_sessions_closes_fds() {
    let manager = SessionManager::new(10);

    // Baseline FD count
    let baseline_fds = get_open_fd_count();
    println!("Baseline FD count: {}", baseline_fds);

    // Create short-lived sessions
    let mut session_ids = Vec::new();
    for _ in 0..3 {
        let session_id = manager
            .create_session(Some("/usr/bin/true".to_string()), None)
            .expect("Failed to create short-lived session");
        session_ids.push(session_id);
    }

    let after_create_fds = get_open_fd_count();
    println!("FD count after creating 3 sessions: {}", after_create_fds);

    // Wait for sessions to exit
    std::thread::sleep(Duration::from_millis(500));

    // Reap dead sessions
    let reaped = manager
        .reap_dead_sessions()
        .expect("Failed to reap dead sessions");

    assert_eq!(reaped.len(), 3, "Should have reaped all 3 sessions");

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(300));

    let after_reap_fds = get_open_fd_count();
    println!("FD count after reaping: {}", after_reap_fds);

    // FD count should return close to baseline
    let fd_leak = after_reap_fds.saturating_sub(baseline_fds);
    assert!(
        fd_leak <= 5,
        "FD leak detected after reaping: {} FDs remain (baseline: {}, current: {})",
        fd_leak, baseline_fds, after_reap_fds
    );
}

/// Test that mixed operations don't leak file descriptors
#[test]
fn test_mixed_operations_no_fd_leak() {
    let manager = SessionManager::new(10);

    // Baseline FD count
    let baseline_fds = get_open_fd_count();
    println!("Baseline FD count: {}", baseline_fds);

    // Create a mix of sessions
    let long_lived = manager
        .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
        .expect("Failed to create long-lived session");

    let short_lived = manager
        .create_session(Some("/usr/bin/true".to_string()), None)
        .expect("Failed to create short-lived session");

    let after_create_fds = get_open_fd_count();
    println!("FD count after creating 2 sessions: {}", after_create_fds);

    // Wait for short-lived to exit
    std::thread::sleep(Duration::from_millis(500));

    // Reap dead sessions
    let reaped = manager.reap_dead_sessions().expect("Failed to reap");
    assert_eq!(reaped.len(), 1, "Should have reaped 1 session");

    // Delete the long-lived session
    manager
        .delete_session(&long_lived)
        .expect("Failed to delete long-lived session");

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(300));

    let after_cleanup_fds = get_open_fd_count();
    println!("FD count after cleanup: {}", after_cleanup_fds);

    // FD count should return close to baseline
    let fd_leak = after_cleanup_fds.saturating_sub(baseline_fds);
    assert!(
        fd_leak <= 5,
        "FD leak detected: {} FDs remain (baseline: {}, current: {})",
        fd_leak, baseline_fds, after_cleanup_fds
    );
}
