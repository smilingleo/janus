// Terminal page with session management

import { useState, useEffect } from 'react';
import { Terminal } from './Terminal';
import { apiClient } from '../api';
import type { SessionInfo } from '../api';
import './TerminalPage.css';

export function TerminalPage() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load sessions on mount
  useEffect(() => {
    loadSessions();
  }, []);

  const loadSessions = async () => {
    try {
      const response = await apiClient.listSessions();
      if (response.success) {
        setSessions(response.sessions);
        // Auto-select first session if none selected
        if (!activeSessionId && response.sessions.length > 0) {
          setActiveSessionId(response.sessions[0].id);
        }
      }
    } catch (err) {
      console.error('Failed to load sessions:', err);
      setError(err instanceof Error ? err.message : 'Failed to load sessions');
    }
  };

  const handleCreateSession = async () => {
    setLoading(true);
    setError(null);

    try {
      const response = await apiClient.createSession({
        rows: 24,
        cols: 80,
      });

      if (response.success && response.session_id) {
        // Reload sessions to get the new one
        await loadSessions();
        // Set new session as active
        setActiveSessionId(response.session_id);
      } else {
        setError(response.message || 'Failed to create session');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create session');
    } finally {
      setLoading(false);
    }
  };

  const handleDeleteSession = async (sessionId: string) => {
    if (!confirm('Are you sure you want to close this session?')) {
      return;
    }

    try {
      await apiClient.deleteSession(sessionId);

      // Update sessions list
      const newSessions = sessions.filter((s) => s.id !== sessionId);
      setSessions(newSessions);

      // If deleted session was active, switch to first available
      if (activeSessionId === sessionId) {
        setActiveSessionId(newSessions.length > 0 ? newSessions[0].id : null);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete session');
    }
  };

  const handleSessionEnded = async (sessionId: string) => {
    // Automatic cleanup when shell exits - no confirmation needed
    try {
      await apiClient.deleteSession(sessionId);

      // Update sessions list
      const newSessions = sessions.filter((s) => s.id !== sessionId);
      setSessions(newSessions);

      // If ended session was active, switch to first available
      if (activeSessionId === sessionId) {
        setActiveSessionId(newSessions.length > 0 ? newSessions[0].id : null);
      }
    } catch (err) {
      // Ignore errors for automatic cleanup - session is already dead
      console.log('Session cleanup error (ignored):', err);
    }
  };

  const formatLastActivity = (secsAgo: number): string => {
    if (secsAgo < 60) return 'just now';
    if (secsAgo < 3600) return `${Math.floor(secsAgo / 60)}m ago`;
    if (secsAgo < 86400) return `${Math.floor(secsAgo / 3600)}h ago`;
    return `${Math.floor(secsAgo / 86400)}d ago`;
  };

  return (
    <div className="terminal-page">
      <div className="sidebar">
        <div className="sidebar-header">
          <h2>Sessions</h2>
          <button
            onClick={handleCreateSession}
            disabled={loading}
            className="new-session-button"
            title="Create new session"
          >
            +
          </button>
        </div>

        {error && (
          <div className="sidebar-error">
            {error}
            <button onClick={() => setError(null)} className="dismiss-error">
              ×
            </button>
          </div>
        )}

        <div className="session-list">
          {sessions.length === 0 ? (
            <div className="empty-state">
              <p>No sessions</p>
              <button onClick={handleCreateSession} className="create-first-button">
                Create Session
              </button>
            </div>
          ) : (
            sessions.map((session) => (
              <div
                key={session.id}
                className={`session-item ${activeSessionId === session.id ? 'active' : ''}`}
                onClick={() => setActiveSessionId(session.id)}
              >
                <div className="session-item-header">
                  <span className="session-name">
                    {session.id.split('-').slice(-1)[0]}
                  </span>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      handleDeleteSession(session.id);
                    }}
                    className="session-delete"
                    title="Close session"
                  >
                    ×
                  </button>
                </div>
                <div className="session-meta">
                  <span>{session.pty_cols}×{session.pty_rows}</span>
                  <span>{formatLastActivity(session.last_activity_secs_ago)}</span>
                </div>
              </div>
            ))
          )}
        </div>
      </div>

      <div className="terminal-area">
        {activeSessionId ? (
          <Terminal
            key={activeSessionId}
            sessionId={activeSessionId}
            onClose={() => handleDeleteSession(activeSessionId)}
            onSessionEnded={() => handleSessionEnded(activeSessionId)}
            onError={(err) => setError(err.message)}
          />
        ) : (
          <div className="no-session">
            <p>No active session</p>
            <button onClick={handleCreateSession} className="create-session-prompt">
              Create New Session
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
