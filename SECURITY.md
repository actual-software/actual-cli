# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Actual CLI, please report it
responsibly. **Do not open a public GitHub issue.**

**Email:** [john@actual.ai](mailto:john@actual.ai)

Include:

- A description of the vulnerability
- Steps to reproduce
- Affected versions
- Any potential impact assessment

We will acknowledge receipt within 48 hours and aim to provide a fix or
mitigation plan within 7 days for critical issues.

## Scope

The following are in scope for security reports:

- Path traversal or file write outside the target repository
- Credential leakage (API keys, tokens) through logs, error messages, or
  network requests
- Authentication or authorization bypass
- Supply chain risks in CI/CD workflows
- Prompt injection that results in arbitrary file writes or code execution

The following are **out of scope**:

- Social engineering attacks
- Denial of service against the public API
- Issues in third-party dependencies (report these upstream, but let us know
  so we can update)

## Credential Handling

- API keys configured in `~/.actualai/actual/config.yaml` are stored in
  plaintext with `0600` permissions (owner read/write only). Environment
  variables are the recommended alternative.
- The `config show` command redacts API keys in its output.
- All API communication uses HTTPS. Non-HTTPS URLs are rejected for
  non-localhost endpoints.
- The telemetry service key embedded in the binary is a write-only token
  scoped to submitting anonymous counters. It does not grant read or
  admin access.

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | Yes       |
| < latest | Best effort |
