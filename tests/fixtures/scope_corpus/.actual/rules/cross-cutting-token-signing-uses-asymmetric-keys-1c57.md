---
version: 2
name: cross-cutting-token-signing-uses-asymmetric-keys-1c57
description: "Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Signing Uses Asymmetric Keys"
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

# Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Signing Uses Asymmetric Keys

Scope: applies to `services/auth/oauth/**`, `services/auth/jwks/**`.

These rules are ALWAYS ACTIVE for all OAuth access token issuance and verification code, including modules handling public API authentication, token signing, and JWKS key rotation.

### Rules

- **R-OAUTH-011** MUST: Signing operations load the private key from the secrets manager at runtime, never from source.
- **R-OAUTH-012** MUST: Symmetric HMAC signing is limited to short-lived internal state values.
- **R-OAUTH-013** SHOULD: Key rotation publishes the new public key to JWKS before the private key is used to sign.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "token" services/auth/oauth/ --include="*.ts"
grep -rE "token|signing" services/auth/jwks/
test -d services/auth/oauth/ && echo "governed tree present"
```

**Accept when:**
- Signing operations load the private key from the secrets manager at runtime, never from source
- Symmetric HMAC signing is limited to short-lived internal state values
- Key rotation publishes the new public key to JWKS before the private key is used to sign

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
