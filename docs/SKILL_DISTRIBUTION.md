# /actual Skill: Cross-Platform Distribution

Research, architecture, and implementation plan for distributing the `/actual` skill
across Claude Code, OpenCode, Codex (OpenAI), and Cursor.

**Date**: February 2026
**Status**: Research complete, Tier 1 implemented, Tier 2–3 documented for future work
**Epic**: actual-ns1

---

## Table of Contents

1. [The Agent Skills Standard](#1-the-agent-skills-standard)
2. [Platform Support Matrix](#2-platform-support-matrix)
3. [Distribution Tiers](#3-distribution-tiers)
4. [Tier 1: In-Repo Discovery](#4-tier-1-in-repo-discovery)
5. [Tier 2: Standalone Plugin Repo](#5-tier-2-standalone-plugin-repo)
6. [Tier 3: Gallery Submission](#6-tier-3-gallery-submission)
7. [Install Instructions by Platform](#7-install-instructions-by-platform)
8. [Frontmatter Compatibility](#8-frontmatter-compatibility)
9. [Versioning and Sync Strategy](#9-versioning-and-sync-strategy)
10. [Sources Consulted](#10-sources-consulted)
11. [Decision Log](#11-decision-log)

---

## 1. The Agent Skills Standard

[Agent Skills](https://agentskills.io) is an open standard originally created by Anthropic and
adopted industry-wide. As of February 2026, 27+ tools support it, including all four target
platforms. The spec is governed at [github.com/agentskills/agentskills](https://github.com/agentskills/agentskills)
(Apache 2.0).

A skill is a directory with a `SKILL.md` file containing YAML frontmatter (`name` + `description`)
and markdown instructions. Optional directories: `scripts/`, `references/`, `assets/`.

**Progressive disclosure** (the core design principle):
1. **Metadata** (~100 tokens): `name` and `description` loaded at startup for all skills
2. **Instructions** (<5000 tokens recommended): Full `SKILL.md` body loaded on activation
3. **Resources** (as needed): Files in `scripts/`, `references/`, `assets/` loaded on demand

Our `/actual` skill already fully conforms: 236-line SKILL.md, 5 reference files, 1 diagnostic
script, proper frontmatter. No changes needed to the skill itself for distribution.

---

## 2. Platform Support Matrix

### Discovery Paths

| Platform       | Primary Path              | Also Reads                          | Global Path                    |
|----------------|---------------------------|-------------------------------------|--------------------------------|
| **Claude Code** | `.claude/skills/*/SKILL.md` | —                                   | `~/.claude/skills/*/SKILL.md`  |
| **OpenCode**   | `.opencode/skills/*/SKILL.md` | `.claude/skills/`, `.agents/skills/` | `~/.config/opencode/skills/`, `~/.claude/skills/`, `~/.agents/skills/` |
| **Codex**      | `.agents/skills/*/SKILL.md` | —                                   | `~/.agents/skills/`, `/etc/codex/skills/` |
| **Cursor**     | `.cursor/skills/*/SKILL.md` | `.claude/skills/`, `.agents/skills/` | `~/.claude/skills/`, `~/.agents/skills/` |

### Marketplace / Install Mechanisms

| Platform       | Marketplace System        | Install Command                                          | Maturity |
|----------------|---------------------------|----------------------------------------------------------|----------|
| **Claude Code** | Plugin marketplaces       | `/plugin marketplace add org/repo` → `/plugin install`   | Mature   |
| **OpenCode**   | None                      | Manual clone/symlink                                     | N/A      |
| **Codex**      | `$skill-installer`        | `$skill-installer install the X skill from org/repo`     | Basic    |
| **Cursor**     | None                      | Manual clone/symlink                                     | N/A      |

### Platform-Specific Extensions

| Platform       | Extension                     | Purpose                                    | In Spec? |
|----------------|-------------------------------|--------------------------------------------|----------|
| **Claude Code** | `context: fork`, `agent:`, `hooks`, `model`, `disable-model-invocation`, `user-invocable`, `argument-hint` | Subagent execution, invocation control, tool restriction | No (CC-only) |
| **Codex**      | `agents/openai.yaml`          | UI metadata (icon, brand color, invocation policy, MCP deps) | No (Codex-only) |
| **OpenCode**   | Permission config in `opencode.json` | Per-skill allow/deny/ask                  | No (OC-only) |
| **Cursor**     | None documented               | —                                          | N/A      |

---

## 3. Distribution Tiers

### Tier 1: In-Repo Discovery (IMPLEMENTED)

Anyone who clones this repo gets the skill automatically on all four platforms.

```
actual-cli/
├── .claude/skills/actual/          ← Claude Code (canonical)
├── .opencode/skills/actual → ...   ← OpenCode (symlink)
└── .agents/skills/actual → ...     ← Codex + Cursor (symlink)
```

**Cost**: 2 symlinks. **Coverage**: All 4 platforms for repo users.

### Tier 2: Standalone Plugin Repo (DESIGNED, NOT IMPLEMENTED)

A separate GitHub repo that serves as both a Claude Code plugin marketplace and a
general-purpose skill distribution point. Users outside this repo can install the skill
on any platform.

### Tier 3: Gallery Submission (FUTURE WORK)

Submit a PR to [anthropics/skills](https://github.com/anthropics/skills) (73k stars) to add
the `/actual` skill to the official Agent Skills example gallery. Pure visibility play.

---

## 4. Tier 1: In-Repo Discovery

### Current State

| Path                            | Type      | Covers          | Status |
|---------------------------------|-----------|-----------------|--------|
| `.claude/skills/actual/`        | Directory | Claude Code     | Done (PR #365) |
| `.opencode/skills/actual`       | Symlink   | OpenCode        | Done (PR #365) |
| `.agents/skills/actual`         | Symlink   | Codex, Cursor   | Done (this PR) |

### Symlink Structure

All symlinks point to the canonical location at `.claude/skills/actual/`:

```
.opencode/skills/actual → ../../.claude/skills/actual
.agents/skills/actual   → ../../.claude/skills/actual
```

### Verification

```bash
# Claude Code
cat .claude/skills/actual/SKILL.md | head -3

# OpenCode
readlink .opencode/skills/actual
cat .opencode/skills/actual/SKILL.md | head -3

# Codex / Cursor
readlink .agents/skills/actual
cat .agents/skills/actual/SKILL.md | head -3
```

---

## 5. Tier 2: Standalone Plugin Repo

### Repo Name

Recommended: `actual-software/actual-skill`

### Directory Structure

```
actual-skill/
├── .claude-plugin/
│   ├── plugin.json                    # Claude Code plugin manifest
│   └── marketplace.json               # Claude Code marketplace catalog
├── skills/
│   └── actual/
│       ├── SKILL.md                   # Skill entry point (copy from .claude/skills/actual/)
│       ├── references/
│       │   ├── sync-workflow.md
│       │   ├── runner-guide.md
│       │   ├── error-catalog.md
│       │   ├── config-reference.md
│       │   └── output-formats.md
│       └── scripts/
│           └── diagnose.sh
├── agents/
│   └── openai.yaml                    # Codex UI metadata (optional)
├── README.md                          # Install instructions for all platforms
├── LICENSE                            # Apache-2.0
└── CHANGELOG.md                       # Version history
```

### plugin.json (Claude Code)

```json
{
  "name": "actual-cli",
  "description": "Feature-complete companion for the actual CLI — runs and troubleshoots sync, status, auth, config, runners, and models across all 5 runners and 3 output formats",
  "version": "1.0.0",
  "author": {
    "name": "actual-software"
  },
  "homepage": "https://github.com/actual-software/actual-cli",
  "repository": "https://github.com/actual-software/actual-skill",
  "license": "Apache-2.0",
  "keywords": ["actual", "cli", "adr", "claude-md", "agents-md", "cursor-rules"]
}
```

### marketplace.json (Claude Code)

```json
{
  "name": "actual-cli-skills",
  "owner": {
    "name": "actual-software",
    "email": "dev@actual-software.com"
  },
  "metadata": {
    "description": "Skills for the actual CLI (ADR-powered CLAUDE.md/AGENTS.md generator)"
  },
  "plugins": [
    {
      "name": "actual-cli",
      "source": ".",
      "description": "Feature-complete companion for the actual CLI"
    }
  ]
}
```

### agents/openai.yaml (Codex)

```yaml
interface:
  display_name: "actual CLI"
  short_description: "Run and troubleshoot the actual CLI"
  brand_color: "#2563EB"

policy:
  allow_implicit_invocation: true
```

This file is Codex-specific. It configures:
- `display_name`: User-facing name in the Codex app
- `short_description`: Shown in skill selector
- `brand_color`: Accent color in Codex UI
- `allow_implicit_invocation`: Whether Codex can auto-activate based on task context

Other platforms silently ignore this file.

### README.md Template

```markdown
# actual CLI Skill

A feature-complete AI companion for the [actual CLI](https://github.com/actual-software/actual-cli),
an ADR-powered CLAUDE.md/AGENTS.md generator.

## What it does

- Runs and troubleshoots `actual sync` with pre-flight → dry-run → execute → diagnose → retry
- Covers all 5 runners (claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli)
- Covers all 3 output formats (claude-md, agents-md, cursor-rules)
- Includes error catalog, config reference, runner guide, and diagnostic script
- Works as inline knowledge AND operational automation

## Install

### Claude Code (recommended)

```bash
# Add this repo as a marketplace
/plugin marketplace add actual-software/actual-skill

# Install the plugin
/plugin install actual-cli@actual-cli-skills
```

### Codex (OpenAI)

```
$skill-installer install the actual skill from actual-software/actual-skill
```

### OpenCode / Cursor / Manual

Clone and symlink to your global skills directory:

```bash
git clone https://github.com/actual-software/actual-skill.git ~/.local/share/actual-skill

# For OpenCode
ln -s ~/.local/share/actual-skill/skills/actual ~/.config/opencode/skills/actual

# For Cursor
ln -s ~/.local/share/actual-skill/skills/actual ~/.cursor/skills/actual

# For Claude Code (alternative to marketplace)
ln -s ~/.local/share/actual-skill/skills/actual ~/.claude/skills/actual

# For Codex (alternative to $skill-installer)
ln -s ~/.local/share/actual-skill/skills/actual ~/.agents/skills/actual
```

## Requirements

- The [actual CLI](https://github.com/actual-software/actual-cli) installed (`cargo install actual-cli`)
- At least one runner configured (see `actual runners`)

## License

Apache-2.0
```

---

## 6. Tier 3: Gallery Submission

### Target

PR to [github.com/anthropics/skills](https://github.com/anthropics/skills) — the official
Agent Skills example gallery with 73k stars.

### Requirements

1. Standalone repo must exist first (Tier 2)
2. Skill should be validated by external users
3. Must conform to gallery structure (skills/ subdirectory with SKILL.md)
4. Should include a LICENSE (Apache-2.0 compatible)

### Submission Process

1. Fork `anthropics/skills`
2. Add `skills/actual-cli/` directory with the skill files
3. Open PR with description explaining the skill's purpose
4. Wait for review from Anthropic maintainers

### Timing

Not yet. Wait until:
- The standalone repo is created and tested
- The skill has been used by at least a few external users
- Any feedback from real usage has been incorporated

---

## 7. Install Instructions by Platform

### Claude Code (in-repo, automatic)

When working in a repo that contains `.claude/skills/actual/SKILL.md`, the skill is
automatically discovered. Ask Claude about the actual CLI, or invoke directly:

```
/actual run sync
/actual fix ClaudeNotFound
/actual set up anthropic-api
```

### Claude Code (marketplace, from standalone repo)

```
/plugin marketplace add actual-software/actual-skill
/plugin install actual-cli@actual-cli-skills
```

### OpenCode (in-repo, automatic)

When working in a repo that contains `.opencode/skills/actual/SKILL.md` (or the symlink),
the skill is automatically discovered via the `skill` tool.

### OpenCode (global install)

```bash
git clone https://github.com/actual-software/actual-skill.git /tmp/actual-skill
ln -s /tmp/actual-skill/skills/actual ~/.config/opencode/skills/actual
```

### Codex (in-repo, automatic)

When working in a repo that contains `.agents/skills/actual/SKILL.md` (or the symlink),
the skill is automatically discovered.

### Codex (remote install)

```
$skill-installer install the actual skill from actual-software/actual-skill
```

### Cursor (in-repo, automatic)

When working in a repo that contains `.agents/skills/actual/SKILL.md` (or the symlink
through `.claude/skills/actual/`), the skill is automatically discovered.

### Cursor (global install)

```bash
git clone https://github.com/actual-software/actual-skill.git /tmp/actual-skill
ln -s /tmp/actual-skill/skills/actual ~/.claude/skills/actual
```

---

## 8. Frontmatter Compatibility

### Current Frontmatter

```yaml
---
name: actual
description: >-
  Feature-complete companion for the actual CLI, an ADR-powered
  CLAUDE.md/AGENTS.md generator. ...
argument-hint: "[question or command] e.g. 'run sync', 'set up anthropic-api', 'fix ClaudeNotFound'"
---
```

### Compatibility Analysis

| Field           | In Spec? | Claude Code | OpenCode | Codex | Cursor |
|-----------------|----------|-------------|----------|-------|--------|
| `name`          | Yes      | Used        | Used     | Used  | Used   |
| `description`   | Yes      | Used        | Used     | Used  | Used   |
| `argument-hint` | No (CC)  | Used        | Ignored  | Ignored | Ignored |

The `argument-hint` field is a Claude Code extension. Per the Agent Skills spec:
"Unknown frontmatter fields are ignored." This means all platforms will successfully
parse the SKILL.md — they'll just skip `argument-hint`.

### Recommended Addition

Add the `compatibility` field to signal runtime requirements:

```yaml
compatibility: Requires the actual CLI (cargo install actual-cli)
```

This is an optional spec field (max 500 chars) that helps platforms surface
environment requirements. Currently no platform enforces it, but it's good
documentation.

### Fields We Do NOT Use (and why)

| Field                      | Why Not                                                |
|----------------------------|--------------------------------------------------------|
| `disable-model-invocation` | We WANT auto-discovery — this is a knowledge+ops skill |
| `user-invocable`           | Defaults to true, which is correct                     |
| `allowed-tools`            | No tool restrictions needed                            |
| `context: fork`            | Not a subagent skill — runs inline                     |
| `license`                  | Could add "Apache-2.0" but not required                |

---

## 9. Versioning and Sync Strategy

### Versioning

The skill version should be **independent of the CLI version**. Rationale:
- CLI releases don't always change the skill (e.g., performance fixes)
- Skill updates don't always require CLI changes (e.g., improved docs)
- Independent versioning avoids phantom releases

Recommended: Start at `1.0.0`, bump `MINOR` for new content, `PATCH` for fixes.

### Sync: In-Repo to Standalone

When the CLI changes in ways that affect the skill:
1. Update `.claude/skills/actual/` in the CLI repo (canonical)
2. Copy changes to the standalone repo
3. Bump version in `plugin.json`
4. Commit and push the standalone repo

**Future automation**: A GitHub Action on the CLI repo could auto-create a PR
on the standalone repo when skill files change. The CI workflow
`.github/workflows/skill-update-reminder.yml` (PR #367) already detects
skill-relevant source changes — it could be extended to trigger a cross-repo PR.

### What Triggers a Skill Update

| CLI Change                          | Skill Files Affected                    |
|-------------------------------------|-----------------------------------------|
| New error variant in `src/error.rs` | `references/error-catalog.md`           |
| New config key in `src/config/`     | `references/config-reference.md`, SKILL.md |
| New runner                          | `references/runner-guide.md`, SKILL.md  |
| New output format                   | `references/output-formats.md`, SKILL.md |
| New CLI flag                        | SKILL.md (Commands table)               |
| Sync pipeline change                | `references/sync-workflow.md`           |
| New binary name                     | `scripts/diagnose.sh`                   |

---

## 10. Sources Consulted

| Source | URL | Key Finding |
|--------|-----|-------------|
| Agent Skills Spec | agentskills.io/specification | Canonical format: SKILL.md with name+description frontmatter, <500 lines, progressive disclosure |
| Agent Skills Intro | agentskills.io/what-are-skills | Discovery → activation → execution flow; 27+ adopters |
| Agent Skills Integration | agentskills.io/integrate-skills | Filesystem-based vs tool-based agents; XML metadata injection |
| Anthropic Skills Repo | github.com/anthropics/skills | 73k stars; .claude-plugin/ structure for marketplace; example skills |
| Agent Skills Spec Repo | github.com/agentskills/agentskills | Apache 2.0; skills-ref validation library |
| Claude Code Skills Docs | code.claude.com/docs/en/skills | Frontmatter extensions (context, agent, hooks, model); plugin integration; `$ARGUMENTS` |
| Claude Code Plugins Docs | code.claude.com/docs/en/plugins | plugin.json schema; marketplace.json; `${CLAUDE_PLUGIN_ROOT}`; strict mode |
| Claude Code Plugin Marketplaces | code.claude.com/docs/en/plugin-marketplaces | Source types (github, url, npm, pip); version resolution; release channels |
| Claude Code Plugins Reference | code.claude.com/docs/en/plugins-reference | Full manifest schema; caching; directory structure; CLI commands |
| Codex Skills Docs | developers.openai.com/codex/skills | .agents/skills/ path; agents/openai.yaml; $skill-installer; progressive disclosure |
| Codex Customization | developers.openai.com/codex/concepts/customization | AGENTS.md + skills + MCP layering; skill scopes (REPO, USER, ADMIN, SYSTEM) |
| OpenCode Skills Docs | opencode.ai/docs/skills | .opencode/, .claude/, .agents/ discovery; permission config; validation |
| Cursor (via agentskills.io) | agentskills.io (adopter list) | Confirmed adopter; reads .claude/ and .agents/ paths |

---

## 11. Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Canonical skill location is `.claude/skills/actual/` | Claude Code is the primary platform; other paths are symlinks to avoid duplication |
| 2 | Use symlinks (not copies) for .opencode/ and .agents/ | Single source of truth; no drift between copies; all platforms follow symlinks |
| 3 | Skill version independent of CLI version | Decouples release cadences; avoids phantom releases |
| 4 | Standalone repo named `actual-software/actual-skill` | Clear, discoverable; follows `org/tool-skill` convention |
| 5 | plugin.json name is `actual-cli` (not `actual`) | Avoids ambiguity; matches the CLI binary name; skills get namespaced as `/actual-cli:actual` |
| 6 | Don't remove `argument-hint` for cross-platform compat | Spec says unknown fields are ignored; removing it degrades Claude Code experience for no benefit |
| 7 | Gallery submission (Tier 3) deferred | Need external validation first; skill should be battle-tested |
| 8 | No agents/openai.yaml in-repo | Codex-specific metadata only makes sense in the standalone repo |
| 9 | Keep DESIGN.md in-repo (not in standalone) | DESIGN.md is an implementation guide for contributors, not for end users |
| 10 | Marketplace catalog is self-referencing (source: ".") | The plugin IS the marketplace repo; simplest possible structure |
