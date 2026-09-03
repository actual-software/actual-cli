# Adopt Explicit Timeout and Memory Sizing for Lambda Functions: Dead Letter Queues Are Configured

These rules are ALWAYS ACTIVE for all AWS Lambda function definitions in Terraform targeting production environments with external service dependencies and timeout values at or above thirty seconds.

### Rules

- **R-LAMBDA-021** MUST: Asynchronously invoked functions declare a dead letter target.
- **R-LAMBDA-022** MUST: The dead letter queue has an alarm on depth.
- **R-LAMBDA-023** SHOULD: Retry attempts for asynchronous invocation are set explicitly.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "dead" infra/terraform/lambda/ --include="*.tf"
grep -rE "dead|letter" infra/terraform/modules/lambda/
test -d infra/terraform/lambda/ && echo "governed tree present"
```

**Accept when:**
- Asynchronously invoked functions declare a dead letter target
- The dead letter queue has an alarm on depth
- Retry attempts for asynchronous invocation are set explicitly

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
