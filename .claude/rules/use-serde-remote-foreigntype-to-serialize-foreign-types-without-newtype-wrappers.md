---
glob: "**/*.rs"
---

<rule_activation adr-id="72226678-b630-4646-a769-5d02b7d7619c">
<!-- ADR: Use `#[serde(remote = "ForeignType")]` to Serialize Foreign Types Without Newtype Wrappers -->
</rule_activation>

- Use `#[serde(remote = "path::to::ForeignType")]` when adding serde support to foreign types where per-field attribute control is required
- Prefer `serde_with` `#[serde_as]` adapters over `remote` when the conversion matches a standard adapter such as `DisplayFromStr` or `DurationSeconds`
