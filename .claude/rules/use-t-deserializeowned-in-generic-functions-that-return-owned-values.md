---
glob: "**/*.rs"
---

<rule_activation adr-id="fd3d52a0-e094-4be0-9ce7-15d6e5bec286">
<!-- ADR: Use `T: DeserializeOwned` in Generic Functions That Return Owned Values -->
</rule_activation>

- Bound generic deserialization functions with `T: DeserializeOwned` when the returned value does not borrow from the input buffer
