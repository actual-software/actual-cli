# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `plan-check` and `impl-check` take `--max-rules N`, the most individual rules one check judges (default 40, the cap that applied before), and `--selection-only`, which prints as JSON what the judge would be handed — every stage-1 candidate with its stage-1 and stage-2 scores, the selected documents, the gathered rules, whether the cap cut any, and the rank's time — without calling the judge. `rules select --json` lists every stage-1 candidate the same way, under `selection.considered`. See [docs/RULE_SELECTION.md](docs/RULE_SELECTION.md#commands)
- Signed in to Actual's API, the `actual.session.ended` event the plugin's `SessionEnd` hook sends carries the session's token totals per model: `actual.usage.models`, with `actual.usage.input_tokens`, `actual.usage.output_tokens`, `actual.usage.cache_read_tokens` and `actual.usage.cache_write_tokens` in the same order, and `actual.usage.source` saying whether they came from Claude Code's last `cost-state` line (`session_totals`, which also counts subagents and background calls) or from adding up each assistant message in the transcript and its `subagents/` folder (`messages`). The hook reads the transcript on your machine and sends only the counts and the model names, as Claude Code recorded them. A signed-out end never opens the transcript, and every request without your sign-in's token leaves the six attributes out. The scan gives up after 500 ms and the end then carries no totals. The totals are those at the last end the CLI reported, so a session resumed for work that completes no hook check keeps its earlier end's totals. A Codex session's end carries the same totals, once the plugin's Codex hooks send `SessionEnd`: the usage its rollout records for each model response, added up per model with input less the cached tokens Codex counts inside it, under the source `responses`. `actual.event.schema_version` is now `11`. See `PRIVACY.md`
- `actual adr discover` sends an `actual.adr.surveyed` event and `actual adr normalize` an `actual.adr.normalized` event, one per run, signed in or out. Each carries only counts and flags: discover counts the files it listed by what `normalize` would do with each and says whether it compared them with Actual AI; normalize counts the files it converted, re-indexed, left with questions, skipped or left unchanged, says whether it ran with `--dry-run` or `--push`, and counts the decisions `--push` saved and couldn't save. Both add the outcome, exit code, terminal flag and duration the other `actual adr` events send. Neither carries a path, title, id or decision text. `actual.event.schema_version` is now `13`. See `PRIVACY.md`

### Changed
- Rule ids that `actual adr` and `actual adr refresh` write take the first eight hex digits of the decision's id, such as `R-0B6F2A1E-001`, where they took four (`R-0B6F-001`), since four digits often gave two decisions in one repository the same id prefix. Once Actual AI's repository sync writes eight-digit ids too, a rule's prefix matches the sync's, though the sync numbers rules on its own. The next `actual adr refresh` renames the ids only in rule files that are still the text this CLI wrote; it leaves files Actual AI's sync wrote, and files you edited, as they are. Signed out, `actual adr show` and `update` name a decision file's policies by the new ids and refuse the four-digit ones. Overrides and notes that name a four-digit id do not carry over to the new id. Going back to 0.17 after 0.18 refreshed a checkout makes 0.17's next refresh rewrite every governing rule file with its own text, the sync's files included, so keep one version per checkout; `git checkout -- .actual/rules` or the next sync brings the files back
- The conformance judge behind `plan-check` and `impl-check` answers in less text and in parallel. Its prompt numbers the rules, and its answer is one verdict per number, with a quote and a reason only for a rule that applies; a rule that does not apply is its verdict alone, though every rule still needs one. More than 20 rules are judged in calls of at most 20, of near-equal size, sent at once within the same 150-second deadline, so a check takes about as long as its largest call. A judge's time grew by about 1.2 seconds a rule in one call, with timeouts and incomplete answers at 80 rules. An answer in the earlier form, an array keyed by document and rule id, is still read
- Stage 2 of rule selection, the model rank behind `rules select`, `plan-check` and `impl-check`, asks for one score from 0 to 9 per candidate instead of a verdict and a reason for each, so it writes about 150 tokens instead of about 2,400 and answers in about 4 seconds instead of about 16. A score of 7 to 9 counts as `governs`, 3 to 6 as `related` and 0 to 2 as `unrelated`, and the order within each is the index's, as before. Each selected rule's reason is now the index's own evidence for it, the same reason `--no-rank` gives. See [docs/RULE_SELECTION.md](docs/RULE_SELECTION.md)
- A stage-2 rank asks for low reasoning effort and no extended thinking: `--effort low` and `alwaysThinkingEnabled: false` on claude-cli, `model_reasoning_effort=low` on codex-cli. Haiku 4.5 used to think through most ranks and miss the 90-second deadline, and Codex ran at the effort set in your own Codex config. The conformance judge's existing low effort now reaches codex-cli too
- With no `--model` and no `model:` in the config, a stage-2 rank uses `claude-sonnet-5-5` on claude-cli and anthropic-api and `gpt-6.2-sol` on codex-cli, where it used `claude-sonnet-4-6` and Codex's own configured model. The conformance judge and `adr adopt` keep their defaults
- The plan and implementation gates rank before they judge. `plan-check` and `impl-check` run from a hook now make the same stage-2 rank they make when you run them yourself, with the plan or the turn's diff as the query, and the conformance judge sees the rules it keeps. Over 25 merged pull requests from one repository, the rank raised the share of governing rules among the first ten selected by 0.10 to 0.20 on both queries. The gates keep up to 20 documents (`--limit`), so they rank only when the first pass finds more than that. A hook's rank keeps to a 15-second deadline, so the judge keeps the rest of the hook's timeout, and falls back to the first-pass selection when it misses it or fails. `--no-rank` now skips it in hook mode too; the plugin passes it when `ACTUAL_GATE_RANK=off`. Each gate round now makes two model calls instead of one when the first pass finds more rules than `--limit` keeps. See [docs/RULE_SELECTION.md](docs/RULE_SELECTION.md)

### Fixed
- On codex-cli, a stage-2 rank or a `plan-check` or `impl-check` judge run outside a git repository no longer fails with "Not inside a trusted directory". These calls run read-only, so they pass `--skip-git-repo-check`. Tailoring, which writes files, still requires a git repository

## [0.17.0] - 2026-11-24

