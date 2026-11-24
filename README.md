<p align="center">
  <a href="https://actual.ai/cli">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="assets/logo-white-text.png">
      <img src="assets/logo-black-text.png" alt="Actual AI" width="240">
    </picture>
  </a>
</p>

<h1 align="center">Architectural context for coding agents</h1>

<p align="center"><strong>Align coding agents with your architecture decisions</strong></p>

<p align="center">
  <a href="https://www.npmjs.com/package/@actualai/actual"><img src="https://img.shields.io/npm/v/@actualai/actual?style=flat" alt="npm"></a>
  <a href="https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml"><img src="https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/actual-software/actual-cli/blob/main/LICENSE"><img src="https://img.shields.io/github/license/actual-software/actual-cli?style=flat" alt="License"></a>
  <a href="https://github.com/actual-software/actual-cli/stargazers"><img src="https://img.shields.io/github/stars/actual-software/actual-cli?style=flat" alt="Stars"></a>
</p>

The Actual CLI checks your coding agent's plans and changes against your team's architecture decisions. In Claude Code, the [Actual.AI Plugin](https://github.com/actual-software/actual-skill) runs the checks for you: a plan that conflicts with a decision goes back to the agent before you approve it, and so does a change at the end of a turn. The same checks run from your terminal and in CI.

## Installation

Install the Actual.AI Plugin for Claude Code. Run each command on its own, in a Claude Code session:

```
/plugin marketplace add actual-software/actual-skill
```

```
/plugin install actual-cli@actual-cli-skills
```

```
/reload-plugins
```

The first time you run `/actual`, the plugin offers to install the CLI. To install it yourself, or to run it in CI:

```bash
npm install -g @actualai/actual
```

It runs on macOS and Linux. [Getting Started](docs/GETTING_STARTED.md#installation) covers Homebrew, prebuilt binaries and building from source. [`actual self-update`](docs/SELF_UPDATE.md) upgrades a prebuilt binary you downloaded by hand.

## Signing in (optional)

The checks and `actual rules` run signed out, so you can skip this step. To connect the CLI to your Actual AI account, `actual login` signs you in through your browser and stores your credentials locally. `actual whoami` shows the signed-in account and organization, and `actual logout` signs you out.

## Adding decisions

Keep your team's architecture decisions under `docs/adr/`. From your own terminal, `actual adr new` (CLI 0.14.0 or later) writes one there:

```bash
actual adr new "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
```

From a coding agent's shell, it writes a draft instead, which no check reads until its status is accepted. To accept a draft, change `status: draft` to `status: accepted` in the header of both files `adr new` wrote, and the decision file's `Status: proposed` line to `Status: accepted`. An agent that passes `--status accepted` writes an accepted decision to begin with. You can also manage your decisions at [app.actual.ai](https://app.actual.ai) and merge the pull request it opens. Either way, each decision lands in `docs/adr/`, with the rule file the checks read beside it under `.actual/rules/`. `actual rules ls` lists what the CLI read and warns about anything it couldn't.

`actual adr new` writes a decision and its rule file from the terminal, and `actual adr check` runs Actual AI's save check on the decision files under `docs/adr/` before you commit them. Signed in, `actual adr create` saves a decision to your Actual AI account, where `list`, `show`, `update`, `publish` and the other `actual adr` commands manage it. [ADR commands](docs/ADR_COMMANDS.md) covers each one.

## Checking plans and changes

In Claude Code, the plugin runs `actual plan-check` on your agent's plan when it leaves plan mode, before you see the approval dialog, and `actual impl-check` on its changes at the end of every turn. A conflict goes back to the agent with the rule and the reason. The turn continues until it's fixed. To clear a denied rule for the rest of the session, type `/actual-cli:override <rule-id> [code] [reason]` in the session (CLI 0.13.0 or later, and 0.14.0 or later to record the code); [Governance](docs/HOW_IT_WORKS.md#governance) has the details.

From a terminal or in CI:

```bash
actual plan-check --plan-file plan.md   # check a plan against .actual/rules/
actual impl-check                       # check the working tree against HEAD, untracked files included
git diff HEAD | actual impl-check       # check a diff from stdin
```

Each check reports one of four outcomes: `conforming`, `conflicting`, `requires_decision` (the plan deliberately supersedes a rule and needs human review) or `not_checked`. Only `conflicting` exits non-zero, so a CI job that has to tell "checked and clean" from "could not check" should read the `status` field from `--json`. To judge your rules, the checks need a runner: the Claude Code CLI by default, or any other that `actual runners` lists.

## Filtering rules

`actual rules` shows which of your rules apply where:

```bash
actual rules ls                         # list rule documents, level counts and warnings
actual rules select "Add a caching layer in front of the user repository."
actual rules brief --file src/api/users.ts
```

`rules select` names the rule documents that govern a plan, and `rules brief` the rules that govern one file. `rules ls`, `rules index` and `rules brief` use no model and no network, and `rules select` asks a runner only to rank more matches than it can keep (`--no-rank` skips that). In Claude Code, the plugin runs `rules brief` as your agent reads a file, so the rules for that file reach it before its first edit.

## Documentation

- [Actual.AI Plugin in depth](docs/HOW_IT_WORKS.md): governance, rules in context, sign-in and troubleshooting.
- [Getting Started](docs/GETTING_STARTED.md): installation, runners, commands and configuration.
- [ADR commands](docs/ADR_COMMANDS.md): writing and checking ADRs in your repository, and managing your organization's decisions from the terminal.
- [adr-bot and the Advisor](docs/ADR_BOT_AND_ADVISOR.md): ADR guidance written into your agent's context file, and architecture questions answered from your ADRs.

## Privacy & Telemetry

The CLI collects minimal telemetry to improve the product, whether the plugin manages it for you or you run it directly. No event carries raw ids, source code, or file paths, apart from the reason someone types for an `other` override, which a signed-in run sends with secret-looking values stripped. Your sign-in's access token, which names your account, goes with the OpenTelemetry events sent to Actual. Events sent while you're signed in carry identity-linked one-way hashes; signed-out events are pseudonymous, not anonymous. Turn telemetry off with `actual config set telemetry.enabled false` while no `actual adr-bot` run is in progress, since a run that's already going can save the old value back. That one setting stops every telemetry event, including the plan-governance events that reach PostHog; `ACTUAL_NO_TELEMETRY=1` does the same for CI and one-off runs. See [PRIVACY.md](PRIVACY.md) for what the setting leaves running, who can attribute the data and how long it's kept.

Once a day the CLI also asks the npm registry whether a newer version is out, and names the upgrade command for how you installed it. `ACTUAL_NO_UPDATE_CHECK=1` turns that off; see [docs/UPDATE_CHECK.md](docs/UPDATE_CHECK.md).

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, coding standards, and PR guidelines. Changes to how agents call the CLI, such as prompts, hooks, and plugin setup, belong in [actual-skill](https://github.com/actual-software/actual-skill).

To report a vulnerability, **do not open a public issue**. See [SECURITY.md](SECURITY.md) for responsible disclosure.

## License

MIT
