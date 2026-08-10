---
glob: "**/*.rs"
---

<rule_activation adr-id="fc39f638-f694-4e2a-b271-2a3c94ea95e5">
<!-- ADR: Use `#[serde(transparent)]` on Newtype Structs to Serialize as the Inner Type -->
</rule_activation>

- Add `#[serde(transparent)]` to all newtype structs (single-field tuple structs and single-field named structs) that should serialize and deserialize as their inner type
