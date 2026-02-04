//! Session management module for terminal sessions.
//!
//! Manages the lifecycle of PTY-backed terminal sessions, including creation, tracking,
//! WebSocket communication, and cleanup. Handles multiple concurrent sessions with proper
//! isolation and resource management.

use chrono::Utc;
use portable_pty::{CommandBuilder, PtySize, PtySystem};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;
use thiserror::Error;
use uuid::Uuid;
use crate::session_logger::SessionLogger;

/// Errors that can occur during session operations
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("Session limit reached (max: {0})")]
    LimitReached(usize),

    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Failed to create PTY: {0}")]
    PtyCreationFailed(String),

    #[error("Failed to spawn shell: {0}")]
    ShellSpawnFailed(String),

    #[error("Session already exists: {0}")]
    AlreadyExists(String),

    #[error("Lock poisoned")]
    LockPoisoned,
}

/// Session metadata and state
pub struct Session {
    /// Unique session ID (format: YYYY-MM-DD-HH-MM-SS-randomsuffix)
    pub id: String,

    /// PTY pair (master side stored here, child has slave)
    pub pty_master: Box<dyn portable_pty::MasterPty + Send>,

    /// Child process handle
    pub child: Box<dyn portable_pty::Child + Send + Sync>,

    /// Session creation time
    pub created_at: SystemTime,

    /// Last activity time (updated on any I/O)
    pub last_activity: Arc<RwLock<SystemTime>>,

    /// PTY dimensions
    pub pty_size: PtySize,
}

/// Session manager for coordinating multiple terminal sessions
pub struct SessionManager {
    /// Active sessions: session_id -> Session
    sessions: Arc<RwLock<HashMap<String, Session>>>,

    /// Maximum number of concurrent sessions
    max_sessions: usize,

    /// PTY system instance (wrapped in Arc<Mutex> for interior mutability and Sync)
    pty_system: Arc<std::sync::Mutex<Box<dyn PtySystem + Send>>>,

    /// Optional session logger for audit trails
    session_logger: Option<SessionLogger>,
}

// Safety: SessionManager is Send because:
// - Arc<RwLock<HashMap<...>>> is Send + Sync
// - usize is Send + Sync + Copy
// - Arc<Mutex<Box<dyn PtySystem + Send>>> is Send + Sync
// All operations on Session objects (which contain trait objects) are protected by the RwLock
unsafe impl Send for SessionManager {}

// Safety: SessionManager is Sync because all access to interior state is protected by:
// - Arc<RwLock<...>> for sessions (provides synchronized access)
// - Arc<Mutex<...>> for pty_system (provides synchronized access)
// - usize is immutable after construction
unsafe impl Sync for SessionManager {}

impl SessionManager {
    /// Create a new SessionManager
    ///
    /// # Arguments
    /// * `max_sessions` - Maximum number of concurrent sessions allowed
    pub fn new(max_sessions: usize) -> Self {
        SessionManager {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            max_sessions,
            pty_system: Arc::new(std::sync::Mutex::new(portable_pty::native_pty_system())),
            session_logger: None,
        }
    }

    /// Create a SessionManager with logging enabled
    ///
    /// # Arguments
    /// * `max_sessions` - Maximum number of concurrent sessions allowed
    /// * `logger` - SessionLogger instance for audit trails
    pub fn with_logger(max_sessions: usize, logger: SessionLogger) -> Self {
        SessionManager {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            max_sessions,
            pty_system: Arc::new(std::sync::Mutex::new(portable_pty::native_pty_system())),
            session_logger: Some(logger),
        }
    }

    /// Generate a unique session ID with timestamp
    ///
    /// Format: YYYY-MM-DD-HH-MM-SS-<8-char-random-suffix>
    ///
    /// # Returns
    /// A unique session ID string
    fn generate_session_id() -> String {
        let timestamp = Utc::now().format("%Y-%m-%d-%H-%M-%S");
        let random_suffix = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>();
        format!("{}-{}", timestamp, random_suffix)
    }

    /// Create a new terminal session atomically
    ///
    /// This method enforces the session limit and creates the session while holding
    /// the write lock to prevent race conditions.
    ///
    /// # Arguments
    /// * `shell_command` - Optional shell command (defaults to $SHELL or /bin/bash)
    /// * `initial_size` - Initial PTY size (default: 80x24)
    ///
    /// # Returns
    /// Session ID on success
    ///
    /// # Errors
    /// Returns SessionError if:
    /// - Session limit is reached
    /// - PTY creation fails
    /// - Shell spawn fails
    pub fn create_session(
        &self,
        shell_command: Option<String>,
        initial_size: Option<PtySize>,
    ) -> Result<String, SessionError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        // Check session limit atomically
        if sessions.len() >= self.max_sessions {
            tracing::warn!(
                current = sessions.len(),
                max = self.max_sessions,
                "Session limit reached"
            );
            return Err(SessionError::LimitReached(self.max_sessions));
        }

