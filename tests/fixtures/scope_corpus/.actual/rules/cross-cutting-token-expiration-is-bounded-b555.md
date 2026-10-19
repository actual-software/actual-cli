---
version: 2
name: cross-cutting-token-expiration-is-bounded-b555
description: "Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Expiration Is Bounded"
metadata:
  actual:
    adr_id: 1243b051-95a3-50a8-be4f-9f652693c20c
    status: accepted
    candidate_group_id: 990bb253-6cba-5551-a3ea-4b58289b03f8
    evidence:
      source: deep
  scope:
    root: .
    paths:
      - "services/auth/oauth/**"
      - "services/auth/jwks/**"
    languages:
      - typescript
---

# Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Expiration Is Bounded

Scope: applies to `services/auth/oauth/**`, `services/auth/jwks/**`.

These rules are ALWAYS ACTIVE for all OAuth access token issuance and verification code, including modules handling public API authentication, token signing, and JWKS key rotation.

### Rules

- **R-OAUTH-031** MUST: Access token lifetime is configured through an environment variable and capped at one hour.
- **R-OAUTH-032** MUST: Refresh tokens rotate on every use and the superseded token is revoked.
- **R-OAUTH-033** SHOULD: Clock skew tolerance during expiry validation stays under sixty seconds.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "token" services/auth/oauth/ --include="*.ts"
grep -rE "token|expiration" services/auth/jwks/
test -d services/auth/oauth/ && echo "governed tree present"
```

**Accept when:**
- Access token lifetime is configured through an environment variable and capped at one hour
- Refresh tokens rotate on every use and the superseded token is revoked
- Clock skew tolerance during expiry validation stays under sixty seconds

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
