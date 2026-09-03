# Adopt Structured Log Events Across Services: Log Events Are Named Not Formatted

These rules are ALWAYS ACTIVE for all log emission in packages/logger/ and every service that imports it, covering event naming, contextual fields, and redaction of sensitive values.

### Rules

- **R-LOGGIN-001** MUST: Log calls pass an event name and a field object rather than an interpolated string.
- **R-LOGGIN-002** MUST: Event names are stable identifiers and are never built at runtime.
- **R-LOGGIN-003** SHOULD: The standard library logger is never imported directly by a service.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "events" packages/logger/ --include="*.ts"
grep -rE "events|named" services/
test -d packages/logger/ && echo "governed tree present"
```

**Accept when:**
- Log calls pass an event name and a field object rather than an interpolated string
- Event names are stable identifiers and are never built at runtime
- The standard library logger is never imported directly by a service

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
