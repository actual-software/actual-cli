---
version: 2
name: cross-cutting-token-verification-checks-revocation-963a
description: "Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Verification Checks Revocation"
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

# Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Verification Checks Revocation

Scope: applies to `services/auth/oauth/**`, `services/auth/jwks/**`.

These rules are ALWAYS ACTIVE for all OAuth access token issuance and verification code, including modules handling public API authentication, token signing, and JWKS key rotation.

### Rules

- **R-OAUTH-021** MUST: Verification consults the revocation list before accepting an otherwise valid token.
- **R-OAUTH-022** MUST: A revocation lookup failure fails closed for administrative scopes.
- **R-OAUTH-023** SHOULD: Verification failures are logged with the error code and token id but never the token itself.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "token" services/auth/oauth/ --include="*.ts"
grep -rE "token|verification" services/auth/jwks/
test -d services/auth/oauth/ && echo "governed tree present"
```

**Accept when:**
- Verification consults the revocation list before accepting an otherwise valid token
- A revocation lookup failure fails closed for administrative scopes
- Verification failures are logged with the error code and token id but never the token itself

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
