# Adopt Structured Log Events Across Services: Errors Log Cause Chains

These rules are ALWAYS ACTIVE for all log emission in packages/logger/ and every service that imports it, covering event naming, contextual fields, and redaction of sensitive values.

### Rules

- **R-LOGGIN-021** MUST: An error log includes the error name, code, and the full cause chain.
- **R-LOGGIN-022** MUST: Stack traces are attached at error level only.
- **R-LOGGIN-023** SHOULD: A caught error that is rethrown is logged exactly once.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "errors" packages/logger/ --include="*.ts"
grep -rE "errors|cause" services/
test -d packages/logger/ && echo "governed tree present"
```

**Accept when:**
- An error log includes the error name, code, and the full cause chain
- Stack traces are attached at error level only
- A caught error that is rethrown is logged exactly once

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
