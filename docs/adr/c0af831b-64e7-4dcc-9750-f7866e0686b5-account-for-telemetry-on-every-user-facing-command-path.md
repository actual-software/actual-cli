---
version: 2
name: c0af831b-64e7-4dcc-9750-f7866e0686b5-account-for-telemetry-on-every-user-facing-command-path
description: Account for telemetry on every user-facing command path
metadata:
  actual:
    adr_id: c0af831b-64e7-4dcc-9750-f7866e0686b5
    status: accepted
  scope:
    root: .
    paths:
      - src/**
      - PRIVACY.md
      - CHANGELOG.md
---

# Account for telemetry on every user-facing command path

Status: accepted
Date: 2026-11-23
Deciders: actual-cli maintainers

## Context

- Each command family sends one OpenTelemetry event per run, and PRIVACY.md is the user-facing record of what each event carries.
- Signed-out actual adr publish, update and delete arrived with no event, which would have turned a small signal into none just as those decisions became real.
- A new attribute needs its PRIVACY.md row, privacy class and schema version in the build that first sends it; a docs PR that lands later leaves every build in between describing telemetry it doesn't send.
- Two telemetry PRs in flight usually claim the same schema version, so the version has to be settled at merge time.

## Decision

1. MUST: A change that adds or alters a user-facing command path makes that path send its command family's telemetry event, signed in and signed out, or adds to PRIVACY.md the reason that path sends none.
2. MUST: A change that adds or alters a telemetry event or attribute updates PRIVACY.md in the same pull request, with each new attribute's row and privacy class.
3. MUST: A change that adds or alters a telemetry attribute bumps SCHEMA_VERSION in src/telemetry/otlp.rs and records the new version in PRIVACY.md and CHANGELOG.md in the same pull request.
4. MUST_NOT: A telemetry attribute carries decision titles, context, policy text, scope globs, file paths or other free text, apart from the identity-linked fields PRIVACY.md lists.
5. SHOULD: Each new command path has a test that asserts its event's attributes and outcome on every pathway it runs on.

## Rationale

- Keeping the event, its PRIVACY.md rows and the schema version in one pull request means no released build describes telemetry it doesn't send.
- Counts, enums and booleans let local and cloud usage be compared without collecting content.

## Consequences

Positive:
- New command paths can't ship uncounted without a written reason.
- PRIVACY.md stays accurate for every release.

Negative:
- Telemetry pull requests conflict on the schema version and have to agree an order.
- Small command changes carry documentation work.

## References

- PRIVACY.md
- src/telemetry/otlp.rs
- https://github.com/actual-software/actual-cli/pull/963
