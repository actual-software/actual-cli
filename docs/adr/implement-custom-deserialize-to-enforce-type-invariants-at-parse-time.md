# Implement Custom `Deserialize` to Enforce Type Invariants at Parse Time

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- Deserializing into a plain type and validating afterwards means invalid data briefly exists as a valid Rust value
- Callers that omit the post-deserialization validation step silently propagate corrupt state through the application
- Embedding validation in the `Deserialize` implementation makes it impossible to construct an invalid instance from external data
- serde provides `#[serde(try_from = "T")]` as a convenient shortcut for types that implement `TryFrom<T>`
- Symmetrical serialization can be achieved with `#[serde(into = "T")]` and a corresponding `From` impl

## Problem Statement

Deserializing into types with invariants (non-empty strings, valid emails, bounded integers) without enforcing those invariants at parse time allows invalid instances to enter the system. Without this ADR, validation is optional and caller-dependent, leading to corrupt state propagation when any single callsite forgets to validate.

## Decision

1. MUST: Implement a custom `Deserialize` or use `#[serde(try_from = "PrimitiveType")]` for any newtype that enforces invariants such as non-empty strings, valid emails, or bounded integers
2. MUST NOT: Expose a public constructor for invariant-enforcing types that bypasses validation
3. SHOULD: Use `#[serde(try_from = "String")]` with `TryFrom<String>` as the preferred approach over manual `Deserialize` impls
4. SHOULD: Add `#[serde(into = "String")]` with a corresponding `From` impl for serialization symmetry
5. SHOULD: Test that invalid input returns a serde error rather than constructing an invalid value

## Policy Block

- MUST implement custom `Deserialize` or use `#[serde(try_from)]` for invariant-enforcing newtypes
- MUST NOT expose public constructors that bypass validation for invariant-enforcing types
- SHOULD prefer `#[serde(try_from)]` with `TryFrom` over manual `Deserialize` impls
- SHOULD implement `#[serde(into)]` for serialization symmetry
- SHOULD include tests verifying invalid input produces deserialization errors

In scope:
- Newtypes with validation invariants: `NonEmptyString`, `Email`, `BoundedInt<MIN, MAX>`, `PositiveAmount`
- `#[serde(try_from = "T")]` attribute usage and `TryFrom<T>` implementation
- `#[serde(into = "T")]` for round-trip serialization symmetry
- Manual `Deserialize` impl pattern: deserialize primitive, validate, construct
- Constructor visibility and validation bypass prevention

Out of scope:
- Validation of complex multi-field structs (use `#[serde(deserialize_with)]` or post-deserialization validation)
- Schema-level validation (JSON Schema, OpenAPI)
- Runtime validation frameworks (e.g., `validator` crate) that operate independently of serde
- `#[serde(transparent)]` usage for newtypes without invariants (covered by separate ADR)

Exceptions:
- EXC-001: Types used only internally where all construction paths are controlled and validated may skip custom `Deserialize` if the validation overhead is prohibitive and the type is not exposed to external data

## Rationale

- Parse-time validation follows the "parse, don't validate" principle, making invalid states unrepresentable from external data
- Centralizing validation in `Deserialize` or `TryFrom` ensures every deserialization path enforces invariants without relying on caller discipline
- serde's `try_from` attribute provides a low-boilerplate path that leverages existing `TryFrom` implementations

## Consequences

Positive:
- Invalid external data is rejected at the system boundary with clear error messages rather than silently corrupting state
- Downstream code can trust that invariant-enforcing types are always valid, eliminating redundant checks
- `TryFrom` implementations are reusable beyond serde (e.g., from user input, database rows, CLI arguments)

Negative:
- Each invariant-enforcing type requires a `TryFrom` impl and potentially a custom error type, increasing initial setup cost
- Deserialization errors from validation failures may be less informative than serde's default errors without careful error message design

## Alternatives

- Validate after deserialization in a separate step (rejected)
  Rejected because: Validation becomes optional and caller-dependent; any callsite that forgets validation allows invalid data to propagate
  When valid: For complex multi-field validations that depend on relationships between fields rather than individual field invariants

- Use the `validator` crate with `#[derive(Validate)]` for post-construction validation (rejected)
  Rejected because: Still requires callers to explicitly call `.validate()`, leaving the same optional-validation gap; does not prevent construction of invalid instances
  When valid: When validation rules are complex, dynamic, or need to produce structured error reports for API responses

## Risks

- Overly strict validation in `Deserialize` can make schema evolution difficult (e.g., a new valid email TLD is rejected)
  Mitigation: Keep validation rules focused on structural invariants (non-empty, bounded range) rather than business rules that may change; use feature flags or configuration for evolving validation criteria