        // Generate unique session ID
        let session_id = Self::generate_session_id();

        // Determine shell command
        let shell = shell_command.unwrap_or_else(|| {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
        });

        // Set PTY size
        let pty_size = initial_size.unwrap_or(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        });

        tracing::info!(
            session_id = %session_id,
            shell = %shell,
            rows = pty_size.rows,
            cols = pty_size.cols,
            "Creating new terminal session"
        );

        // Create PTY pair (lock pty_system mutex)
        let pty_pair = {
            let pty_system = self
                .pty_system
                .lock()
                .map_err(|_| SessionError::LockPoisoned)?;
            pty_system
                .openpty(pty_size)
                .map_err(|e| SessionError::PtyCreationFailed(e.to_string()))?
        };

        // Build shell command with environment variables
        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("LANG", "en_US.UTF-8");
        cmd.env("COLORTERM", "truecolor");

        // Spawn shell process
        let child = pty_pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SessionError::ShellSpawnFailed(e.to_string()))?;

        tracing::info!(
            session_id = %session_id,
            pid = ?child.process_id(),
            "Shell process spawned successfully"
        );

        // Create session
        let now = SystemTime::now();
        let session = Session {
            id: session_id.clone(),
            pty_master: pty_pair.master,
            child,
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            pty_size,
        };

        // Insert session while holding lock
        sessions.insert(session_id.clone(), session);

        // Log session creation if logger is enabled
        if let Some(ref logger) = self.session_logger {
            let _ = logger.log_session_created(
                session_id.clone(),
                shell.clone(),
                pty_size.rows,
                pty_size.cols,
            );
        }

        Ok(session_id)
    }

    /// Get a session by ID (returns a reference, not ownership)
    ///
    /// # Arguments
    /// * `session_id` - The session ID to retrieve
    ///
    /// # Returns
    /// Result containing success or SessionError
    ///
    /// # Errors
    /// Returns SessionError::NotFound if session doesn't exist
    pub fn get_session(&self, session_id: &str) -> Result<(), SessionError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SessionError::LockPoisoned)?;

        if sessions.contains_key(session_id) {
            Ok(())
        } else {
            Err(SessionError::NotFound(session_id.to_string()))
        }
    }

    /// List all active session IDs
    ///
    /// # Returns
    /// Vector of session IDs with metadata
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, SessionError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SessionError::LockPoisoned)?;

        let session_infos: Vec<SessionInfo> = sessions
            .iter()
            .map(|(id, session)| {
                let last_activity = session
                    .last_activity
                    .read()
                    .ok()
                    .and_then(|t| (*t).elapsed().ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                SessionInfo {
                    id: id.clone(),
                    created_at: session.created_at,
                    last_activity_secs_ago: last_activity,
                    pty_rows: session.pty_size.rows,
                    pty_cols: session.pty_size.cols,
                }
            })
            .collect();

        Ok(session_infos)
    }

    /// Delete a session and cleanup resources
    ///
    /// # Arguments
    /// * `session_id` - The session ID to delete
    ///
    /// # Returns
    /// Result indicating success or error
    ///
    /// # Errors
    /// Returns SessionError::NotFound if session doesn't exist
    pub fn delete_session(&self, session_id: &str) -> Result<(), SessionError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        match sessions.remove(session_id) {
            Some(mut session) => {
                tracing::info!(
                    session_id = %session_id,
                    "Terminating session and cleaning up resources"
                );

                // Kill the child process
                if let Err(e) = session.child.kill() {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "Failed to kill child process (may have already exited)"
                    );
                }

                // Wait for child to exit (with timeout handled by caller)
                if let Err(e) = session.child.wait() {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "Failed to wait for child process"
                    );
                }

                // Log session deletion if logger is enabled
                if let Some(ref logger) = self.session_logger {
                    let _ = logger.log_session_deleted(session_id.to_string());
                }

                Ok(())
            }
            None => Err(SessionError::NotFound(session_id.to_string())),
        }
    }

    /// Get the number of active sessions
    pub fn session_count(&self) -> Result<usize, SessionError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SessionError::LockPoisoned)?;
        Ok(sessions.len())
    }

    /// Update last activity timestamp for a session
    ///
    /// # Arguments
    /// * `session_id` - The session ID to update
    pub fn touch_session(&self, session_id: &str) -> Result<(), SessionError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SessionError::LockPoisoned)?;

        match sessions.get(session_id) {
            Some(session) => {
                let mut last_activity = session
                    .last_activity
                    .write()
                    .map_err(|_| SessionError::LockPoisoned)?;
                *last_activity = SystemTime::now();
                Ok(())
            }
            None => Err(SessionError::NotFound(session_id.to_string())),
        }
    }

    /// Get a PTY reader for the session
    ///
    /// This method clones a new reader from the PTY master. Multiple readers
    /// can coexist, allowing concurrent reading (though typically only one is needed).
    ///
    /// # Arguments
    /// * `session_id` - The session ID
    ///
    /// # Returns
    /// A boxed Read trait object that can read from the PTY
    ///
    /// # Errors
    /// Returns SessionError if session doesn't exist or reader creation fails
    pub fn get_pty_reader(
        &self,
        session_id: &str,
    ) -> Result<Box<dyn std::io::Read + Send>, SessionError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SessionError::LockPoisoned)?;

        match sessions.get(session_id) {
            Some(session) => {
                let reader = session
                    .pty_master
                    .try_clone_reader()
                    .map_err(|e| SessionError::PtyCreationFailed(e.to_string()))?;
                Ok(reader)
            }
            None => Err(SessionError::NotFound(session_id.to_string())),
        }
    }

    /// Get a PTY writer for the session
    ///
    /// This method takes the writer from the PTY master. This can only be called ONCE
    /// per session - the writer has exclusive ownership and cannot be cloned.
    ///
    /// # Arguments
    /// * `session_id` - The session ID
    ///
    /// # Returns
    /// A boxed Write trait object that can write to the PTY
    ///
    /// # Errors
    /// Returns SessionError if session doesn't exist or writer was already taken
    pub fn get_pty_writer(
        &self,
        session_id: &str,
    ) -> Result<Box<dyn std::io::Write + Send>, SessionError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        match sessions.get_mut(session_id) {
            Some(session) => {
                let writer = session
                    .pty_master
                    .take_writer()
                    .map_err(|e| SessionError::PtyCreationFailed(e.to_string()))?;
                Ok(writer)
            }
            None => Err(SessionError::NotFound(session_id.to_string())),
        }
    }

    /// Resize the PTY for a session
    ///
    /// # Arguments
    /// * `session_id` - The session ID
    /// * `new_size` - The new PTY size
    ///
    /// # Returns
    /// Result indicating success or error
    ///
    /// # Errors
    /// Returns SessionError if session doesn't exist or resize fails
    pub fn resize_pty(&self, session_id: &str, new_size: PtySize) -> Result<(), SessionError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        match sessions.get_mut(session_id) {
            Some(session) => {
                session
                    .pty_master
                    .resize(new_size)
                    .map_err(|e| SessionError::PtyCreationFailed(e.to_string()))?;
                session.pty_size = new_size;
                tracing::info!(
                    session_id = %session_id,
                    rows = new_size.rows,
                    cols = new_size.cols,
                    "Resized PTY"
                );

                // Log resize if logger is enabled
                if let Some(ref logger) = self.session_logger {
                    let _ = logger.log_resize(
                        session_id.to_string(),
                        new_size.rows,
                        new_size.cols,
                    );
                }

                Ok(())
            }
            None => Err(SessionError::NotFound(session_id.to_string())),
        }
    }

    /// Execute a function with mutable access to a session's PTY master
    ///
    /// This method is useful for operations that need direct access to the PTY master,
    /// such as taking the writer (which can only be done once).
    ///
    /// # Arguments
    /// * `session_id` - The session ID
    /// * `f` - A closure that receives mutable access to the PTY master
    ///
    /// # Returns
    /// The result of the closure
    ///
    /// # Errors
    /// Returns SessionError if session doesn't exist or lock is poisoned
    pub fn with_pty_master<F, R>(&self, session_id: &str, f: F) -> Result<R, SessionError>
    where
        F: FnOnce(&mut Box<dyn portable_pty::MasterPty + Send>) -> R,
    {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        match sessions.get_mut(session_id) {
            Some(session) => Ok(f(&mut session.pty_master)),
            None => Err(SessionError::NotFound(session_id.to_string())),
        }
    }

    /// Clean up sessions whose child processes have exited (zombie reaping)
    ///
    /// This method checks all active sessions for dead child processes and removes
    /// them from the session map. Should be called periodically to prevent zombie
    /// processes and session leaks.
    ///
    /// # Returns
    /// A vector of session IDs that were cleaned up
    ///
    /// # Errors
    /// Returns SessionError if lock is poisoned
    pub fn reap_dead_sessions(&self) -> Result<Vec<String>, SessionError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        let mut dead_sessions = Vec::new();

        // Check each session's child process
        sessions.retain(|session_id, session| {
            // try_wait() returns Some(ExitStatus) if child has exited, None if still running
            match session.child.try_wait() {
                Ok(Some(exit_status)) => {
                    tracing::info!(
                        session_id = %session_id,
                        exit_status = ?exit_status,
                        "Child process exited, cleaning up session"
                    );
                    dead_sessions.push(session_id.clone());
                    false // Remove this session
                }
                Ok(None) => {
                    // Child is still running, keep the session
                    true
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "Failed to check child process status, keeping session"
                    );
                    // Keep the session if we can't check status
                    true
                }
            }
        });

        if !dead_sessions.is_empty() {
            tracing::info!(
                count = dead_sessions.len(),
                session_ids = ?dead_sessions,
                "Reaped dead sessions"
            );
        }

        Ok(dead_sessions)
    }

    /// Clean up sessions that have been idle for longer than the timeout
    ///
    /// This method checks all active sessions and removes those that haven't had
    /// any activity within the idle timeout period. Should be called periodically
    /// to free resources from abandoned sessions.
    ///
    /// # Arguments
    /// * `idle_timeout_secs` - Number of seconds of inactivity before cleanup
    ///
    /// # Returns
    /// A vector of session IDs that were cleaned up
    ///
    /// # Errors
    /// Returns SessionError if lock is poisoned
    pub fn cleanup_idle_sessions(&self, idle_timeout_secs: u64) -> Result<Vec<String>, SessionError> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|_| SessionError::LockPoisoned)?;

        let now = SystemTime::now();
        let mut idle_sessions = Vec::new();

        // Check each session's last activity time
        sessions.retain(|session_id, session| {
            let last_activity = session
                .last_activity
                .read()
                .ok()
                .map(|t| *t);

            match last_activity {
                Some(last_activity_time) => {
                    match now.duration_since(last_activity_time) {
                        Ok(idle_duration) => {
                            if idle_duration.as_secs() >= idle_timeout_secs {
                                tracing::info!(
                                    session_id = %session_id,
                                    idle_secs = idle_duration.as_secs(),
                                    timeout_secs = idle_timeout_secs,
                                    "Session idle timeout, cleaning up"
                                );

                                // Kill the child process before removing
                                if let Err(e) = session.child.kill() {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        error = ?e,
                                        "Failed to kill child process during idle cleanup"
                                    );
                                }

                                idle_sessions.push(session_id.clone());
                                false // Remove this session
                            } else {
                                true // Keep the session
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                session_id = %session_id,
                                error = ?e,
                                "Failed to calculate idle duration, keeping session"
                            );
                            true // Keep the session if we can't calculate duration
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        session_id = %session_id,
                        "Failed to read last activity, keeping session"
                    );
                    true // Keep the session if we can't read last activity
                }
            }
        });

        if !idle_sessions.is_empty() {
            tracing::info!(
                count = idle_sessions.len(),
                session_ids = ?idle_sessions,
                timeout_secs = idle_timeout_secs,
                "Cleaned up idle sessions"
            );
        }

        Ok(idle_sessions)
    }
}

