# 02 - CLI Design

## Command Structure

```
actual [OPTIONS] [COMMAND]
```

### Primary Command: `actual adr-bot`

The main workflow. Analyzes the repo, fetches ADRs, tailors them, and writes the output file.

```
actual adr-bot [OPTIONS]

Options:
  --dry-run                 Show summary of what would change (combine with --full for complete output)
  --full                    With --dry-run, output the full rendered file to stdout
  --force                   Skip user confirmation (still respects rejection memory)
  --reset-rejections        Clear remembered ADR rejections and show all ADRs again
  --project <PATH>          Target a specific sub-project in a monorepo (can be repeated)
  --model <MODEL>           Override Claude Code model (e.g., "sonnet", "opus")
  --api-url <URL>           Override the ADR bank API endpoint
  --verbose                 Show detailed progress and Claude Code output
  --no-tailor               Skip the local tailoring step; use ADRs as-is from the bank
  --output-format <FORMAT>  Output file format (default: claude-md)
                            Values: claude-md, agents-md, cursor-rules
```

#### Output Format Compatibility

| Format         | Tool / IDE       | File(s) written                        |
|----------------|------------------|----------------------------------------|
| `claude-md`    | Claude Code      | `CLAUDE.md`                            |
| `agents-md`    | Codex CLI        | `AGENTS.md`                            |
| `cursor-rules` | Cursor IDE       | `.cursor/rules/actual-policies.mdc`    |

The default is `claude-md`.  To change it for a single run:

```sh
actual adr-bot --output-format agents-md
actual adr-bot --output-format cursor-rules
```

To change it permanently (persisted in `~/.actualai/actual/config.yaml`):

```sh
actual config set output_format agents-md
```

Only one format is active per `actual adr-bot` run.  Teams that need multiple output
files (e.g., both `CLAUDE.md` and `AGENTS.md`) should run `actual adr-bot` twice with
different `--output-format` values, or configure separate CI jobs targeting each format.

### Utility Commands

```
actual status        Show CLAUDE.md state (use --verbose for full cached analysis and ADR counts)
actual auth          Check Claude Code authentication status (delegates to `claude auth status`)
actual config        View/edit configuration
  actual config show       Print current config
  actual config set <K> <V>  Set a config value (supports dotpath: e.g., `actual config set options.batch_size 15`)
  actual config path       Print config file location
```

> **No `actual init` command.** The config file (`~/.actualai/actual/config.yaml`) is created
> automatically with defaults on the first `actual adr-bot` run if it doesn't exist. Users can
> customize settings afterward via `actual config set`.

### Global Options

```
--config <PATH>      Use a specific config file (overrides default)
--quiet              Suppress non-essential output
--no-color           Disable colored output
--version            Print version
--help               Print help
```

## UX Flow: `actual adr-bot`

### Step 1: Environment Check

```
$ actual adr-bot

    ╔═══════════════════════════════════╗
    ║           actual CLI              ║
    ║     ADR-powered CLAUDE.md         ║
    ╚═══════════════════════════════════╝

✓ Claude Code found (v1.0.x)
✓ Claude Code authenticated (claude.ai / Pro subscription)
✓ Git repository detected
```

If Claude Code is not installed or not authenticated, print a clear error with remediation steps:

```
✗ Claude Code not found
  Install it: npm install -g @anthropic-ai/claude-code
  Then authenticate: claude auth login
```

### Step 2: Repository Analysis

```
Analyzing repository...
  ├── Scanning project structure...
  └── Identifying languages and frameworks...

Detected projects:
  ┌─────────────────────────────────────────────────────┐
  │ Project              │ Languages    │ Frameworks     │
  ├──────────────────────┼──────────────┼────────────────┤
  │ / (root)             │ TypeScript   │ —              │
  │ apps/web             │ TypeScript   │ Next.js, React │
  │ apps/api             │ TypeScript   │ Fastify        │
  │ packages/shared      │ TypeScript   │ —              │
  └─────────────────────────────────────────────────────┘

Proceed with these projects? [Y/n/edit]
```

The `edit` option lets users exclude projects or correct misdetections before the API call.

### Step 3: Fetching ADRs

```
Fetching ADRs from bank...
  ├── Next.js: 12 ADRs
  ├── React: 8 ADRs
  ├── Fastify: 5 ADRs
  ├── TypeScript: 15 ADRs
  └── General: 6 ADRs

  Total: 46 ADRs (32 unique after deduplication)
```

### Step 4: Tailoring ADRs

```
Tailoring ADRs to your repository...
  ├── apps/web (Next.js + React): tailoring 20 ADRs...
  ├── apps/api (Fastify): tailoring 15 ADRs...
  └── packages/shared: tailoring 10 ADRs...

Done (took 12.3s)
```

### Step 5: User Confirmation (Per-File Review)

```
Generated CLAUDE.md files:

━━━ [1] CLAUDE.md (root) ━━━
  General rules: 6 ADRs (TypeScript conventions, commit style, etc.)

━━━ [2] apps/web/CLAUDE.md ━━━
  Next.js + React rules: 14 ADRs
  Reasoning: App Router, server components, Tailwind, testing conventions

━━━ [3] apps/api/CLAUDE.md ━━━
  Fastify rules: 8 ADRs
  Reasoning: Plugin pattern, schema validation, error handling

Skipped 4 ADRs (not applicable to this repo).

Actions:
  [a]ccept all    [r]eject #    [p]review #    [q]uit
>
```

- **Accept**: Write the file (merge with existing content via managed markers)
- **Reject**: Don't write that file. Not remembered across syncs.
- **Preview**: Show the full rendered markdown for a specific file

Previously rejected ADRs (by ID) are pre-filtered before tailoring. Use `actual adr-bot --reset-rejections` to clear.

### Step 6: Write CLAUDE.md Files

```
Writing CLAUDE.md files...
  ├── CLAUDE.md (root): created (6 ADRs)
  ├── apps/web/CLAUDE.md: created (14 ADRs)
  └── apps/api/CLAUDE.md: created (8 ADRs)

✓ 3 files written successfully
  (No backups created -- use git to recover previous versions)
```

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Claude Code not installed | Error with install instructions |
| Claude Code not authenticated | Error pointing to `claude auth login` |
| Not in a git repo | Warning: "Not a git repository. Analysis caching and telemetry will be limited." |
| API unreachable | Error with retry suggestion; offer `--no-tailor` fallback |
| Claude Code subprocess fails | Show stderr, suggest `--verbose` for full output |
| No ADRs match | Info message, suggest broader categories in config |
| Existing CLAUDE.md files | Merge via managed markers; user content outside markers preserved |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Environment error (missing Claude Code, no auth) |
| 3 | API error (unreachable, bad response) |
| 4 | User cancelled |
| 5 | File write error |