### Added
- `actual adr discover` lists every ADR under `docs/adr/` by state: synced by Actual AI, managed by this CLI, stale (its doc changed since its rule file was written), missing its rule file, broken, unmanaged (written by hand or by an agent, so no check reads it), a possible duplicate of a decision the repository already has, written by `actual adr-bot` (`legacy`), an orphan rule file, or ignored. Signed in, it also marks a synced decision whose doc differs from Actual AI, since the next sync overwrites that change, and it marks a synced doc with uncommitted changes either way. `actual adr normalize` converts unmanaged ADRs to Actual's format, writes their rule files, and re-indexes stale docs. Nygard, MADR, Context File and adr-bot layouts convert without a model, and sections the format has no place for are carried over word for word. A decision written as prose goes to the configured runner, and no Actual AI account is needed. Whatever is still open becomes a question: `normalize` asks at a terminal, takes answers with `--answers` or the safe answer with `--yes` (the decision is written as a draft), and reads an agent's own extraction with `--extracted`. `--dry-run` shows the change as a diff, `--rename` gives the sync's file name, `--legacy` converts adr-bot's ADRs, and `--push` saves each decision to Actual AI, exiting 3 when a decision was written but not saved. A rule file this CLI writes, `adr new`'s included, now records its doc's path and a digest of what it governs under `metadata.actual.source`. See "ADRs written elsewhere" in `docs/ADR_COMMANDS.md`
- `actual adr refresh` brings the rule files under `.actual/rules/` in step with your decisions, the step the plugin's hooks are to run on every call. Signed out, each accepted decision file under `docs/adr/` gets a rule file, and a file that is deleted or stops being accepted loses it, while one that can't be read keeps it until it reads again. Signed in, each active decision of the connected repository in Actual AI gets one, read within `--timeout-ms` (1,000 by default), an updated decision keeps whatever rule file it has, and when Actual AI is slower or can't be reached every rule file stays as it is. A checkout whose remote names no connected repository is told so once, and looked up again after ten minutes or at once for another remote or organization. A rule file is rewritten only when its decision changed after the file was written, so a file from Actual AI's sync pull request stays as it is until its decision next changes; a record per checkout under `adr-refresh/` in the user config directory keeps what that takes. The refresh also lists, with reasons, each file under `docs/adr/` that isn't normalized (a version 2 ADR header naming its id, passing `actual adr check`) whenever that list changes. Signed in, it also lists the files `actual adr create --all` would send whenever that list changes. When none of the decisions it read governs, it recommends the plugin's `adr-bootstrap` skill for drafting the repository's first ADRs, the first time and again only once that changes, and `--json` names the skill as `recommendation`. It never renews a sign-in itself, starts no renewal while another holds the sign-in, and once Actual AI has turned a renewal down it asks for `actual login` instead of starting another; a record it can't save is reported among its problems. It always exits 0, and `--json` reports what it did with a `message` for a person when there is one. `rules brief`, `plan-check` and `impl-check` never refresh by themselves. See [docs/ADR_COMMANDS.md](docs/ADR_COMMANDS.md#keeping-actualrules-in-step)
- Signed in, `actual adr create --all` sends every decision file under `docs/adr/` that Actual AI doesn't have yet: a person's way to send the files a repository already kept when it was first signed in, and any added later. Each file is checked before it's sent, and nothing under `docs/adr/drafts/` is. A file whose id, in its header or at the start of its name, is one of the repository's decisions is skipped, which leaves that decision as Actual AI holds it, and so are a file whose status doesn't govern and one that looks like a decision there or one saved earlier in the run (`--allow-duplicate` sends a look-alike). Each save names the file's path, so a second save of one file, from any checkout, returns the decision its first save made, and a draft takes the new text; a file unchanged since this machine saved it as a draft isn't sent again, so edits made to that draft in Actual AI stay. A file keeps its own status: a draft becomes a draft and keeps its name, and a file marked accepted is published and renamed to the decision's own names with a person at the terminal, or with no one there saved as a draft that a later run publishes once the file is on the default branch. A run reads the repository's decisions once, reports each file on standard error, stops at the first save whose outcome is unknown (exit 3), sending that file again on the next run, and exits 1 when a file wasn't saved. What each file last did is kept under `adr-create-all/` in the user config directory, one record per organization and repository, and a run that can't save it says so. Signed out it's refused (exit 2), and it sends no telemetry event. See [docs/ADR_COMMANDS.md](docs/ADR_COMMANDS.md#sending-every-decision-file)
- Signed out, `actual adr list`, `show`, `update`, `publish` and `delete` work on the decision files under `docs/adr/`, leaving `docs/adr/drafts/` out as the refresh does, finding a decision by the id its header names, and each change brings the decision's rule file in step at once. `show` names each policy by the rule id its rule file prints, which `update --policy` and `--level` take. `publish` sets a file's status to accepted, only for a file that passes the check an accepted decision has to pass, `delete` removes the file and says whether git can bring it back, and `update` refuses a file with text it would drop when writing the file back. None of them leaves a file there in a status other than accepted: `unpublish` is refused (exit 2) since a draft lives in Actual AI, and `update` refuses a file whose status doesn't govern (exit 3) until `publish` accepts it. `restore`, `accept` and `reject` act on what only Actual AI keeps, so signed out they're refused with the reason, and `restore` says git brings back a deleted file. Signed in, every command works on your decisions in Actual AI as before
- `plan-check` and `impl-check` record each round that reaches a verdict in a local enforcement log, `enforcement.jsonl` in the user config directory, whether a hook ran the check or you ran the command. Each line holds the time, the gate (`plan` or `impl`), `hook` or `direct`, the session id and that gate's round count for a hook round inside a session, the absolute rules directory, the decision (`allow`, `warn` or `block`) and each ADR judged with its strongest verdict. A round with no verdict (nothing applied, no runner, a failed judge call) adds nothing. The log holds no plan, diff, rule text or reason, is readable only by you, keeps at most 90 days and 10 MB, and is never uploaded. It's written whatever the telemetry setting, and `ACTUAL_ENFORCEMENT_LOG=off` stops it. See "Local enforcement log" in `PRIVACY.md`
- `actual stats [--since 7d] [--repo <path>] [--json]` shows how ADR enforcement went on your machine over a period, read from the enforcement log and `plan-check-overrides.log` with no sign-in or network: gate rounds by gate and by hook or direct run, how many blocked, warned and were allowed, the share that passed, and per ADR its checks, breaches, decisions needed, overrides and pass rate, worst first. Overrides are grouped by reason, with round-limit passes and partially judged rounds alongside. A hook call with nothing left to judge is counted on its own line, outside the rounds and the pass rate. The enforcement log is pruned and the override log is not, so when the period's overrides reach back past the oldest round still in the log, the report says how far back the rounds go (`rounds_since` in `--json`). `--since` takes a count of hours, days or weeks such as `24h`, `7d` or `4w`, or `all`, and `--repo` keeps only the rounds and overrides whose rules directory sits inside that repository

### Changed
- The README, `docs/GETTING_STARTED.md`, `docs/HOW_IT_WORKS.md` (now "Actual.AI Plugin in depth") and the new-issue chooser call the plugin by its new title, the Actual.AI Plugin, and the CLI the Actual CLI. Install commands, the plugin id `actual-cli` and the marketplace id `actual-cli-skills` stay as they are
- `actual.adr.created` follows the pathway: where the decision went is `local` files signed out or `cloud` signed in, the unfinished scope or publish step is reported for any signed-in save, a signed-in run adds the saved decision's id as `actual.adr.decision_id`, and a flag the pathway can't take is refused before the run starts and sends nothing. See `PRIVACY.md`
- `actual adr new` is an alias of `actual adr create`, one command that takes both commands' flags: the title as an argument or as `--title`, `--context`, `--policy`, `--scope`, `--status` and `--root` either way, `--deciders` signed out, and `--file`, `--repo` and `--allow-duplicate` signed in. A flag the pathway can't use is refused with the reason, and exits 2. Signed out it writes an accepted decision file under `docs/adr/`, now also in a coding agent's shell, a script or CI, and writes its rule file through the refresh; `--status draft` is refused (exit 2), since a draft lives in Actual AI. Signed in it saves to Actual AI as `create` did, and a `--scope` given with flags is now set on the new decision
- The `actual.adr.reviewed` event names its pathway as `actual.adr.review.pathway`, `local` signed out or `cloud` signed in, and the state the decision was in before the run as `actual.adr.review.prior_status` (`active`, `draft`, `suggested`, `updated`, `archived`, or `other` for a file status none of these covers), so local and cloud review counts stay apart. Signed out, `publish`, `update` and `delete`, which now work on `docs/adr/`, each send one with no decision id, and `accept`, `reject`, `restore` and `unpublish` still send the outcome `not_signed_in`. `actual.event.schema_version` is now `10`. See `PRIVACY.md`
- Signed in, `actual adr create --file` that saves a draft leaves the file and its rule file as they were, where it used to rename both and mark them draft, so no file under `docs/adr/` turns into a draft. A save that ends accepted still renames both to the decision's own names
- The `actual adr` help and `docs/ADR_COMMANDS.md` give each command's two pathways, and no longer say a change to the repository's rule files arrives only through Actual AI's sync pull request
- `docs/HOW_IT_WORKS.md` no longer says every round is recorded in `plan-check-overrides.log`, which holds only overrides, round-limit passes and partially judged hook rounds. It now names the enforcement log as the record of every round

## [0.16.0] - 2026-11-23

### Added
- The rules brief and both gates send an OpenTelemetry event, `actual.filter.evaluated`, describing how their selection narrowed the repository's rule files: which selection it was (`context`, `planning` or `implementation`), the score floor it applied, how many rule files the index holds, how many each step dropped and why (retired, excluded by path, no match, under the floor, capped, judged unrelated, already settled this session, outside the judge's window, or cut by the brief's own limits), and how many were kept, with the kept files' ADR references and best scores. The counts add up to the index size. `plan-check` and `impl-check` send one every round of `--claude-hook` or `--codex-hook`, and its `actual.action.id` joins it to that round's verdicts. `rules brief` sends one from either hook mode only when its brief adds an ADR new to the session. Dropped rule files are never named, and no rule text, plan or diff text, path or glob is sent. `actual.event.schema_version` is now `6`. See `PRIVACY.md`
- `plan-check` and `impl-check` send an OpenTelemetry event from `--claude-hook` or `--codex-hook`, `actual.gate.resolved`, once per ADR when a block on it ends, saying how it ended: `fixed` when a later round judges its rules followed, `not_rechecked` when a later round decides without judging them followed (left out by the selection or the judged window, nothing applied, or judged not applicable), `round_limit` when the round limit lets the call through, `overridden` when `check-override`, `/override`, or a Codex user's `actual override` or picker answer overrides them, and `still_open` from Claude Code's `SessionEnd` hook for a block still open. Codex has no session-end hook, so a Codex session's open blocks send nothing when it ends. A block opens at the first round that denies one of the ADR's rules and ends when the last of them stops blocking, so blocked, fixed and blocked again sends two events, and an ADR whose rules end differently reports the least resolved of them. Each event carries the gate, how it blocked (`plan_deny` for `plan-check`, including in Codex, where it blocks a `Stop`, or `stop_block` for `impl-check`), the judged rounds and denials the block took, and the `actual.action.id` of the round that opened it, which joins it to that round's other events. That round sent a `breached` event for the ADR only if it was the ADR's first block at that gate in the session. A session killed before its end hook sends nothing for its open blocks. The existing events are unchanged. `actual.event.schema_version` is now `7`. See `PRIVACY.md`
- Each run of `actual adr new` and `actual adr create` sends an OpenTelemetry event, `actual.adr.created`: where the decision went (`local` or `cloud`), whether it came from flags or `create --file`, the status it was written or saved with, how many policies it has, whether a `create --file` save left its scope or publish step unfinished, its outcome (`succeeded`, `invalid`, `duplicate`, `not_signed_in` or `failed`) and exit code, whether standard input was a terminal, and how long it took. `create` adds the saved decision's id as `actual.adr.decision_id`, which goes only in a request carrying your sign-in's token. No title, context, policy text, scope glob or file path is sent, and turning telemetry off stops it. `actual.event.schema_version` is now `9`; version 8 changed only what Actual's web app records. See `PRIVACY.md`

## [0.15.0] - 2026-11-19

### Added
- `whoami --check` asks the auth server whether the stored sign-in still works, renewing the access token first when it expires within a minute, under the same lock the other commands renew it with. It exits 2 when the server no longer accepts the sign-in (a sign-out on another machine, a revoke, or a refresh token the server refuses) and 3 when the server can't be reached or fails, so a script can tell a lapsed sign-in from an outage. Plain `whoami` still reads only the local credentials file
- `actual adr check` sends an OpenTelemetry event, `actual.adr.discovered`, once per run, signed in or not: how many decision files it checked, how many failed the save rules and how many likely duplicates it listed, its outcome (`passed`, `failed` or `error`) and exit code, whether standard input was a terminal, and how long it took. Each run of `accept`, `reject`, `publish`, `unpublish`, `delete`, `restore` and `update` sends `actual.adr.reviewed`: the command, its outcome (`succeeded`, `refused` when the CLI's check of the decision's state or of the requested edit turns it down, `not_signed_in` or `failed`) and exit code, the same terminal flag and duration, whether the decision is AI-grouped and how many rows its group action covers, and the decision's id as `actual.adr.decision_id`. The id goes only in a request carrying your sign-in's token, so a signed-out run, a request whose token Actual refuses and any other endpoint send the event without it. Neither event sends a title, context, policy, scope glob, file path or `reject --reason` text, and turning telemetry off stops both. `actual.event.schema_version` is now `3`. See `PRIVACY.md`
- While telemetry is on, at the end of each Claude Code turn that changed files, `impl-check --claude-hook` asks the agent which of the session's ADRs it used, listing every ADR the brief added to its context. With telemetry off, or in a build without telemetry, nothing reads the answer, so it isn't asked; debug mode still asks. The agent answers with the new `actual rules report-use --claim <adr>=<repo-relative-path>:<line>` (or `--plan-claim <adr>=<line>`, or `--none`), and the CLI checks each claim against the turn's diff or plan on your machine. The question counts an ADR only when it changed or decided what the agent wrote, says `--none` is a normal answer, and asks for the line that best shows each ADR's effect. A diff claim up to three lines off still checks out on the nearest added or changed line in the same hunk, and the session state records which line it matched. On a blocked turn the question follows the block's reason; on a passed turn it continues the agent once. Each listed ADR named sends an `actual.adr.reported` event with whether its claim checked out, and every name the question never listed shares one more. Only a verified diff claim sends its file path, and only in a signed-in request. `verified` means the claim's line is one the turn changed or planned; nothing checks that the ADR relates to it. `plan-check` keeps the plan it judged in local session state until that turn's answer is recorded or the next prompt starts, then only its line count. `actual.event.schema_version` is now `5`
- `ACTUAL_TELEMETRY_DEBUG=1` writes every telemetry event the CLI queues, sends or holds back to a local log under `~/.actualai/actual/telemetry-debug/`, and each run that records one prints `telemetry debug: <id> -> <path>` on stderr. Inside a coding-agent hook the id is the session id, so one session's hooks share a file. Each line names the stream, the stage (`queued`, `sent`, `refused`, `retained`, `dropped` or `not_sent`) and the event in the form it goes out. Signed-in lines name the account and organization in a `local_only` field that is never sent, and the token is never logged. With telemetry off the events are still built and logged as `not_sent`, and nothing is spooled or sent. See "Seeing what telemetry sends" in [PRIVACY.md](PRIVACY.md)
- `--codex-hook` on `plan-check`, `impl-check`, `rules brief` and `session summary`, and `--codex-session-start` on `rules brief`, for the actual-cli plugin's Codex hooks. Codex has no `ExitPlanMode` tool, so `plan-check --codex-hook` runs on `Stop` and checks the plan a Plan-mode turn proposed, reading the turn's mode and plan from the session rollout; `impl-check --codex-hook` leaves those turns alone. A conflict is a `{"decision":"block","reason":…}` reply, which sends the agent back to revise. `rules brief --codex-hook` briefs the files a read-only shell command reads (`PostToolUse` on `Bash`) and the files a patch writes (`PreToolUse` on `apply_patch`), with a `systemMessage` line telling the user what each brief carried and for which files, since Codex shows hook context to the model only (`--no-brief-notice` leaves it out). Every reply carries only keys Codex's strict hook parser accepts, informational notices such as "no rule applies" are silent on Codex, and a deny reason names `actual check-override` from a terminal rather than the Claude Code plugin's `/actual-cli:override`. Each command's `--help` names the capability marker `codex-hook`
- `--codex-ask` on `plan-check --codex-hook` and `impl-check --codex-hook`: a conflict ends the Codex turn (`{"continue":false,"stopReason":…}`) and puts the choice to the user, the way the Claude Code plugin's dialog does. The user's next message reaches the agent with the conflicts, so a reply such as "revise" works, and typing `actual override <rule-id|all> [code] [reason]` records the override from inside the session (`impl-check --codex-hook` on `UserPromptSubmit`) without the prompt reaching the agent. An override that arrives from the agent's own shell, which carries `CODEX_THREAD_ID` or `CODEX_SESSION_ID`, is refused, and `check-override --claude-hook` now refuses those markers too
- `--codex-picker` with `--codex-ask`: where the agent can show a picker (`request_user_input`; always in Codex's Plan mode, and in Default mode with Codex's `default_mode_request_user_input` feature on, read from the session rollout), a conflict asks the agent to show the user "Revise" or "Override", and after Override a second picker asking why, with the Claude Code dialog's reason codes. `impl-check --codex-hook` reads the answers from a `PostToolUse` on `request_user_input` and records the override. A picker that goes unanswered ends the turn with the plain `--codex-ask` choices
- `rules brief --max-session-chars` caps the characters of brief one session receives. The first brief that doesn't fit is replaced, once, by a note that briefing has stopped
- In Codex hook modes, when nothing names a runner, the judge tries `codex-cli` and `openai-api` before `claude-cli` and `anthropic-api`

### Changed
- The OpenTelemetry `actual.adr.detected` event a `plan-check` or `impl-check` hook sends for an ADR it judged now carries `actual.adr.verdict`: the strongest verdict the check gave any of the ADR's rules, from `conflicting`, `requires_decision`, `followed` and `not_applicable`, in that order. The event still goes once per ADR per session at each gate, and again each time a later check there gives the ADR a stronger verdict, so a rule that didn't apply on an early Stop and is followed on a later one is reported as `followed`. The word is sent and the quoted line stays on the machine. `actual.event.schema_version` is now `4`
- The judge behind `plan-check` and `impl-check` splits the per-rule verdict `conforming` into `followed` and `not_applicable`. `followed` means the plan or diff falls within the rule's domain and meets it, and its `span` quotes the one line that shows it; `not_applicable` means nothing in the change falls within the rule's domain. Neither blocks, and a rule judged either way is cleared for the session as before. In the `--json` report each rule's `verdict` now reads `followed` or `not_applicable` where it read `conforming`; the report's own `status` still reads `conforming` when nothing conflicts
- The README and `docs/GETTING_STARTED.md` send new decisions to `docs/adr/` through `actual adr new` or the pull request app.actual.ai opens, both of which write the rule file the checks read, and no longer show a rule file to write by hand. Optional sign-in now comes right after installation, and the README and `docs/HOW_IT_WORKS.md` point at this repository's docs for the commands in place of actual.ai/cli/docs

### Fixed
- `check-override` also refuses to run where Codex's shell markers (`CODEX_THREAD_ID`, `CODEX_SESSION_ID`) are set, as it already did under Claude Code's, so an agent in Codex can't override its own denial
- The `codex-cli` judge runs with Codex hooks turned off (`-c features.hooks=false`) and `ACTUAL_CLI_SUBPROCESS=1`, so the actual-cli plugin's own `Stop` hook can't rerun `impl-check` inside the judge's session
- The reminder a check prints while an override is in force names the override's code, such as `accepted`, when it was given no typed reason, instead of saying "no reason given"

## [0.14.0] - 2026-11-16

### Added
- An override records why, as a code: `misread`, `not-here`, `adr-wrong`, `adr-outdated` or `accepted`. In the session, the first word after the target is read as the code (`/actual-cli:override all misread`), and a reason that starts with any other word is recorded as `other`. From a terminal, `check-override --reason-code <code>` sets it, and a `--reason` without one is `other`. The code is stored with the override and in `plan-check-overrides.log`, and the OpenTelemetry override event sends it as `actual.override.reason`. For an `other` override, a request carrying your sign-in's token also sends the typed text as `actual.override.reason_text`, with anything that looks like a secret replaced by `[redacted]` and cut to 280 characters; Actual shows it only to the organization's admins. A signed-out run, a request whose token Actual refuses and any other endpoint send the code alone. Words typed after one of the five named codes stay on the machine. `actual.event.schema_version` is now `2`
- `actual adr` writes and checks ADRs in your repository and manages your organization's decisions in Actual AI. `adr new` writes a decision and its rule file under the names Actual AI's sync uses, and `adr check` runs Actual AI's save check on decision files and lists likely duplicates; neither needs a sign-in. Signed in, `adr list`, `show`, `create`, `update`, `publish`, `unpublish`, `delete`, `restore`, `accept` and `reject` manage the decisions in your account through the same decision routes as the web app, and refuse a change the decision's state doesn't allow by the web app's rules. `create` refuses a decision the repository already has unless given `--allow-duplicate`, and sends each decision once: when no answer comes back it exits 3 and asks you to look with `adr list` before trying again. `new` and `create` default to a draft when no person is at the terminal, so a coding agent never publishes a governing decision on its own. `create --file` renames the pushed pair to the names Actual AI's sync uses, and both headers take the status the decision was saved with. See [docs/ADR_COMMANDS.md](docs/ADR_COMMANDS.md)

### Changed
- The `plan_governance_rule_override` analytics event no longer sends a hash of the override reason, and its length bucket is renamed from `reason_category` to `reason_length_bucket`
- `actual --help` lists `plan-check`, `impl-check`, `check-override`, `rules`, `login`, `logout` and `whoami` first, and describes the CLI as "Align coding agents with your architecture decisions"
- When `actual adr-bot` can't match ADRs to a repository's languages or frameworks yet, its message links [docs/ADR_BOT_AND_ADVISOR.md](docs/ADR_BOT_AND_ADVISOR.md) in place of `https://cli.actual.ai/docs`
- The README, `docs/HOW_IT_WORKS.md` and `docs/GETTING_STARTED.md` lead with installing the plugin and the CLI, adding rules, `plan-check` and `impl-check`, `rules` and optional sign-in. The `adr-bot` and `advisor` docs moved to `docs/ADR_BOT_AND_ADVISOR.md`

### Fixed
- `impl-check` judges a rule whose ADR file header sets a `root` other than the whole repository. It used to read the diff's `a/` and `b/` header prefixes as part of each path, which put every changed file outside the root, so the rule was dropped and the check reported that no rule applied. A file a rule excludes no longer gets that rule through the same prefixes. The default diff now keeps the `a/` and `b/` prefixes even when git config sets `diff.noprefix` or `diff.mnemonicPrefix`
- The scope index behind `rules select`, `plan-check` and `impl-check` reads the 16 hex characters that end a rule name from Actual AI's context sync as a content hash. It used to read pieces of the hash, such as `bf`, as words of the rule's slug, and they counted toward matching a plan. An index built by an earlier version is rebuilt the first time it's loaded

## [0.13.0] - 2026-11-10

### Added
- Override a denied rule without leaving the session: type `/actual-cli:override <rule-id|all> [reason]` as a message, or choose an override in the dialog the actual-cli plugin opens when a gate denies. The plugin records it with `actual check-override --claude-hook`, whose contract is in [docs/CHECK_OVERRIDE_HOOK.md](docs/CHECK_OVERRIDE_HOOK.md). `all` covers only the rules a gate's latest check denied, and the dialog's "Override all" covers only the denying gate's (`--gate plan|impl`), listing exactly those rules first (`--list-blocking`). A named rule that isn't blocking is still overridden, and the reply says so. Each override records how it was issued (`terminal`, `session_prompt` or `session_dialog`) and which gate was blocking it. The OpenTelemetry override event adds them as `actual.override.source` and `actual.override.blocking_types`; `actual.gate.denied_types` keeps its 0.11.0 meaning, any gate that had denied the rule in the session
- The companion plugin's `SessionEnd` hook can send `actual.session.ended` with the session id, a coarse end reason and whether the last completed hook check blocked. A later hook check reopens a resumed conversation for its next end; repeated ends without a check are suppressed. Telemetry opt-out stops both the event and its local state. Killed sessions can't send the event, so they still need the inactivity fallback. See `PRIVACY.md`

### Changed
- `check-override`'s `--reason` is optional. An override without one is still recorded

### Fixed
- Concurrent hooks no longer lose each other's writes to a session's governance state. An override recorded while `impl-check` recorded the turn's starting commit, or while another check was judging, could be dropped after the user had been told it was recorded

## [0.12.0] - 2026-11-04

### Added
- From v0.12.0, release pages in [actual-cli](https://github.com/actual-software/actual-cli/releases) and [actual-releases](https://github.com/actual-software/actual-releases/releases) include `SHA256SUMS` for the platform archives. A signer adds `SHA256SUMS.minisig` by hand after publication; the release remains unsigned until then. See [Release signing](docs/RELEASE_SIGNING.md#checking-a-download) for verification.
- `actual self-update` replaces a binary downloaded by hand with the latest release. It checks `SHA256SUMS.minisig` against the keys in `release-keys/`, including the tag in its signed comment, and the archive against `SHA256SUMS`, before renaming the new binary over the old one under a lock. A copy installed with Homebrew or npm gets that tool's upgrade command instead, and a build from source, wherever it's installed, gets the `cargo install` command. It won't replace a binary in a directory the user can't write or one another user owns, and won't run as root, under CI, or with update checks turned off. See `docs/SELF_UPDATE.md`

### Changed
- Building from source now requires the package name: `cargo install --git https://github.com/actual-software/actual-cli.git actual-cli`. The workspace also contains the `release-sign` binary package, so the earlier command without `actual-cli` fails with multiple binary packages to choose from.
- For a release binary no package manager owns, the daily update notice and `actual version` name `actual self-update` instead of linking the release page, and for a build from source they name the `cargo install` command that rebuilds it. `actual version --json` is unchanged
- When `actual adr-bot` retries a failed `codex-cli` run through `openai-api`, it asks `Proceed with sync?` again and shows the per-file review, unless you passed `--force`. Before, the retry skipped both and wrote every generated file unreviewed. That prompt now shows a `Runner` row directly above the question, so the retry's reads `openai-api (fallback)`
- `plan-check`, `impl-check` and `rules select` run `codex` in its read-only sandbox, so it can read your repository on those calls but can't change it
- `PRIVACY.md` and the README lead with one setting for turning telemetry off, `actual config set telemetry.enabled false`, and say it stops every telemetry event, including the plan-governance events that reach PostHog. `ACTUAL_NO_TELEMETRY` stays for CI and one-off runs, and what the CLI sends is unchanged

### Fixed
- `--runner codex-cli` works on Codex 0.147.0 and later, which removed the `--full-auto` flag the CLI passed. On those versions every call failed before a prompt left, and `plan-check` and `impl-check` reported `not_checked` with exit 0. Tailoring now passes `--sandbox workspace-write`, which is what the old flag meant
- `codex` no longer inherits the CLI's stdin. Codex adds piped text to its prompt and waits for the pipe to close, so text piped into `actual` could reach the model, and a pipe left open could stall the run until it timed out

### Security
- zip moves from 2.4.2 to 9.0.5, which fixes GHSA-m665-v7pm-7grj (critical), GHSA-q7v9-x93c-xqv3 and GHSA-8hxf-qr8p-62x4. The CLI uses zip only to take `semgrep-core` out of the semgrep wheel it downloads, after checking the wheel against a pinned SHA-256

## [0.11.0] - 2026-10-30

### Added
- Release builds send OpenTelemetry log events about ADRs and checks to `/v1/logs` on the config file's `api_url`, which is Actual's API unless you've changed it. `ACTUAL_NO_TELEMETRY`, `telemetry.enabled: false` and a build without the `telemetry` feature each stop them before they're built. `telemetry.otlp_logs_endpoint` or `ACTUAL_OTLP_LOGS_ENDPOINT` sends them to another OTLP/HTTP logs URL instead (HTTPS, or plain HTTP to localhost, with no redirects followed), and `telemetry.otlp_file` or `ACTUAL_OTLP_FILE` appends a copy of each request to an absolute file path, while the request still goes to the endpoint. A debug build sends them only to an endpoint you set. Events wait in a bounded, owner-only local spool for the background uploader, which takes them a batch at a time for up to five seconds in all. A batch the endpoint refuses, or one still being sent when the time runs out, is dropped and not queued again
- `actual.adr.detected` is sent the first time any of an ADR's rules enters an agent's context during a Claude Code session (`rules brief --claude-hook`), and when `plan-check --claude-hook` or `impl-check --claude-hook` judges any of its rules, once per session at each gate. `actual.gate.evaluated` is sent when either gate blocks on one of an ADR's rules, counted the same way, and each time a human overrides them with `check-override`. Every `plan-check` or `impl-check` run that reaches a verdict sends `actual.action.started` and `actual.action.completed`, whether Claude Code ran it as a hook or you ran the command, and the completed event carries the outcome, what made a `warn` one and how long the check took. Rule files without a v2 header send no ADR events. An event names an ADR by its ids, which Actual can map back to your repository and its organization, and carries Claude Code's session and subagent ids and random event and run ids, but no rule text, plan or diff text, override reasons, file paths, raw account identifiers, identity hashes or installation id. See `PRIVACY.md` for every attribute and its privacy class
- Signed in with `actual login`, an upload of these events to the address `actual advisor` uses carries your sign-in's access token, which names your account as well as the organization you signed in to, so Actual can tie its events to that organization. Events recorded while signed out, or under another sign-in, go in a request of their own without the token, and an upload that runs after you've signed out, or when your sign-in can't be renewed, sends everything without it. A request Actual refuses with a 401 is sent once more without it, as is the rest of that upload, and no other endpoint gets it. `PRIVACY.md` covers who can attribute the events Actual receives, why it keeps them, and their retention and deletion terms
- `actual config set` takes both OpenTelemetry settings, refuses an endpoint or file path that breaks the rules above, and clears either one with `""`. A cleared endpoint goes back to the default, so an opt-out is what stops these events. An endpoint that breaks the rules sends nothing when it's set in the config file or `ACTUAL_OTLP_LOGS_ENDPOINT`, and never falls back to the default
- The endpoint receives each request as binary protobuf (`Content-Type: application/x-protobuf`), and `telemetry.otlp_file` receives it as one line of OTLP/JSON

### Changed
- `telemetry.otlp_file`, `plan-check-overrides.log` and the telemetry spools are restricted to their owner before each write, including a file that already exists with wider access, which keeps its lines. Nothing is written to a path that isn't a regular file or whose permissions can't be changed
- `login`, `logout`, `advisor` and `auth create-token` take a lock on the stored sign-in: `login` to save it, `logout` for the server-side revoke and the local delete, and `advisor` and `auth create-token` to renew an access token that expires within a minute. The telemetry uploader renews a token under the same lock before it sends, so these commands can wait while a background renewal holds it
- `PRIVACY.md` now covers what `actual advisor` sends to Actual's API, what tailoring, `plan-check`, `impl-check` and `rules select` send to your AI provider, how a recorded override reason comes back to the agent, and the match request's `canonical_ir` and `options` fields and where its `name` comes from. Its table of where data is sent now also lists the sign-in server, the `semgrep-core` download from PyPI, `auth create-token`'s request to Actual's API and `models`' request to your AI provider. The CLI sends what it sent before; only the notice changed

### Fixed
- A config file the CLI can't read or parse turns telemetry off. Before, one invalid value anywhere in the file, such as `batch_size: lots`, sent `plan-check`, `impl-check`, `check-override` and `rules select` back to the default settings, which have telemetry on, so their events went out even when the same file set `telemetry.enabled: false`. The background uploader made the same fallback, and now sends nothing while the file can't load, leaving queued events in place. The OpenTelemetry events above follow the same rule. `ACTUAL_NO_TELEMETRY` was never affected
- Commands that renew the same sign-in at the same time share one renewal. The server takes a refresh token once, so the slower command used to report "Not signed in". A `logout` that starts during a renewal waits for it and revokes the renewed session, which could otherwise stay live on the server

## [0.10.0] - 2026-10-28

### Added
- `whoami` shows when the access token expired, once it has, instead of only "Signed in"
- `status` has an Actual AI panel with the signed-in account and organization, read from the local credentials file with no network call

### Changed
- `login --device` is hidden from help while the Actual AI server lacks the device-authorization grant, which a device sign-in needs. The flag still parses and the flow is unchanged
- `plan-check`, `impl-check` and `check-override` queue their telemetry events in the local spool and leave the sending to the background uploader, as `rules select` does, so no check waits on the network, even when the telemetry endpoint never answers
- YAML is read and written with serde-saphyr in place of serde_yml and libyml, which have security advisories and no fixed release. The config file, stored credentials and caches keep their format, so files an earlier version wrote read back the same and 0.9.0 reads what this version writes. A YAML parse error reads differently

### Fixed
- The page the browser shows after you approve `login` says to return to the terminal to finish signing in, instead of "Signed in". It appears before the CLI exchanges the code for a session, so when that exchange failed, the browser said "Signed in" and the terminal reported the error
- The error box wraps a long message and its `Fix:` line instead of cutting them off at the terminal edge, so the server's reason for a failed sign-in is on screen. A message with line breaks stays inside the box, and a runner's stderr appears once, under "Subprocess output"
- A failed sign-in, token mint, token issuance or API request reports the status line plus the OAuth `error` and `error_description` (or the API's error code and message), not up to 2 KB of the response body, which for an error page was HTML
- Text the auth server or the API sends for an error message loses its control characters before the error box prints it, so a misbehaving server can't write escape sequences to the terminal. A tab or line break in that text prints as a space, and a field longer than 1,000 characters is cut with "…"
- The same filter covers what the CLI prints from a server outside error messages: the `advisor` answer, its related ADRs and its scope notes, the sign-in details `whoami`, `login` and `status` show, the `login --device` prompt, the token details `auth create-token` and `mint-token` show, and the Anthropic or OpenAI response body in a runner error. The advisor's answer and a response body keep their line breaks
- When the session refresh fails because of a server error or a network problem, `advisor` and `auth create-token` report that failure and name the refresh step. Only a refresh token the server rejects (`invalid_grant`), or a credentials file whose auth server URL is missing or fails the HTTPS check, reports "Not signed in"
- A success response from the OpenAI API that the runner can't parse no longer crashes the command when it runs past 2 KB with a multibyte character across the 2 KB mark. The runner error shows the body cut before that character
- The reason OpenAI gives when it refuses a request loses its control characters before the CLI prints it, and a line break in it prints as a space
- The telemetry uploader runs in a process group of its own, so it finishes its send after the command that started it exits. Before, it died with its parent's group: Codex kills that group once a command returns, and a CLI that leads its own terminal session (`ssh -t`, or a terminal or tmux window that runs it directly) hangs it up on exit
- A batch the telemetry endpoint refuses with a 400 or 422 is resent one event at a time and only the refused event is dropped, so one bad event no longer holds back the events queued behind it. Any other failure, a 401, 403, 404 or 413 included, keeps the batch queued for a later run, and scope events and check events go out as separate streams, so a failure in one doesn't stop the other

### Security
- Dependencies with published security advisories are replaced or updated: serde_yml and libyml are replaced by serde-saphyr (see Changed), jsonwebtoken moves to 10.4 on its `rust_crypto` backend, and rustls, rustls-webpki, quinn-proto and rand take patched releases

## [0.9.0] - 2026-10-23

### Added
- Once-a-day update notice: after a successful command run at a terminal (stdout and stderr both), the CLI prints the upgrade command for the channel that installed the binary (Homebrew, npm, or a link to the latest release elsewhere). No command waits on the network; a detached background lookup refreshes a cached answer. Hook modes, `--json` output, CI and redirected output never show it, and a failed command does not spend the day's notice. `ACTUAL_NO_UPDATE_CHECK=1` or `update_check.enabled: false` stops every registry request, and a config that fails to load counts as off. See `docs/UPDATE_CHECK.md`
- `actual version` reports the version, install channel and upgrade command offline; `--check` adds one bounded registry lookup and `--json` prints the object documented in `docs/UPDATE_CHECK.md`
- ADR file header v2: rule files may open with a YAML frontmatter block, as the ADR generator now writes. The CLI uses `metadata.scope.{root,paths}` and `metadata.actual.{adr_id,status}`; `candidate_group_id` also groups files into decisions; `languages` and the other fields are parsed and carried but do not affect selection yet. **Requires CLI 0.9.0 or later**; an earlier CLI folds the block into the scope sentence and scopes the decision wrongly, and CLI 0.8.0 also ignores `status`, so it still enforces `draft`, `rejected` and the other retired decisions. Files without a header work exactly as before, and both formats can share a directory
- Declared `paths` replace the globs derived from `### Verify` and prose; `!` paths are excludes, and paths are relative to `root`. When a header declares include paths, a file outside the decision's root is never governed by it, and neither is a file matching its exclude, in selection, the brief, `plan-check` and `impl-check`
- Decisions with a retired status (`deleted`, `draft`, `archived-draft`, `deferred`, `rejected`, `suggested`, `superseded`, `deprecated`) are not indexed, so they are never selected, briefed, judged or counted; a missing or unknown status is active. `rules index` reports how many were left out
- Decisions are keyed on `candidate_group_id`, else `adr_id`, when a file has one, so files sharing an id are one decision and renaming a file does not re-brief it within a session
- Additive JSON fields: `adr_id`, `candidate_group_id`, `status` and `root` in `rules ls`, and `adr_id`, `candidate_group_id` and `root` in `rules select` and `rules select --by-adr` (`root` only when the header declares paths); `inactive_documents` and declared-versus-derived path counts in `rules index`. Fields are omitted for files without a header
- Warnings for a malformed or unrecognised header (`malformed_frontmatter`, `unknown_version`) and for two rule files that share a file stem (`duplicate_slug`)
- `rules brief --help` names the capability marker `adr-header-v2`, which the Actual Skill probes to learn that the CLI reads the v2 header

### Changed
- The `plan-check` folder pass is bounded: each folder is decided once, smallest first, searching the files the plan names first and then the rest in name order, at most 500 files per folder and nothing after 3 seconds. A folder either limit stops short of a governed file is undecided, and the session record notes it as neither covered nor a gap. `plan-check --json` reports the pass as `folders`
- The scope index cache format version is bumped and the brief memory and ledger move to version 2, so existing caches rebuild on first use and a session may be briefed once more on a decision after upgrading
- The reported decision key for a file with a v2 header is `group:<candidate_group_id>` when it has one, else `id:<adr_id>` (`rules select --by-adr` `key`); the brief heading is still the decision title

### Fixed
- When two rule files share a file stem, `rules select` and its `--explain` output no longer resolve both to the first one; lookups key on the file path

## [0.8.0] - 2026-10-19

### Added
- `rules brief` records the folder of every file it is asked about in the session record: covered when the search finds a decision, uncovered when it finds none. Docs, config, lockfiles and generated output never count. The brief also takes the repository root from a `--rules-dir` shaped `<root>/.actual/rules` when no `--repo` is given
- `plan-check --claude-hook` notes the folders a plan names, with no model call, so a planned folder no ADR covers counts as a gap. A folder a plan names is judged by the files directly in it
- `session summary` shows the areas with no ADR beside the ADR count, e.g. `Actual AI: 3 of 12 ADRs added to context · 2 areas with no ADR`, or `Actual AI: no ADR covers the 3 areas you worked in` when every area is a gap. `--json` adds `line`, `gaps` and a sorted `areas_without_adr`

### Changed
- The session summary line replaces "Actual AI: added 3 of 12 ADRs to context this session."

### Fixed
- `rules brief` decides whether a file is governed before `--limit` caps the search, so `--limit 0` no longer reports every governed read as `ungoverned` and records a gap for every folder
- The session summary keeps the count beside the gaps when the session also worked in a folder a decision covers

## [0.7.0] - 2026-10-14

### Added
- `rules brief`: the hook's view of what governs a file. Decisions are briefed once per session and reset on compaction (including a compacted subagent's own memory), the brief's total size is capped with a disclosed note of anything left out, and a path search falls back to text matching (two shared words minimum) only when the path finds nothing. It prints a stderr line saying why a brief stayed quiet
- `rules select` groups rule documents by decision and caps the selection in decisions rather than rules
- Rules score floor: a selection can now answer nothing. Settable persistently with `config` (`rules_min_score`), per repository, with validated inputs and the effective floor reported
- Session summary hook that reports the ADR count and keeps a session-wide record of the decisions briefed

### Changed
- `impl-check`'s Stop check now sees work committed during the turn; the turn start is recorded from the hook envelope's event and compared by checkout root rather than the working directory, and `--turn-start` stays silent when it cannot read its payload
- Claude subprocesses spawned by the CLI are marked with `ACTUAL_CLI_SUBPROCESS=1`
- README leads with the Actual.ai Skill

### Fixed
- `rules select` preserves colons in decision titles and accepts a blank plan alongside `--file`
- Brief hook: resolves symlinks and dot segments in paths, keeps brace lists whole and expands them before a glob answers, no longer splits a spaced path in two, and only answers documented event and tool pairs

## [0.6.2] - 2026-10-01

### Fixed
- npm publishing: the platform packages (`@actualai/actual-{darwin,linux}-{arm64,x64}`) now declare `repository` metadata, which OIDC trusted publishing requires to generate and validate build provenance. Without it the 0.6.1 npm publish was rejected (`E422`, provenance repository mismatch), so 0.6.1 never reached npm. No functional changes to the CLI itself since 0.6.0.

## [0.6.1] - 2026-10-01

### Fixed
- Release pipeline: npm packages are now published correctly via OIDC trusted publishing. 0.6.0 shipped to GitHub Releases and Homebrew, but its npm publish failed (CI installed an npm version incompatible with the runner's Node), so 0.6.0 never reached npm. There are no functional changes to the CLI itself since 0.6.0.

## [0.6.0] - 2026-10-01

### Added
- Plan-governance and scope telemetry now carry a hashed identity + timestamp envelope so metrics can be sliced by user, org and repo without any raw identifier ever leaving the machine: `user_id_hash`/`org_id_hash`/`repo_id_hash` (unsalted, domain-separated SHA-256 of the logged-in ids), a synthetic repo-slug-derived `org_id` on anonymous runs, and `datetime_utc`. Ids are read locally (stored credentials + a config sticky-scope pin) with no network call on the hook path; the backend applies a secret pepper before anything reaches PostHog
- `rules select` / the signals scope workflow now emit a `plan_governance_scope_select` event (narrative 6) describing each local-ADR selection — rules scanned, stage-1 candidates, whether stage 2 (the LLM rank) ran and its status/degradation, selected count, cache hit/miss, rank latency and runner — plus a random `scope_run_id` correlation id. Only counts, enums, ids and hashes are sent, never plan or rule text
- `check-override` now emits a first-class `plan_governance_rule_override` event (narrative 4) instead of a plain completion, so overrides are cleanly separable and override rate has an unambiguous denominator. The override reason is captured privacy-preservingly as `reason_category`/`reason_len`/`reason_hash` — never the raw text
- `plan-check --claude-hook` / `impl-check --claude-hook` completion events now carry `round_index`/`round_total` (narrative 5) for measuring revision-loop convergence

### Changed
- `PRIVACY.md` documents every new telemetry field; all remain opt-out and fire-and-forget, and no raw identifiers, plan text, diff content or reason text are ever sent

### Fixed
- `rules select` no longer waits on the scope-telemetry network send, and no longer risks losing the event when the process exits: the event is written to a small durable local spool (owner-readable only, bounded, never plan/rule text) and delivered by a separate best-effort uploader process that outlives the command, retrying on a later run until the proxy confirms it. A slow or unreachable endpoint can never delay, fail, or block the selection result or the command's exit, and an opted-out run still spools and sends nothing

## [0.5.0] - 2026-09-23

### Added
- `actual rules select --file <path>` no longer requires a plan (AK-786): a hook selecting rules for the file an agent is about to touch can pass only a path. A path-only query skips the stage-2 rank rather than ranking against empty prose, reports why it skipped, and the panel drops its `Plan:` row; a call naming neither a plan nor a path is still a usage error

### Changed
- Scope-index ties are now broken by the rarest glob a document matched, then by how few distinct globs it declares, and only then by slug (AK-785), so sibling rule documents generated from one decision no longer rank alphabetically on path-only queries. Scores, the index format and the cache are unchanged

### Fixed
- `claude --print` subprocesses spawned by the CLI now run with hooks disabled, so `impl-check --claude-hook` no longer recurses through the user's own `Stop` hook (e.g. `impl-gate.sh`), times out, or leaves orphaned nested judge calls running
- Multi-line diffs, verdict reasons and quoted spans no longer break the box borders of the `plan-check` / `impl-check` panel

## [0.4.0] - 2026-09-22

### Added
- `actual impl-check` (AK-755): judges a `git diff` against the rule documents selected for it, sharing `plan-check`'s discovery/scoring/judging pipeline, revision-loop and telemetry machinery via a new shared `check_engine` module rather than duplicating it — same four outcomes, same exit-code contract, and the same `--claude-hook` JSON contract (driven by the separate plugin repo's `hooks/impl-gate.sh`); defaults to the working tree vs `HEAD` in the current repo (including untracked, non-ignored files, without mutating the user's index), with `--diff-file` or piped stdin to supply the diff explicitly. Its own revision loop is budgeted independently via `--max-rounds`/`ACTUAL_IMPL_CHECK_MAX_ROUNDS` (deny counts, rounds, and content-scoped clearances are per command inside the shared session file). A `check-override` recorded against the session still applies to both commands.

### Changed
- `plan-check-override` renamed to `check-override` (reflecting that the override mechanism is shared by both `plan-check` and `impl-check` denials), with `plan-check-override` kept as a backward-compatible alias for existing scripts
- `impl-check` panels, `--json` `not_checked` detail, and `--claude-hook` deny/round-limit copy now name a diff and working tree rather than a plan
- `impl-check` runs `git` with `--no-pager`, `color.ui=never` and `--no-ext-diff`, so a user's `color.ui=always` or `diff.external` no longer corrupts the diff the judge reads; an over-limit diff now stops and reaps the `git` child; the round-limit and partial-coverage notices say the `plan-check-overrides.log` audit log is shared with `plan-check`

## [0.3.0] - 2026-09-14

### Added
- Local scope resolution over `.actual/rules/`: an offline, deterministic index that ranks rule documents against a plan using path globs extracted from `### Verify` operands, the prose scope sentence, the aspect slug and the title
- `actual rules index` to build or refresh that index, `actual rules index --clear` to drop every cached index (including those left by other repositories) and rebuild this one, `actual rules select` to rank rule documents against a plan, and `actual rules eval` to score the index against the status-quo filename scan on a golden set (`--rebuild` forces a fresh index so the measurement is not silently cached)
- `actual rules select --explain`, which attributes every hit to the signal and terms that carried it and prints what the filename scan would have chosen instead
- Two-stage rule selection: `actual rules select` now ranks the deterministic prefilter's surplus with a configured runner when it returns more candidates than the cap allows, and returns a verdict and a reason for every rule it selects
- `--no-rank`, `--candidates`, `--runner` and `--model` on `actual rules select`, and `--rank` on `actual rules eval` to score the two-stage selector against the offline ones on the same golden set
- `StructuredRunner`, which lets all five runners answer a schema other than the tailoring one; stage-2 selection reuses each runner's existing structured-output plumbing rather than duplicating it
- A 90-second wall-clock deadline on the stage-2 rank, since a runner's own timeout is an inactivity timer and does not bound the call
- Design notes and measurements for scope resolution in `docs/SCOPE_INDEX.md`, and for two-stage selection in `docs/RULE_SELECTION.md`
- `actual advisor` command: ask the Advisor org-scoped architecture questions from the terminal and print the answer with any related ADRs (requires signing in with `actual login`)
- Repository scoping for `actual advisor`: `--repo <name|owner/name|uuid>` to target a connected repository, automatic detection from the working tree's `origin` remote, a scope remembered per repository, the `none` and `auto` keywords to pin org-level or reset, and `--show-scope` to print the active scope
- Documentation for the advisor command and repository scoping in the README and Getting Started guide
- `actual rules ls` to list the rule documents under `.actual/rules/` with a per-document level histogram, parse warnings and errors, and `--json` for the same report as structured data
- `actual plan-check` to judge an implementation plan against the rule documents selected for it, printing a panel (or `--json`) with one of four outcomes — conforming, conflicting, requires_decision, or not_checked — and exiting non-zero only for a real `conflicting` verdict; `--claude-hook` runs the same pipeline against a Claude Code `PreToolUse` hook envelope on stdin and emits that hook's own JSON contract instead — nothing on a clean pass, a `permissionDecision: "deny"` object on a real conflict, never an explicit "allow" — always exiting 0 and failing open on any infrastructure failure (no runner, no rules, a crashed judge call)
- The plan-check revision loop (AK-677): under `--claude-hook`, a rule the judge already cleared for the session is never re-asked, a `requires_decision` verdict blocks exactly like a conflict pending explicit human review, and a single rule stops blocking on its own after `--max-rounds` (`ACTUAL_PLAN_CHECK_MAX_ROUNDS`, default 3) denied rounds — every round, override, and partial-coverage disclosure is appended to a durable, append-only audit log
- `actual plan-check-override` for a human to explicitly clear one or more rules a plan-check session has denied, with a required `--reason` recorded to the audit log; refused outright unless run from an ordinary interactive terminal (never from a script, an agent's tool call, or Claude Code's own integrated terminal)

## [0.1.4] - 2026-03-31

### Fixed
- Semgrep-core download routed through TUI pipeline to prevent terminal corruption

## [0.1.3] - 2026-03-30

### Added
- PHP detection, including WordPress-specific detection fixes
- Swift language detection
- C/C++ project detection
- Java Vert.x and Kotlin Compose Multiplatform framework detection
- C# ASP.NET Core detection via `.csproj` parsing
- jQuery detection
- Signals pipeline: tree-sitter + semgrep embedded detectors wired into match requests
- V2 ADR schema support (MkII three-tier output model)
- Character budget enforcement on managed sections with TUI warnings when approaching limits
- Fast-fail on Claude Code hard rate-limit messages
- Semgrep installed in CI test and coverage jobs

### Changed
- Framework detection now prefers manifest-sourced frameworks over config-file-sourced ones in auto-selection
- Language and framework name matching with normalization improvements
- CLI homepage updated from `app.actual.ai` to `cli.actual.ai`
- HTTP retry logic extracted into shared `runner/http_retry.rs` module
- Runner utilities factored into `runner/util.rs`

### Fixed
- Tree-sitter query errors resolved
- Framework name matching fixes and normalization

## [0.1.2] - 2026-03-11

### Added
- Open-source readiness: MIT license, README, SECURITY.md, PRIVACY.md
- CONTRIBUTING.md and code of conduct
- CLI distribution guide (`docs/CLI_DISTRIBUTION.md`)
- Local development guide (`docs/LOCAL_DEVELOPMENT.md`)

### Changed
- Extracted symphony/factory module to `actual-software/factory`
- Moved publication scaffolding to distribution repos (`actual-software/actual-releases`, `actual-software/homebrew-actual`)
- Security contact email updated to `john@actual.ai`
- Removed hardcoded staging API URL

## [0.1.1] - 2026-03-10

### Added
- Initial public release of the `actual` CLI
- ADR-powered AI context file generator
- Multi-runner support: claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli
- Output formats: `CLAUDE.md`, `AGENTS.md`, `.cursor/rules/actual-policies.mdc`
- Homebrew tap (`actual-software/actual/actual`)
- npm packages (`@actualai/actual`)

## [0.1.0] - 2026-02-27

### Added
- Initial release
