---
glob: "**/*.rs"
---

<rule_activation adr-id="7f1866e8-f641-4830-a905-5e2015e85d5f">
<!-- ADR: Apply `#[serde(deny_unknown_fields)]` to Configuration and Strict-Contract Structs -->
</rule_activation>

- Apply `#[serde(deny_unknown_fields)]` to all configuration structs and types that represent strict internal contracts
- Never combine `#[serde(deny_unknown_fields)]` with `#[serde(flatten)]`—they are incompatible and produce incorrect runtime behavior
