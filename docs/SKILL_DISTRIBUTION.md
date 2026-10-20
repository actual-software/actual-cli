# actual Skill Distribution

How the `actual` companion skill is distributed to users across platforms.

---

## Overview

The `actual` companion skill teaches AI agents how to run and troubleshoot the actual CLI. It is distributed through two channels, each in its own standalone GitHub repo:

| Channel | Repo | Variant | License | Install |
|---------|------|---------|---------|---------|
| Claude Code marketplace, Codex, manual | [actual-software/actual-skill](https://github.com/actual-software/actual-skill) | Standard | Apache-2.0 | See repo README |
| ClawHub (OpenClaw registry) | [actual-software/actual-skill-openclaw](https://github.com/actual-software/actual-skill-openclaw) | OpenClaw | MIT-0 | `clawhub install actual` |

ClawHub serves its own stored copy of a skill, so a change in either repo reaches ClawHub users only through a new publish to ClawHub. Its `actual` entry is version 0.1.0, published on 2026-03-10. That copy installs only the skill and never the CLI: its frontmatter requires an `actual` binary on `PATH` and declares no way to install one. It also predates the skill's Advisor, `plan-check`, and `impl-check` guidance.

The skill is **not** bundled in the actual-cli repo. It was removed in March 2026 to avoid maintaining copies across three places.

---

## Variants

The OpenClaw variant started as the standard skill plus the section below, and the two have drifted since. The standard `SKILL.md` now also covers `plan-check` and `impl-check` governance, which runs through Claude Code hooks the OpenClaw variant doesn't ship. The OpenClaw-only section is:

**ADR Pre-Check (OpenClaw)**: Before creating a new skill, component, or feature, the agent checks whether the project has ADR context available (managed section markers in CLAUDE.md/AGENTS.md). This nudges agents in the OpenClaw ecosystem to align new work with existing architectural decisions.

---

## Repos

### actual-software/actual-skill (standard)

For Claude Code marketplace, Codex, and manual installs.

```
actual-skill/
├── .claude-plugin/
│   ├── plugin.json              # Claude Code plugin manifest; its version decides who gets updates
│   └── marketplace.json         # Claude Code marketplace catalog
├── .codex-plugin/
│   └── plugin.json              # Codex plugin manifest
├── hooks/                       # Claude Code hooks: plan-check and impl-check gates, rule briefs
├── skills/
│   └── actual/
│       ├── SKILL.md
│       ├── references/          # reference files the skill loads on demand
│       └── scripts/diagnose.sh
├── agents/
│   └── openai.yaml              # Codex UI metadata
├── assets/logo.png
├── README.md
└── LICENSE                      # Apache-2.0
```

**Install methods:**

```bash
# Claude Code marketplace
/plugin marketplace add actual-software/actual-skill
/plugin install actual-cli@actual-cli-skills

# Codex
$skill-installer install the actual skill from actual-software/actual-skill

# Manual (any platform)
git clone https://github.com/actual-software/actual-skill.git ~/.local/share/actual-skill
ln -s ~/.local/share/actual-skill/skills/actual ~/.claude/skills/actual
```

### actual-software/actual-skill-openclaw (OpenClaw)

For ClawHub / OpenClaw users.

```
actual-skill-openclaw/
├── SKILL.md                     # OpenClaw variant (with ADR Pre-Check, openclaw frontmatter)
├── references/                  # 5 reference files
├── scripts/diagnose.sh
├── README.md
└── LICENSE                      # MIT-0
```

**Install:**

```bash
clawhub install actual
```

**ClawHub details:**
- Slug: `actual`, display name `Actual AI CLI`
- Owner: the ClawHub account `actual-software-inc`
- Latest version: 0.1.0, published on 2026-03-10 through a direct API call (see "Publishing" below). It installs the skill only, as described in the [Overview](#overview).
- Check what it serves: `curl -s https://clawhub.ai/api/v1/skills/actual | jq '.latestVersion.version, .owner.handle'`

---

## Publishing

### Standard variant (actual-skill)

Merge to `main` on `actual-software/actual-skill`. A new install copies whatever `main` holds, but an existing install changes only when `version` in `.claude-plugin/plugin.json` changes. Claude Code compares that string, so commits merged without a bump never reach anyone who already has the plugin ([Release a new version](https://code.claude.com/docs/en/plugins/host-marketplace#release-a-new-version)). Bump it in every merge users should receive.

After a bump, an install updates itself only if its user turned on auto-update for the marketplace, which is off by default for a third-party marketplace like this one: `/plugin`, then **Marketplaces**, `actual-cli-skills`, **Enable auto-update**. Otherwise users update by hand, then restart Claude Code or run `/reload-plugins`:

```bash
claude plugin marketplace update actual-cli-skills
claude plugin update actual-cli@actual-cli-skills
```

### OpenClaw variant (actual-skill-openclaw)

Published to ClawHub with the `clawhub` CLI. A new version has to come from the `actual-software-inc` account, or from an account with publisher access to it (`--owner actual-software-inc`).

```bash
clawhub login
clawhub publish . --slug actual --name "Actual AI CLI" --version <VERSION> --tags latest --changelog "<CHANGELOG>"
```

**Using curl:** the March 2026 publish went through the API directly, because clawhub CLI v0.7.0 failed with `acceptLicenseTerms: invalid value` ([#648](https://github.com/openclaw/clawhub/issues/648), fixed upstream on 2026-03-13). The call, as documented at the time:

```bash
TOKEN=$(python3 -c "import json; print(json.load(open('$HOME/Library/Application Support/clawhub/config.json'))['token'])")

curl -s -X POST "https://clawhub.ai/api/v1/skills" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Accept: application/json" \
  -F 'payload={"slug":"actual","displayName":"actual","version":"<VERSION>","changelog":"<CHANGELOG>","acceptLicenseTerms":true,"tags":["latest"]}' \
  -F "files=@SKILL.md;filename=SKILL.md" \
  -F "files=@references/config-reference.md;filename=references/config-reference.md" \
  -F "files=@references/error-catalog.md;filename=references/error-catalog.md" \
  -F "files=@references/output-formats.md;filename=references/output-formats.md" \
  -F "files=@references/runner-guide.md;filename=references/runner-guide.md" \
  -F "files=@references/sync-workflow.md;filename=references/sync-workflow.md" \
  -F "files=@scripts/diagnose.sh;filename=scripts/diagnose.sh"
```

---

## Updating the Skill

When CLI changes affect user-facing behavior (new errors, config keys, flags, output format changes, runner changes), the skill repos need to be updated.

### What triggers a skill update

| CLI Change | Skill File(s) to Update |
|------------|------------------------|
| New error variant in `src/error.rs` | `references/error-catalog.md` |
| New config key in `src/config/` | `references/config-reference.md`, `SKILL.md` |
| New runner | `references/runner-guide.md`, `SKILL.md` |
| New output format | `references/output-formats.md`, `SKILL.md` |
| New CLI flag | `SKILL.md` (Commands table) |
| Sync pipeline change | `references/sync-workflow.md` |
| New binary name | `scripts/diagnose.sh` |

### Checking a CLI change against the table

Nothing in CI flags skill-relevant changes, so check the table above by hand in any PR that changes user-facing CLI behavior.

### Update workflow (step-by-step)

#### 1. Clone both repos (if you haven't already)

```bash
# Standard variant
git clone https://github.com/actual-software/actual-skill.git ~/repos/actual-skill

# OpenClaw variant
git clone https://github.com/actual-software/actual-skill-openclaw.git ~/repos/actual-skill-openclaw
```

#### 2. Make the change in the standard variant

Edit the relevant files in `actual-skill`. The skill files live under `skills/actual/`:

```bash
cd ~/repos/actual-skill

# Example: update the error catalog after adding a new error variant
vim skills/actual/references/error-catalog.md

# Example: update SKILL.md after adding a new CLI flag
vim skills/actual/SKILL.md
```

#### 3. Copy the change to the OpenClaw variant

The OpenClaw variant has the same files but at the repo root (no `skills/actual/` prefix):

```bash
cd ~/repos/actual-skill-openclaw

# Copy changed files from standard variant
cp ~/repos/actual-skill/skills/actual/references/error-catalog.md references/error-catalog.md
cp ~/repos/actual-skill/skills/actual/SKILL.md SKILL.md
```

**Important**: If you copied `SKILL.md`, the OpenClaw variant has extra content that the standard variant doesn't:
- The `argument-hint` line and the `metadata.openclaw` block in the frontmatter
- The "ADR Pre-Check (OpenClaw)" section in the body

After copying, re-add these sections. They appear:
- **Frontmatter**: `argument-hint`, then the `metadata.openclaw` block, after `description`
- **Body**: "ADR Pre-Check (OpenClaw)" section after "CLI Not Installed"

The standard `SKILL.md` also carries the Claude Code governance section, which depends on hooks the OpenClaw variant doesn't ship, so leave that section out of the copy.

If the change only affects reference files or `scripts/diagnose.sh`, a straight copy works with no edits needed.

#### 4. Bump the version in the standard variant

```bash
cd ~/repos/actual-skill

# Bump version in plugin.json (e.g., 1.0.0 -> 1.1.0 for new content, 1.0.1 for fixes)
vim .claude-plugin/plugin.json
```

Existing Claude Code installs pick up the change only through this bump (see [Publishing](#standard-variant-actual-skill)).

#### 5. Commit and push both repos

```bash
# Standard variant
cd ~/repos/actual-skill
git add -A && git commit -m "update: <what changed>" && git push

# OpenClaw variant
cd ~/repos/actual-skill-openclaw
git add -A && git commit -m "update: <what changed>" && git push
```

#### 6. Republish the OpenClaw variant to ClawHub

This step hasn't been run since the 2026-03-10 publish, which is why ClawHub still serves 0.1.0.

Choose a new semver version (must be higher than the current one). Check the current version:

```bash
clawhub inspect actual
```

Then publish (use the next version):

```bash
cd ~/repos/actual-skill-openclaw
clawhub publish . --slug actual --name "Actual AI CLI" --version <NEW_VERSION> --tags latest --changelog "<what changed>"
```

[Publishing](#openclaw-variant-actual-skill-openclaw) has the account this needs and the direct API call the March publish used.

#### 7. Verify

```bash
# Check ClawHub listing shows new version
clawhub inspect actual

# Test install in a scratch directory
clawhub install actual --workdir /tmp/clawhub-test --dir skills
cat /tmp/clawhub-test/skills/actual/SKILL.md | head -5
rm -rf /tmp/clawhub-test
```

#### Special case: OpenClaw-only changes

If the change only affects the "ADR Pre-Check (OpenClaw)" section or the `metadata.openclaw` frontmatter, skip steps 2 and 4 — only update and publish the OpenClaw repo.

---

## Known Gotchas

### Claude Code marketplace

| Gotcha | Status | Notes |
|--------|--------|-------|
| **`marketplace.json` `source` field** | Not reproduced on 2.1.304 | Keep it `"./"`. An earlier Claude Code release rejected `"."` with a schema validation error; 2.1.304 installs from a local checkout with either |
| **Conflicting manifests** | Active | If `marketplace.json` includes a `skills` array AND `plugin.json` exists, Claude Code throws "conflicting manifests." Fix: remove `skills`/`strict` from marketplace.json and let auto-discovery find `skills/` from the plugin root |
| **Updates key on `version`** | Active | An install keeps its cached copy until `plugin.json`'s `version` changes, so a merge without a bump reaches new installs only. See [Publishing](#standard-variant-actual-skill) |
| **Plugin skills are namespaced** | By design | A plugin's skills are slash commands prefixed with the plugin name, so the skill's full command is `/actual-cli:actual` ([skills docs](https://code.claude.com/docs/en/skills)) |

### ClawHub

| Gotcha | Status | Notes |
|--------|--------|-------|
| **CLI v0.7.0 publish bug** | Fixed upstream | `clawhub publish` failed with "acceptLicenseTerms: invalid value" ([#648](https://github.com/openclaw/clawhub/issues/648), closed on 2026-03-13). The direct curl API call was the workaround |
| **Account age gate** | Active | GitHub accounts must be 14+ days old to publish skills. No workaround |
| **Bun FormData bug** | Active | When running under Bun, the CLI uses curl for multipart uploads. Subdirectory paths in filenames aren't created in the temp dir, causing ENOENT errors. Workaround: use Node.js or direct curl |
| **MIT-0 license** | By design | All skills published on ClawHub are released under MIT-0 regardless of what's in the repo. The OpenClaw repo uses MIT-0 for consistency |

---

## Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Skill removed from actual-cli repo | Avoid maintaining copies in three places (actual-cli, actual-skill, actual-skill-openclaw) |
| 2 | Two standalone repos (standard + OpenClaw) | Different distribution channels have different requirements (marketplace manifests vs ClawHub frontmatter, Apache-2.0 vs MIT-0) |
| 3 | OpenClaw variant adds ADR Pre-Check section | Nudges OpenClaw agents to check for ADR context; standard variant doesn't need this |
| 4 | Skill version independent of CLI version | Decouples release cadences; avoids phantom releases |
| 5 | Standard variant uses Apache-2.0, OpenClaw uses MIT-0 | ClawHub releases every published skill under MIT-0, and the OpenClaw repo matches it. actual-cli itself is MIT |
| 6 | ClawHub entry owned by the `actual-software-inc` account | The first publish went out under a personal account while `actual-software-inc` was inside ClawHub's 14-day account-age gate. The entry belongs to `actual-software-inc` now |
| 7 | Direct curl for the March 2026 ClawHub publish | clawhub CLI v0.7.0 had the acceptLicenseTerms bug, fixed upstream on 2026-03-13 |
| 8 | Container E2E fetches skill from GitHub | Skill no longer in-repo; E2E test downloads from actual-software/actual-skill at runtime |
| 9 | Skill-update reminder workflow removed | Dropped on 2026-03-13 along with the repo's other internal workflows, so the table under [Updating the Skill](#updating-the-skill) is a manual check |
| 10 | `argument-hint` kept only in the OpenClaw variant | The standard variant dropped it on 2026-07-28 when it was packaged as a Codex and ChatGPT plugin; Codex's skill validator accepts only `name`, `description`, `license`, `allowed-tools`, and `metadata` in the frontmatter |
