# Adopt Pinned Provider Versions for Terraform Configurations: Provider Versions Are Pinned

These rules are ALWAYS ACTIVE for all Terraform configuration files in infra/terraform/, including root modules and child modules requiring specific provider versions across multi-cloud deployments.

### Rules

- **R-TERRAF-001** MUST: Every required_providers block pins an exact provider version rather than a range.
- **R-TERRAF-002** MUST: The provider lock file is committed and updated in the same change as the version bump.
- **R-TERRAF-003** SHOULD: A module never declares a provider configuration of its own.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "provider" infra/terraform/ --include="*.tf"
grep -rE "provider|versions" infra/terraform/modules/
test -d infra/terraform/ && echo "governed tree present"
```

**Accept when:**
- Every required_providers block pins an exact provider version rather than a range
- The provider lock file is committed and updated in the same change as the version bump
- A module never declares a provider configuration of its own

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
