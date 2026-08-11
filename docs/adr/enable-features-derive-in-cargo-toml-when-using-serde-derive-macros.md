# Enable `features = ["derive"]` in Cargo.toml When Using serde Derive Macros

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- serde derive macros (`#[derive(Serialize, Deserialize)]`) are gated behind an opt-in Cargo feature flag
- Declaring `serde = "1"` without `features = ["derive"]` silently omits the macros
- The resulting compile error ("cannot find derive macro `Serialize`") is misleading and does not mention the missing feature flag
- Application crates and library crates have different best practices for declaring the serde dependency
- Library crates should make serde optional to avoid forcing the dependency on downstream users

## Problem Statement

Omitting `features = ["derive"]` from the serde dependency produces a confusing compile error that wastes developer time diagnosing a missing feature flag. Without this ADR, new crates repeatedly hit this pitfall, and library crates risk forcing an unnecessary serde dependency on all downstream consumers.

## Decision

1. MUST: Declare `serde = { version = "1", features = ["derive"] }` in every application crate that uses `#[derive(Serialize, Deserialize)]`
2. SHOULD: In library crates, declare serde as `optional = true` with the derive feature and gate all derives behind `#[cfg(feature = "serde")]`
3. SHOULD: Use `#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]` in library crate types
4. SHOULD: Define a `serde` feature in the library's `[features]` section using `serde = ["dep:serde"]`

## Policy Block

- MUST declare `serde = { version = "1", features = ["derive"] }` in application crates using serde derive macros
- SHOULD declare serde as `optional = true` with derive feature in library crates
- SHOULD gate serde derives behind `#[cfg(feature = "serde")]` or `#[cfg_attr(...)]` in library crates
- SHOULD define a `serde` feature in library `[features]` sections using `dep:serde` syntax

In scope:
- `Cargo.toml` dependency declarations for serde in application and library crates
- Feature flag configuration for optional serde support in libraries
- `cfg_attr` patterns for conditional derive macro application
- Cargo feature section declarations using `dep:` syntax

Out of scope:
- serde serialization format selection (JSON, TOML, bincode, etc.)
- serde attribute usage on structs and enums (covered by other ADRs)
- Non-serde serialization frameworks
- Workspace-level dependency management (`[workspace.dependencies]`)

Exceptions:
- EXC-001: Internal-only library crates that are never published and always consumed with serde may declare it as non-optional for simplicity

## Rationale

- Explicitly enabling `features = ["derive"]` prevents a common and misleading compile error that wastes developer time
- Making serde optional in library crates follows Rust ecosystem conventions and keeps dependency trees lean for consumers who don't need serialization
- Using `dep:serde` syntax in features prevents implicit feature activation from crate name collision

## Consequences

Positive:
- New crate setup is frictionless: the correct dependency declaration is documented and copy-pasteable
- Library consumers only pay for serde when they opt in, reducing compile times and binary size for non-serde users
- Compile errors become actionable rather than misleading

Negative:
- Library authors must maintain `cfg_attr` annotations on all public types, adding boilerplate
- Testing library crates requires running tests with and without the `serde` feature to ensure both paths compile

## Alternatives

- Use `serde_derive` as a separate dependency instead of the `derive` feature (rejected)
  Rejected because: The separate `serde_derive` crate is the legacy approach; the `derive` feature is the modern, recommended method and avoids version mismatches between `serde` and `serde_derive`
  When valid: When pinned to an older serde version that predates the `derive` feature flag

- Always declare serde as non-optional in library crates (rejected)
  Rejected because: Forces all downstream consumers to compile serde even when they don't use serialization, violating Rust ecosystem norms for library design
  When valid: When the library's sole purpose is serialization/deserialization and serde is integral to its API

## Risks

- Forgetting `optional = true` in a library crate silently forces serde on all consumers without a compile error
  Mitigation: Add a CI check or `cargo-deny` rule that flags non-optional serde dependencies in library crates