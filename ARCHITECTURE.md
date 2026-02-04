# Architecture

This document describes the technical architecture and design decisions of the web-based terminal application.

## System Overview

```
┌─────────────────────────────────────────────────┐
│              Browser (Client)                   │
│  ┌──────────────┐  ┌────────────────────────┐   │
│  │  React App   │  │   xterm.js Terminal    │   │
│  │  (TypeScript)│  │   (Terminal Emulator)  │   │
│  └──────┬───────┘  └────────┬───────────────┘   │
│         │ HTTP/WS            │ WebSocket        │
└─────────┼────────────────────┼──────────────────┘
          │                    │
          │                    │
┌─────────┼────────────────────┼──────────────────┐
│         ▼                    ▼                  │
│  ┌──────────────┐  ┌─────────────────────────┐  │
│  │ Axum Router  │  │  WebSocket Handler      │  │
│  │  (HTTP API)  │  │  (Terminal Streaming)   │  │
│  └──────┬───────┘  └─────────┬───────────────┘  │
│         │                     │                 │
│  ┌──────▼──────────────────...▼───────────────┐ │
│  │         Application State                  │ │
│  │  ┌──────────┐  ┌─────────────┐  ┌────────┐ │ │
│  │  │TokenStore│  │SessionMgr   │  │ Config │ │ │
│  │  └──────────┘  └─────────────┘  └────────┘ │ │
│  └──────────────────────────────────────────..┘ │
│                    │                            │
│              ┌─────▼──────┐                     │
│              │ PTY System │                     │
│              │  (portable-pty)                  │
│              └─────┬──────┘                     │
│                    │                            │
│              ┌─────▼──────┐                     │
│              │ Shell      │                     │
│              │  Processes │                     │
│              └────────────┘                     │
└─────────────────────────────────────────────────┘
           Rust Backend (Axum + Tokio)
```

## Components

### Backend Architecture

#### 1. Web Server (Axum)

**Routes:**

```rust
/ (GET)                         → Static files (SPA)
/api/health (GET)               → Health check + server instance ID
/api/token/generate (POST)      → Generate auth token [Rate Limited]
/api/auth/login (POST)          → Validate token, create session
/api/sessions (GET)             → List sessions
/api/sessions (POST)            → Create session
/api/sessions/:id (DELETE)      → Delete session
/api/sessions/:id/ws (GET)      → WebSocket upgrade for terminal
```

**Middleware Stack:**

1. **TraceLayer**: HTTP request logging
2. **CookieManagerLayer**: Cookie parsing
3. **validate_origin**: Origin/Host validation (CSRF protection)
4. **RequestBodyLimitLayer**: 10KB body size limit
5. **GovernorLayer**: Rate limiting (token generation only)

#### 2. Token Store

**Purpose**: Manage one-time authentication tokens

**Data Structure:**

```rust
struct TokenStore {
    tokens: Arc<RwLock<HashMap<String, TokenMetadata>>>,
    validity_duration: Duration,
}

struct TokenMetadata {
    expires_at: SystemTime,
    used: AtomicBool,  // For CAS-based one-time use
}
```

**Operations:**

- `generate_and_store()`: Generate 64-char hex token
- `validate_token()`: Atomic check + mark-as-used (CAS)
- `cleanup_expired()`: Remove expired tokens
- `is_valid()`, `exists()`: Query methods

**Concurrency:**

- `RwLock` for token map access
- `AtomicBool` with CAS for one-time use enforcement
- Lock poisoning handled gracefully

**Background Tasks:**

- Cleanup task runs hourly
- Removes expired tokens from map

#### 3. Session Manager

**Purpose**: Manage PTY-backed terminal sessions

**Data Structure:**

```rust
struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    max_sessions: usize,
    pty_system: Arc<Mutex<Box<dyn PtySystem>>>,
    session_logger: Option<SessionLogger>,
}

struct Session {
    id: String,
    master: Box<dyn MasterPty>,
    child: Box<dyn Child>,
    created_at: SystemTime,
    last_activity: AtomicCell<SystemTime>,
    pty_size: Mutex<PtySize>,
}
```

**Operations:**

- `create_session()`: Spawn PTY + shell, enforce limit
- `delete_session()`: Kill child, wait for exit
- `get_pty_reader()`: Get reader for PTY output
- `resize_pty()`: Change terminal dimensions
- `touch_session()`: Update activity timestamp
- `cleanup_idle_sessions()`: Remove inactive sessions
- `reap_dead_sessions()`: Clean up exited processes

**Concurrency:**

- `RwLock` for session map
- Atomic session limit check under write lock
- `AtomicCell` for last_activity updates
- `Mutex` for pty_size (rarely accessed)

**Background Tasks:**

1. Dead session reaping (30s interval)
2. Idle timeout cleanup (60s interval)

