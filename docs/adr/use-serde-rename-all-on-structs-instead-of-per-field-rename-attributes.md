# Use `#[serde(rename_all)]` on Structs Instead of Per-Field `rename` Attributes

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- REST APIs and JSON schemas commonly use naming conventions (camelCase, snake_case, kebab-case) that differ from Rust's snake_case field naming
- Applying `#[serde(rename = "...")]` on every field is tedious and inconsistent across a codebase
- When new fields are added to a struct without the per-field rename annotation, the serialized output silently uses Rust's naming convention, breaking API contracts
- serde provides `#[serde(rename_all = "...")]` at the struct/enum level to apply a consistent naming transform to all fields

## Problem Statement

Per-field `#[serde(rename)]` annotations are error-prone and fail silently when new fields are added without the annotation, leading to inconsistent serialized key naming that breaks API contracts.

## Decision

1. MUST: Apply `#[serde(rename_all = "camelCase")]` or equivalent at the struct or enum level when all fields share a naming convention
2. SHOULD: Use per-field `#[serde(rename = "...")]` only for individual exceptions to the struct-level rule
3. SHOULD: For enums, apply `rename_all` to the enum (variant names) and to embedded struct variants (field names) separately
4. SHOULD: Verify output format with a unit test asserting the serialized string contains the expected key casing

## Policy Block

- MUST apply `#[serde(rename_all = "...")]` at the struct or enum level when all fields share a naming convention
- SHOULD use per-field `#[serde(rename = "...")]` only for individual exceptions to the struct-level rule
- SHOULD apply `rename_all` separately to enum variant names and embedded struct variant field names
- SHOULD verify serialized key casing with unit tests

In scope:
- All `#[derive(Serialize)]` and `#[derive(Deserialize)]` structs and enums interfacing with external JSON APIs
- Naming convention alignment between Rust field names and JSON/API key naming
- Enum variant renaming for tagged, untagged, and adjacently tagged representations

Out of scope:
- Custom `Serialize`/`Deserialize` implementations that handle naming manually
- Non-JSON formats where field naming conventions differ (e.g., XML attributes)
- Internal-only structs that are never serialized to external consumers

Exceptions:
- EXC-001: Structs where a majority of fields require unique renames that don't follow any single convention may use per-field `rename` throughout

## Rationale

- A struct-level `rename_all` guarantees that every new field automatically follows the naming convention without developer intervention
- Reduces annotation noise: one attribute on the struct replaces N attributes on individual fields
- Prevents silent API contract breakage when fields are added without remembering to annotate them

## Consequences

Positive:
- New fields automatically adopt the correct naming convention without any additional annotation
- Struct definitions are cleaner and less cluttered with repetitive attributes
- API contract consistency is enforced structurally rather than by developer discipline

Negative:
- Fields that genuinely need a different name from the convention require an additional per-field `rename` on top of the struct-level `rename_all`, which can be confusing to newcomers

## Alternatives

- Apply `#[serde(rename = "...")]` individually to every field (rejected)
  Rejected because: Tedious, error-prone on new field additions, and produces noisy struct definitions
  When valid: When the struct has fewer than 3 fields and no convention applies, or when every field name is an irregular exception

## Risks

- Applying `rename_all` to a struct that interfaces with an API using inconsistent key naming (mix of camelCase and snake_case) produces incorrect serialization for some fields
  Mitigation: Use `rename_all` for the majority convention and per-field `rename` for exceptions; add serialization round-trip tests for each struct