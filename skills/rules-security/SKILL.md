---
name: rules-security
description: Security and code protection rules. Use when working with user input, authentication, data storage, external integrations, as well as during security code review and vulnerability analysis.
---

# Security

Every external input is hostile, every secret is sacred, every authorization check is mandatory.

## Always (without exceptions)

- Validate all external input at the system boundary (API routes, form handlers)
- Parameterize all database queries — never concatenate user input into SQL
- Encode output to prevent XSS (use framework auto-escaping)
- HTTPS for all external communication
- Check dependencies for known vulnerabilities before every release

## Ask first (requires human approval)

- New authentication flows or authorization logic changes
- Storing new categories of sensitive data (PII, payments)
- New integrations with external services
- Changing CORS configuration
- Adding file uploads
- Changing rate limiting
- Escalating permissions or roles

## Never

- Don't commit secrets to VCS (API keys, passwords, tokens)
- Don't log sensitive data (passwords, tokens, card numbers)
- Don't trust client-side validation as a security boundary
- Don't disable security headers for convenience
- Don't use `eval()` or `innerHTML` with user data
- Don't store sessions in localStorage (for auth tokens)
- Don't show stack traces or internal error details to users

## Authentication

- Passwords: bcrypt ≥12 rounds, scrypt, or argon2
- Cookies: `httpOnly`, `secure`, `sameSite: 'lax'`
- Rate limiting on login: ≤10 attempts / 15 minutes
- Password reset tokens: ≤1 hour, single use
- MFA for sensitive operations (recommended)

## Authorization

- Every endpoint checks authentication
- Every resource access checks owner/role (prevents IDOR)
- JWT: signature validation, expiration, issuer

## Input Validation

- Whitelists, not blacklists
- SQL: parameterized queries
- HTML: framework auto-escaping
- File uploads: type restriction, size limit, content verification

## Data Protection

- Sensitive fields excluded from API responses and logs
- HTTPS everywhere, encrypted backups
- Security headers: CSP, HSTS, X-Frame-Options, X-Content-Type-Options

## Red flags

- User input directly in SQL, shell commands, or HTML
- Secrets in source code or commit history
- Endpoints without authentication/authorization checks
- CORS with `origin: '*'` or without configuration
- No rate limiting on authentication endpoints
- Stack traces or internal errors visible to users
- Dependencies with critical vulnerabilities

## Typical Excuses

| Excuse | Reality |
|---|---|
| "It's an internal tool, security doesn't matter" | Internal tools get hacked. Attackers target the weakest link |
| "We'll add security later" | Retrofitting is 10x harder. Do it right away |
| "No one will exploit this" | Automated scanners will find it |
| "The framework handles everything" | Frameworks provide tools, not guarantees |
| "It's just a prototype" | Prototypes become production |

## OWASP Top 10 (brief)

| Vulnerability | Prevention |
|---|---|
| Broken access control | Authorization checks, owner verification |
| Injections | Parameterized queries, validation |
| Insecure design | Threat modeling |
| Vulnerable components | Dependency audits, minimum dependencies |
| SSRF | URL whitelists |

## Severity Classification (for audits)

| Severity | Criteria | Action |
|----------|----------|--------|
| **Critical** | Remotely exploitable, data leak, or full compromise | Fix immediately, block the release |
| **High** | Exploitable with conditions, significant data leak | Fix before release |
| **Medium** | Limited impact or requires authorization | Fix in the current sprint |
| **Low** | Theoretical risk or defense-in-depth | Schedule for next sprint |
| **Info** | Best practice recommendation | Consider |
