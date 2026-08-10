---
glob: "**/*.rs"
---

<rule_activation adr-id="8c7f2d63-72e2-4225-8577-66ff53cd9ab6">
<!-- ADR: Enable `features = ["derive"]` in Cargo.toml When Using serde Derive Macros -->
</rule_activation>

- Declare `serde = { version = "1", features = ["derive"] }` in every application crate that uses `#[derive(Serialize, Deserialize)]`
- In library crates, declare serde as `optional = true` with the derive feature and gate all derives behind `#[cfg(feature = "serde")]` to avoid forcing the dependency on downstream users