#### 4. WebSocket Handler

**Message Protocol:**

```rust
enum TerminalMessage {
    Output { data: Vec<u8> },      // PTY → Client
    Input { data: Vec<u8> },       // Client → PTY
    Resize { rows: u16, cols: u16 }, // Terminal resize
    Ping, Pong,                    // Keepalive
    Error { message: String },     // Error notification
    Attached { session_id: String }, // Connection confirmation
}
```

**Streaming Architecture:**

```
  PTY Reader Task          Main Event Loop          PTY Writer Task
(spawn_blocking)          (tokio::select!)          (tokio::spawn)
       │                          │                        │
       ├─ read(buf) ─────────────▶│                        │
       │  [8KB chunks]            │                        │
       ├─ send(pty_tx)───────────▶│                        │
       │                          │                        │
       │                          ├─ recv(pty_rx)───────▶  │
       │                          │  Output → WS           │
       │                          │                        │
       │                          │◀─ WS.recv()            │
       │                          │  Input received        │
       │                          ├─ send(input_tx)───────▶│
       │                          │                        │
       │                          │                        │◀─ write_all()
       │                          │                        │  [spawn_blocking]
```

**Backpressure Handling:**

- PTY output: 32-buffer tokio channel
- WebSocket input: 32-buffer tokio channel
- If WebSocket is slow, PTY read blocks (natural backpressure)
- If PTY write is slow, WebSocket receive blocks

**Error Handling:**

- PTY read/write errors: Log and break loop
- WebSocket errors: Close connection
- Session not found: Return 404
- Cleanup tasks aborted on disconnect

#### 5. Session Logger

**Purpose**: Audit logging for terminal I/O

**Architecture:**

- Non-blocking tokio channel (unbounded)
- Background task processes log events
- Per-session log files
- Graceful degradation if logging fails

**Log Events:**

```rust
enum LogEvent {
    SessionCreated { session_id, shell_command, rows, cols },
    Input { session_id, data },
    Output { session_id, data },
    Resize { session_id, rows, cols },
    SessionDeleted { session_id },
}
```

**Format:**

```
[2026-02-03T14:30:00.123Z] SessionCreated: shell=/bin/bash rows=24 cols=80
[2026-02-03T14:30:01.456Z] Input: 6563686f2074657374 (echo test)
[2026-02-03T14:30:01.567Z] Output: test
[2026-02-03T14:30:10.890Z] SessionDeleted
```

#### 6. Notification System

**Purpose**: Send authentication tokens via iMessage

**Implementation:**

- Uses AppleScript via `osascript` command
- Async execution with 10s timeout
- Input sanitization (alphanumeric only)
- Degraded mode: Logs error but doesn't crash

**AppleScript Command:**

```applescript
tell application "Messages"
    set targetService to 1st service whose service type = iMessage
    set targetBuddy to buddy "+1234567890" of targetService
    send "Your authentication token: ABC123..." to targetBuddy
end tell
```

### Frontend Architecture

#### 1. React Application

**Structure:**

```
App.tsx                      // Root, auth state, routing
├── LoginPage               // Two-step auth flow
│   ├── Request Token       // Step 1: Generate token
│   └── Enter Token         // Step 2: Validate token
└── TerminalPage            // Main application
    ├── Sidebar             // Session list + controls
    │   ├── Session List    // Active sessions
    │   ├── Create Button   // New session
    │   └── Delete Button   // Remove session
    └── Terminal Component  // xterm.js integration
        ├── WebSocket       // PTY streaming
        ├── FitAddon        // Auto-resize
        └── WebLinksAddon   // Clickable URLs
```

#### 2. API Client

**Features:**

- TypeScript typed interfaces
- CSRF token management (stored from login)
- Automatic credential inclusion (cookies)
- WebSocket URL helper

**Endpoints:**

```typescript
class ApiClient {
    requestToken(): Promise<TokenResponse>
    login(token: string): Promise<LoginResponse>
    listSessions(): Promise<ListSessionsResponse>
    createSession(params): Promise<CreateSessionResponse>
    deleteSession(id: string): Promise<void>
    getWebSocketUrl(sessionId: string): string
}
```

#### 3. Terminal Component

**xterm.js Integration:**

```typescript
useEffect(() => {
    const xterm = new XTerm({ cursorBlink: true });
    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();

    xterm.loadAddon(fitAddon);
    xterm.loadAddon(webLinksAddon);
    xterm.open(containerRef.current);

    // WebSocket connection
    const ws = new WebSocket(wsUrl);

    // Output: PTY → xterm
    ws.onmessage = (event) => {
        const msg: TerminalMessage = JSON.parse(event.data);
        if (msg.type === 'output') {
            xterm.write(new Uint8Array(msg.data));
        }
    };

    // Input: xterm → PTY
    xterm.onData((data) => {
        ws.send(JSON.stringify({
            type: 'input',
            data: Array.from(new TextEncoder().encode(data))
        }));
    });

    // Resize handling
    const resizeObserver = new ResizeObserver(() => {
        fitAddon.fit();
        ws.send(JSON.stringify({
            type: 'resize',
            rows: xterm.rows,
            cols: xterm.cols
        }));
    });

    return () => {
        ws.close();
        xterm.dispose();
    };
}, [sessionId]);
```

