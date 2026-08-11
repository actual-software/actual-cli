# Deserialize into Typed Structs Instead of `serde_json::Value`

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- `serde_json::Value` allocates every string on the heap and erases all type information at the Rust level
- Accessing fields requires `Option`-chained `.get()` calls, which are verbose and error-prone
- Structural mismatches such as missing required fields or wrong value types only surface at runtime, not at compile time
- Typed structs with `#[derive(Deserialize)]` catch schema violations at deserialization time and provide compile-time field access guarantees
- Deserialization into typed structs is typically 2–5× faster than into `Value` for deeply nested JSON

## Problem Statement

Using `serde_json::Value` as the deserialization target sacrifices compile-time type safety, incurs unnecessary heap allocations, and defers structural validation to runtime, leading to fragile code that silently breaks when upstream JSON schemas change.

## Decision

1. MUST: Define typed structs for all JSON shapes parsed more than once or stored beyond a single function call
2. MUST: Replace `value["field"].as_str().unwrap()` callsites with `#[derive(Deserialize)]` struct fields
3. SHOULD: For partially-known schemas, capture unknown fields with `#[serde(flatten)] pub extra: HashMap<String, serde_json::Value>`
4. SHOULD: For heterogeneous arrays, define an enum with `#[serde(untagged)]` rather than `Vec<serde_json::Value>`
5. MAY: Use `serde_json::Value` for genuinely schema-less data such as arbitrary user-supplied JSON blobs

## Policy Block

- MUST define typed structs for all JSON shapes parsed more than once or stored beyond a single function call
- MUST replace `value["field"].as_str().unwrap()` access patterns with typed struct fields
- SHOULD capture unknown fields with `#[serde(flatten)] pub extra: HashMap<String, serde_json::Value>` for partially-known schemas
- SHOULD define an enum with `#[serde(untagged)]` for heterogeneous arrays rather than `Vec<serde_json::Value>`
- MAY use `serde_json::Value` only for genuinely schema-less data such as arbitrary user-supplied JSON blobs

In scope:
- All JSON deserialization via serde_json in application code
- Struct design for API response types, configuration files, and inter-service message formats
- Handling partially-known or extensible JSON schemas
- Heterogeneous JSON array deserialization

Out of scope:
- JSON serialization (output formatting)
- Non-JSON serde formats (TOML, YAML, MessagePack)
- One-off debugging or REPL-style JSON inspection
- JSON schema generation or OpenAPI spec tooling

Exceptions:
- EXC-001: `serde_json::Value` is acceptable for genuinely schema-less data where no struct can be defined (e.g., user-provided arbitrary JSON stored as-is)
- EXC-002: Exploratory or prototype code that parses JSON once in a throwaway script

## Rationale

- Typed deserialization catches schema violations at parse time rather than at field-access time, shifting errors left
- Compile-time field access eliminates an entire class of typo-driven runtime bugs (e.g., `value["feild"]`)
- `serde_json::from_str::<MyStruct>` is typically 2–5× faster than `from_str::<Value>` for deeply nested JSON due to fewer heap allocations

## Consequences

Positive:
- Missing or mis-typed fields are caught at deserialization time with clear error messages
- Code is self-documenting: struct definitions serve as schema documentation
- Improved performance from reduced heap allocations and avoided dynamic lookups

Negative:
- Requires upfront effort to define and maintain struct definitions that mirror external JSON schemas
- Schema changes in upstream APIs require corresponding struct updates, adding a maintenance burden

## Alternatives

- Use `serde_json::Value` everywhere and validate manually at access sites (rejected)
  Rejected because: Duplicates validation logic across every access point, is error-prone, and provides no compile-time guarantees
  When valid: For genuinely schema-less data or one-off inspection scripts where defining a struct provides no reuse benefit

## Risks

- Upstream API schema changes silently add fields that are ignored by the typed struct, potentially missing important data
  Mitigation: Use `#[serde(deny_unknown_fields)]` in strict mode or `#[serde(flatten)]` to capture extras; add integration tests that deserialize real API responses