# /actual Skill Design Framework

Research-derived design framework for the `/actual` skill. All implementation beads MUST follow this document.

## Sources Consulted

| # | Source | Key Takeaways |
|---|--------|---------------|
| 1 | Agent Skills Open Standard (agentskills.io/specification) | Frontmatter schema (name, description required), directory structure, progressive disclosure (metadata → SKILL.md → reference files), keep SKILL.md <500 lines, references one level deep, TOC for files >100 lines |
| 2 | Anthropic Skill Authoring Best Practices (platform.claude.com) | "Only add context Claude doesn't already have." Degrees of freedom: LOW for fragile ops, MEDIUM for flexible guidance, HIGH for open-ended. Evaluation-driven development. Feedback loops (run → validate → fix → repeat). Anti-patterns: too many options, deeply nested references, time-sensitive info |
| 3 | Anthropic Engineering Blog on Agent Skills | Skills are "onboarding guides for a new hire." Progressive disclosure is the core design principle. Code execution saves tokens vs. generation. Three levels: metadata (~100 tokens), SKILL.md body, bundled files on demand |
| 4 | Claude Code Skills Docs (docs.anthropic.com) | $ARGUMENTS for user input, allowed-tools for pre-approved tools, context:fork for isolation, string substitution. Skills are folders with SKILL.md + optional scripts/references/assets |
| 5 | Claude Code Sub-agents Docs | Explore agent (fast, read-only), general agent (full tools). Custom agents with tool restrictions. Permission modes for security |
| 6 | Claude Code Hooks/Plugins Docs | Lifecycle integration, plugin distribution via .claude-plugin manifest. Skills can be packaged as plugins |
| 7 | anthropics/skills Repository (github.com/anthropics/skills) | Production examples: PDF skill with bundled Python scripts + reference files. DOCX skill with conditional workflow pattern (create vs. edit). Template skill shows minimal structure |
| 8 | Existing repo skills (check v3.0, audit v3.0) | Multi-phase orchestrator pattern, {{VAR}} template variables, checkpoint.json for resume, sub-agent decomposition with explore/general agents, file-based inter-agent communication, .check/.audit scratch dirs, context budget tables |

## 1. Skill Type: Hybrid Knowledge + Operational

**Decision**: The /actual skill is a **hybrid** — primarily a knowledge skill with embedded operational workflows.

**Rationale** (citing sources):
- It is NOT a multi-phase orchestrator like check/audit. Those are batch automation tools that scan all commits or audit all code across many sub-agents. The /actual skill responds to a single user question or request. (Source 8: existing repo skills analysis)
- It follows the "reference content" pattern from Anthropic's best practices: domain knowledge that runs inline alongside the conversation. (Source 2: "Use when multiple approaches are valid, decisions depend on context")
- But it also OPERATES the CLI, which requires LOW-freedom exact-command workflows with feedback loops. (Source 2: "Low freedom for fragile/error-prone operations")
- The PDF skill from anthropics/skills is the closest analog: knowledge about PDF processing + bundled scripts for deterministic operations. (Source 7)

**Implication**: No checkpoint.json, no scratch directory, no multi-phase orchestration. The agent reads SKILL.md, optionally loads reference files, and acts inline.

## 2. Progressive Disclosure Strategy

**Three levels** (Source 1, 3):

| Level | Content | Token Budget | Loaded When |
|-------|---------|-------------|-------------|
| Metadata | name + description in frontmatter | ~80 tokens | Always (startup) |
| SKILL.md body | Quick reference, runner decision tree, common flags, operational workflows, troubleshooting table, navigation | <3000 tokens (<500 lines) | When skill activates |
| Reference files | sync-workflow.md, runner-guide.md, error-catalog.md, config-reference.md, output-formats.md | Unbounded per file | On demand only |

**What goes WHERE** (Source 2: "Challenge each piece of information"):

In SKILL.md body (things needed for EVERY interaction):
- All 6 commands with exact syntax (compact)
- Runner decision tree (users always need this)
- Most common sync flags (the ones used 80% of the time)
- Troubleshooting quick-lookup table (error → fix, compact)
- Operational workflow patterns (pre-flight → dry-run → sync → diagnose → retry)
- Navigation pointers to reference files

In reference files (things needed for SPECIFIC questions):
- sync-workflow.md: Step-by-step sync internals (only when user asks "what does sync do" or debugging a sync failure)
- runner-guide.md: Deep runner details (only when user asks about specific runner setup/compatibility)
- error-catalog.md: Complete error catalog (only when troubleshooting a specific error)
- config-reference.md: All config keys with defaults (only when configuring)
- output-formats.md: Output format details and merging (only when dealing with output files)

## 3. Operational Workflow Patterns

