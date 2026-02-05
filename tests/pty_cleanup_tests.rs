//! Integration tests for PTY process cleanup and zombie handling
//!
//! These tests verify that PTY processes are properly cleaned up and no
//! zombie processes remain after sessions are deleted or processes exit.

use std::process::Command;
use std::time::Duration;
use janus::session::SessionManager;

/// Helper function to count zombie processes owned by current user
///
/// Returns the number of zombie (<defunct>) processes found
#[cfg(target_os = "macos")]
fn count_zombie_processes() -> usize {
    let output = Command::new("ps")
        .args(&["-A", "-o", "state,command"])
        .output()
        .expect("Failed to execute ps command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|line| line.starts_with("Z"))
        .count()
}

#[cfg(target_os = "linux")]
fn count_zombie_processes() -> usize {
    let output = Command::new("ps")
        .args(&["-A", "-o", "state,command"])
        .output()
        .expect("Failed to execute ps command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|line| line.starts_with("Z"))
        .count()
}

/// Test that deleted sessions don't leave zombie processes
#[test]
fn test_delete_session_no_zombies() {
    let manager = SessionManager::new(10);

    // Count zombies before test
    let zombies_before = count_zombie_processes();

    // Create multiple sessions
    // Use default shell for long-running processes (will spawn user's shell)
    let session1 = manager
        .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
        .expect("Failed to create session 1");

    let session2 = manager
        .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
        .expect("Failed to create session 2");

    let session3 = manager
        .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
        .expect("Failed to create session 3");

    // Verify all sessions exist
    assert_eq!(manager.session_count().unwrap(), 3);

    // Delete sessions
    manager.delete_session(&session1).expect("Failed to delete session 1");
    manager.delete_session(&session2).expect("Failed to delete session 2");
    manager.delete_session(&session3).expect("Failed to delete session 3");

    // Wait a bit for process cleanup
    std::thread::sleep(Duration::from_millis(200));

    // Count zombies after cleanup
    let zombies_after = count_zombie_processes();

    // Zombie count should not have increased
    assert!(
        zombies_after <= zombies_before,
        "Zombie processes leaked. Before: {}, After: {}",
        zombies_before, zombies_after
    );

    // Verify all sessions are gone
    assert_eq!(manager.session_count().unwrap(), 0);
}

/// Test that reap_dead_sessions properly cleans up exited processes
#[test]
fn test_reap_dead_sessions_comprehensive() {
    let manager = SessionManager::new(10);

    // Create sessions with different lifetimes
    // Use /usr/bin paths which are standard on both macOS and Linux
    let short_lived1 = manager
        .create_session(Some("/usr/bin/true".to_string()), None)
        .expect("Failed to create short-lived session 1");

    let short_lived2 = manager
        .create_session(Some("/usr/bin/false".to_string()), None)
        .expect("Failed to create short-lived session 2");

    let long_lived = manager
        .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
        .expect("Failed to create long-lived session");

    // Verify all sessions exist
    assert_eq!(manager.session_count().unwrap(), 3);

    // Wait for short-lived sessions to exit
    std::thread::sleep(Duration::from_millis(500));

    // Count zombies before reaping
    let zombies_before = count_zombie_processes();

    // Reap dead sessions
    let reaped = manager
        .reap_dead_sessions()
        .expect("Failed to reap dead sessions");

    // Should have reaped exactly 2 sessions
    assert_eq!(reaped.len(), 2);
    assert!(reaped.contains(&short_lived1));
    assert!(reaped.contains(&short_lived2));

    // Long-lived session should still exist
    assert!(manager.get_session(&long_lived).is_ok());
    assert_eq!(manager.session_count().unwrap(), 1);

    // Wait a bit for process cleanup
    std::thread::sleep(Duration::from_millis(200));

    // Count zombies after reaping
    let zombies_after = count_zombie_processes();

    // Zombie count should not have increased (may have decreased if we cleaned up our own zombies)
    assert!(
        zombies_after <= zombies_before,
        "Zombie count increased after reaping. Before: {}, After: {}",
        zombies_before, zombies_after
    );

    // Clean up remaining session
    manager.delete_session(&long_lived).expect("Failed to delete long-lived session");
}

/// Test rapid session creation and deletion doesn't leak processes
#[test]
fn test_rapid_session_churn() {
    let manager = SessionManager::new(20);
    let zombies_before = count_zombie_processes();

    // Rapid create and delete cycle
    for i in 0..10 {
        // Alternate between short-lived command and default shell
        let cmd = if i % 2 == 0 {
            Some("/usr/bin/true".to_string())
        } else {
            None
        };
        let session_id = manager
            .create_session(cmd, None, "192.168.1.1".to_string(), "test-agent".to_string())
            .expect("Failed to create session");

        // Sometimes delete immediately, sometimes wait
        if i % 2 == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }

        manager.delete_session(&session_id).expect("Failed to delete session");
    }

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(500));

    // Verify no new zombies
    let zombies_after = count_zombie_processes();
    assert!(
        zombies_after <= zombies_before,
        "Zombie processes leaked during rapid churn. Before: {}, After: {}",
        zombies_before, zombies_after
    );

    // Verify all sessions cleaned up
    assert_eq!(manager.session_count().unwrap(), 0);
}

/// Test that mixed reaping and deletion works correctly
#[test]
fn test_mixed_reap_and_delete() {
    let manager = SessionManager::new(10);
    let zombies_before = count_zombie_processes();

    // Create mix of sessions
    let auto_exit1 = manager
        .create_session(Some("/usr/bin/true".to_string()), None)
        .expect("Failed to create auto-exit session 1");

    let manual_delete = manager
        .create_session(None, None, "192.168.1.1".to_string(), "test-agent".to_string())
        .expect("Failed to create manual-delete session");

    let auto_exit2 = manager
        .create_session(Some("/usr/bin/false".to_string()), None)
        .expect("Failed to create auto-exit session 2");

    // Wait for auto-exit sessions to finish
    std::thread::sleep(Duration::from_millis(500));

    // Manually delete one session
    manager.delete_session(&manual_delete).expect("Failed to manually delete session");

    // Reap dead sessions
    let reaped = manager.reap_dead_sessions().expect("Failed to reap");

    // Should have reaped the two auto-exit sessions
    assert_eq!(reaped.len(), 2);
    assert!(reaped.contains(&auto_exit1));
    assert!(reaped.contains(&auto_exit2));

    // All sessions should be gone
    assert_eq!(manager.session_count().unwrap(), 0);

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(200));

    // Verify no new zombies
    let zombies_after = count_zombie_processes();
    assert!(
        zombies_after <= zombies_before,
        "Zombie processes leaked during mixed cleanup. Before: {}, After: {}",
        zombies_before, zombies_after
    );
}