## Design Decisions

### 1. In-Memory Session Storage

**Decision**: Store sessions in memory only (no persistence)

**Rationale:**
- Simpler implementation
- Faster access
- No disk I/O
- Security: Sessions don't survive server restart
- Matches ephemeral nature of local dev tool

**Tradeoffs:**
- Sessions lost on restart
- No session migration
- Clients must re-authenticate

**Mitigation:**
- Server instance ID allows detection
- Graceful shutdown cleans up properly
- Clear error messages explain situation

### 2. Localhost-Only Binding

**Decision**: Bind only to 127.0.0.1, never 0.0.0.0

**Rationale:**
- Security: No network exposure
- Simplicity: No need for TLS
- Performance: Lower latency
- Trust model: Local-only

**Tradeoffs:**
- Cannot access from other machines
- Cannot use on remote servers

**Mitigation:**
- Clear documentation
- Configuration validation
- Startup checks

### 3. iMessage Authentication

**Decision**: Use iMessage for token delivery

**Rationale:**
- Physical device ownership proof
- No stored passwords
- Familiar UX for macOS users
- Leverages existing infrastructure

**Tradeoffs:**
- macOS-only
- Requires Messages app
- Phone must be registered
- No offline use

**Alternatives Considered:**
- Email: Less secure, easier to intercept
- TOTP: Requires app installation, more complex
- Password: Must store securely, less secure

### 4. One-Time Tokens

**Decision**: Tokens are single-use, expire in 5 minutes

**Rationale:**
- Prevents replay attacks
- Limits exposure window
- Forces fresh authentication
- Simple to implement

**Implementation:**
- `AtomicBool` with CAS operation
- Ensures exactly-once use
- Race-condition safe

### 5. Channel-Based WebSocket Architecture

**Decision**: Use tokio channels for PTY ↔ WebSocket streaming

**Rationale:**
- Natural backpressure handling
- Decouples PTY I/O from WebSocket I/O
- Allows blocking PTY operations
- Clean task separation

**Architecture:**
- PTY reader: `spawn_blocking` (blocking I/O)
- PTY writer: `spawn` with `spawn_blocking` per write
- Main loop: `tokio::select!` for multiplexing

**Tradeoffs:**
- Slightly more complex
- Small memory overhead (buffers)

**Benefits:**
- Proper backpressure
- Clean error handling
- Testable components

### 6. Comprehensive Testing

**Decision**: Integration tests for resource management

**Rationale:**
- Critical for preventing leaks
- Hard to test with unit tests
- Real system integration needed

**Tests:**
- PTY cleanup (zombie processes)
- File descriptor leaks
- Session lifecycle
- Idle timeout
- Concurrent operations

## Performance Considerations

### Scalability

**Limits:**
- Max sessions: 10 (configurable)
- Rate limit: 3 token requests/minute
- Request body: 10KB max
- WebSocket input: 64KB/message max

**Why Limited:**
- Single-user localhost application
- Resource protection
- Abuse prevention

### Memory Usage

**Typical:**
- ~1MB base memory
- ~500KB per terminal session
- ~10MB total with 10 sessions

**Monitoring:**
- Check with `ps` or `htop`
- No built-in metrics (unnecessary for localhost)

### CPU Usage

**Typical:**
- <1% idle
- 2-5% during active terminal use
- Spikes during PTY I/O

## Future Considerations

### Potential Enhancements

1. **Additional Auth Methods**:
   - OAuth for other platforms
   - TOTP 2FA option
   - Biometric authentication

2. **Extended Platform Support**:
   - Linux (already mostly works)
   - Windows (needs PTY work)
   - Other notification methods

3. **Session Features**:
   - Session persistence (optional)
   - Session sharing (multiple clients)
   - Session recording playback

4. **Terminal Features**:
   - File upload/download
   - Clipboard integration
   - Tmux/screen integration
   - Custom color schemes

### Non-Goals

These are intentionally NOT planned:

- Multi-user support
- Network deployment
- Commercial use
- Mobile clients
- Session persistence by default
- Complex authentication schemes

## References

- [Axum Documentation](https://docs.rs/axum)
- [portable-pty Documentation](https://docs.rs/portable-pty)
- [xterm.js Documentation](https://xtermjs.org/)
- [WebSocket Protocol RFC 6455](https://tools.ietf.org/html/rfc6455)
