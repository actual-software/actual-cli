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
      digest: sha256:2e2fde50199ba20156f9df207935a248884ed8202628bdaf66fbb3d798768c46
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

- **R-C0AF831B-001** MUST: A change that adds or alters a user-facing command path makes that path send its command family's telemetry event, signed in and signed out, or adds to PRIVACY.md the reason that path sends none.
- **R-C0AF831B-002** MUST: A change that adds or alters a telemetry event or attribute updates PRIVACY.md in the same pull request, with each new attribute's row and privacy class.
- **R-C0AF831B-003** MUST: A change that adds or alters a telemetry attribute bumps SCHEMA_VERSION in src/telemetry/otlp.rs and records the new version in PRIVACY.md and CHANGELOG.md in the same pull request.
- **R-C0AF831B-004** MUST NOT: A telemetry attribute carries decision titles, context, policy text, scope globs, file paths or other free text, apart from the identity-linked fields PRIVACY.md lists.
- **R-C0AF831B-005** SHOULD: Each new command path has a test that asserts its event's attributes and outcome on every pathway it runs on.
