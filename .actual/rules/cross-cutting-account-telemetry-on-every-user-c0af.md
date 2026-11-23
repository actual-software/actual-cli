---
version: 2
name: cross-cutting-account-telemetry-on-every-user-c0af
description: Account for telemetry on every user-facing command path
metadata:
  actual:
    adr_id: c0af831b-64e7-4dcc-9750-f7866e0686b5
    status: accepted
    source:
      origin: local
      path: docs/adr/c0af831b-64e7-4dcc-9750-f7866e0686b5-account-for-telemetry-on-every-user-facing-command-path.md
      digest: sha256:af84cac484775003fe0045de722045900a18736b92d395be9d83e2b5b5f33951
  scope:
    root: .
    paths:
      - src/**
      - PRIVACY.md
      - CHANGELOG.md
---

# Account for telemetry on every user-facing command path

Scope: applies to `src/**`, `PRIVACY.md`, `CHANGELOG.md`.

### Rules

- **R-C0AF-001** MUST: Every user-facing command path, signed in and signed out, sends its command family's telemetry event, or PRIVACY.md says why that path sends none.
- **R-C0AF-002** MUST: A change that adds or alters a telemetry event or attribute updates PRIVACY.md in the same pull request, with each new attribute's row and privacy class.
- **R-C0AF-003** MUST: A change that adds or alters a telemetry attribute bumps SCHEMA_VERSION in src/telemetry/otlp.rs and records the new version in PRIVACY.md and CHANGELOG.md in the same pull request.
- **R-C0AF-004** MUST NOT: A telemetry attribute carries decision titles, context, policy text, scope globs, file paths or other free text, apart from the identity-linked fields PRIVACY.md lists.
- **R-C0AF-005** SHOULD: Each new command path has a test that asserts its event's attributes and outcome on every pathway it runs on.
