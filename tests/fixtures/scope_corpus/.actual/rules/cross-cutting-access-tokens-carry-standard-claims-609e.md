---
version: 2
name: cross-cutting-access-tokens-carry-standard-claims-609e
description: "Adopt RS256 JWT Signing for Public OAuth Access Tokens: Access Tokens Carry Standard Claims"
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

# Adopt RS256 JWT Signing for Public OAuth Access Tokens: Access Tokens Carry Standard Claims

Scope: applies to `services/auth/oauth/**`, `services/auth/jwks/**`.

These rules are ALWAYS ACTIVE for all OAuth access token issuance and verification code, including modules handling public API authentication, token signing, and JWKS key rotation.

### Rules

- **R-OAUTH-001** MUST: Access tokens include issuer, audience, expiration and issued-at claims derived from environment configuration.
- **R-OAUTH-002** MUST: Token signing uses RS256 with a key id resolved from the JWKS document.
- **R-OAUTH-003** SHOULD: Claim validation rejects a token whose audience does not match the configured API scope.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "access" services/auth/oauth/ --include="*.ts"
grep -rE "access|tokens" services/auth/jwks/
test -d services/auth/oauth/ && echo "governed tree present"
```

**Accept when:**
- Access tokens include issuer, audience, expiration and issued-at claims derived from environment configuration
- Token signing uses RS256 with a key id resolved from the JWKS document
- Claim validation rejects a token whose audience does not match the configured API scope

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
