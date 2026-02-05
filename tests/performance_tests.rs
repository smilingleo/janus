//! Performance and stress tests
//!
//! These tests verify system behavior under load and rapid operations.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use janus::auth::TokenStore;
use janus::session::SessionManager;

/// Test rapid token generation
#[test]
fn test_rapid_token_generation() {
    let token_store = TokenStore::new(300);
    let start = Instant::now();
    let count = 100;

    for _ in 0..count {
        token_store
            .generate_and_store("192.168.1.1".to_string())
            .expect("Failed to generate token");
    }

    let elapsed = start.elapsed();
    println!(
        "Generated {} tokens in {:?} ({:.2} tokens/sec)",
        count,
        elapsed,
        count as f64 / elapsed.as_secs_f64()
    );

    // Should be reasonably fast
    assert!(elapsed < Duration::from_secs(5));
}

/// Test rapid session creation and deletion
#[test]
#[ignore] // Slow test - creates many sessions
fn test_rapid_session_churn() {
    let manager = SessionManager::new(50);
    let start = Instant::now();
    let iterations = 20;

    for i in 0..iterations {
        let session_id = manager
            .create_session(None, None)
            .expect(&format!("Failed to create session {}", i));

        manager
            .delete_session(&session_id)
            .expect(&format!("Failed to delete session {}", i));
    }

    let elapsed = start.elapsed();
    println!(
        "Created and deleted {} sessions in {:?} ({:.2} ops/sec)",
        iterations,
        elapsed,
        iterations as f64 / elapsed.as_secs_f64()
    );

    // All sessions should be cleaned up
    assert_eq!(manager.session_count().unwrap(), 0);
}

