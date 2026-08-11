---
glob: "**/*.rs"
---

<rule_activation adr-id="e3001676-c9cb-4858-ade8-df0d479a7d9c">
<!-- ADR: Use `#[serde(default)]` for Absent Fields With Meaningful Defaults Instead of `Option<T>` -->
</rule_activation>

- Use `#[serde(default)]` on fields that have a meaningful zero-value default and should never be `None` in application logic
- Reserve `Option<T>` for fields where `None` carries semantic meaning distinct from any default value
