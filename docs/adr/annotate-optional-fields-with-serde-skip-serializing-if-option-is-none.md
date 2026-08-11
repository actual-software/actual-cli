# Annotate Optional Fields with `#[serde(skip_serializing_if = "Option::is_none")]`

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- serde serializes `Option::None` as JSON `null` by default.
- Most REST APIs and JSON protocols treat a missing key differently from an explicit `null`.
- Emitting `null` for every absent optional field violates typical API contracts.
- Clients that test for key presence (e.g., `if "field" in response`) break when `null` is emitted instead of key omission.
- Unnecessary `null` fields inflate payload size across all responses.

## Problem Statement

Default serde serialization of `Option::None` as `null` violates the common API convention that absent keys and explicit `null` have different semantics, breaking clients that rely on key-presence checks and inflating payload sizes.

## Decision

1. MUST: Add `#[serde(skip_serializing_if = "Option::is_none")]` to every `Option<T>` field unless the API contract explicitly requires `null` to represent an absent value.
2. SHOULD: Use `#[serde(skip_serializing_if = "Vec::is_empty")]` for empty collections that should be omitted.
3. SHOULD: Document fields where `null` is intentional with a comment explaining why absence and `null` must be distinguished.
4. MAY: Use `#[serde(skip_serializing_if = "is_default")]` with a custom helper for non-Option default-valued fields.

## Policy Block

- MUST add `#[serde(skip_serializing_if = "Option::is_none")]` to every `Option<T>` field unless the API contract explicitly requires `null`.
- SHOULD use `#[serde(skip_serializing_if = "Vec::is_empty")]` for empty collections that should be omitted.
- SHOULD document fields where `null` is intentional with a comment explaining the semantic distinction.
- MAY use `#[serde(skip_serializing_if = "is_default")]` with a custom helper for non-Option default-valued fields.

In scope:
- serde serialization behavior for `Option<T>` fields on API response and message structs.
- `skip_serializing_if` annotation patterns for `Option`, `Vec`, and default-valued fields.
- Documentation requirements for intentional `null` semantics.

Out of scope:
- Deserialization behavior for missing vs. `null` fields (handled by `#[serde(default)]`).
- Non-JSON serialization formats where `null` vs. absent has different semantics.
- Database column nullability decisions.

Exceptions:
- EXC-001: Fields where the API contract explicitly distinguishes between "not provided" (key absent) and "explicitly cleared" (`null`) must emit `null` and should include a comment documenting this requirement.

## Rationale

- Omitting `None` fields aligns serialization output with the dominant REST API convention where missing keys mean "not applicable" and `null` means "explicitly set to nothing."
- Smaller payloads reduce bandwidth consumption and parsing time for clients, especially on high-volume endpoints.
- Explicit documentation of intentional `null` fields forces developers to consciously decide the semantic meaning.

## Consequences

Positive:
- API responses conform to client expectations around key presence vs. `null`, reducing integration bugs.
- Payload sizes shrink for responses with many optional fields, improving network efficiency.
- Intentional `null` usage is documented and reviewable, making API semantics explicit.

Negative:
- Every `Option<T>` field requires the `skip_serializing_if` annotation, adding per-field boilerplate.
- Forgetting the annotation on a new field silently emits `null`, and the mistake is only caught by API contract tests or client complaints.

## Alternatives

- Use a custom serializer at the struct level that strips all `null` values automatically. (rejected)
  Rejected because: A blanket approach cannot accommodate fields where `null` is semantically meaningful, forcing workarounds that are harder to reason about than per-field annotations.
  When valid: Internal-only message types where `null` vs. absent is never semantically distinguished.

## Risks

- A field that previously emitted `null` is annotated with `skip_serializing_if`, changing the API contract for existing clients.
  Mitigation: Treat addition of `skip_serializing_if` to existing public API fields as a breaking change; gate behind API versioning or coordinate with consumers.