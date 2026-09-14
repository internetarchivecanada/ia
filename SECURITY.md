# Security Policy

## Reporting Vulnerabilities

**Do not open a public issue for security vulnerabilities.**

Instead, please report vulnerabilities through one of:

- [GitHub Security Advisory](https://github.com/internetarchivecanada/ia/security/advisories/new)
- Email: info@archive.org

This is a one-maintainer project. Expect a first response within a week; we will work with you on a fix.

## Security Practices

This project implements several security measures:

- **SSRF protection:** Redirect domain restriction to `*.archive.org` — prevents auth header leakage to external hosts
- **Auth header handling:** No-redirect client preserves auth headers for archive.org redirects without leaking to third parties
- **Path traversal prevention:** Download paths are validated and sanitized (CVE-2025-58438 equivalent caught before public release)
- **Upload validation:** Symlink detection, identifier sanitization, dotfile skipping
- **Mock-only write tests:** Live archive.org is never modified in automated tests
- **Dependency auditing:** `cargo audit` runs as a CI job on every pull request

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest release | Yes |
| Older releases | No |

## Security Audit History

- [Download Security Audit (2026-03-03)](docs/security/2026-03-03-download-security-audit.md)
