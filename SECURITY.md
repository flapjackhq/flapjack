# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Flapjack, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please email: **stuart.clifford@gmail.com** with:

- Description of the vulnerability
- Steps to reproduce
- Potential impact

You should receive a response within 48 hours.

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest  | Yes       |

## Security Best Practices

When deploying Flapjack in production:

1. **Always set `FLAPJACK_ENV=production`** - this enforces API key authentication
2. **Use a strong admin key** (at least 16 characters)
3. **Use secured API keys** for client-side access with restricted permissions
4. **Run behind a reverse proxy** (nginx, Caddy) with TLS termination
5. **Don't expose the server directly to the internet** without authentication enabled
