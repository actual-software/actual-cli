# Adopt Lifecycle Contracts for Gateway Service Registry: Registry Entries Are Dataclasses

These rules are ALWAYS ACTIVE for all service registry and domain coordination modules in services/gateway/routing/ that manage asynchronous lifecycle operations, external client connections, and tool routing.

### Rules

- **R-GATEWA-021** MUST: Registry entries are frozen dataclasses with explicit field types.
- **R-GATEWA-022** MUST: Serialisation goes through explicit to_dict and from_dict functions.
- **R-GATEWA-023** SHOULD: Entry equality is by identifier, not by full structural comparison.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "registry" services/gateway/routing/ --include="*.py"
grep -rE "registry|entries" services/gateway/routing/registry.py
test -d services/gateway/routing/ && echo "governed tree present"
```

**Accept when:**
- Registry entries are frozen dataclasses with explicit field types
- Serialisation goes through explicit to_dict and from_dict functions
- Entry equality is by identifier, not by full structural comparison

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
