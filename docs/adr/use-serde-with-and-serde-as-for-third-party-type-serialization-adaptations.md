# Use `serde_with` and `#[serde_as]` for Third-Party Type Serialization Adaptations

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- Serializing types like `std::time::Duration`, `uuid::Uuid`, or `chrono::DateTime` often requires verbose custom `Serialize`/`Deserialize` implementations
- `#[serde(remote)]` mirrors are fragile and must be manually kept in sync with upstream type definitions
- `serde_with` provides well-tested, community-maintained adapters that express common conversions declaratively via a single field annotation
- Common conversions (duration-as-seconds, bytes-as-base64, display-as-string) are repeated across projects and benefit from standardization

## Problem Statement

Without a declarative adapter library, every project reinvents serialization logic for common third-party types. This leads to inconsistent formats, untested edge cases, and verbose boilerplate that obscures the actual data model.

## Decision

1. MUST: Use `serde_with` with `#[serde_as]` for field-level format conversions such as duration-as-seconds, bytes-as-base64, and display-as-string
2. SHOULD: Use `#[serde(remote)]` only when `serde_with` lacks a matching adapter and full per-field attribute control is required
3. MUST: Add `serde_with` as a dependency with the appropriate feature flags for the types being adapted
4. MUST: Annotate structs with both `#[serde_as]` and `#[derive(Serialize, Deserialize)]`

## Policy Block

- MUST use `serde_with` with `#[serde_as]` for field-level format conversions such as duration-as-seconds, bytes-as-base64, and display-as-string
- SHOULD use `#[serde(remote)]` only when `serde_with` lacks a matching adapter and full per-field attribute control is required
- MUST add `serde_with` dependency with appropriate feature flags (`base64`, `chrono`, `uuid`, etc.)
- MUST annotate structs with `#[serde_as]` alongside `#[derive(Serialize, Deserialize)]`
- SHOULD use `Option<AdapterType>` wrapping for optional fields with adapters

In scope:
- Field-level serialization format conversions for `Duration`, `DateTime`, `Uuid`, byte arrays, and `Display`-implementing types
- Choosing between `serde_with` adapters and `#[serde(remote)]` mirrors
- Feature flag selection for the `serde_with` crate

Out of scope:
- Custom serialization logic that does not map to a standard `serde_with` adapter
- Struct-level serialization strategies (e.g., `#[serde(tag)]`, `#[serde(untagged)]`)
- Wire format or protocol design decisions

Exceptions:
- EXC-001: When `serde_with` does not provide an adapter for a specific conversion, fall back to `#[serde(remote)]` or a manual impl with a justification comment

## Rationale

- `serde_with` adapters are well-tested by the community, reducing the risk of serialization bugs in edge cases (e.g., negative durations, timezone handling)
- Declarative annotations make the serialization format visible at the field definition site, improving readability and reviewability
- Centralizing common conversions behind a single crate eliminates inconsistent ad-hoc implementations across the codebase

## Consequences

Positive:
- Reduces boilerplate for common serialization patterns to a single annotation per field
- Ensures consistent serialization formats across the codebase for standard types
- Leverages community-maintained and tested conversion logic

Negative:
- Adds a compile-time dependency on the `serde_with` crate and its feature flags, increasing the dependency tree
- Developers must learn the `serde_with` adapter vocabulary and `#[serde_as]` macro syntax

## Alternatives

- Manual `Serialize`/`Deserialize` impls per field or type (rejected)
  Rejected because: Verbose, error-prone, and inconsistent across the codebase; each impl must handle edge cases that `serde_with` already covers
  When valid: When the conversion logic is highly custom and no standard adapter exists

- `#[serde(remote)]` for all third-party types (rejected)
  Rejected because: Requires maintaining mirror structs that must stay in sync with upstream types; more boilerplate than a one-line adapter annotation
  When valid: When `serde_with` lacks a matching adapter and per-field serde attributes are needed

## Risks

- A `serde_with` major version upgrade changes adapter behavior or API, requiring migration across all annotated fields
  Mitigation: Pin to a major version range (`"3"`) and review changelogs before upgrading; adapter behavior is well-documented and stable
