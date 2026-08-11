---
glob: "**/*.rs"
---

<rule_activation adr-id="2c68de4d-24b8-4c0d-b926-f9dd98f39647">
<!-- ADR: Use `serde_with` and `#[serde_as]` for Third-Party Type Serialization Adaptations -->
</rule_activation>

- Use `serde_with` with `#[serde_as]` for field-level format conversions such as duration-as-seconds, bytes-as-base64, and display-as-string
- Use `#[serde(remote)]` only when `serde_with` lacks a matching adapter and full per-field attribute control is required
