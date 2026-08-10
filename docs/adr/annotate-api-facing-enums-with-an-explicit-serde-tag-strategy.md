# Annotate API-Facing Enums with an Explicit serde Tag Strategy

Status: accepted
Date: 2026-03-31
Deciders: ADR Bank Curation

## Context

- serde's default enum serialization uses external tagging: `{"Circle": {"radius": 5}}`
- REST APIs and event systems almost always expect internally tagged (`{"type":"Circle","radius":5}`) or adjacently tagged formats
- The mismatch between serde defaults and API conventions is not caught at compile time
- Malformed payloads are produced silently, often discovered only at integration testing or in production
- serde provides explicit tag strategy attributes (`tag`, `tag`+`content`, `untagged`) to control serialization format

## Problem Statement

Relying on serde's default external tagging for API-facing enums produces payloads incompatible with standard API conventions, and this mismatch is silent at compile time. Without an explicit tag strategy, malformed payloads reach consumers undetected until runtime failures occur.

## Decision

1. MUST: Annotate enums that cross an API or persistence boundary with an explicit tag strategy: `#[serde(tag = "type")]`, `#[serde(tag = "t", content = "c")]`, or `#[serde(untagged)]`
2. MUST NOT: Rely on serde's default external tagging for API-facing enums
3. SHOULD: Prefer internally tagged (`#[serde(tag = "type")]`) for JSON APIs as the most common convention
4. SHOULD: Use adjacently tagged (`#[serde(tag = "kind", content = "data")]`) when tuple variants are needed
5. MAY: Use untagged (`#[serde(untagged)]`) only when variant shapes are unambiguous
6. SHOULD: Verify serialization format with a round-trip test for all API-facing enums

## Policy Block

- MUST annotate enums that cross an API or persistence boundary with an explicit serde tag strategy
- MUST NOT rely on serde's default external tagging for API-facing enums
- SHOULD prefer internally tagged enums (`#[serde(tag = "type")]`) for JSON APIs
- SHOULD use adjacently tagged enums when tuple variants are required
- MAY use `#[serde(untagged)]` only when variant shapes are unambiguous
- SHOULD include round-trip serialization tests for all API-facing enums

In scope:
- Rust enums derived with `Serialize` and/or `Deserialize` that appear in HTTP request/response bodies
- Enums serialized to event queues, message buses, or persistent storage
- Choice between internally tagged, adjacently tagged, and untagged strategies
- Variant compatibility constraints per tag strategy (e.g., tuple variants with internal tagging)

Out of scope:
- Internal-only enums that never leave the process boundary
- Enum serialization in non-serde formats (e.g., protobuf, flatbuffers)
- Struct field naming conventions (`rename_all`) which are covered separately
- Serde container-level attributes unrelated to tagging (e.g., `deny_unknown_fields`)

Exceptions:
- EXC-001: Internal enums used only within a single crate for in-memory serialization (e.g., caching) may use default tagging if no external consumer exists

## Rationale

- Explicit tag strategies make the wire format self-documenting in the type definition, reducing surprises during API integration
- Catching format mismatches at definition time prevents silent production failures from malformed payloads
- Internally tagged enums align with the dominant JSON API convention (`{"type": "...", ...}`), reducing friction with frontend and third-party consumers

## Consequences

Positive:
- API payloads match consumer expectations without post-hoc transformation layers
- New team members can read the enum definition and immediately understand the wire format
- Round-trip tests catch format regressions early in the development cycle

Negative:
- Developers must understand the constraints of each tag strategy (e.g., internally tagged enums cannot contain tuple variants), adding a learning curve
- Changing a tag strategy on a deployed API is a breaking change requiring versioning or migration

## Alternatives

- Rely on serde's default external tagging and transform payloads in middleware (rejected)
  Rejected because: Adds a runtime transformation layer that obscures the actual wire format and creates a maintenance burden
  When valid: When wrapping a third-party library's enums that cannot be annotated and the external format is acceptable to consumers

- Use `#[serde(untagged)]` everywhere to match arbitrary JSON shapes (rejected)
  Rejected because: Deserialization error messages are extremely poor ("data did not match any variant"), and ambiguous variant shapes cause silent misparses
  When valid: When consuming polymorphic external APIs where the discriminator field is absent and variant shapes are guaranteed to be non-overlapping

## Risks

- Choosing the wrong tag strategy for an API locks in a wire format that is costly to change after deployment
  Mitigation: Default to internally tagged for new APIs, add round-trip tests before release, and use API versioning for future changes
- `#[serde(untagged)]` enums silently deserialize to the wrong variant when shapes overlap
  Mitigation: Avoid `untagged` unless variant shapes are provably disjoint; add explicit deserialization tests for each variant