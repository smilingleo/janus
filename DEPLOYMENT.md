# Deployment Guide: Public Exposure via Ngrok

This guide covers deploying Janus for public access through reverse proxies like ngrok.

## ⚠️ Security Warning

Exposing a terminal to the internet carries inherent security risks. Ensure you:
- Use strong authentication tokens
- Enable all security features (HTTPS, CSRF, rate limiting)
- Monitor logs for suspicious activity
- Keep token validity short (< 5 minutes recommended)
- Use ngrok's authentication features when available

## Prerequisites

- Ngrok account and installed CLI ([ngrok.com](https://ngrok.com))
- Janus built and configured
- Understanding of security implications

## Quick Start

### 1. Configure Janus

Create/update `config.toml`:

```toml
bind_address = "127.0.0.1:8080"

# Enable HTTPS (required for public exposure)
use_https = true
tls_auto_generate = true

# Allow any ngrok free tier subdomain (recommended for free tier)
allowed_origins = ["https://*.ngrok-free.app"]
# Or use specific domain for ngrok paid plans:
# allowed_origins = ["https://your-domain.ngrok.io"]

# Short token validity for security
token_validity_secs = 300  # 5 minutes

# Session timeout
idle_timeout_secs = 1800   # 30 minutes

# Enable rate limiting
[rate_limit]
requests_per_period = 3
period_secs = 60

[notification.imessage]
phone_number = "+1234567890"
```

### 2. Start Janus

```bash
./target/release/Janus --config config.toml
```

### 3. Start ngrok

```bash
# With a static domain (ngrok paid plan)
ngrok http --domain=your-domain.ngrok.io 8080

# With a random domain (free plan)
ngrok http 8080
```

Note the ngrok URL (e.g., `https://abc123.ngrok.io`) and update `allowed_origins` in config.toml if using a random domain.

### 4. Verify Security Features

Check the startup logs for:
```
WARN Public exposure enabled - ensure security features are working
INFO Security configuration https=true allowed_origins=["https://your-domain.ngrok.io"] trust_proxy=true
```

## Configuration Details

### HTTPS Setup

**Option 1: Auto-generated Self-signed Certificate (Recommended for ngrok)**

```toml
use_https = true
tls_auto_generate = true
```

Ngrok handles the public TLS connection, so a self-signed cert between ngrok and your server is acceptable.

**Option 2: Custom Certificate**

```toml
use_https = true
tls_cert_path = "/path/to/cert.pem"
tls_key_path = "/path/to/key.pem"
tls_auto_generate = false
```

### Origin Validation

Configure allowed origins to permit requests from your ngrok URL:

```toml
# Wildcard for ngrok free tier (URL changes on restart)
allowed_origins = ["https://*.ngrok-free.app"]

# Static ngrok domain (recommended for production)
allowed_origins = ["https://myapp.ngrok.io"]

# Multiple domains
allowed_origins = [
    "https://*.ngrok-free.app",
    "https://myapp.example.com"
]
```

**Wildcard Support**: Use `https://*.ngrok-free.app` for ngrok free tier to avoid updating config when the URL changes. The wildcard matches any subdomain (e.g., `https://abc123.ngrok-free.app`, `https://xyz789.ngrok-free.app`).

### Rate Limiting

Prevents abuse by limiting requests globally:

```toml
[rate_limit]
requests_per_period = 3     # Max requests
period_secs = 60            # Time window
```

Rate limiting is applied globally across all requests (not per-IP) for reliability and consistent behavior across all deployment scenarios.

### Token Validity

For public exposure, use short token validity:

```toml
# Very secure: 2 minutes
token_validity_secs = 120

# Balanced: 5 minutes (recommended)
token_validity_secs = 300

# Less secure: 1 hour (not recommended for public)
token_validity_secs = 3600
```

## Usage Workflow

1. **Request Token** (from anywhere):
   ```bash
   curl -X POST https://your-domain.ngrok.io/api/token/generate
   ```

2. **Receive Token via iMessage**

3. **Login to Get Session**:
   ```bash
   curl -X POST https://your-domain.ngrok.io/api/auth/login \
     -H "Content-Type: application/json" \
     -d '{"token": "YOUR_TOKEN_HERE"}'
   ```

   Response includes:
   - `session_id` cookie
   - `csrf_token` for subsequent requests

4. **Create Terminal Session** (include CSRF token):
   ```bash
   curl -X POST https://your-domain.ngrok.io/api/sessions \
     -H "Cookie: session_id=YOUR_SESSION_ID" \
     -H "X-CSRF-Token: YOUR_CSRF_TOKEN" \
     -H "Content-Type: application/json" \
     -d '{"rows": 24, "cols": 80}'
   ```

5. **Connect via WebSocket**:
   ```
   wss://your-domain.ngrok.io/api/sessions/{session_id}/ws
   ```

## Security Checklist

Before going public, verify:

- [ ] HTTPS enabled (`use_https = true`)
- [ ] Allowed origins configured with correct ngrok URL
- [ ] Rate limiting enabled with `trust_proxy = true`
- [ ] Token validity < 5 minutes
- [ ] Session timeout configured appropriately
- [ ] Server logs monitored for suspicious activity
- [ ] Ngrok authentication/authorization configured (if available)
- [ ] Firewall rules allow only ngrok traffic (optional)

## Monitoring

### Essential Log Messages

**Good**: Normal operation
```
INFO Web Terminal API starting bind_address=127.0.0.1:8080 version=0.1.0 https_enabled=true
INFO TLS certificates loaded successfully
INFO Security configuration https=true allowed_origins=["https://..."]
```

**Warning**: Potential security issues
```
WARN CSRF validation failed: token mismatch session_id=...
WARN Rejected request with invalid origin/referer origin=...
```

**Error**: Security violations
```
ERROR Rate limit exceeded for IP: 1.2.3.4
```

### Monitor for Abuse

Watch for:
- Repeated CSRF validation failures (possible attack)
- Many rate limit violations from same IP
- Invalid origin/referer rejections
- Rapid token generation requests

## Troubleshooting

### "Rejected request with invalid origin/referer"

**Cause**: Origin not in `allowed_origins` or using HTTP instead of HTTPS

**Fix**:
1. Check your ngrok URL matches exactly
2. Ensure URL uses `https://`
3. Restart server after config change

### "CSRF validation failed"

**Cause**: Missing or invalid `X-CSRF-Token` header

**Fix**: Include the CSRF token from login response in subsequent requests

### "Rate limit exceeded"

**Cause**: Too many requests from same IP

**Fix**:
- Wait for rate limit window to pass (default: 60 seconds)
- Adjust `requests_per_period` if legitimate use case
- Check if bot/script making excessive requests

### WebSocket "session not found in storage"

**Cause**: Auth session expired or server restarted

**Fix**: Login again to get a new session

## Advanced: Multiple Instances

For high availability, use ngrok load balancing with multiple Janus instances:

1. Run multiple Janus instances on different ports
2. Configure ngrok with backend load balancing
3. Use shared session storage (requires code modification)

Note: Current implementation uses in-memory sessions, so sessions won't survive server restarts.

## Ngrok-Specific Features

### Edge Configuration

Add authentication at the ngrok level:

```bash
ngrok http 8080 --basic-auth "username:password"
```

### Static Domains

Get consistent URLs (paid plan):

```bash
ngrok http --domain=myapp.ngrok-free.app 8080
```

### IP Restrictions

Limit access by IP (paid plan):

```bash
ngrok http 8080 --cidr-allow 1.2.3.4/32
```

## Alternative Reverse Proxies

This guide focuses on ngrok, but Janus works with other reverse proxies:

- **Cloudflare Tunnel**: Similar setup, use cloudflared
- **Tailscale Funnel**: Private network exposure
- **nginx**: Traditional reverse proxy with TLS termination

Configuration principles remain the same: HTTPS, correct origin, and trust_proxy settings.

## Production Considerations

For production deployments:

1. **Use systemd service** for automatic restart
2. **Rotate logs** to prevent disk fill
3. **Set up monitoring** (Prometheus/Grafana recommended)
4. **Implement alerting** for security events
5. **Regular security audits** of logs
6. **Token expiration notifications** to users
7. **Session limit enforcement** per user
8. **Backup strategy** for session logs

## Getting Help

- Check server logs first: `journalctl -u Janus -f`
- Verify configuration: `./Janus --config config.toml` (should not error)
- Test locally before public exposure
- Review SECURITY.md for security considerations

---

**Remember**: Public terminal access is powerful but risky. Always follow security best practices.
