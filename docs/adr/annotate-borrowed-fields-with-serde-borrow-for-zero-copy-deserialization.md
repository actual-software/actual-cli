# Annotate Borrowed Fields with `#[serde(borrow)]` for Zero-Copy Deserialization

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- serde supports zero-copy deserialization by borrowing `&str` or `&[u8]` directly from the input buffer, avoiding heap allocations for string data
- Without `#[serde(borrow)]`, the compiler cannot infer the lifetime relationship between the input buffer and the deserialized struct
- The resulting lifetime errors are confusing and obscure the actual one-line fix (`#[serde(borrow)]`)
- Developers often reach for owned types unnecessarily when the real issue is a missing annotation

## Problem Statement

Without `#[serde(borrow)]` on borrowed fields, Rust's compiler produces opaque lifetime errors during zero-copy deserialization, leading developers to either abandon zero-copy entirely or spend significant time debugging what is ultimately a one-line annotation fix.

## Decision

1. MUST: Add `#[serde(borrow)]` to every field typed as `&'de str`, `&'de [u8]`, or any type containing a borrowed lifetime
2. MUST: Use owned `String` or `Vec<u8>` for fields in structs that outlive the parsing call site
3. MUST: Use `serde_json::from_slice` or `serde_json::from_str` (not `from_reader`) so the input buffer remains in scope for the struct's lifetime
4. SHOULD: Switch to owned types for values stored in async tasks, returned across await points, or placed in long-lived data structures

## Policy Block

- MUST add `#[serde(borrow)]` to every field typed as `&'de str`, `&'de [u8]`, or any type containing a borrowed lifetime
- MUST use owned `String` or `Vec<u8>` for fields in structs that outlive the parsing call site
- MUST use `serde_json::from_slice` or `serde_json::from_str` (not `from_reader`) when deserializing into borrowing structs
- SHOULD switch to owned types for values stored in async tasks, returned across await points, or placed in long-lived data structures

In scope:
- Structs deriving `Deserialize` with lifetime parameters (`<'de>`)
- Fields borrowing `&str` or `&[u8]` from the input buffer
- Choosing between borrowed and owned field types based on struct lifetime requirements
- serde zero-copy deserialization with `from_slice` and `from_str`

Out of scope:
- Custom `Deserialize` implementations (manual `impl<'de> Deserialize<'de>`)
- Non-serde deserialization frameworks
- Serialization-side concerns (`Serialize` derive)
- Binary deserialization formats (bincode, postcard) which have their own borrowing rules

Exceptions:
- EXC-001: Structs used exclusively in benchmarks or hot paths where zero-copy is measured as unnecessary may omit `#[serde(borrow)]` and use owned types for simplicity

## Rationale

- Zero-copy deserialization significantly reduces heap allocations and improves throughput for large payloads, but only works when the lifetime relationship is explicitly annotated
- The `#[serde(borrow)]` annotation is a single-line fix that eliminates an entire class of confusing lifetime compilation errors
- Making the borrowed-vs-owned decision explicit at the struct level communicates intent about data ownership to future readers

## Consequences

Positive:
- Developers can leverage zero-copy deserialization without fighting opaque lifetime errors
- Reduced heap allocations for read-heavy parsing workloads improve performance
- Clear ownership semantics at the struct level make code easier to reason about

Negative:
- Developers must reason about whether the deserialized struct outlives the input buffer, adding cognitive overhead to struct design
- Using `from_slice`/`from_str` instead of `from_reader` requires the entire input to be in memory, which may increase memory usage for streaming scenarios

## Alternatives

- Always use owned types (`String`, `Vec<u8>`) and avoid zero-copy entirely (rejected)
  Rejected because: Sacrifices significant performance gains for parsing-heavy workloads and ignores a core serde capability
  When valid: When structs are long-lived, passed across async boundaries, or when the performance difference is unmeasurable for the use case

## Risks

- Borrowing structs tied to input buffer lifetime can cause borrow-checker issues if the struct is accidentally stored beyond the buffer's scope
  Mitigation: Default to owned types unless profiling shows zero-copy is beneficial; document lifetime requirements in struct-level comments