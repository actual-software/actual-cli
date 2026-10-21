<h1 align="center">Actual.ai CLI</h1>

<p align="center">
  <a href="https://actual.ai/cli">
    <img src="assets/logo.png" alt="Actual.ai" width="160">
  </a>
</p>

<p align="center"><strong>Architecture questions, answered from your ADRs.</strong></p>

<p align="center">Use it through the <a href="https://github.com/actual-software/actual-skill">Actual.ai Skill</a>, which wraps the Actual.ai CLI for your coding agent.</p>

<p align="center">
  <a href="https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml"><img src="https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/actual-software/actual-cli/blob/main/LICENSE"><img src="https://img.shields.io/github/license/actual-software/actual-cli" alt="License"></a>
  <a href="https://github.com/actual-software/actual-cli/stargazers"><img src="https://img.shields.io/github/stars/actual-software/actual-cli?style=flat" alt="Stars"></a>
  <a href="https://github.com/actual-software/actual-cli/issues"><img src="https://img.shields.io/github/issues/actual-software/actual-cli" alt="Issues"></a>
</p>

## Quickstart

### Install Actual.ai Skill — Claude Code CLI

Start with the [Actual.ai Skill](https://github.com/actual-software/actual-skill). The skill wraps the Actual.ai CLI so your coding agent can use it, and installs the CLI for you on first run. Run each step as its own copy/paste.

Step 1 — add the actual-skill repo as a marketplace:

```
/plugin marketplace add actual-software/actual-skill
```

Step 2 — install the plugin:

```
/plugin install actual-cli@actual-cli-skills
```

Step 3 — reload plugins so Claude Code CLI loads the newly installed plugin:

```
/reload-plugins
```

Step 4 — run the skill in your coding agent:

```
/actual
```

The first time you run it, the skill asks for your permission to install the Actual.ai CLI for you. For example:

```
/actual

The `actual` CLI is not installed. Should I install it with:

    npm install -g @actualai/actual

Then I'll verify it with `actual --version`.
```

<details>
<summary><strong>Manual install Actual.ai CLI — npm / npx</strong></summary>

The skill installs the CLI for you, so you only need a manual install for scripts and CI or to run the CLI outside an agent.

Step 1 — install the CLI globally:

```
npm install -g @actualai/actual
```

Or skip the install and run any command with `npx @actualai/actual <command>`.

Step 2 — sign in with browser OAuth:

```
actual login
```

Step 3 — ask your first architecture question:

```
actual advisor "Should new services talk over gRPC or REST?"
```

</details>

<details>
<summary><strong>Install Actual.ai CLI — Homebrew</strong></summary>

```bash
brew install actual-software/actual/actual
```

</details>

<details>
<summary><strong>Install Actual.ai CLI — Manual download / Build from source</strong></summary>

Download the binary for your platform from the
[releases repository](https://github.com/actual-software/actual-releases/releases)
(binaries are published separately from source), then:

```bash
chmod +x ./actual
sudo mv ./actual /usr/local/bin/actual

# macOS only: remove quarantine before running
xattr -dr com.apple.quarantine /usr/local/bin/actual
```

To build from source you need a C compiler for the tree-sitter native dependencies:

```bash
# macOS: Xcode command-line tools (usually already installed)
xcode-select --install

# Debian/Ubuntu
sudo apt-get install build-essential

cargo install --git https://github.com/actual-software/actual-cli.git
```

</details>

## Use Actual.ai Skill + CLI

Once the [Actual.ai Skill](https://github.com/actual-software/actual-skill) is installed, ask your agent in plain language. The skill manages the CLI and picks the right command for each request. For example:

- "Set up Actual for this repository."
- "Preview the ADR guidance Actual would add here."
- "Ask the Actual advisor how we should handle database access in a new service."
- "Sign me in to Actual AI."
- "Diagnose why Actual is failing."

In Codex, you can also call the skill directly by starting the prompt with `$actual`.

<details>
<summary><strong>Use Actual.ai CLI directly — Terminal examples</strong></summary>

These are the commands the skill manages for you. To run them yourself, sign in and run them from inside any git repository. For example:

```bash
actual advisor "How should we handle database access in a new service?"
actual adr-bot --dry-run      # preview the ADR guidance Actual would write
actual adr-bot                # write it to CLAUDE.md
actual plan-check --plan-file plan.md
actual impl-check             # check your working tree against .actual/rules/
actual whoami                 # show the signed-in account and organization
```

</details>

## Privacy & Telemetry

The CLI collects minimal telemetry to improve the product, whether the skill manages it for you or you run it directly. Raw ids, source code, and file paths are never sent. Events sent while you're signed in carry identity-linked one-way hashes; signed-out events are pseudonymous, not anonymous. Turn telemetry off with `ACTUAL_NO_TELEMETRY=1`, the config file, or a compile-time feature flag. See [PRIVACY.md](PRIVACY.md) for who can attribute the data and how long it's kept.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, coding standards, and PR guidelines. Changes to how agents call the CLI, such as prompts, hooks, and plugin setup, belong in [actual-skill](https://github.com/actual-software/actual-skill).

To report a vulnerability, **do not open a public issue**. See [SECURITY.md](SECURITY.md) for responsible disclosure.

## License

MIT
