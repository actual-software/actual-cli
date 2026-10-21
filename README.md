<p align="center">
  <a href="https://actual.ai/cli">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="assets/logo-white-text.png">
      <img src="assets/logo-black-text.png" alt="Actual AI" width="240">
    </picture>
  </a>
</p>

<h1 align="center">Architectural context for coding agents</h1>

<p align="center"><strong>Architecture questions, answered from your ADRs.</strong></p>

<p align="center">
  <a href="https://www.npmjs.com/package/@actualai/actual"><img src="https://img.shields.io/npm/v/@actualai/actual?style=flat" alt="npm"></a>
  <a href="https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml"><img src="https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/actual-software/actual-cli/blob/main/LICENSE"><img src="https://img.shields.io/github/license/actual-software/actual-cli?style=flat" alt="License"></a>
  <a href="https://github.com/actual-software/actual-cli/stargazers"><img src="https://img.shields.io/github/stars/actual-software/actual-cli?style=flat" alt="Stars"></a>
</p>

The Actual.ai CLI answers architecture questions from your team's Architectural Decision Records (ADRs) and cites the decisions behind each answer. It also puts those decisions in your coding agent's context and checks the agent's plans and changes against them.

## Installation

Most teams install the [Actual.ai Skill](https://github.com/actual-software/actual-skill), which runs the CLI for their coding agent and installs it on first use. To run the CLI yourself or in CI:

```bash
npm install -g @actualai/actual
```

It runs on macOS and Linux. [Getting Started](docs/GETTING_STARTED.md#installation) covers Homebrew, prebuilt binaries and building from source.

## Examples

From inside a git repository:

```bash
actual login                  # sign in through your browser
actual advisor "How should we handle database access in a new service?"
actual adr-bot                # write ADR guidance tailored to this repo into CLAUDE.md
actual impl-check             # check your working tree against .actual/rules/
```

## Documentation

- [Actual.ai Skill and CLI in depth](docs/HOW_IT_WORKS.md): what they do and the commands behind each capability.
- [Getting Started](docs/GETTING_STARTED.md): installation, runners, flags and configuration.
- [Command reference](https://actual.ai/cli/docs) on actual.ai, also as [Markdown](https://actual.ai/cli/docs.md) for your agent.

## Privacy & Telemetry

The CLI collects minimal telemetry to improve the product, whether the skill manages it for you or you run it directly. Raw ids, source code, and file paths are never sent. Events sent while you're signed in carry identity-linked one-way hashes; signed-out events are pseudonymous, not anonymous. Turn telemetry off with `ACTUAL_NO_TELEMETRY=1`, the config file, or a compile-time feature flag. See [PRIVACY.md](PRIVACY.md) for who can attribute the data and how long it's kept.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, coding standards, and PR guidelines. Changes to how agents call the CLI, such as prompts, hooks, and plugin setup, belong in [actual-skill](https://github.com/actual-software/actual-skill).

To report a vulnerability, **do not open a public issue**. See [SECURITY.md](SECURITY.md) for responsible disclosure.

## License

MIT
