# Security

This document outlines the security design and considerations for the web-based terminal application.

## Security Model

**Primary use case: Localhost access**

The application is designed primarily for localhost use, but can be securely exposed via reverse proxies (e.g., ngrok) with proper configuration. See [DEPLOYMENT.md](DEPLOYMENT.md) for public exposure guidelines.

### Threat Model

**In Scope:**
- Malicious JavaScript running in the same browser
- Accidental token disclosure
- CSRF attacks from other localhost services
- Resource exhaustion attacks
- Privilege escalation attempts

**Out of Scope:**
- Network-based attacks (application is localhost-only)
- Physical access to the machine
- OS-level vulnerabilities
- Browser vulnerabilities

## Authentication

### Token-Based Authentication

The application uses one-time authentication tokens instead of passwords:

1. **Token Generation**:
   - 32-byte (64-char hex) cryptographically random tokens
   - Generated using OS random number generator
   - Stored server-side with metadata

2. **Token Delivery**:
   - Sent via iMessage to registered phone number
   - Ensures only physical device owner can authenticate

3. **Token Properties**:
   - One-time use (atomically marked as used via CAS operation)
   - 5-minute expiration (configurable)
   - Cannot be reused after validation
   - Automatically cleaned up after expiration

4. **Token Validation**:
   - Format validation (64-char hex string)
   - Existence check
   - Expiration check
   - Atomic mark-as-used operation

### Session Management

After successful authentication:

1. **Session Cookie**:
   - Server-generated UUID v4 session ID
   - HTTP-only flag (not accessible to JavaScript)
   - SameSite=Strict (protects against CSRF)
   - Secure flag when using HTTPS
   - Configurable idle timeout

2. **CSRF Protection**:
   - Per-session CSRF token generated on login
   - Stored server-side (not in cookie)
   - Must be sent in X-CSRF-Token header for state-changing operations
   - Validated on all non-GET requests using constant-time comparison
   - Prevents cross-site request forgery attacks

3. **Server-side Sessions**:
   - Session data stored in server memory (HashMap)
   - Lost on server restart (intentional - no persistence)
   - Server instance ID allows clients to detect restarts

## Network Security

### TLS/HTTPS Support

The application supports HTTPS for secure connections:
- Auto-generates self-signed certificates for development
- Supports custom TLS certificates for production
- Required when using `allowed_origins` for public exposure
- WebSocket connections automatically upgrade to WSS
- Session cookies set with Secure flag when HTTPS enabled

### Localhost-Only Binding

The application binds exclusively to `127.0.0.1`:
- Cannot be accessed from other machines directly
- Not exposed on public network interfaces
- Public access requires reverse proxy (ngrok, Cloudflare Tunnel, etc.)

### Origin/Host Validation

Middleware validates Origin and Referer headers:
- Allows localhost origins (HTTP and HTTPS variants)
- Supports configured public origins via `allowed_origins` config
- Public origins must use HTTPS (validated at startup)
- Supports wildcard patterns for dynamic subdomains (e.g., `https://*.ngrok-free.app`)
- Wildcard matching is restricted to subdomain position only
- Prevents CSRF from unauthorized domains

**Wildcard Security**: Wildcards like `https://*.ngrok-free.app` are safe when:
- The domain requires authentication to create subdomains (like ngrok)
- You trust anyone who can create subdomains under that domain
- The wildcard only matches the subdomain part (no URL boundary crossing)

### Rate Limiting

Token generation and login endpoints are rate-limited:
- Default: 3 requests per 60 seconds (applied globally)
- Prevents token generation spam and brute force attacks
- Uses `tower-governor` with GlobalKeyExtractor
- Works reliably across all deployment scenarios (localhost, ngrok, etc.)

### Reverse Proxy Security

When exposing via reverse proxies (ngrok, Cloudflare, nginx):

**Requirements:**
- HTTPS must be enabled (`use_https = true`)
- Reverse proxy URL must be in `allowed_origins`

**Security Considerations:**
- Validate reverse proxy configuration before going public
- Monitor logs for unusual activity
- Use short token validity periods (< 5 minutes)
- Consider additional proxy-level authentication (ngrok basic auth, etc.)
- Rate limiting applies globally (all requests count toward the same limit)

**Configuration Validation:**
- Server validates HTTPS required when origins configured
- Public origins must use `https://` (no HTTP allowed)
- Startup logs warn when public exposure enabled

### Request Size Limits

- HTTP request bodies limited to 10KB
- WebSocket input messages limited to 64KB per message
- Prevents resource exhaustion

## Terminal Security

### Root Prevention

