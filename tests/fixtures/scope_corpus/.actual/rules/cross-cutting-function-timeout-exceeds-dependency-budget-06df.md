---
version: 2
name: cross-cutting-function-timeout-exceeds-dependency-budget-06df
description: "Adopt Explicit Timeout and Memory Sizing for Lambda Functions: Function Timeout Exceeds Dependency Budget"
metadata:
  actual:
    adr_id: 014a15ec-8ff3-5961-9d21-e6a5eb76f45a
    status: accepted
    candidate_group_id: beb08fda-1bd1-59b6-a789-07ee0af5b81e
    evidence:
      source: signal
  scope:
    root: .
    paths:
      - "infra/terraform/lambda/**"
      - "infra/terraform/modules/lambda/**"
---

# Adopt Explicit Timeout and Memory Sizing for Lambda Functions: Function Timeout Exceeds Dependency Budget

Scope: applies to `infra/terraform/lambda/**`, `infra/terraform/modules/lambda/**`.

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
