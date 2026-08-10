---
glob: "**/*.rs"
---

<rule_activation adr-id="50fbe4a9-665e-45d8-bd35-b20f48014820">
<!-- ADR: Deserialize into Typed Structs Instead of `serde_json::Value` -->
</rule_activation>

- Define typed structs for all JSON shapes parsed more than once or stored beyond a single function call
- Use `serde_json::Value` only for genuinely schema-less data such as arbitrary user-supplied JSON blobs
