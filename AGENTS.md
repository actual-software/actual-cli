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

## Adopt Modular Library Architecture for Test and Benchmark Infrastructure

1. Implement a modular library architecture where core functionality is organized into reusable library modules that can be consumed by benchmarks, tests, and test utilities. This decision establishes a clear separation between production code (src/), test utilities (src/testutil.rs), integration tests (tests/), and performance benchmarks (benches/), with each component importing and utilizing core library modules as needed. The architecture promotes code reuse by centralizing common test utilities in dedicated modules that can be shared across different testing contexts.

---

## Adopt Secure Secrets Management in CI/CD Pipeline

1. Implement a dedicated secrets management system integrated into the CI/CD pipeline that separates secret storage from code. This includes using environment variables, encrypted secret stores, or dedicated secrets management services (such as HashiCorp Vault, AWS Secrets Manager, or CI platform-native solutions) to inject credentials at runtime rather than hardcoding them in configuration files or source code. The pattern is consistently applied across CLI commands and testing infrastructure to ensure uniform security practices.