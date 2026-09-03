# Adopt RS256 JWT Signing for Public OAuth Access Tokens: Token Verification Checks Revocation

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
