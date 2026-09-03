# Adopt Pinned Provider Versions for Terraform Configurations: Remote State Is Encrypted

These rules are ALWAYS ACTIVE for all Terraform configuration files in infra/terraform/, including root modules and child modules requiring specific provider versions across multi-cloud deployments.

### Rules

- **R-TERRAF-011** MUST: Remote state backends enable server-side encryption and versioning.
- **R-TERRAF-012** MUST: State locking is configured for every workspace.
- **R-TERRAF-013** SHOULD: State buckets deny public access at the bucket policy level.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "remote" infra/terraform/ --include="*.tf"
grep -rE "remote|state" infra/terraform/modules/
test -d infra/terraform/ && echo "governed tree present"
```

**Accept when:**
- Remote state backends enable server-side encryption and versioning
- State locking is configured for every workspace
- State buckets deny public access at the bucket policy level

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
