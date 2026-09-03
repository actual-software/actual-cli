# Adopt OpenTelemetry Metrics for Service Instrumentation: Span Attributes Are Bounded

These rules are ALWAYS ACTIVE for all metric instrumentation in packages/telemetry/ and the services that consume it, covering counters, histograms, and the exporter configuration used in production.

### Rules

- **R-OTEL-021** MUST: Span attributes never include user identifiers or request bodies.
- **R-OTEL-022** MUST: Attribute values are truncated before they are recorded.
- **R-OTEL-023** SHOULD: Spans are ended in a finally block so an exception cannot leak one.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "span" packages/telemetry/ --include="*.ts"
grep -rE "span|attributes" services/gateway/telemetry/
test -d packages/telemetry/ && echo "governed tree present"
```

**Accept when:**
- Span attributes never include user identifiers or request bodies
- Attribute values are truncated before they are recorded
- Spans are ended in a finally block so an exception cannot leak one

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
