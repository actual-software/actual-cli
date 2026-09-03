# Adopt Additive Schema Evolution for the GraphQL API: Schema Changes Are Additive

These rules are ALWAYS ACTIVE for all GraphQL schema definitions and resolvers in services/api/graphql/, covering field deprecation, nullability changes, and the compatibility checks run before release.

### Rules

- **R-GRAPHQ-001** MUST: A released field is deprecated rather than removed.
- **R-GRAPHQ-002** MUST: A nullable field never becomes non-nullable in place.
- **R-GRAPHQ-003** SHOULD: Breaking changes require a new field name and a migration window.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "schema" services/api/graphql/ --include="*.ts"
grep -rE "schema|changes" services/api/graphql/resolvers/
test -d services/api/graphql/ && echo "governed tree present"
```

**Accept when:**
- A released field is deprecated rather than removed
- A nullable field never becomes non-nullable in place
- Breaking changes require a new field name and a migration window

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
