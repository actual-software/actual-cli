# Use `T: DeserializeOwned` in Generic Functions That Return Owned Values

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- Generic functions bounded with `T: Deserialize<'de>` tie the return type to the input buffer's lifetime, making it impossible to return `T` from the function without the caller holding the input in scope
- The resulting lifetime errors are difficult to diagnose and the correct bound (`DeserializeOwned`) is buried in serde's lifetimes documentation
- `DeserializeOwned` is equivalent to `for<'de> Deserialize<'de>` but is far more readable as a trait bound
- The explicit `T: Deserialize<'de>` form is only needed inside `impl<'de> Deserialize<'de>` blocks or for structs that genuinely borrow from the input buffer

## Problem Statement

Using `T: Deserialize<'de>` as a bound on generic functions that return owned values creates lifetime entanglement between the return type and the input buffer, producing confusing compiler errors that are difficult to resolve without knowledge of serde's `DeserializeOwned` alias.

## Decision

1. MUST: Bound generic deserialization functions with `T: DeserializeOwned` when the returned value does not borrow from the input buffer
2. SHOULD: Prefer the `DeserializeOwned` alias over `for<'de> Deserialize<'de>` for readability
3. SHOULD: Use the explicit `T: Deserialize<'de>` form only inside `impl<'de> Deserialize<'de>` blocks or for structs that genuinely borrow from the input buffer

## Policy Block

- MUST bound generic deserialization functions with `T: DeserializeOwned` when the returned value does not borrow from the input buffer
- SHOULD prefer the `DeserializeOwned` alias over the equivalent `for<'de> Deserialize<'de>` for readability
- SHOULD reserve `T: Deserialize<'de>` for custom `Deserialize` implementations or borrowing structs

In scope:
- Generic function signatures that deserialize and return owned data (`fn load<T>(...) -> Result<T>`)
- Choosing between `DeserializeOwned` and `Deserialize<'de>` trait bounds
- Import paths for `DeserializeOwned` (`serde::de::DeserializeOwned` or `serde::DeserializeOwned`)

Out of scope:
- Custom `Deserialize` trait implementations
- Zero-copy deserialization with borrowed data (covered by the `#[serde(borrow)]` ADR)
- Serialization trait bounds (`Serialize`)
- Non-serde deserialization frameworks

Exceptions:
- EXC-001: Functions that intentionally return borrowed data tied to an input buffer's lifetime should use `T: Deserialize<'de>` with an explicit lifetime parameter

## Rationale

- `DeserializeOwned` communicates intent clearly: the deserialized value owns all its data and has no lifetime ties to the input
- Eliminates an entire class of confusing lifetime errors that arise from using `Deserialize<'de>` in contexts where ownership transfer is intended
- The alias is a standard serde idiom, making code more recognizable to experienced Rust developers

## Consequences

Positive:
- Generic deserialization functions compile without lifetime entanglement issues
- Function signatures are clearer about ownership semantics of the returned type
- Reduces time spent debugging opaque lifetime errors in deserialization code

Negative:
- Precludes zero-copy deserialization in the bounded function, since `DeserializeOwned` requires the type to own all its data
- Developers must understand the distinction between `DeserializeOwned` and `Deserialize<'de>` to choose correctly

## Alternatives

- Use `T: Deserialize<'de>` everywhere and restructure callers to keep the input buffer in scope (rejected)
  Rejected because: Forces callers into awkward lifetime management, produces confusing errors, and is unnecessary when the returned value is owned
  When valid: When implementing custom deserializers or when zero-copy deserialization is explicitly needed for performance-critical paths

## Risks

- Developers may default to `DeserializeOwned` even in contexts where borrowing deserialization would provide meaningful performance benefits
  Mitigation: Document the zero-copy deserialization ADR alongside this one; use `DeserializeOwned` as the default and `Deserialize<'de>` only when profiling shows borrowing is beneficial