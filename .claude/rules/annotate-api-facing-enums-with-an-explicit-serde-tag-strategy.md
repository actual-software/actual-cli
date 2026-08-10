---
glob: "**/*.rs"
---

<rule_activation adr-id="cb9f71d8-38b5-431a-95b1-ba9ec0bc5365">
<!-- ADR: Annotate API-Facing Enums with an Explicit serde Tag Strategy -->
</rule_activation>

- Always annotate enums that cross an API or persistence boundary with an explicit tag strategy: `#[serde(tag = "type")]`, `#[serde(tag = "t", content = "c")]`, or `#[serde(untagged)]`
- Never rely on serde's default external tagging for API-facing enums
