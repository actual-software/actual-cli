---
glob: "**/*.rs"
---

<rule_activation adr-id="cf3bce7c-83b4-456d-b4d2-4dd01e8fc9ee">
<!-- ADR: Implement Custom `Deserialize` to Enforce Type Invariants at Parse Time -->
</rule_activation>

- Implement a custom `Deserialize` or use `#[serde(try_from)]` for any newtype that enforces invariants such as non-empty strings, valid emails, or bounded integers
- Never expose a public constructor for invariant-enforcing types that bypasses validation
