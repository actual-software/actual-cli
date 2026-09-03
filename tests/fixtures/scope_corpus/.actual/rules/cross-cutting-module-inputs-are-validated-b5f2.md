# Adopt Pinned Provider Versions for Terraform Configurations: Module Inputs Are Validated

These rules are ALWAYS ACTIVE for all Terraform configuration files in infra/terraform/, including root modules and child modules requiring specific provider versions across multi-cloud deployments.

### Rules

- **R-TERRAF-021** MUST: Every module variable declares a type and a description.
- **R-TERRAF-022** MUST: Variables constrained to a set of values carry a validation block.
- **R-TERRAF-023** SHOULD: Sensitive variables are marked sensitive so they are redacted from plan output.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "module" infra/terraform/ --include="*.tf"
grep -rE "module|inputs" infra/terraform/modules/
test -d infra/terraform/ && echo "governed tree present"
```

**Accept when:**
- Every module variable declares a type and a description
- Variables constrained to a set of values carry a validation block
- Sensitive variables are marked sensitive so they are redacted from plan output

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
