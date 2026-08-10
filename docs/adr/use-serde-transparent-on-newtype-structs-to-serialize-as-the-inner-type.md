# Use `#[serde(transparent)]` on Newtype Structs to Serialize as the Inner Type

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- serde serializes `struct Wrapper(Inner)` as a single-field map `{"0": value}` by default rather than as the inner value directly
- This default behavior affects the most common newtype uses in Rust: ID types, validated strings, and measurement newtypes
- The broken output resembles a serde bug rather than a missing attribute, confusing developers
- `#[serde(transparent)]` instructs serde to serialize and deserialize the wrapper as if it were the inner type
- This applies to both single-field tuple structs and single-field named structs

## Problem Statement

Without `#[serde(transparent)]`, newtype structs serialize as single-field maps (`{"0": value}`) instead of as the inner value, producing unexpected and malformed output in APIs and persistence layers. Developers often misidentify this as a serde bug rather than a missing annotation.

## Decision

1. MUST: Add `#[serde(transparent)]` to all newtype structs (single-field tuple structs and single-field named structs) that should serialize and deserialize as their inner type
2. SHOULD: Apply to common newtype patterns: ID types, validated strings, measurement newtypes, and thin wrappers
3. SHOULD: Verify serialization output matches the inner type's format (e.g., `UserId("abc")` produces `"abc"`, not `{"0":"abc"}`)
4. MAY: Combine with other serde container attributes such as `rename_all` as needed

## Policy Block

- MUST add `#[serde(transparent)]` to all newtype structs that should serialize as their inner type
- SHOULD apply to ID types, validated strings, measurement newtypes, and thin wrappers
- SHOULD verify serialization produces the inner type's format via tests
- MAY combine `transparent` with other serde container attributes

In scope:
- Single-field tuple structs: `struct Wrapper(Inner)`
- Single-field named structs: `struct Wrapper { inner: Inner }`
- Common newtype patterns: `UserId(String)`, `Email(String)`, `Meters(f64)`
- Interaction with other serde attributes on the same struct

Out of scope:
- Multi-field structs (not newtypes)
- Enums with single-variant wrappers (use `#[serde(untagged)]` or tag strategies instead)
- Non-serde serialization of newtype patterns
- Validation logic within newtypes (covered by `try_from` ADR)

Exceptions:
- EXC-001: When the newtype intentionally needs a distinct serialization format from its inner type (e.g., a wrapper that adds metadata fields), omit `transparent`

## Rationale

- Newtype structs are idiomatic Rust for type safety, and their serialization should be invisible to API consumers
- The default `{"0": value}` output is never the desired behavior for newtypes, making `transparent` the correct default annotation
- Adding `transparent` at definition time prevents subtle serialization bugs that are difficult to trace in integration tests

## Consequences

Positive:
- API consumers see clean scalar values instead of unexpected single-field objects
- Newtype wrappers can be added for type safety without changing the wire format, enabling incremental refactoring
- Deserialization from the inner type's format works automatically, simplifying client-side code

Negative:
- Developers must remember to add `transparent` to every newtype, adding a small annotation burden
- If a newtype later gains a second field, `transparent` must be removed and the serialization format changes, which is a breaking API change

## Alternatives

- Implement custom `Serialize`/`Deserialize` for each newtype (rejected)
  Rejected because: Requires significantly more boilerplate for the same result that `#[serde(transparent)]` achieves in one line
  When valid: When the newtype needs custom serialization logic beyond transparent delegation (e.g., formatting a number as a string)

- Avoid newtypes and use type aliases instead (rejected)
  Rejected because: Type aliases provide no compile-time safety and cannot implement traits, defeating the purpose of the newtype pattern
  When valid: Never valid as a replacement for newtypes; aliases serve a different purpose (readability, not safety)

## Risks

- Forgetting `#[serde(transparent)]` on a newtype goes unnoticed until a consumer encounters the malformed `{"0": value}` output
  Mitigation: Add a clippy lint or CI check that flags newtype structs deriving `Serialize` without `#[serde(transparent)]`; include round-trip serialization tests for all public newtype structs