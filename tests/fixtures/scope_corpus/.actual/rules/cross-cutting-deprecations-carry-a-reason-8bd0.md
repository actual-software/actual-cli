# Adopt Additive Schema Evolution for the GraphQL API: Deprecations Carry A Reason

These rules are ALWAYS ACTIVE for all GraphQL schema definitions and resolvers in services/api/graphql/, covering field deprecation, nullability changes, and the compatibility checks run before release.

### Rules

- **R-GRAPHQ-021** MUST: Every deprecated field states the replacement and the removal window.
- **R-GRAPHQ-022** MUST: Deprecated field usage is counted in metrics.
- **R-GRAPHQ-023** SHOULD: Removal happens only after usage reaches zero.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "deprecations" services/api/graphql/ --include="*.ts"
grep -rE "deprecations|carry" services/api/graphql/resolvers/
test -d services/api/graphql/ && echo "governed tree present"
```

**Accept when:**
- Every deprecated field states the replacement and the removal window
- Deprecated field usage is counted in metrics
- Removal happens only after usage reaches zero

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
