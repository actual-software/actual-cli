# Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Signing Uses Asymmetric Keys

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
