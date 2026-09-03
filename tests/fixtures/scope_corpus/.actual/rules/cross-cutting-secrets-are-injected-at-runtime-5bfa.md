# Adopt Runtime Secret Injection in CI Workflows: Secrets Are Injected At Runtime

These rules are ALWAYS ACTIVE for all continuous integration workflow definitions in .github/workflows/ and the scripts they invoke, covering credential storage, masking, and the separation of pull request and release permissions.

### Rules

- **R-CISECR-001** MUST: Credentials reach a job through the secret store, never through a checked-in file.
- **R-CISECR-002** MUST: A workflow never echoes a secret, including into a debug step.
- **R-CISECR-003** SHOULD: Secret values are masked before any third-party action receives them.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "secrets" .github/workflows/ --include="*.yml"
grep -rE "secrets|injected" scripts/ci/
test -d .github/workflows/ && echo "governed tree present"
```

**Accept when:**
- Credentials reach a job through the secret store, never through a checked-in file
- A workflow never echoes a secret, including into a debug step
- Secret values are masked before any third-party action receives them

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
