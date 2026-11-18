# In-session overrides: the `check-override --claude-hook` contract

When `plan-check` or `impl-check` denies a rule, the user can override it without leaving the session by typing `/actual-cli:override <rule-id|all> [code] [reason]` as a message, or by choosing an override in the dialog the actual-cli plugin raises. The plugin's mod records either one by running `actual check-override --claude-hook`. This page is the contract between the two. The plugin lives in a separate repository and relies on everything below.

## Invocation

```bash
actual check-override --claude-hook [--via prompt|dialog] [--gate plan|impl [--list-blocking]] [--repo <path>] [--rules-dir <path>]
```

| Flag | Meaning |
|---|---|
| `--claude-hook` | Read one hook envelope on stdin instead of taking `--session` and `--rule`. No terminal is needed. |
| `--via prompt` | The user typed the command as a message. Recorded as source `session_prompt`. The default. |
| `--via dialog` | The user chose an override in the plugin's dialog. Recorded as source `session_dialog`. Only valid with `--claude-hook`. |
| `--gate plan`, `--gate impl` | The gate whose denial this answers. `all` then covers only the rules that gate is blocking now, and a bare rule id is matched against that gate's blocking rules first. Without it, both gates' blocking rules count. The dialog passes it; a typed command does not. Only valid with `--claude-hook`. |
| `--list-blocking` | Record nothing: print the rules the `--gate` is blocking now (see [Listing a gate's blocking rules](#listing-a-gates-blocking-rules)). Requires `--gate`. |
| `--repo`, `--rules-dir` | Which governed context to override, resolved the same way the gates resolve it. Both default as they do for `plan-check`. |

The command refuses to run from an agent's own shell: one carrying Claude Code's `CLAUDECODE` marker, or Codex's `CODEX_THREAD_ID` or `CODEX_SESSION_ID`. It exits with code 2, prints why on stderr, prints nothing on stdout and records nothing. Only the mod's own spawn is expected to call it.

## Input

A Claude Code `UserPromptSubmit` hook envelope as JSON on stdin. The fields read are:

| Field | Required | Meaning |
|---|---|---|
| `hook_event_name` | yes | Must be `UserPromptSubmit` |
| `session_id` | yes | Claude Code's session id. Must be non-empty |
| `prompt` | yes, except with `--list-blocking` | The user's message |

The prompt must start with `/actual-cli:override` or a bare `/override`. Another plugin's `/<plugin>:override` is not matched. The first word after the command is the target, and anything after it is the reason:

- `all` overrides every rule a gate is blocking now: a rule the gate's latest check denied that isn't already overridden. With `--gate`, only that gate's.
- A rule id, bare (`R-A-001`) or full (`<doc-slug>::<rule-id>`), overrides that rule. A rule a gate is blocking now wins over others with the same id; otherwise any rule in the corpus matches. A rule that isn't blocking is still overridden, and the reply says so.

The reason's first word can be a reason code: `misread`, `not-here`, `adr-wrong`, `adr-outdated` or `accepted`, matched without regard to case or trailing punctuation. That word is stored as the override's code and the rest as its text. A reason that starts with any other word is stored whole, with the code `other`, and no reason means no code. So the mod's dialog sends a picked code as `/actual-cli:override all misread` and passes text typed under "Other" through unchanged. Other text that starts with a code word is read the same way: `accepted, in a hurry` is stored as the code `accepted` with the text `in a hurry`, and that text is not sent. CLI 0.13.0 and earlier store the whole reason as text, code word included. Telemetry sends the code, and for `other` from a signed-in run the typed text as well, stripped of anything that looks like a secret and cut to 280 characters; see [PRIVACY.md](../PRIVACY.md).

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

**Listing a gate's blocking rules.** With `--list-blocking`, any envelope with a `session_id` prints the rules the `--gate` is blocking now, sorted, and records nothing:

```json
{ "blocking": ["<doc-slug>::<rule-id>", "..."] }
```

This is the set `/actual-cli:override all` with the same `--gate` records, so a dialog that lists it shows exactly what choosing "Override all" will turn off. Read it from here rather than from the deny text: the deny text quotes the plan or diff, which the agent writes.

**Nothing recorded.** The command named no rule, no rule matched, a bare id was ambiguous, or `all` found nothing blocking. The output has `hookSpecificOutput` with an explanation for the agent, and no `overridden` field.

## What is recorded

Each overridden rule gets an entry in the session file and a line in `~/.actualai/actual/plan-check-overrides.log` with its source (`session_prompt` or `session_dialog`), the reason and its code if one was given, and which gate was blocking it (`plan`, `impl`, `both`, or none). An override applies to both gates for the rest of the session.

## Codex

Codex has no mod, so the plugin's Codex hooks reach the same overrides another way. Two pieces work together: the gates' `--codex-ask` flag, and `impl-check --codex-hook` on `UserPromptSubmit`.

**`--codex-ask` on `plan-check --codex-hook` and `impl-check --codex-hook`.** A conflict ends the turn instead of sending the agent back. The reply is:

```json
{ "continue": false, "stopReason": "..." }
```

Codex shows `stopReason` to the user and never to the model, and it outranks a block from another `Stop` hook running alongside. The text lists the conflicts and the two answers: reply to have the agent revise, or type `actual override <rule-id|all> [code]`. The conflicts are also kept in the session, worded for the agent, until the user's next message. They are dropped by the gate's next judged round, by a round that finds nothing blocking, and once overrides leave nothing blocking. Without `--codex-ask`, a conflict is `{"decision":"block","reason":…}`, which sends the agent back to revise at once. The plugin passes the flag unless the run is unattended.

**`impl-check --codex-hook` on `UserPromptSubmit`.** It records the turn start as on Claude Code, then reads the envelope's `prompt`:

- **An override.** The prompt starts with `actual override` (any case) followed by `all`, a rule id with a hyphen, or a full `<doc-slug>::<rule-id>`, or with nothing at all. `/override` and `/actual-cli:override` work too. The target, code and reason are read as above, and the gate whose conflicts are waiting is the one `all` covers. The override is recorded with source `session_prompt`. The reply is `{"continue":false,"stopReason":"Actual AI: overrode ..."}`, which keeps the prompt from reaching the model. It says what was recorded, what still blocks, or why nothing was recorded. A message such as "actual override is confusing" is not an override and reaches the agent.
- **Otherwise, with conflicts waiting.** The reply is `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"..."}}`. It tells the agent that its last turn was stopped on these conflicts and that the user's message answers them, and the conflicts are no longer kept.
- **Otherwise.** Nothing.

An override never comes from the agent's own shell. Codex sets `CODEX_THREAD_ID` and `CODEX_SESSION_ID` there, and a Codex hook ran without either on Codex CLI 0.167.0. When either is set, nothing is recorded, and the stop reason names the variable.

