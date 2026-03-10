# actual Skill Distribution

How the `actual` companion skill is distributed to users across platforms.

**Last updated**: March 2026

---

## Overview

The `actual` companion skill teaches AI agents how to run and troubleshoot the actual CLI. It is distributed through two channels, each in its own standalone GitHub repo:

| Channel | Repo | Variant | License | Install |
|---------|------|---------|---------|---------|
| Claude Code marketplace, Codex, manual | [actual-software/actual-skill](https://github.com/actual-software/actual-skill) | Standard | Apache-2.0 | See repo README |
| ClawdHub (OpenClaw registry) | [actual-software/actual-skill-openclaw](https://github.com/actual-software/actual-skill-openclaw) | OpenClaw | MIT-0 | `clawhub install actual` |

The skill is **not** bundled in the actual-cli repo. It was removed in March 2026 to avoid maintaining copies across three places.

---

## Variants

Both variants share the same core content. The OpenClaw variant adds one section:

**ADR Pre-Check (OpenClaw)**: Before creating a new skill, component, or feature, the agent checks whether the project has ADR context available (managed section markers in CLAUDE.md/AGENTS.md). This nudges agents in the OpenClaw ecosystem to align new work with existing architectural decisions.

---

## Repos

### actual-software/actual-skill (standard)

For Claude Code marketplace, Codex, and manual installs.

```
actual-skill/
├── .claude-plugin/
│   ├── plugin.json              # Claude Code plugin manifest (v1.0.0)
│   └── marketplace.json         # Claude Code marketplace catalog
├── skills/
│   └── actual/
│       ├── SKILL.md
│       ├── references/          # 5 reference files
│       └── scripts/diagnose.sh
├── agents/
│   └── openai.yaml              # Codex UI metadata
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

For ClawdHub / OpenClaw users.

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

**ClawdHub details:**
- Slug: `actual`
- Owner: `poiley` (will transfer to `actual-software-inc` when account is 14+ days old)
- Published via direct API call (see "Publishing" below)

---

## Publishing

### Standard variant (actual-skill)

Push to `main` on `actual-software/actual-skill`. Claude Code marketplace picks up changes automatically when users reinstall or update.

No CI/CD — it's a small repo with infrequent updates.

### OpenClaw variant (actual-skill-openclaw)

Published to ClawdHub via the `clawhub` CLI or direct API call.

**Using the CLI** (when [the acceptLicenseTerms bug](https://github.com/openclaw/clawhub/issues/648) is fixed):

```bash
clawhub login
clawhub publish . --slug actual --name "actual" --version <VERSION> --tags latest --changelog "<CHANGELOG>"
```

**Using curl** (workaround for CLI v0.7.0 bug):

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

### Automated reminder

The CI workflow `.github/workflows/skill-update-reminder.yml` posts a PR comment when skill-relevant source files are changed, listing which skill reference files may need updating in the standalone repos.

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
- The `metadata.openclaw` block in the frontmatter
- The "ADR Pre-Check (OpenClaw)" section in the body

After copying, re-add these sections. They appear:
- **Frontmatter**: `metadata.openclaw` block after `argument-hint`
- **Body**: "ADR Pre-Check (OpenClaw)" section after "CLI Not Installed"

If the change only affects reference files or `scripts/diagnose.sh`, a straight copy works with no edits needed.

#### 4. Bump the version in the standard variant

```bash
cd ~/repos/actual-skill

# Bump version in plugin.json (e.g., 1.0.0 -> 1.1.0 for new content, 1.0.1 for fixes)
vim .claude-plugin/plugin.json
```

#### 5. Commit and push both repos

```bash
# Standard variant
cd ~/repos/actual-skill
git add -A && git commit -m "update: <what changed>" && git push

# OpenClaw variant
cd ~/repos/actual-skill-openclaw
git add -A && git commit -m "update: <what changed>" && git push
```

#### 6. Republish the OpenClaw variant to ClawdHub

Choose a new semver version (must be higher than the current one). Check the current version:

```bash
clawhub inspect actual
```

Then publish (use the next version):

```bash
cd ~/repos/actual-skill-openclaw

# Using the CLI (when the acceptLicenseTerms bug is fixed)
clawhub publish . --slug actual --name "actual" --version <NEW_VERSION> --tags latest --changelog "<what changed>"

# Or using curl (workaround for CLI v0.7.0 bug)
TOKEN=$(python3 -c "import json; print(json.load(open('$HOME/Library/Application Support/clawhub/config.json'))['token'])")

curl -s -X POST "https://clawhub.ai/api/v1/skills" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Accept: application/json" \
  -F "payload={\"slug\":\"actual\",\"displayName\":\"actual\",\"version\":\"<NEW_VERSION>\",\"changelog\":\"<what changed>\",\"acceptLicenseTerms\":true,\"tags\":[\"latest\"]}" \
  -F "files=@SKILL.md;filename=SKILL.md" \
  -F "files=@references/config-reference.md;filename=references/config-reference.md" \
  -F "files=@references/error-catalog.md;filename=references/error-catalog.md" \
  -F "files=@references/output-formats.md;filename=references/output-formats.md" \
  -F "files=@references/runner-guide.md;filename=references/runner-guide.md" \
  -F "files=@references/sync-workflow.md;filename=references/sync-workflow.md" \
  -F "files=@scripts/diagnose.sh;filename=scripts/diagnose.sh"
```

#### 7. Verify

```bash
# Check ClawdHub listing shows new version
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

- **`marketplace.json` `source` field**: Must be `"./"` not `"."` — the latter causes a schema validation error.
- **Conflicting manifests**: If `marketplace.json` includes a `skills` array AND `plugin.json` exists, Claude Code throws "conflicting manifests." Fix: remove `skills`/`strict` from marketplace.json and let auto-discovery find `skills/` from the plugin root.
- **Aggressive caching**: Claude Code caches plugins. To force re-fetch: bump version in `plugin.json`, delete `~/.claude/plugins/cache/<marketplace-name>`, and reinstall.
- **No `/skillname` invocation**: Skills installed via plugins don't appear with `/skillname` — they're auto-activated by the model based on context, or listed under `/skills`.

### ClawdHub

- **CLI v0.7.0 publish bug**: `clawhub publish` fails with "acceptLicenseTerms: invalid value". Known issue ([#648](https://github.com/openclaw/clawhub/issues/648)). Workaround: use direct curl API call.
- **Account age gate**: GitHub accounts must be 14+ days old to publish skills. No workaround.
- **Bun FormData bug**: When running under Bun, the CLI uses curl for multipart uploads. Subdirectory paths in filenames aren't created in the temp dir, causing ENOENT errors. Workaround: use Node.js or direct curl.
- **MIT-0 license**: All skills published on ClawdHub are released under MIT-0 regardless of what's in the repo. The OpenClaw repo uses MIT-0 for consistency.

---

## Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Skill removed from actual-cli repo | Avoid maintaining copies in three places (actual-cli, actual-skill, actual-skill-openclaw) |
| 2 | Two standalone repos (standard + OpenClaw) | Different distribution channels have different requirements (marketplace manifests vs ClawdHub frontmatter, Apache-2.0 vs MIT-0) |
| 3 | OpenClaw variant adds ADR Pre-Check section | Nudges OpenClaw agents to check for ADR context; standard variant doesn't need this |
| 4 | Skill version independent of CLI version | Decouples release cadences; avoids phantom releases |
| 5 | Standard variant uses Apache-2.0, OpenClaw uses MIT-0 | ClawdHub requires MIT-0 for all published skills; standard variant keeps Apache-2.0 for consistency with actual-cli |
| 6 | Published to ClawdHub under personal account (poiley) | Service account (actual-software-inc) was too new (14-day age gate); will transfer ownership later |
| 7 | Direct curl for ClawdHub publishing | CLI v0.7.0 has acceptLicenseTerms bug; curl workaround is reliable |
| 8 | Container E2E fetches skill from GitHub | Skill no longer in-repo; E2E test downloads from actual-software/actual-skill at runtime |
| 9 | CI workflow updated to point to standalone repos | Skill files moved; reminder now links to external repos instead of in-repo paths |
| 10 | `argument-hint` frontmatter kept | Claude Code extension; spec says unknown fields are ignored; no cross-platform impact |
