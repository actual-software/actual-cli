# Adopt Explicit Timeout and Memory Sizing for Lambda Functions: Memory Is Sized From Measurement

These rules are ALWAYS ACTIVE for all AWS Lambda function definitions in Terraform targeting production environments with external service dependencies and timeout values at or above thirty seconds.

### Rules

- **R-LAMBDA-011** MUST: Memory allocation is derived from observed peak usage rather than the default.
- **R-LAMBDA-012** MUST: Functions above one gigabyte of memory carry a comment justifying the size.
- **R-LAMBDA-013** SHOULD: Provisioned concurrency is declared for latency-sensitive functions.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "memory" infra/terraform/lambda/ --include="*.tf"
grep -rE "memory|sized" infra/terraform/modules/lambda/
test -d infra/terraform/lambda/ && echo "governed tree present"
```

**Accept when:**
- Memory allocation is derived from observed peak usage rather than the default
- Functions above one gigabyte of memory carry a comment justifying the size
- Provisioned concurrency is declared for latency-sensitive functions

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
