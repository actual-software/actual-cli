# Use `#[serde(remote = "ForeignType")]` to Serialize Foreign Types Without Newtype Wrappers

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- Rust's orphan rule prevents implementing `Serialize`/`Deserialize` on types defined in other crates
- Newtype wrappers are verbose, requiring `Deref`, `From`, and forwarding impls to remain ergonomic
- `serde(remote)` generates a serialization helper from a local mirror definition without introducing a wrapper type in the public API
- Per-field attribute control (e.g., `#[serde(rename)]`, `#[serde(default)]`) is needed when adapting foreign types for a specific wire format

## Problem Statement

When a project needs to serialize or deserialize a type from another crate, the orphan rule blocks direct trait impls. Without `serde(remote)`, developers resort to newtype wrappers that leak into the public API, require boilerplate forwarding impls, and obscure the actual data model.

## Decision

1. MUST: Use `#[serde(remote = "path::to::ForeignType")]` when adding serde support to foreign types where per-field attribute control is required
2. SHOULD: Prefer `serde_with` `#[serde_as]` adapters over `remote` when the conversion matches a standard adapter such as `DisplayFromStr` or `DurationSeconds`
3. MUST: Define a local struct mirroring the foreign type's fields exactly, including enum variants
4. MUST: Derive `Serialize` and `Deserialize` on the local mirror struct, not on the foreign type
5. MUST: Reference the mirror at the callsite using `#[serde(with = "ForeignTypeDef")]`

## Policy Block

- MUST use `#[serde(remote = "path::to::ForeignType")]` when adding serde support to foreign types where per-field attribute control is required
- SHOULD prefer `serde_with` `#[serde_as]` adapters over `remote` when the conversion matches a standard adapter such as `DisplayFromStr` or `DurationSeconds`
- MUST define a local struct mirroring the foreign type with all fields and variants replicated exactly
- MUST derive `Serialize` and `Deserialize` on the local mirror struct, not on the foreign type
- MUST reference the mirror at the callsite via `#[serde(with = "ForeignTypeDef")]`
- MUST mirror all enum variants including tuple and unit variants exactly

In scope:
- Rust structs and enums from external crates that need `Serialize`/`Deserialize`
- Per-field serde attribute customization on foreign types (rename, default, skip)
- Choosing between `serde(remote)` and `serde_with` for foreign type serialization

Out of scope:
- Serialization of types owned by the current crate (use normal `#[derive]`)
- Custom `Serialize`/`Deserialize` impls for complex transformation logic
- Network protocol or wire format design decisions

Exceptions:
- EXC-001: When the foreign type has private fields that cannot be mirrored, a newtype wrapper may be necessary instead of `serde(remote)`

## Rationale

- `serde(remote)` avoids polluting the public API with wrapper types that add complexity without semantic value
- The approach keeps serialization concerns isolated to the serde layer while preserving full attribute control per field
- Mirror definitions are checked at compile time — if the foreign type changes, the build breaks immediately

## Consequences

Positive:
- Eliminates boilerplate `Deref`, `From`, and forwarding impls required by newtype wrappers
- Keeps the public API clean — consumers work with the original foreign type directly
- Provides full per-field serde attribute control (`rename`, `default`, `skip`, etc.)

Negative:
- The local mirror struct must be kept in sync with the upstream type manually; upstream changes may cause compilation failures that require updating the mirror
- Developers unfamiliar with `serde(remote)` may find the pattern non-obvious compared to simple `#[derive]`

## Alternatives

- Newtype wrappers with manual `Serialize`/`Deserialize` impls (rejected)
  Rejected because: Requires `Deref`, `From`, and forwarding impls that add verbosity and leak wrapper types into the public API
  When valid: When the foreign type has private fields that prevent mirroring

- `serde_with` adapters (conditionally preferred)
  Rejected because: Not rejected — preferred when a matching standard adapter exists. Only use `remote` when per-field attribute control is needed beyond what adapters provide
  When valid: When the conversion matches a standard adapter like `DisplayFromStr` or `DurationSeconds`

## Risks

- Upstream crate changes field names, types, or adds new enum variants, breaking the local mirror at compile time
  Mitigation: Pin dependency versions and review changelogs before upgrading; compile-time breakage ensures the drift is caught immediately
