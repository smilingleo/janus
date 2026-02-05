# CLI Reference

Command-line interface reference for Janus.

## Synopsis

```bash
janus [OPTIONS]
```

## Description

Janus is a web-based terminal with token-based authentication. Configure via config file and/or CLI arguments. CLI arguments override config file settings.

## Options

### `-c, --config <FILE>`

Path to configuration file.

**Default:** `config.toml`

**Example:**
```bash
janus --config /etc/janus/production.toml
```

### `-b, --bind <ADDRESS>`

Bind address and port for the HTTP server.

**Format:** `IP:PORT` (must include port)

**Validation:**
- Must include colon separator (`:`)
- Must specify both IP and port

**Examples:**
```bash
# Bind to localhost on port 8080
janus --bind 127.0.0.1:8080

# Bind to localhost on port 9090
janus --bind 127.0.0.1:9090

# Invalid: missing port
janus --bind 127.0.0.1  # ERROR
```

**Overrides:** `bind_address` in config file

### `--https`

Enable HTTPS with TLS.

**Examples:**
```bash
# Enable HTTPS
janus --https

# Use with auto-generated certificate (default)
janus --https
```

**Overrides:** `use_https` in config file

**Note:** When enabled without explicit certificate paths, a self-signed certificate is auto-generated.

### `--no-https`

Disable HTTPS.

**Examples:**
```bash
# Disable HTTPS for local testing
janus --no-https
```

**Overrides:** `use_https` in config file and `--https` flag

**Conflicts with:** `--https`

### `-o, --origin <ORIGIN>`

Allowed CORS origin (can be specified multiple times).

**Format:**
- `https://domain.com` - Specific HTTPS domain
- `https://*.domain.com` - Wildcard subdomain
- `http://127.0.0.1:PORT` - Local development
- `http://localhost:PORT` - Local development

**Validation:**
- Public origins must use `https://`
- Local origins can use `http://` (127.0.0.1 or localhost only)
- Wildcards (`*`) must be in subdomain position
- Only one wildcard allowed per origin
- Format: `https://*.domain.com` (not `https://domain.*.com`)

**Examples:**
```bash
# Single origin
janus --origin "https://myapp.ngrok.io"

# Multiple origins
janus --origin "https://myapp.ngrok.io" \
      --origin "https://*.ngrok-free.app"

# Wildcard for ngrok free tier
janus --origin "https://*.ngrok-free.app"

# Local development
janus --origin "http://localhost:3000"

# Invalid examples
janus --origin "http://example.com"           # ERROR: public HTTP
janus --origin "https://**.ngrok.io"          # ERROR: multiple wildcards
janus --origin "https://app.*.example.com"    # ERROR: wildcard not in subdomain position
```

**Overrides:** `allowed_origins` array in config file

**Security Note:** When origins are specified, HTTPS is recommended (use `--https`).

### `-p, --phone <NUMBER>`

Phone number for iMessage notifications.

**Format:** `+<country><number>` (international format)

**Validation:**
- Must start with `+`
- Must contain 4-20 digits after `+`
- Only digits allowed (no spaces, dashes, parentheses)

**Examples:**
```bash
# US number
janus --phone "+14155551234"

# International number
janus --phone "+447700900123"

# Invalid examples
janus --phone "14155551234"        # ERROR: missing +
janus --phone "+1-415-555-1234"    # ERROR: contains dashes
janus --phone "+123"               # ERROR: too few digits
janus --phone "+1 415 555 1234"    # ERROR: contains spaces
```

**Overrides:** `notification.phone_number` in config file

### `-l, --log-dir <DIR>`

Session log directory path.

**Examples:**
```bash
# Absolute path
janus --log-dir /var/log/janus/sessions

# Home directory expansion
janus --log-dir ~/.janus/logs

# Relative path
janus --log-dir ./logs
```

**Overrides:** `session_log_dir` in config file

**Note:** Tilde (`~`) expansion is supported.

## Usage Examples

### Basic Usage

```bash
# Use default config file (config.toml)
janus

# Use specific config file
janus --config production.toml
```

### Local Development

```bash
# Local testing without config file
janus \
  --bind 127.0.0.1:8080 \
  --phone "+14155551234" \
  --no-https

# Local testing with frontend dev server
janus \
  --bind 127.0.0.1:8080 \
  --origin "http://localhost:5173" \
  --phone "+14155551234"
```

### Production Deployment (ngrok)

```bash
# Static domain (ngrok paid)
janus \
  --bind 127.0.0.1:8080 \
  --https \
  --phone "+14155551234" \
  --origin "https://myapp.ngrok.io"

# Dynamic domain (ngrok free tier)
janus \
  --bind 127.0.0.1:8080 \
  --https \
  --phone "+14155551234" \
  --origin "https://*.ngrok-free.app"
```

### Override Single Setting

```bash
# Use production config but different port
janus --config production.toml --bind 127.0.0.1:9090

# Use production config but test phone
janus --config production.toml --phone "+15555555555"

# Use production config but disable HTTPS for debugging
janus --config production.toml --no-https
```

### Multiple Overrides

```bash
# Override several settings
janus \
  --config base.toml \
  --bind 127.0.0.1:9090 \
  --https \
  --origin "https://test.ngrok.io" \
  --origin "https://test2.ngrok.io" \
  --log-dir ~/janus-test-logs
```

## Configuration Precedence

Settings are applied in this order (later overrides earlier):

1. **Default values** (hardcoded defaults)
2. **Config file** (`config.toml` or specified via `--config`)
3. **CLI arguments** (highest priority)

Example:
```toml
# config.toml
bind_address = "127.0.0.1:8080"
use_https = false
```

```bash
# CLI overrides config file
janus --config config.toml --bind 127.0.0.1:9090 --https

# Result:
# - bind_address = "127.0.0.1:9090" (from CLI)
# - use_https = true (from CLI)
# - Other settings from config.toml
```

## Validation

CLI arguments are validated at startup. Invalid values cause immediate exit with error message.

**Validation checks:**
- Bind address must include port (`:`)
- Phone number must start with `+` and contain 4-20 digits
- Origins must use HTTPS (except localhost/127.0.0.1)
- Origin wildcards must be in subdomain position
- Only one wildcard per origin

**Error handling:**
```bash
# Invalid bind address
$ janus --bind 127.0.0.1
ERROR Invalid bind address format: must include port (e.g., 127.0.0.1:8080)

# Invalid phone number
$ janus --phone "1234567890"
ERROR Invalid phone number format: must start with + (e.g., +1234567890)

# Invalid origin
$ janus --origin "http://example.com"
ERROR Invalid origin format: public origins must use HTTPS
```

## Environment Variables

Currently, Janus does not support environment variable configuration. Use CLI arguments or config file.

**Future consideration:** Environment variables may be added in future releases (e.g., `JANUS_BIND_ADDRESS`, `JANUS_PHONE_NUMBER`).

## Exit Codes

- `0` - Success
- `1` - Configuration error (invalid args, missing config, validation failure)

## See Also

- `config.example.toml` - Example configuration file
- `DEPLOYMENT.md` - Deployment guide
- `README.md` - Project overview
- `SECURITY_ENHANCEMENTS.md` - Security features

## Version

Run `janus --version` to see the current version.

```bash
$ janus --version
janus 0.1.0
```
