---

## Architecture Decision Records

<adr_governance source="docs/adr/">
ADRs govern validated architectural standards for this project.
Full ADR documents: @docs/adr/
</adr_governance>

<activation>
These directives are ALWAYS ACTIVE. All AI coding agents MUST apply all rules in this
document to every code generation, modification, and review action within this
project. No exceptions unless explicitly noted per-rule.
</activation>

---

### Verification Protocol

<verification_protocol>
All rules in this document follow the **Verify → Fix → Repeat** loop.
</verification_protocol>

After generating or modifying code for any rule, the agent MUST:

1. **RUN** the targeted verification command(s) in the rule's **Verify** block.
2. **CAPTURE** the full command output (stdout + stderr).
3. **EVALUATE** whether the **Accept when** criteria are satisfied.
4. **IF FAILING:** diagnose the root cause, apply a fix, and re-run from step 1.
5. **IF PASSING:** include the passing output as inline evidence before proposing further changes.
6. **MAX ITERATIONS:** 5 attempts per rule. If still failing after 5 attempts, STOP and report the failure with all captured outputs.

<enforcement>
Compliance is not optional. Agents must not skip verification steps, assume
correctness, or defer verification to a later task. Evidence of a passing
verification run must accompany every code change that touches a governed area.
</enforcement>

---

## Adopt Comprehensive Unit Testing Strategy for CI/CD Pipeline Components

1. Implement comprehensive unit testing for all CI/CD pipeline components and build tooling modules. Each module in the build, delivery, and tooling infrastructure must include unit tests that validate core functionality in isolation. This includes testing static analysis components (manifests, languages, registry), CLI command handlers (sync, auth, status), UI interaction components (confirm dialogs, file confirmations), generation and tailoring logic, signal processing (language resolver, tree-sitter, IR), API utilities (retry mechanisms), and configuration types. Unit tests should be co-located with source code or in parallel test directories, following Rust's standard testing conventions with #[cfg(test)] modules or separate test files.

---