# Adopt Runtime Secret Injection in CI Workflows: Third Party Actions Are Pinned

These rules are ALWAYS ACTIVE for all continuous integration workflow definitions in .github/workflows/ and the scripts they invoke, covering credential storage, masking, and the separation of pull request and release permissions.

### Rules

- **R-CISECR-021** MUST: Third-party actions are pinned to a full commit sha.
- **R-CISECR-022** MUST: A pinned action is reviewed before its sha is advanced.
- **R-CISECR-023** SHOULD: Actions from unreviewed publishers are vendored instead of referenced.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "third" .github/workflows/ --include="*.yml"
grep -rE "third|party" scripts/ci/
test -d .github/workflows/ && echo "governed tree present"
```

**Accept when:**
- Third-party actions are pinned to a full commit sha
- A pinned action is reviewed before its sha is advanced
- Actions from unreviewed publishers are vendored instead of referenced

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