The application refuses to run as root:
- Checks effective UID on startup
- Exits immediately if EUID is 0
- Prevents privilege escalation

### PTY Isolation

Each terminal session runs in its own PTY:
- Separate process for each session
- Session limit enforced (default: 10)
- Proper cleanup on session deletion

### Session Limits

- Maximum concurrent sessions per server (configurable)
- Prevents resource exhaustion
- Enforced atomically during session creation

### Input Validation

All input is validated:
- Terminal dimensions (1-999 rows/cols)
- Token format (64-char hex)
- Session IDs (UUID format)
- WebSocket message sizes

## Audit Logging

### Session Logging

All terminal activity is logged (if enabled):
- Session creation/deletion events
- Input data (hex-encoded)
- Output data (text or hex)
- Resize events
- Timestamps for all events

Log files are stored per-session:
- Default location: `~/.web-terminal/session-logs/`
- File permissions: User-only read/write
- Useful for debugging and audit trails

### Structured Logging

Application uses structured logging via `tracing`:
- All authentication attempts logged
- Session lifecycle events logged
- Errors logged with context
- Debug info available with RUST_LOG env var

## Resource Management

### Process Cleanup

Multiple mechanisms ensure no orphaned processes:

1. **Manual Deletion**: Kills child process and waits for exit
2. **Zombie Reaping**: Background task runs every 30 seconds
3. **Idle Timeout**: Cleans up inactive sessions every 60 seconds
4. **Graceful Shutdown**: Kills all PTY processes on SIGTERM/SIGINT

Comprehensive tests verify no zombie processes or FD leaks.

### File Descriptor Management

PTY file descriptors are properly managed:
- Closed on session deletion
- Closed during zombie reaping
- Verified by integration tests
- No FD leaks confirmed

## Best Practices

### For Users

1. **Never expose the server to the network**:
   - Keep `bind_address` as `127.0.0.1`
   - Don't use `0.0.0.0` or public IPs
   - Don't forward ports to this service

2. **Protect your configuration**:
   - Keep `config.toml` readable only by you
   - Don't commit it to version control
   - Use strong file permissions

3. **Monitor session logs**:
   - Review logs periodically
   - Check for unusual activity
   - Delete old logs to save space

4. **Use reasonable timeouts**:
   - Keep `idle_timeout_secs` reasonable (default: 3600)
   - Keep `token_validity_secs` short (default: 300)
   - Don't disable timeouts

### For Developers

1. **Never bypass security checks**:
   - Don't remove origin validation
   - Don't disable CSRF protection
   - Don't increase rate limits without consideration

2. **Validate all input**:
   - Check sizes, ranges, formats
   - Return proper HTTP status codes
   - Log validation failures

3. **Handle errors securely**:
   - Don't leak sensitive info in error messages
   - Log errors with full context
   - Return generic errors to clients

4. **Test security features**:
   - Add tests for new security checks
   - Test error paths
   - Verify resource cleanup

## Vulnerability Disclosure

If you discover a security vulnerability:

1. **Do NOT open a public issue**
2. Contact the maintainer privately
3. Provide detailed reproduction steps
4. Allow reasonable time for a fix

## Security Updates

- Review dependencies regularly
- Update to latest stable Rust
- Monitor security advisories for dependencies
- Test thoroughly after updates

## Limitations

### Known Limitations

1. **Public Exposure Risks**:
   - Public exposure requires careful configuration (see DEPLOYMENT.md)
   - Rate limiting protects against abuse but not DDoS
   - Session hijacking possible if tokens leaked
   - Recommend short token validity for public use (< 5 minutes)

2. **No User Isolation**:
   - Single-user application
   - All sessions run as same user
   - No multi-user support

3. **Session Persistence**:
   - Sessions lost on server restart
   - No session migration
   - Clients must re-authenticate

4. **iMessage Dependency**:
   - Requires macOS and Messages app
   - Phone must be registered
   - No fallback authentication

### Out of Scope

The following are intentionally NOT supported:

- Multi-user authentication
- Network deployment
- TLS/HTTPS (not needed for localhost)
- Session persistence across restarts
- Password-based authentication
- 2FA beyond iMessage token delivery

## Compliance

This application:
- Does NOT collect user data
- Does NOT send data externally (except iMessage tokens)
- Does NOT persist credentials
- Does NOT require GDPR/CCPA compliance (personal use)

## References

- [OWASP Top 10](https://owasp.org/www-project-top-ten/)
- [Rust Security Guidelines](https://anssi-fr.github.io/rust-guide/)
- [Web Security Cheat Sheet](https://cheatsheetseries.owasp.org/)
