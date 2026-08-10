# Apply `#[serde(deny_unknown_fields)]` to Configuration and Strict-Contract Structs

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- serde silently drops unknown fields by default to support forward-compatible parsing.
- When applied to configuration structs, silent field dropping accepts misspelled keys without error.
- Misspelled configuration keys leave fields at their default values, producing bugs that are extremely difficult to diagnose.
- Internal message types sharing the same silent-drop behavior can mask contract violations between services.

## Problem Statement

Without `deny_unknown_fields`, configuration structs and strict internal contracts silently accept typos and extraneous fields, leading to bugs where fields quietly fall back to defaults with no error signal to the developer.

## Decision

1. MUST: Apply `#[serde(deny_unknown_fields)]` to all configuration structs and types that represent strict internal contracts.
2. MUST NOT: Combine `#[serde(deny_unknown_fields)]` with `#[serde(flatten)]` — they are incompatible and produce incorrect runtime behavior.
3. SHOULD: Define separate structs for forward-compatible external API responses and strict internal configurations when both behaviors are needed for the same data.
4. SHOULD: Test that an unknown field key in input returns a serde deserialization error rather than silently succeeding.

## Policy Block

- MUST apply `#[serde(deny_unknown_fields)]` to all configuration structs and strict internal contract types.
- MUST NOT combine `#[serde(deny_unknown_fields)]` with `#[serde(flatten)]` on the same struct.
- SHOULD define separate structs when a type needs both forward-compatible and strict parsing modes.
- SHOULD test that unknown fields produce deserialization errors.

In scope:
- serde `deny_unknown_fields` annotation on configuration and internal contract structs.
- Interaction between `deny_unknown_fields` and `flatten` attributes.
- Test coverage for unknown-field rejection behavior.

Out of scope:
- External API response types where forward-compatible parsing is intentional.
- serde serialization behavior (this ADR covers deserialization only).
- Non-serde configuration formats (e.g., clap CLI argument parsing).

Exceptions:
- EXC-001: Structs deserializing external third-party API responses should NOT use `deny_unknown_fields`, as upstream APIs may add fields at any time.

## Rationale

- Failing fast on unknown fields surfaces typos and contract mismatches immediately, rather than allowing them to propagate as silent default-value bugs.
- Separating strict and forward-compatible structs makes the parsing contract explicit in the type system.

## Consequences

Positive:
- Configuration typos are caught at deserialization time with a clear error message identifying the unknown field.
- Internal service contracts enforce schema conformance, preventing silent field additions from passing unnoticed.

Negative:
- Adding a new field to a configuration format requires updating all consumers simultaneously, reducing rollout flexibility.
- Developers must maintain separate struct definitions when both strict and lenient parsing is needed for similar data shapes.

## Alternatives

- Use `#[serde(default)]` on all fields and validate configuration values at application startup instead. (rejected)
  Rejected because: Defaults with validation catches missing or invalid values but still silently ignores misspelled keys — the core problem remains.
  When valid: When the configuration format is intentionally extensible and unknown keys are expected (e.g., plugin systems).

## Risks

- A deploy includes a new configuration key that older binary versions reject as unknown, causing rollback failures.
  Mitigation: Coordinate configuration schema changes with binary deployments; use versioned configuration schemas for rolling deployments.