**Pattern: Pre-flight → Dry-run → Confirm → Execute → Diagnose → Retry** (Source 2: feedback loops)

For `actual sync`:
```
1. PRE-FLIGHT (LOW freedom - exact checks):
   - Run: actual runners  (verify runner available)
   - Run: actual auth     (verify authenticated, if claude-cli)
   - Check model/runner compatibility
   - If any check fails → diagnose and fix before proceeding

2. DRY-RUN (LOW freedom - exact command):
   - Run: actual sync --dry-run [--full] [user's flags]
   - Show the user what would change

3. CONFIRM (HIGH freedom):
   - Ask user if they want to proceed
   - If yes → step 4. If no → done.

4. EXECUTE (LOW freedom - exact command):
   - Run: actual sync [user's flags]
   - Monitor for errors

5. DIAGNOSE (if step 4 failed):
   - Match error against error-catalog.md
   - Run targeted diagnostics (diagnose.sh or specific commands)
   - Identify root cause

6. FIX + RETRY:
   - Apply fix (install binary, set API key, adjust config)
   - Return to step 1
```

**Pattern: Interpret + Act** (for status, config, runners, models):
```
1. Run the command (exact)
2. Interpret the output (MEDIUM freedom)
3. Suggest or apply actions based on context
```

## 4. Frontmatter

```yaml
---
name: actual
description: >-
  Feature-complete companion for the actual CLI, an ADR-powered
  CLAUDE.md/AGENTS.md generator. Runs and troubleshoots actual sync,
  status, auth, config, runners, and models. Covers all 5 runners
  (claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli),
  all model patterns, all 3 output formats (claude-md, agents-md,
  cursor-rules), and all error types. Use when working with the
  actual CLI, running actual sync, configuring runners or models,
  troubleshooting errors, or managing output files.
argument-hint: "[question or command] e.g. 'run sync', 'set up anthropic-api', 'fix ClaudeNotFound'"
---
```

