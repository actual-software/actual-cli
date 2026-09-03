# Adopt Lifecycle Contracts for Gateway Service Registry: Registry Exposes Start And Stop

These rules are ALWAYS ACTIVE for all service registry and domain coordination modules in services/gateway/routing/ that manage asynchronous lifecycle operations, external client connections, and tool routing.

### Rules

- **R-GATEWA-001** MUST: The registry exposes start and stop coroutines and is safe to stop before it has started.
- **R-GATEWA-002** MUST: Registration and deregistration are idempotent.
- **R-GATEWA-003** SHOULD: Stop cancels in-flight routing tasks and awaits their completion.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "registry" services/gateway/routing/ --include="*.py"
grep -rE "registry|exposes" services/gateway/routing/registry.py
test -d services/gateway/routing/ && echo "governed tree present"
```

**Accept when:**
- The registry exposes start and stop coroutines and is safe to stop before it has started
- Registration and deregistration are idempotent
- Stop cancels in-flight routing tasks and awaits their completion

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
