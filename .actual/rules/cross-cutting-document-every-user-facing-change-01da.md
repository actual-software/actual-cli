---
version: 2
name: cross-cutting-document-every-user-facing-change-01da
description: Document every user-facing change on docs.actual.ai
metadata:
  actual:
    adr_id: 01da20f8-3923-4a2e-bd3f-1449f44f1fbf
    status: accepted
    source:
      origin: local
      path: docs/adr/01da20f8-3923-4a2e-bd3f-1449f44f1fbf-document-every-user-facing-change-on-docs-actual-ai.md
      digest: sha256:4fd2e6095b0fd7d2aafccb436f5246d98d6d41d2f22ec3946a1fc10788fc5aef
  scope:
    root: .
    paths:
      - src/**
      - CHANGELOG.md
---

# Document every user-facing change on docs.actual.ai

Scope: applies to `src/**`, `CHANGELOG.md`.

### Rules

- **R-01DA-001** MUST: A change that adds, removes or alters a user-facing command, flag, argument, exit code, output field, configuration key or environment variable opens a PR in the docs.actual.ai repository that updates the page covering it, labelled `awaits-cli-release`. The diff shows this as a CHANGELOG line that links that page.
- **R-01DA-002** MUST: A user-facing change adds its CHANGELOG line under Unreleased.
