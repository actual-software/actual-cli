# Actual.ai Skill and CLI in depth

The detail behind the [README](../README.md): what the skill and CLI do and the commands behind each capability.

## What are the Actual.ai Skill and CLI?

The Actual.ai CLI checks your coding agent's plans and changes against your team's Architectural Decision Records (ADRs), committed as rule files in `.actual/rules/`. The [Actual.ai Skill](https://github.com/actual-software/actual-skill) is its Claude Code plugin: it runs the checks at the right moments, briefs your agent on the rules for each file it reads, and manages the CLI for you.

## Why do I need the Actual.ai Skill and CLI?

Coding agents don't know the architecture decisions your team has already made, so they write code that ignores them. The skill puts the rules for each file in front of your agent and checks its plans and changes against them, running the CLI underneath, so its code follows your architecture from the start. You don't have to remember a command to get there.

## Who are the Actual.ai Skill and CLI for?

AI-native software teams: developers who build with coding agents and want plans and code grounded in the architecture decisions their team has already made. Most teams install the skill and let it manage the CLI. Teams wiring Actual into scripts or CI can also call the CLI directly.

## Where do the Actual.ai Skill and CLI work?

The checks and rule briefing run in Claude Code, through the skill's hooks. Outside Claude Code there's no hook to run them, so the CLI runs on its own in your terminal and in CI. Either way it judges your rules with one of five runners: `claude-cli` (the default), `anthropic-api`, `openai-api`, `codex-cli`, and `cursor-cli`.

| Platform | Architecture | Install method |
|----------|--------------|----------------|
| macOS | Apple Silicon (arm64) | Actual.ai Skill, Homebrew, npm, manual download |
| macOS | Intel (x64) | Actual.ai Skill, Homebrew, npm, manual download |
| Linux (glibc 2.39 or newer) | x64 | Actual.ai Skill, Homebrew, npm, manual download |
| Linux (glibc 2.39 or newer) | arm64 | Actual.ai Skill, Homebrew, npm, manual download |

Windows is not currently supported.

On Linux, every install method in the table puts a prebuilt binary on your machine that needs glibc 2.39 or newer: Ubuntu 24.04, Debian 13, Fedora 40, RHEL 10, or a later release of any of them. Run `ldd --version` to see which glibc you have. Ubuntu 22.04, Debian 12, RHEL 9 and Amazon Linux 2023 ship an older one. There the arm64 binary exits with ``version `GLIBC_2.39' not found``, and the x64 binary runs but prints ``weak version `GLIBC_2.39' not found`` every time it starts. On those releases, build from source instead (see [Build from source](GETTING_STARTED.md#build-from-source)).

## When do the Actual.ai Skill and CLI run?

Whenever your agent works in a repository that has rules. The skill decides when each command runs: `actual plan-check` when your agent leaves plan mode, before implementation starts, `actual impl-check` at the end of every turn, and `actual rules brief` as your agent reads a file. In a terminal or CI, the CLI doesn't run until you call it.

## How do the Actual.ai Skill and CLI work?

Each capability below is something the skill does for you. It wraps a CLI command. If you're scripting or running in CI, open the Direct CLI section under each one.

### Governance

In Claude Code, the skill checks your agent's work against the rule documents committed in `.actual/rules/`. When your agent leaves plan mode, the skill runs `actual plan-check` on the plan before you see the approval dialog. At the end of every turn, it runs `actual impl-check` on the changes. A conflict goes back to the agent with the rule and the reason, and the turn continues until it's fixed.

Governance never gets in the way of unrelated work. Infrastructure failures fail open, and an unresolved rule stops blocking after three denied rounds per session. To clear a denied rule for the rest of the session, type this as a message in the session, or pick the rule in the dialog that opens when a gate denies:

```
/actual-cli:override <rule-id> [code] [reason]
```

`/actual-cli:override all` clears every rule a gate is blocking now. You can also run `actual check-override --session <id> --rule <doc-slug>::<rule-id>` from an ordinary terminal, outside Claude Code, with an optional `--reason "..."` and `--reason-code`. The code is one of `misread`, `not-here`, `adr-wrong`, `adr-outdated` or `accepted`; in the session, it is the first word after the target. Telemetry sends the code. When the code is `other` (the reason names no code), it also sends the typed text, with anything that looks like a secret stripped, cut to 280 characters and shown only to your organization's admins. Words typed after one of the five named codes stay on your machine. See [PRIVACY.md](../PRIVACY.md). An override applies to both gates for the rest of the session.

Every round and override is recorded in `~/.actualai/actual/plan-check-overrides.log`. Governance relies on Claude Code hooks, which the skill installs.

`plan-check` and `impl-check` report one of four outcomes: `conforming`, `conflicting`, `requires_decision` (the plan deliberately supersedes a rule and needs human review), or `not_checked`. Only `conflicting` exits non-zero, so a CI job that needs to tell "checked and clean" from "could not check" should read the `status` field from `--json`.

<details>
<summary><strong>Direct CLI</strong></summary>

```bash
actual plan-check --plan-file plan.md --json
actual impl-check                     # working tree vs HEAD, including untracked files
git diff HEAD | actual impl-check
```

With `--claude-hook`, both commands speak Claude Code's hook contract, which is how the skill manages them in Claude Code.

</details>

### Rules in context

Your agent is briefed as it works. When it reads a file, the skill runs `actual rules brief`, so the rules that govern that file reach the agent before its first edit. A file it writes without reading first, usually a new one, is briefed when it writes it. `actual rules` also shows you which rules apply where: `rules ls`, `rules index` and `rules brief` use no model and no network, and `rules select` asks a runner only to rank more matches than it can keep.

<details>
<summary><strong>Direct CLI</strong></summary>

```bash
actual rules ls                       # list rule documents, level counts, warnings
actual rules select "Add a caching layer in front of the user repository."
actual rules select --file src/api/   # the rule documents that govern a path
actual rules brief --file src/api/users.ts
```

Rule files may open with a YAML header (ADR file header v2) that declares the decision's id, status, root and path scope; files without one work as before, and the two can share a directory. A decision whose status is `draft`, `deferred`, `rejected` or similar does not govern. See [Rule selection](RULE_SELECTION.md) and [Scope index](SCOPE_INDEX.md) for details.

</details>

### Sign-in

Signing in is optional, since the checks and `rules` run signed out. Ask your agent to "sign me in to Actual AI." The skill runs `actual login`, which signs you in through your browser and stores your credentials locally.

<details>
<summary><strong>Direct CLI</strong></summary>

`actual whoami` shows the signed-in account, organization, and scopes, and `actual logout` signs you out. For CI and headless agents, see [Agent authentication](AGENT_AUTH.md): `actual auth create-token` mints a scoped access token from an existing session, and `actual mint-token` signs one from a service-account key when there is no human to sign in.

</details>

### Troubleshooting

Ask your agent to "diagnose why Actual is failing." The skill knows every error the CLI can return and all five runners, so it can find the cause and retry without you reading logs.

<details>
<summary><strong>Direct CLI — Commands the skill wraps</strong></summary>

```
actual plan-check     # check a plan against the rules selected for it
actual impl-check     # check a git diff against the rules selected for it
actual check-override # human override for a rule plan-check/impl-check denied
actual rules          # list, index, and select rule documents under .actual/rules/ (ADR file header v2 or the original format)
actual login          # sign in to your Actual AI account via browser OAuth
actual logout         # sign out and clear local credentials
actual whoami         # show the signed-in account and organization
actual adr-bot        # analyze the repo and write AI context files
actual status         # check output file state (managed markers, staleness)
actual auth           # check the runner's auth, or mint a scoped access token
actual advisor        # ask the Advisor an org-scoped architecture question
actual mint-token     # mint a token from a service-account key (no browser)
actual config         # show, set, or locate configuration
actual runners        # list available AI backend runners
actual models         # list known model names grouped by runner
actual cache clear    # clear local analysis and tailoring caches
```

</details>

### Configuration

The skill manages the CLI with its defaults, so most teams never touch configuration. The CLI's config lives at `~/.actualai/actual/config.yaml` and is created on first run. See [Getting Started](GETTING_STARTED.md) for every runner setting, config key, and environment variable override.

### Documentation

See the [Actual.ai Skill README](https://github.com/actual-software/actual-skill) for skill setup, and the [Actual CLI docs](https://actual.ai/cli/docs) for the full command reference the skill wraps. Point your agent at [docs.md](https://actual.ai/cli/docs.md) for the same docs in Markdown. [adr-bot and the Advisor](ADR_BOT_AND_ADVISOR.md) covers those two commands, and [ADR commands](ADR_COMMANDS.md) covers writing decisions and managing them in Actual AI with `actual adr`.

### Requirements

- The [Actual.ai Skill](https://github.com/actual-software/actual-skill) in Claude Code, for the checks and rule briefing. The skill offers to install the CLI on first use.
- For the checks: a configured runner. Claude Code CLI is the default; `actual runners` lists the rest. The checks and `rules` need no Actual AI account.
