---
version: 2
name: 01da20f8-3923-4a2e-bd3f-1449f44f1fbf-document-every-user-facing-change-on-docs-actual-ai
description: Document every user-facing change on docs.actual.ai
metadata:
  actual:
    adr_id: 01da20f8-3923-4a2e-bd3f-1449f44f1fbf
    status: accepted
  scope:
    root: .
    paths:
      - src/**
      - CHANGELOG.md
---

# Document every user-facing change on docs.actual.ai

Status: accepted
Date: 2026-11-24
Deciders: actual-cli maintainers

## Context

- docs.actual.ai is the one home for the Actual CLI's user documentation, and its pages are written in the docs.actual.ai repository, which builds the site, so a change to what the CLI offers its users needs a second pull request there.
- The site describes only released behavior, so a CLI change's docs pull request is reviewed beside the CLI pull request and then waits under the `awaits-cli-release` label.
- That docs pull request names the CLI pull request in a `CLI pull request:` line, and the docs repository's release job merges it once a published release carries that pull request.
- The plugin's end-of-turn check judges a change's diff alone and can't see a pull request in another repository, so these rules ask for what the diff shows: a CHANGELOG line under Unreleased that links the docs.actual.ai page the docs pull request updates.

## Decision

1. MUST: A change that adds, removes or alters a user-facing command, flag, argument, exit code, output field, configuration key or environment variable opens a PR in the docs.actual.ai repository that updates the page covering it, labelled `awaits-cli-release`. The diff shows this as a CHANGELOG line that links that page.
2. MUST: A user-facing change adds its CHANGELOG line under Unreleased.

## Rationale

- A CHANGELOG line that links its docs.actual.ai page is the one trace of the docs pull request that a diff carries, so the plugin's check and a reviewer can both confirm it.
- Holding the docs pull request until a release carries the change keeps the site to released behavior, while the page is still reviewed with the code.

## Consequences

Positive:
- Each release's notes link the pages that describe its changes.
- docs.actual.ai changes in step with each release.

Negative:
- Every user-facing change takes a second pull request, reviewed in another repository.
- A contributor without access to the docs.actual.ai repository can't open the docs pull request, so a maintainer opens it for them.

## References

- https://docs.actual.ai
- CHANGELOG.md
