# Adopt Explicit Timeout and Memory Sizing for Lambda Functions: Function Timeout Exceeds Dependency Budget

These rules are ALWAYS ACTIVE for all AWS Lambda function definitions in Terraform targeting production environments with external service dependencies and timeout values at or above thirty seconds.

### Rules

- **R-LAMBDA-001** MUST: Function timeout is set to at least the sum of downstream call budgets plus a margin.
- **R-LAMBDA-002** MUST: Functions calling an external API set a client-side timeout below the function timeout.
- **R-LAMBDA-003** SHOULD: Timeout values are declared in the module rather than left to the provider default.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "function" infra/terraform/lambda/ --include="*.tf"
grep -rE "function|timeout" infra/terraform/modules/lambda/
test -d infra/terraform/lambda/ && echo "governed tree present"
```

**Accept when:**
- Function timeout is set to at least the sum of downstream call budgets plus a margin
- Functions calling an external API set a client-side timeout below the function timeout
- Timeout values are declared in the module rather than left to the provider default

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
