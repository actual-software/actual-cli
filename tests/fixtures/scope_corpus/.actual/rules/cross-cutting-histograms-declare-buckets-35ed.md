# Adopt OpenTelemetry Metrics for Service Instrumentation: Histograms Declare Buckets

These rules are ALWAYS ACTIVE for all metric instrumentation in packages/telemetry/ and the services that consume it, covering counters, histograms, and the exporter configuration used in production.

### Rules

- **R-OTEL-011** MUST: Latency histograms declare explicit bucket boundaries rather than accepting the default.
- **R-OTEL-012** MUST: Bucket boundaries are expressed in milliseconds and documented.
- **R-OTEL-013** SHOULD: A histogram carries at most six attributes to bound cardinality.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "histograms" packages/telemetry/ --include="*.ts"
grep -rE "histograms|declare" services/gateway/telemetry/
test -d packages/telemetry/ && echo "governed tree present"
```

**Accept when:**
- Latency histograms declare explicit bucket boundaries rather than accepting the default
- Bucket boundaries are expressed in milliseconds and documented
- A histogram carries at most six attributes to bound cardinality

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