/// Test concurrent token operations
#[test]
fn test_concurrent_token_operations() {
    let token_store = Arc::new(TokenStore::new(300));
    let mut handles = vec![];
    let threads = 10;
    let ops_per_thread = 10;

    let start = Instant::now();

    for thread_id in 0..threads {
        let store = Arc::clone(&token_store);
        let handle = thread::spawn(move || {
            for i in 0..ops_per_thread {
                // Generate token
                let token = store
                    .generate_and_store("192.168.1.1".to_string())
                    .expect(&format!("Thread {} op {} failed", thread_id, i));

                // Validate immediately
                assert!(store.validate_token(&token).is_ok());
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let elapsed = start.elapsed();
    let total_ops = threads * ops_per_thread;

    println!(
        "Performed {} token operations across {} threads in {:?} ({:.2} ops/sec)",
        total_ops,
        threads,
        elapsed,
        total_ops as f64 / elapsed.as_secs_f64()
    );

    // All tokens should be used
    let cleanup_count = token_store.cleanup_expired().expect("Cleanup failed");
    assert_eq!(cleanup_count, 0); // Used tokens not cleaned up by expiry
}

/// Test concurrent session operations with contention
#[test]
fn test_concurrent_session_contention() {
    let manager = Arc::new(SessionManager::new(30));
    let mut handles = vec![];
    let threads = 5;
    let ops_per_thread = 3;

    let start = Instant::now();

    for thread_id in 0..threads {
        let mgr = Arc::clone(&manager);
        let handle = thread::spawn(move || {
            let mut session_ids = Vec::new();

            // Create sessions
            for i in 0..ops_per_thread {
                let session_id = mgr
                    .create_session(None, None)
                    .expect(&format!("Thread {} failed to create session {}", thread_id, i));
                session_ids.push(session_id);
            }

            // List sessions (read operation)
            let sessions = mgr.list_sessions().expect("Failed to list sessions");
            assert!(sessions.len() >= ops_per_thread);

            // Delete sessions
            for session_id in session_ids {
                mgr.delete_session(&session_id)
                    .expect("Failed to delete session");
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let elapsed = start.elapsed();
    println!(
        "{} threads performed session operations in {:?}",
        threads, elapsed
    );

    // All sessions should be cleaned up
    assert_eq!(manager.session_count().unwrap(), 0);
}

/// Test session limit under concurrent load
#[test]
fn test_session_limit_under_load() {
    let limit = 10;
    let manager = Arc::new(SessionManager::new(limit));
    let mut handles = vec![];
    let threads = 20; // More threads than limit

    for thread_id in 0..threads {
        let mgr = Arc::clone(&manager);
        let handle = thread::spawn(move || {
            // Try to create a session
            match mgr.create_session(None, None) {
                Ok(session_id) => {
                    // Successfully created, hold briefly then delete
                    thread::sleep(Duration::from_millis(100));
                    mgr.delete_session(&session_id).ok();
                }
                Err(_) => {
                    // Hit limit, this is expected
                    println!("Thread {} hit session limit", thread_id);
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Should not exceed limit
    let final_count = manager.session_count().unwrap();
    assert!(
        final_count <= limit,
        "Session count {} exceeded limit {}",
        final_count,
        limit
    );
}

/// Test rapid resize operations
#[test]
fn test_rapid_resize_operations() {
    use portable_pty::PtySize;

    let manager = SessionManager::new(10);
    let session_id = manager
        .create_session(None, None)
        .expect("Failed to create session");

    let start = Instant::now();
    let resize_count = 50;

    for i in 0..resize_count {
        let size = PtySize {
            rows: (24 + i % 50) as u16,
            cols: (80 + i % 40) as u16,
            pixel_width: 0,
            pixel_height: 0,
        };

        manager
            .resize_pty(&session_id, size)
            .expect(&format!("Failed to resize {}", i));
    }

    let elapsed = start.elapsed();
    println!(
        "Performed {} resize operations in {:?} ({:.2} ops/sec)",
        resize_count,
        elapsed,
        resize_count as f64 / elapsed.as_secs_f64()
    );

    // Cleanup
    manager.delete_session(&session_id).unwrap();
}

/// Test token cleanup performance with many tokens
#[test]
fn test_token_cleanup_performance() {
    let token_store = TokenStore::new(1); // 1 second expiry

    // Generate many tokens
    for _ in 0..100 {
        token_store
            .generate_and_store("192.168.1.1".to_string())
            .expect("Failed to generate token");
    }

    // Wait for expiration
    thread::sleep(Duration::from_secs(2));

    // Measure cleanup time
    let start = Instant::now();
    let removed = token_store.cleanup_expired().expect("Cleanup failed");
    let elapsed = start.elapsed();

    println!(
        "Cleaned up {} tokens in {:?} ({:.2} tokens/sec)",
        removed,
        elapsed,
        removed as f64 / elapsed.as_secs_f64()
    );

    assert_eq!(removed, 100);
    assert!(elapsed < Duration::from_secs(1));
}

/// Test session activity updates under load
#[test]
fn test_session_activity_updates() {
    let manager = Arc::new(SessionManager::new(20));
    let session_id = manager
        .create_session(None, None)
        .expect("Failed to create session");

    let mut handles = vec![];
    let threads = 10;
    let touches_per_thread = 20;

    let start = Instant::now();

    for thread_id in 0..threads {
        let mgr = Arc::clone(&manager);
        let sid = session_id.clone();
        let handle = thread::spawn(move || {
            for i in 0..touches_per_thread {
                mgr.touch_session(&sid)
                    .expect(&format!("Thread {} touch {} failed", thread_id, i));
                thread::sleep(Duration::from_millis(5));
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let elapsed = start.elapsed();
    let total_touches = threads * touches_per_thread;

    println!(
        "Performed {} activity updates in {:?} ({:.2} ops/sec)",
        total_touches,
        elapsed,
        total_touches as f64 / elapsed.as_secs_f64()
    );

    // Session should still be active
    let sessions = manager.list_sessions().expect("Failed to list sessions");
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].last_activity_secs_ago < 2);

    // Cleanup
    manager.delete_session(&session_id).unwrap();
}

/// Test memory usage stability during session churn
#[test]
#[ignore] // Very slow test - runs for extended period
fn test_memory_stability() {
    let manager = SessionManager::new(20);
    let iterations = 100;

    for i in 0..iterations {
        // Create sessions
        let mut session_ids = Vec::new();
        for _ in 0..5 {
            let session_id = manager
                .create_session(None, None)
                .expect("Failed to create session");
            session_ids.push(session_id);
        }

        // Brief activity
        thread::sleep(Duration::from_millis(50));

        // Delete sessions
        for session_id in session_ids {
            manager
                .delete_session(&session_id)
                .expect("Failed to delete session");
        }

        if i % 10 == 0 {
            println!("Completed {} iterations", i);
        }
    }

    // All sessions should be cleaned up
    assert_eq!(manager.session_count().unwrap(), 0);
    println!("Memory stability test completed {} iterations", iterations);
}

/// Test token validation performance
#[test]
fn test_token_validation_performance() {
    let token_store = TokenStore::new(300);

    // Generate tokens
    let mut tokens = Vec::new();
    for _ in 0..50 {
        let token = token_store
            .generate_and_store("192.168.1.1".to_string())
            .expect("Failed to generate token");
        tokens.push(token);
    }

    // Measure validation time
    let start = Instant::now();
    for token in &tokens {
        assert!(token_store.is_valid(token).expect("Check failed"));
    }
    let elapsed = start.elapsed();

    println!(
        "Validated {} tokens in {:?} ({:.2} validations/sec)",
        tokens.len(),
        elapsed,
        tokens.len() as f64 / elapsed.as_secs_f64()
    );

    assert!(elapsed < Duration::from_millis(100));
}

/// Test session listing performance with many sessions
#[test]
#[ignore] // Slow test - creates many sessions
fn test_session_listing_performance() {
    let manager = SessionManager::new(30);

    // Create many sessions
    let mut session_ids = Vec::new();
    for i in 0..25 {
        let session_id = manager
            .create_session(None, None)
            .expect(&format!("Failed to create session {}", i));
        session_ids.push(session_id);
    }

    // Measure listing time
    let start = Instant::now();
    let iterations = 100;
    for _ in 0..iterations {
        let sessions = manager.list_sessions().expect("Failed to list sessions");
        assert_eq!(sessions.len(), 25);
    }
    let elapsed = start.elapsed();

    println!(
        "Listed {} sessions {} times in {:?} ({:.2} ops/sec)",
        session_ids.len(),
        iterations,
        elapsed,
        iterations as f64 / elapsed.as_secs_f64()
    );

    // Cleanup
    for session_id in session_ids {
        manager.delete_session(&session_id).unwrap();
    }
}
