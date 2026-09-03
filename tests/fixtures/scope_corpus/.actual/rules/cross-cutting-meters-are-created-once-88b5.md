# Adopt OpenTelemetry Metrics for Service Instrumentation: Meters Are Created Once

These rules are ALWAYS ACTIVE for all metric instrumentation in packages/telemetry/ and the services that consume it, covering counters, histograms, and the exporter configuration used in production.

### Rules

- **R-OTEL-001** MUST: A meter is created once per module at import time, never inside a request handler.
- **R-OTEL-002** MUST: Instrument names use the service prefix and dotted lower case.
- **R-OTEL-003** SHOULD: Instruments are cached rather than recreated per observation.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "meters" packages/telemetry/ --include="*.ts"
grep -rE "meters|created" services/gateway/telemetry/
test -d packages/telemetry/ && echo "governed tree present"
```

**Accept when:**
- A meter is created once per module at import time, never inside a request handler
- Instrument names use the service prefix and dotted lower case
- Instruments are cached rather than recreated per observation

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