/// Session information for listing
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub created_at: SystemTime,
    pub last_activity_secs_ago: u64,
    pub pty_rows: u16,
    pub pty_cols: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_generation() {
        let id1 = SessionManager::generate_session_id();
        let id2 = SessionManager::generate_session_id();

        // Should be unique
        assert_ne!(id1, id2);

        // Should contain timestamp and random suffix
        assert!(id1.contains("-"));
        assert_eq!(id1.split('-').count(), 7); // YYYY-MM-DD-HH-MM-SS-random (7 parts)
    }

    #[test]
    fn test_session_manager_creation() {
        let manager = SessionManager::new(4);
        let count = manager.session_count().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_session_limit_enforcement() {
        let manager = SessionManager::new(2);

        // Create first session
        let session1 = manager.create_session(None, None);
        assert!(session1.is_ok());

        // Create second session
        let session2 = manager.create_session(None, None);
        assert!(session2.is_ok());

        // Third session should fail
        let session3 = manager.create_session(None, None);
        assert!(session3.is_err());
        assert!(matches!(session3.unwrap_err(), SessionError::LimitReached(_)));
    }

    #[test]
    fn test_session_lifecycle() {
        let manager = SessionManager::new(4);

        // Create session
        let session_id = manager.create_session(None, None).unwrap();

        // Verify it exists
        assert!(manager.get_session(&session_id).is_ok());

        // List sessions
        let sessions = manager.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session_id);

        // Delete session
        assert!(manager.delete_session(&session_id).is_ok());

        // Verify it's gone
        assert!(manager.get_session(&session_id).is_err());
        assert_eq!(manager.session_count().unwrap(), 0);
    }

    #[test]
    fn test_touch_session() {
        let manager = SessionManager::new(4);
        let session_id = manager.create_session(None, None).unwrap();

        // Touch session (update last activity)
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(manager.touch_session(&session_id).is_ok());

        // Clean up
        manager.delete_session(&session_id).unwrap();
    }

    #[test]
    fn test_nonexistent_session_operations() {
        let manager = SessionManager::new(4);

        // Operations on nonexistent session should fail
        assert!(manager.get_session("nonexistent").is_err());
        assert!(manager.touch_session("nonexistent").is_err());
        assert!(manager.delete_session("nonexistent").is_err());
    }

    #[test]
    fn test_reap_dead_sessions() {
        let manager = SessionManager::new(4);

        // Create a session with a command that exits immediately
        let session_id = manager
            .create_session(Some("true".to_string()), None)
            .unwrap();

        // Verify session exists
        assert!(manager.get_session(&session_id).is_ok());
        assert_eq!(manager.session_count().unwrap(), 1);

        // Wait for child process to exit
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Reap dead sessions
        let reaped = manager.reap_dead_sessions().unwrap();

        // Verify the session was reaped
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0], session_id);

        // Verify session is gone
        assert_eq!(manager.session_count().unwrap(), 0);
    }

    #[test]
    fn test_reap_no_dead_sessions() {
        let manager = SessionManager::new(4);

        // Create a long-running session
        let session_id = manager.create_session(None, None).unwrap();

        // Immediately reap (should find nothing)
        let reaped = manager.reap_dead_sessions().unwrap();
        assert_eq!(reaped.len(), 0);

        // Session should still exist
        assert!(manager.get_session(&session_id).is_ok());
        assert_eq!(manager.session_count().unwrap(), 1);

        // Clean up
        manager.delete_session(&session_id).unwrap();
    }

    #[test]
    fn test_cleanup_idle_sessions() {
        let manager = SessionManager::new(4);

        // Create a session
        let session_id = manager.create_session(None, None).unwrap();

        // Verify session exists
        assert!(manager.get_session(&session_id).is_ok());

        // Wait a bit to ensure some idle time
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check for idle sessions with 0 second timeout
        // This should clean up the session since it's been idle for >0 seconds
        let cleaned = manager.cleanup_idle_sessions(0).unwrap();
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0], session_id);

        // Verify session is gone
        assert_eq!(manager.session_count().unwrap(), 0);
    }

    #[test]
    fn test_cleanup_no_idle_sessions() {
        let manager = SessionManager::new(4);

        // Create a session
        let session_id = manager.create_session(None, None).unwrap();

        // Immediately check for idle sessions with a long timeout
        let cleaned = manager.cleanup_idle_sessions(3600).unwrap();
        assert_eq!(cleaned.len(), 0);

        // Session should still exist
        assert!(manager.get_session(&session_id).is_ok());
        assert_eq!(manager.session_count().unwrap(), 1);

        // Clean up
        manager.delete_session(&session_id).unwrap();
    }

    #[test]
    fn test_touch_prevents_idle_cleanup() {
        let manager = SessionManager::new(4);

        // Create a session
        let session_id = manager.create_session(None, None).unwrap();

        // Wait to make it idle
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Touch the session (update last activity to now)
        manager.touch_session(&session_id).unwrap();

        // Check for idle sessions with 1 second timeout
        // Session should not be cleaned up because we just touched it (idle time < 1 second)
        let cleaned = manager.cleanup_idle_sessions(1).unwrap();
        assert_eq!(cleaned.len(), 0);

        // Session should still exist
        assert!(manager.get_session(&session_id).is_ok());

        // Now wait again and verify it does get cleaned up
        std::thread::sleep(std::time::Duration::from_secs(2));
        let cleaned = manager.cleanup_idle_sessions(1).unwrap();
        assert_eq!(cleaned.len(), 1);

        // Session should be gone
        assert_eq!(manager.session_count().unwrap(), 0);
    }
}
