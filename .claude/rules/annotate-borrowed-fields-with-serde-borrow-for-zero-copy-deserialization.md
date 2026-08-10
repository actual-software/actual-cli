---
glob: "**/*.rs"
---

<rule_activation adr-id="623b42d7-a222-44c4-a764-c0024a283fd7">
<!-- ADR: Annotate Borrowed Fields with `#[serde(borrow)]` for Zero-Copy Deserialization -->
</rule_activation>

- Add `#[serde(borrow)]` to every field typed as `&'de str`, `&'de [u8]`, or any type containing a borrowed lifetime
- Use owned `String` or `Vec<u8>` for fields in structs that outlive the parsing call site
