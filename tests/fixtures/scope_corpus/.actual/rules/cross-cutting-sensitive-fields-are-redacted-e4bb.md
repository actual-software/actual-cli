# Adopt Structured Log Events Across Services: Sensitive Fields Are Redacted

These rules are ALWAYS ACTIVE for all log emission in packages/logger/ and every service that imports it, covering event naming, contextual fields, and redaction of sensitive values.

### Rules

- **R-LOGGIN-011** MUST: Field redaction is applied by the logger, not by each call site.
- **R-LOGGIN-012** MUST: Redaction covers credentials, tokens, and personal identifiers.
- **R-LOGGIN-013** SHOULD: A field whose name is unknown to the redaction table is dropped at warn level.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "sensitive" packages/logger/ --include="*.ts"
grep -rE "sensitive|fields" services/
test -d packages/logger/ && echo "governed tree present"
```

**Accept when:**
- Field redaction is applied by the logger, not by each call site
- Redaction covers credentials, tokens, and personal identifiers
- A field whose name is unknown to the redaction table is dropped at warn level

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
