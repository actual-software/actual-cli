# Use `#[serde(default)]` for Absent Fields With Meaningful Defaults Instead of `Option<T>`

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- Developers commonly reach for `Option<T>` whenever a field might be absent during deserialization
- Many fields have meaningful zero values such as `false`, `0`, or an empty vec that make `Option` unnecessary
- Wrapping fields with meaningful defaults in `Option` forces every callsite to unwrap or match on `None`
- This pattern leaks deserialization concerns into the domain model, coupling business logic to serialization format details
- serde provides `#[serde(default)]` specifically for fields that should fall back to a known value when absent

## Problem Statement

Using `Option<T>` for fields that have meaningful zero-value defaults pollutes the domain model with deserialization concerns and forces unnecessary unwrapping at every callsite. Without this ADR, developers default to `Option<T>` out of habit, producing verbose and fragile code where `None` carries no real semantic meaning.

## Decision

1. MUST: Use `#[serde(default)]` on fields that have a meaningful zero-value default and should never be `None` in application logic
2. MUST: Reserve `Option<T>` for fields where `None` carries semantic meaning distinct from any default value
3. SHOULD: Prefer field-level `#[serde(default)]` over struct-level for explicitness
4. SHOULD: Use custom default functions via `#[serde(default = "fn_name")]` when the zero value of the type is not the desired default
5. SHOULD: Verify that deserializing an empty JSON object `{}` produces the expected default-filled struct

## Policy Block

- MUST use `#[serde(default)]` on fields that have a meaningful zero-value default and should never be `None` in application logic
- MUST reserve `Option<T>` for fields where `None` carries semantic meaning distinct from any default value
- SHOULD prefer field-level `#[serde(default)]` over struct-level `#[serde(default)]` for explicitness
- SHOULD use custom default functions via `#[serde(default = "fn_name")]` when the type's `Default` impl does not produce the desired value
- SHOULD verify deserialization of empty input produces the expected default-filled struct

In scope:
- Rust struct fields using serde `Serialize`/`Deserialize` derives
- Fields with meaningful zero values: booleans, numeric counters, empty collections
- Custom default functions for non-trivial defaults (e.g., `fn default_timeout() -> u64 { 5000 }`)
- Struct-level vs field-level `#[serde(default)]` guidance

Out of scope:
- Serde enum deserialization strategies (covered by separate ADR)
- Non-serde deserialization frameworks (e.g., manual JSON parsing)
- Database ORM field defaults or migration-level column defaults
- Runtime configuration loading that does not use serde

Exceptions:
- EXC-001: When a field's absence must be distinguishable from its default value for business logic (e.g., a missing `timeout` means "use server default" while `0` means "no timeout"), `Option<T>` is correct even if the field has a zero value

## Rationale

- Eliminating unnecessary `Option` wrappers reduces boilerplate unwrapping across all callsites, making code shorter and less error-prone
- Domain models stay clean: fields reflect actual business semantics rather than serialization quirks
- serde's `#[serde(default)]` is a purpose-built mechanism for this exact scenario, providing compile-time guarantees and clear intent

## Consequences

Positive:
- Callsites no longer need to unwrap or provide fallback values for fields that always have a sensible default
- Domain types accurately reflect invariants: if a field is never `None` in practice, the type system enforces it
- Deserialization of partial payloads (e.g., config files, API versioning) becomes seamless and predictable

Negative:
- Developers must explicitly reason about whether `None` carries semantic meaning for each field, adding a small design cost
- Changing a field from `#[serde(default)]` to `Option<T>` (or vice versa) is a breaking serialization change that requires migration consideration

## Alternatives

- Always use `Option<T>` and unwrap with `.unwrap_or_default()` at callsites (rejected)
  Rejected because: Pushes the default-value decision to every consumer rather than declaring it once at the type level, leading to inconsistent defaults and noisy code
  When valid: When different callsites genuinely need different fallback values for the same absent field

- Use struct-level `#[serde(default)]` on all structs (rejected)
  Rejected because: Applies defaults indiscriminately, masking fields where `None` has genuine semantic meaning and hiding the developer's intent per field
  When valid: Small, internal-only configuration structs where every field has a sensible `Default` impl

## Risks

- A field's `Default` impl may silently change (e.g., a library update), altering deserialization behavior without any compile-time warning
  Mitigation: Use explicit custom default functions for critical fields rather than relying on `T::default()`, and add deserialization round-trip tests for key structs