**Decisions**:
- `disable-model-invocation`: NOT set (default false). We WANT auto-activation when user mentions the actual CLI. (Source 8: check/audit set this true because they're manual-trigger-only slash commands; /actual should be discoverable)
- `description`: Written in third person per Source 2. Includes trigger keywords: actual CLI, sync, status, auth, config, runners, models, runner names, output format names, error, troubleshoot. Under 1024 chars.
- `argument-hint`: Shows example invocations. (Source 8: audit skill pattern)
- No `allowed-tools`: The skill runs in the same permission context as the agent. (Source 4: allowed-tools is experimental)
- No `compatibility` field: No special system requirements beyond what the repo already has. (Source 1: "Most skills do not need the compatibility field")

## 5. Reference File Architecture

Five files, each self-contained, one level deep from SKILL.md (Source 1, 2):

| File | Purpose | Estimated Size | Loaded When |
|------|---------|---------------|-------------|
| references/sync-workflow.md | Complete 13-step sync workflow with failure points | ~200 lines | "what does sync do", sync debugging |
| references/runner-guide.md | All 5 runners: install, auth, models, compatibility | ~250 lines | Runner setup, model questions |
| references/error-catalog.md | All 18 error types: cause, hint, diagnosis, fix | ~300 lines | Troubleshooting specific errors |
| references/config-reference.md | All config keys, dotpath, defaults, examples | ~150 lines | Configuration questions |
| references/output-formats.md | All 3 formats, managed sections, merge behavior | ~120 lines | Output format questions |

**Rules** (Source 2):
- Each file has a table of contents at top (files >100 lines)
- Structured for lookup (tables, sections), not sequential reading
- No deeply nested references (no file referencing another reference file)
- Consistent terminology throughout all files
- No time-sensitive information
- Forward slashes only in all paths

## 6. Script Design

**One diagnostic script**: `scripts/diagnose.sh` (Source 2, 7)

**Decision**: Human-readable output, not JSON.

**Rationale**: The agent reads this output and interprets it. Human-readable with clear pass/fail indicators is easier for the agent to parse than JSON (the agent doesn't need to run `jq`). The PDF skill uses Python scripts with structured output; we use bash because our diagnostics are just command-existence and env-var checks, not data processing.

**Rules** (Source 2: "Solve, don't punt"):
- Script handles errors explicitly (check before running each command)
- Script NEVER modifies anything (read-only)
- Script NEVER prints secrets (print "set" not the value)
- Script is self-contained (no dependencies beyond bash, which, command -v)
- Script exits 0 even when diagnostics find issues (exit 1 only for script errors)
- Portable: works on macOS and Linux

**When to use script vs. inline commands**:
- Use the diagnostic script for comprehensive environment checks (combines ~20 individual checks into one invocation, saves tokens)
- Use inline commands for targeted single checks (e.g., just `actual auth` when user asks about auth specifically)

## 7. Error Handling Patterns

**Pattern: Match → Diagnose → Fix → Verify** (Source 2: feedback loops)

```
1. MATCH: When actual CLI returns an error, match it against the
   error-catalog.md lookup table (error name → cause → fix)

2. DIAGNOSE: Run targeted checks for that error category:
   - Auth errors → check binary, check auth status
   - API errors → check API key env vars, check credits
   - Config errors → check config file, permissions
   - Timeout → check invocation_timeout_secs

3. FIX: Apply the resolution:
   - Auto-fixable (install binary, set config) → do it
   - Manual (add credits, set API key) → give exact instructions

4. VERIFY: Re-run the original check to confirm the fix worked
   - If still broken → escalate with more diagnostics
```

**Degrees of freedom** (Source 2):
- LOW for diagnostic commands (exact commands, no variation)
- MEDIUM for fix application (depends on user's environment)
- LOW for verification (re-run the exact same check)

## 8. Testing Strategy

**Evaluation-driven** (Source 2: "Create evaluations BEFORE writing extensive documentation"):

Three categories of test scenarios:

**Knowledge scenarios** (verify skill provides correct information):
- First-time setup guidance
- Runner selection advice
- Config key lookup
- Sync workflow explanation
- Output format explanation

**Operational scenarios** (verify skill correctly runs commands):
- Run sync with pre-flight checks
- Diagnose and fix auth errors
- Configure settings via config set
- Run diagnostic script and interpret

**Edge cases**:
- Model/runner mismatch detection
- Monorepo --project flag usage
- Timeout diagnosis and fix
- Multiple simultaneous issues

**Iteration process** (Source 2: "Develop skills iteratively with Claude"):
1. Run each scenario without the skill (baseline)
2. Run with the skill loaded
3. Compare: Does the skill improve accuracy and reduce back-and-forth?
4. Iterate on SKILL.md and reference files based on observations

## 9. Anti-Patterns to Avoid

From Source 2 (Anthropic best practices):

| Anti-Pattern | Why It's Bad | What to Do Instead |
|-------------|-------------|-------------------|
| Explaining what PDFs/CLI tools are | Claude already knows | Only add context specific to THIS CLI |
| Offering multiple approaches | Confusing, wastes tokens | Provide ONE default, escape hatch for edge cases |
| Windows-style paths | Breaks on Unix | Always use forward slashes |
| Time-sensitive info | Becomes wrong | Use "current method" / "legacy" sections |
| Deeply nested references | Claude partially reads | Keep all references one level deep |
| Verbose explanations | Wastes context window | Concise, assume Claude is smart |
| Magic numbers in scripts | Claude can't reason about them | Document every constant |
| Punting errors to Claude | Unreliable | Handle errors explicitly in scripts |

Additional anti-patterns from Source 8 (existing skills analysis):

| Anti-Pattern | What to Do Instead |
|-------------|-------------------|
| Monolithic SKILL.md that tries to cover everything | Progressive disclosure with reference files |
| One agent doing all work | Sub-agent decomposition (but NOT needed for this knowledge skill) |
| No checkpointing for long tasks | Not applicable (this is a knowledge skill, not a batch job) |

## 10. Decision Log

| # | Decision | Rationale | Source |
|---|----------|-----------|--------|
| 1 | Hybrid knowledge + operational skill, not multi-phase orchestrator | /actual responds to individual questions/commands, not batch workflows | Sources 2, 7, 8 |
| 2 | No checkpoint.json or scratch directory | Not a batch job; inline knowledge/operations don't need resume | Source 8 (contrast with check/audit) |
| 3 | disable-model-invocation: false (auto-discoverable) | Users should get skill help when they mention "actual" naturally | Sources 2, 4 |
| 4 | Human-readable script output, not JSON | Agent interprets text; avoids jq dependency; simpler | Source 2 |
| 5 | One combined diagnostic script, not 4 separate scripts | Saves tokens (one bash invocation vs. four); comprehensive view | Source 2 ("utility scripts") |
| 6 | Pre-flight → dry-run → confirm → execute pattern for sync | Source 2: "feedback loops for quality-critical tasks"; sync modifies files | Source 2 |
| 7 | All reference files one level deep with TOC | Source 2: "Avoid deeply nested references"; Source 1: "TOC for files >100 lines" | Sources 1, 2 |
| 8 | Description includes exhaustive trigger keywords | Source 2: "Include specific keywords that help agents identify relevant tasks" | Source 2 |
| 9 | No allowed-tools field | Experimental feature; skill runs in existing permission context | Source 4 |
| 10 | Feature-complete: every command, flag, error, config key covered | Epic requirement; skill is the canonical CLI companion | Epic actual-hje |
