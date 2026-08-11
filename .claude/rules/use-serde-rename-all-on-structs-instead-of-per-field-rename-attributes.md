---
glob: "**/*.rs"
---

<rule_activation adr-id="3d95c725-845b-4ba4-a2c7-508b9fc4687d">
<!-- ADR: Use `#[serde(rename_all)]` on Structs Instead of Per-Field `rename` Attributes -->
</rule_activation>

- Apply `#[serde(rename_all = "camelCase")]` or equivalent at the struct or enum level when all fields share a naming convention
- Use per-field `#[serde(rename = "...")]` only for individual exceptions to the struct-level rule
