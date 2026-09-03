# Adopt Runtime Secret Injection in CI Workflows: Workflow Permissions Are Least Privilege

These rules are ALWAYS ACTIVE for all continuous integration workflow definitions in .github/workflows/ and the scripts they invoke, covering credential storage, masking, and the separation of pull request and release permissions.

### Rules

- **R-CISECR-011** MUST: Every workflow declares an explicit permissions block.
- **R-CISECR-012** MUST: Pull request workflows from forks receive read-only permissions.
- **R-CISECR-013** SHOULD: Release credentials are scoped to the release workflow alone.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "workflow" .github/workflows/ --include="*.yml"
grep -rE "workflow|permissions" scripts/ci/
test -d .github/workflows/ && echo "governed tree present"
```

**Accept when:**
- Every workflow declares an explicit permissions block
- Pull request workflows from forks receive read-only permissions
- Release credentials are scoped to the release workflow alone

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
