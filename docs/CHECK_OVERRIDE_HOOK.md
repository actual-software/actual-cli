# In-session overrides: the `check-override --claude-hook` contract

When `plan-check` or `impl-check` denies a rule, the user can override it without leaving the session by typing `/actual-cli:override <rule-id|all> [reason]` as a message, or by choosing an override in the dialog the actual-cli plugin raises. The plugin's mod records either one by running `actual check-override --claude-hook`. This page is the contract between the two. The plugin lives in a separate repository and relies on everything below.

## Invocation

```bash
actual check-override --claude-hook [--via prompt|dialog] [--repo <path>] [--rules-dir <path>]
```

| Flag | Meaning |
|---|---|
| `--claude-hook` | Read one hook envelope on stdin instead of taking `--session` and `--rule`. No terminal is needed. |
| `--via prompt` | The user typed the command as a message. Recorded as source `session_prompt`. The default. |
| `--via dialog` | The user chose an override in the plugin's dialog. Recorded as source `session_dialog`. Only valid with `--claude-hook`. |
| `--repo`, `--rules-dir` | Which governed context to override, resolved the same way the gates resolve it. Both default as they do for `plan-check`. |

The command refuses to run from an agent's own shell. It exits with code 2, prints why on stderr, prints nothing on stdout and records nothing. Only the mod's own spawn is expected to call it.

## Input

A Claude Code `UserPromptSubmit` hook envelope as JSON on stdin. The fields read are:

| Field | Required | Meaning |
|---|---|---|
| `hook_event_name` | yes | Must be `UserPromptSubmit` |
| `session_id` | yes | Claude Code's session id. Must be non-empty |
| `prompt` | yes | The user's message |

The prompt must start with `/actual-cli:override` or a bare `/override`. Another plugin's `/<plugin>:override` is not matched. The first word after the command is the target, and anything after it is the reason:

- `all` overrides every rule a gate is blocking now: a rule the gate's latest check denied that isn't already overridden.
- A rule id, bare (`R-A-001`) or full (`<doc-slug>::<rule-id>`), overrides that rule. A rule a gate is blocking now wins over others with the same id; otherwise any rule in the corpus matches. A rule that isn't blocking is still overridden, and the reply says so.

## Output

Exit code 0 in every case below. Stdout is one JSON object, or nothing.

**Not an override command.** Anything else (another event, no `session_id`, unparseable JSON, a prompt that isn't `/actual-cli:override` or `/override`) prints nothing and records nothing. The mod should treat silence as "not ours".

**Override recorded.**

```json
{
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "The user overrode <keys> via /actual-cli:override (reason: ...). ..."
  },
  "overridden": ["<doc-slug>::<rule-id>", "..."]
}
```

`overridden` lists the full keys actually recorded, in order. Its presence is how the mod tells an override that landed from one that didn't, without reading the prose. `additionalContext` is for the agent: it says what was overridden, and with `--via prompt` it tells the agent to retry the blocked action and not to run the command itself. Its wording is not part of the contract.

**Nothing recorded.** The command named no rule, no rule matched, a bare id was ambiguous, or `all` found nothing blocking. The output has `hookSpecificOutput` with an explanation for the agent, and no `overridden` field.

## What is recorded

Each overridden rule gets an entry in the session file and a line in `~/.actualai/actual/plan-check-overrides.log` with its source (`session_prompt` or `session_dialog`), the reason if one was given, and which gate was blocking it (`plan`, `impl`, `both`, or none). An override applies to both gates for the rest of the session.
