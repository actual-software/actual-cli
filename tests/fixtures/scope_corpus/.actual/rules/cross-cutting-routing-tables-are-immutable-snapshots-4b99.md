# Adopt Lifecycle Contracts for Gateway Service Registry: Routing Tables Are Immutable Snapshots

These rules are ALWAYS ACTIVE for all service registry and domain coordination modules in services/gateway/routing/ that manage asynchronous lifecycle operations, external client connections, and tool routing.

### Rules

- **R-GATEWA-011** MUST: A routing decision reads one immutable snapshot rather than a mutating table.
- **R-GATEWA-012** MUST: Snapshot replacement is atomic.
- **R-GATEWA-013** SHOULD: Routing never blocks on registry mutation.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "routing" services/gateway/routing/ --include="*.py"
grep -rE "routing|tables" services/gateway/routing/registry.py
test -d services/gateway/routing/ && echo "governed tree present"
```

**Accept when:**
- A routing decision reads one immutable snapshot rather than a mutating table
- Snapshot replacement is atomic
- Routing never blocks on registry mutation

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
