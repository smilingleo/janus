# Security Enhancements

This document describes the IP address validation and session fingerprinting features implemented to protect against session hijacking attacks.

## Overview

Two critical security mitigations have been added:

1. **IP Address Validation** - Binds tokens and sessions to the originating IP address
2. **Session Fingerprinting** - Validates browser characteristics remain consistent

## IP Address Validation

### How It Works

When a client requests an authentication token, their IP address is extracted and stored with the token. All subsequent operations with that token must come from the same IP address.

### Flow

1. **Token Generation** (`/api/token/generate`)
   - Extracts client IP from `X-Forwarded-For` or `X-Real-IP` headers
   - Stores IP with token in `TokenMetadata`
   - IP is bound to the token for its lifetime

2. **Login** (`/api/login`)
   - Extracts current client IP
   - Validates IP matches the token's stored IP
   - If mismatch: returns `403 Forbidden`
   - If match: creates session with stored IP

3. **Authenticated Requests** (all endpoints with CSRF middleware)
   - Extracts current client IP
   - Validates IP matches session's stored IP
   - If mismatch: returns `403 Forbidden`

### Implementation Details

- Uses `X-Forwarded-For` header (set by ngrok and reverse proxies)
- Falls back to `X-Real-IP` if `X-Forwarded-For` unavailable
- Takes first IP from comma-separated list (original client)
- Validates IP format to prevent injection attacks
- Supports both IPv4 and IPv6 addresses

### Local Testing Fallback

When IP address cannot be determined (e.g., local testing without proxy headers):
- Uses sentinel value `"local"` instead of failing
- IP validation is **skipped** when both stored and current IPs are `"local"`
- Browser fingerprint validation remains active
- Provides graceful degradation for development environments

**Security Notes:**
- If session was created with real IP, requests with sentinel value are rejected (mode mismatch)
- If session was created with sentinel value, requests with real IP are rejected (mode mismatch)
- This prevents attacks where attacker bypasses IP check by removing headers

### Security Guarantees

- **Token theft protection**: Stolen token cannot be used from different IP (or different validation mode)
- **Session hijacking protection**: Stolen session cookie cannot be used from different IP
- **Proxy-aware**: Works correctly behind ngrok, nginx, CloudFlare, etc.
- **Local testing friendly**: Gracefully falls back to fingerprint-only validation

### Limitations

- **Dynamic IPs**: Users with frequently changing IPs may experience logout
- **Mobile networks**: Cellular networks may rotate IPs during session
- **Shared IPs**: Multiple users behind same NAT share IP (not a security issue)
- **Local mode**: When using sentinel value, only browser fingerprint provides protection

### Configuration

No configuration required. IP validation is always enabled with automatic fallback.

## Session Fingerprinting

### How It Works

When a session is created, browser characteristics are captured and stored. All subsequent requests must present the same fingerprint.

### Fingerprint Components

The fingerprint includes:
- `User-Agent` - Browser and OS identification
- `Accept` - Content type preferences
- `Accept-Language` - Language preferences
- `Accept-Encoding` - Compression preferences

### Flow

1. **Login** (`/api/login`)
   - Extracts browser fingerprint from headers
   - Validates fingerprint has required fields (User-Agent)
   - Stores fingerprint with session in `SessionData`

2. **Authenticated Requests** (all endpoints with CSRF middleware)
   - Extracts current browser fingerprint
   - Compares with session's stored fingerprint
   - If mismatch: returns `403 Forbidden`

### Implementation Details

- Exact string matching (no fuzzy logic)
- All components must match exactly
- Empty strings allowed for optional headers
- User-Agent is required (validation fails if missing)

### Security Guarantees

- **Cross-browser hijacking protection**: Attacker with different browser cannot use stolen cookie
- **Automated attack detection**: Scripts without proper headers are rejected
- **Defense in depth**: Complements IP validation

### Limitations

- **Browser updates**: Major browser updates may change User-Agent
- **Browser extensions**: Some extensions modify headers
- **Privacy tools**: Privacy-focused tools may randomize headers
- **False positives**: Legitimate users may be locked out if headers change

### Configuration

No configuration required. Fingerprinting is always enabled.

## Logging and Monitoring

### Security Events Logged

All security events are logged with structured logging:

```
INFO  Token generation request from IP: 203.0.113.1
WARN  Security: IP address mismatch during login (token_ip=203.0.113.1, request_ip=198.51.100.1)
WARN  Security: IP address mismatch detected (expected_ip=203.0.113.1, actual_ip=198.51.100.1)
WARN  Security: Browser fingerprint mismatch detected
INFO  Session created with security validation (client_ip=203.0.113.1, user_agent=Mozilla/5.0...)
```

### Monitoring Recommendations

Monitor for these patterns:
- High rate of IP mismatch errors → Possible attack or user with dynamic IP
- Fingerprint mismatches → Possible session hijacking attempt
- Multiple failed logins from different IPs with same token → Token theft

