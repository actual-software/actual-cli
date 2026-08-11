---
glob: "**/*.rs"
---

<rule_activation adr-id="9d22d261-7ec4-457e-aa69-6e36992dfb0a">
<!-- ADR: Annotate Optional Fields with `#[serde(skip_serializing_if = "Option::is_none")]` -->
</rule_activation>

- Add `#[serde(skip_serializing_if = "Option::is_none")]` to every `Option<T>` field unless the API contract explicitly requires `null` to represent an absent value