## API Changes

### Breaking Changes

**Token Generation**
```diff
- POST /api/token/generate
+ POST /api/token/generate
+ Requires: X-Forwarded-For or X-Real-IP header
```

**Login**
```diff
- POST /api/login
+ POST /api/login
+ Requires: X-Forwarded-For or X-Real-IP header
+ Requires: User-Agent header
+ New error: 403 Forbidden - IP address validation failed
```

**Authenticated Endpoints**
```diff
- All endpoints with CSRF middleware
+ All endpoints with CSRF middleware
+ New error: 403 Forbidden - IP address mismatch
+ New error: 403 Forbidden - Browser fingerprint mismatch
```

## Testing

### Unit Tests

- `client_info::tests` - IP extraction and fingerprint validation
- `auth::tests` - Token storage with IP address
- `middleware::csrf::tests` - Session validation with IP and fingerprint

### Integration Testing

Test with curl:

```bash
# Generate token (captures IP)
curl -H "X-Forwarded-For: 203.0.113.1" \
     -X POST http://localhost:8080/api/token/generate

# Login (validates IP and captures fingerprint)
curl -H "X-Forwarded-For: 203.0.113.1" \
     -H "User-Agent: Mozilla/5.0" \
     -H "Accept: text/html" \
     -H "Accept-Language: en-US" \
     -H "Accept-Encoding: gzip" \
     -X POST http://localhost:8080/api/login \
     -d '{"token": "TOKEN_FROM_ABOVE"}'

# Make authenticated request (validates IP and fingerprint)
curl -H "X-Forwarded-For: 203.0.113.1" \
     -H "User-Agent: Mozilla/5.0" \
     -H "Accept: text/html" \
     -H "Accept-Language: en-US" \
     -H "Accept-Encoding: gzip" \
     -H "Cookie: session_id=SESSION_FROM_ABOVE" \
     -H "X-CSRF-Token: CSRF_FROM_ABOVE" \
     -X POST http://localhost:8080/api/sessions
```

### Security Testing

Test IP validation bypass:
```bash
# Try to use token from different IP (should fail)
curl -H "X-Forwarded-For: 198.51.100.1" \
     -X POST http://localhost:8080/api/login \
     -d '{"token": "TOKEN_GENERATED_FROM_203.0.113.1"}'

# Expected: 403 Forbidden - IP address validation failed
```

Test fingerprint validation bypass:
```bash
# Try to use session with different User-Agent (should fail)
curl -H "X-Forwarded-For: 203.0.113.1" \
     -H "User-Agent: Chrome/90.0" \
     -H "Cookie: session_id=SESSION_CREATED_WITH_MOZILLA" \
     -X POST http://localhost:8080/api/sessions

# Expected: 403 Forbidden - Browser fingerprint mismatch
```

## Deployment Considerations

### Reverse Proxy Configuration

Ensure your reverse proxy forwards client IP:

**nginx:**
```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Real-IP $remote_addr;
```

**Apache:**
```apache
RequestHeader set X-Forwarded-For %{REMOTE_ADDR}s
```

**ngrok:**
ngrok automatically sets `X-Forwarded-For` - no configuration needed.

### CloudFlare

CloudFlare sets `CF-Connecting-IP` but code uses standard headers. Configure CloudFlare to set `X-Forwarded-For`:

```
Transform Rules → HTTP Request Header Modification
Add header: X-Forwarded-For = ip.src.ip
```

### Load Balancers

Ensure load balancer preserves client IP in `X-Forwarded-For` chain.

## Migration Guide

### Upgrading from Previous Version

No migration required. Security features are automatically enabled.

### Existing Sessions

Sessions created before this update will be invalidated on next request (missing IP/fingerprint).
Users will need to re-authenticate.

### Rollback Plan

If issues arise:
1. Revert to previous commit
2. Restart server
3. Existing tokens remain valid (IP validation skipped in old code)

## Future Enhancements

Potential improvements:

1. **Configurable IP validation** - Allow disabling for development
2. **IP range allowlists** - Accept IPs within configured ranges
3. **Fingerprint scoring** - Fuzzy matching instead of exact match
4. **Session notifications** - Alert on IP/fingerprint mismatch
5. **Geolocation validation** - Validate country/region consistency
6. **Device ID tracking** - More stable identifier than IP
7. **Rate limiting by IP** - Limit attempts from suspicious IPs

## Related Files

- `src/client_info.rs` - IP extraction and fingerprinting logic
- `src/auth.rs` - Token storage with IP address
- `src/middleware/csrf.rs` - Session validation with IP and fingerprint
- `src/main.rs` - Handler updates for IP/fingerprint extraction

## References

- [OWASP Session Management Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html)
- [RFC 7239 - Forwarded HTTP Extension](https://tools.ietf.org/html/rfc7239)
- [Device Fingerprinting Best Practices](https://owasp.org/www-community/controls/Device_fingerprint)
