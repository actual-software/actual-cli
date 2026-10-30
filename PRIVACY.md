# Privacy & Telemetry

Actual CLI collects minimal telemetry to help us understand usage patterns and
improve the product. How your data is handled depends on whether you are logged
in and on who processes it. This notice is explicit about both:

- **Logged-in events link to your Actual account.** When you are logged in,
  plan-governance events carry one-way `user_id_hash`/`org_id_hash` values.
  Because the CLI computes them deterministically from your account and
  organization ids, Actual's backend resolves them back to your account and
  organization — so logged-in telemetry is first-party, identity-linked data,
  **not** anonymous. Plan-governance events never carry the raw ids, but
  hashing alone does not make identity-linked data anonymous. OpenTelemetry
  events sent to the API address `actual advisor` uses while you're logged in
  carry your sign-in's access token, which names your account as well as the
  organization you signed in to, so Actual can tie them to that organization.
- **Logged-out events are pseudonymous, not anonymous.** A logged-out run sends
  no account credential, but it still carries the installation's persistent
  `distinct_id` — the same id its logged-in runs use, which is not reset on
  logout — and may carry repository-derived hashes such as `repo_id_hash`. Once
  an installation has ever sent a logged-in event, Actual can link that
  `distinct_id`, and so the installation's logged-out events, back to the
  account.
- **Third-party analytics (PostHog) cannot recover your raw ids, but can match
  events to a known repository.** Before telemetry reaches PostHog, the
  account/org/repo identity hashes and the override-reason hash are peppered
  server-side (the pepper and the hash→account map are never shared), so PostHog
  cannot reverse them to raw account, user, or organization ids. The
  repository-derived keys — `repo_hash`, `repo_url_hash`, and the logged-out
  `org_id` — are **not** peppered: they are deterministic hashes of the
  repository URL/slug that anyone who knows the repository can recompute, so
  someone with access to the analytics data could correlate events to a known
  repository (and a repository under a personal account names its owner).

**One setting turns telemetry off.** `actual config set telemetry.enabled false`
stops every telemetry event in this notice: the counters, the plan-governance
events that reach PostHog through Actual's proxy, and the ADR and check events.
You don't need a second switch for any of them. It covers telemetry, not every
network request: "How to opt out of telemetry" below says what it leaves running.

This document describes exactly what data is collected, its privacy class, where
it goes, who can attribute it to you, how long it is kept, and how to opt out.

## What data is collected

### Telemetry counters (pseudonymous)

On each successful run of `actual adr-bot`, the CLI sends counter metrics:

| Metric | Description |
|--------|-------------|
| `cli.sync.adrs_fetched` | Number of ADRs returned by the API |
| `cli.sync.adrs_tailored` | Number of ADRs successfully tailored by the LLM |
| `cli.sync.adrs_rejected` | Number of ADRs filtered out |
| `cli.sync.adrs_written` | Number of ADRs written to output files |
| `cli.sync.adrs_recommended` | Number of ADRs matched and recommended for this repo (only emitted when > 0) |

Each metric includes these tags:

| Tag | Description |
|-----|-------------|
| `repo_hash` | SHA-256 hash of `(repo_url + commit_hash)` -- the counter **never** carries the raw URL or commit |
| `repo_url_hash` | SHA-256 hash of the normalized repo URL alone -- stable across commits, used to count unique repos |
| `source` | Always `"actual-cli"` |
| `version` | CLI version string (e.g. `"0.1.2"`) |

The `cli.sync.adrs_recommended` metric also includes:

| Tag | Description |
|-----|-------------|
| `adr_ids` | Comma-separated list of ADR IDs that were matched for this repo (e.g. `"adr-001,adr-002"`) |

ADR IDs are internal Actual AI identifiers and are not sensitive. Repository identity
remains one-way hashed -- counters never carry raw URLs.

Counters carry no account identity, but they are **pseudonymous, not
anonymous**: `repo_url_hash` is a stable key that links a repository's counters
to each other, anyone who knows the repository can recompute it, and
plan-governance events carry the same hash beside `user_id_hash` when you are
logged in.

Telemetry does not include raw usernames, email addresses, IP addresses, file
contents, file paths, or source code. When you are logged in, plan-governance
events include one-way identity hashes that let Actual, as the first party,
attribute activity to your Actual account, organization, and connected
repository; the events never carry the raw identifiers, but these hashes are
**identity-linked, not anonymous** — see "First-party identity data" below. When
you are not logged in, no account credential is included, but the event still
carries the installation's persistent `distinct_id` and may carry
repository-derived hashes, so it is **pseudonymous rather than anonymous** — see
"Logged-out runs" below.

We do not sell or share telemetry with third parties for their own purposes.
Telemetry is sent to PostHog only as Actual's analytics service provider, and
only for operating Actual's product analytics. Before anything reaches PostHog,
the account/org/repo identity hashes and the override-reason hash are peppered
server-side (the pepper and the hash→account map are never shared), so PostHog
cannot reverse them to raw account, user, or organization ids, and never
receives raw identifiers or customer content. The repository-derived keys
(`repo_hash`, `repo_url_hash`, and the logged-out `org_id`) are **not** peppered,
however: they are deterministic hashes of the repository URL/slug that anyone who
knows the repository can recompute, so events can be correlated to a known
repository.

### Plan-governance events (pseudonymous when logged out; first-party identity-linked when logged in)

When `actual plan-check` or `actual impl-check` (including each command's
`--claude-hook` path Claude Code drives automatically) reaches a verdict --
i.e. it actually judged the plan or diff against at least one selected rule
-- and on `actual check-override`, the CLI sends discrete events describing
the governance outcome, proxied through `api-service.api.prod.actual.ai` to
PostHog. The CLI never talks to PostHog directly and holds no PostHog
credentials — the proxy is its only outbound path for this data.

A run that fails open before reaching a verdict -- no rules apply, no runner
is available, or the check itself fails -- sends no plan-governance events at
all. This stream cannot be used to measure how often `plan-check`/`impl-check`
run in total, or how often either fails open versus produces a verdict; it
only describes verdicts that were actually reached.

| Event | Sent when |
|-------|-----------|
| `plan_governance_check_started` | Built and queued together with `plan_governance_check_completed` once a run has reached a verdict (the uploader may send the two in different requests) -- not emitted separately at the actual start of the check |
| `plan_governance_check_completed` | A `plan-check` or `impl-check` run reaches a verdict |
| `plan_governance_rule_violation` | One rule did not conform (once per violating rule) |
| `plan_governance_rule_override` | `check-override` clears a rule (once per cleared rule) |
| `plan_governance_scope_select` | A `rules select` / signals-workflow run selected which local ADRs apply (once per run) |

Each event includes a subset of these properties:

| Property | Description |
|----------|--------------|
| `cli_version` | CLI version string |
| `command` | Which subcommand/mode ran (e.g. `"plan-check"`, `"plan-check --claude-hook"`, `"impl-check"`, `"impl-check --claude-hook"`, `"check-override"`, `"rules select"`) |
| `rule_id` | The internal id of the specific rule involved -- an Actual-internal identifier, same category as `adr_ids` above, not a file path |
| `rule_source` | The rule document's internal slug, derived from its filename -- not a filesystem path |
| `decision` | `allow`, `warn`, or `block` -- the outcome for this rule or run |
| `duration_ms` | How long the check or selection took |
| `exit_code` | The process exit code |
| `round_index` / `round_total` | `--claude-hook` revision-loop round and configured maximum, on completion events only |
| `repo_hash` | Same one-way SHA-256 as the sync counters above -- a plain hash of the repo URL + commit, **not** peppered, so recomputable by anyone who knows the repository |
| `repo_url_hash` | Same one-way SHA-256 as the sync counters above -- a plain hash of the repo URL, **not** peppered, so recomputable by anyone who knows the repository |
| `datetime_utc` | RFC3339 UTC timestamp of the event |
| `user_id_hash` | One-way SHA-256 of `"user:"+<user id>`, only when logged in -- the event **never** carries the raw user id; peppered server-side before PostHog |
| `org_id_hash` | One-way SHA-256 of `"org:"+<organization id>`, only when logged in -- the event **never** carries the raw org id; peppered server-side before PostHog |
| `repo_id_hash` | One-way SHA-256 of `"repo:"+<connected-repo id>`, sent whenever that id is known locally -- **including logged-out runs where a repository pin persists**; the event **never** carries the raw id; peppered server-side before PostHog |
| `org_id` | A synthetic, repo-derived org identifier (a UUID from the repo slug), sent **only when not logged in**, so logged-out runs still have an org-shaped grouping key -- derived deterministically from the repo slug and **not** peppered, so recomputable from the repository |
| `reason_category` | `check-override` only: a coarse length bucket of the override reason (`empty`/`short`/`medium`/`long`) -- derived from length, never the text |
| `reason_len` | `check-override` only: the character length of the override reason |
| `reason_hash` | `check-override` only: a one-way SHA-256 of the override reason (peppered server-side before PostHog) -- lets identical reasons group without exposing the text |
| `scope_run_id` | `rules select` only: a random per-run correlation id (not derived from any data) |
| `rules_scanned` / `stage1_candidates` / `selected` | `rules select` only: counts of rules scanned, stage-1 candidates, and rules finally selected -- numbers only |
| `stage2_invoked` / `stage2_status` | `rules select` only: whether the LLM rank ran, and its status/degradation (`applied`/`not-needed`/`unavailable`/…) |
| `cache_hit` | `rules select` only: whether the scope index came from cache |
| `runner` | `rules select` only: the rank runner label when stage 2 ran |

**Plan-governance events never carry plan text, diff content, matched rule file
paths, or conflicting plan/diff spans** -- only the rule's internal id/slug and
a coarse allow/warn/block verdict. The check itself does send plan and diff text
to your AI provider: see "Plan and diff checks" below.

#### First-party identity data (logged-in runs)

The `user_id_hash` and `org_id_hash` fields are present only when you are logged
in; `repo_id_hash` is present whenever a connected-repo id is known locally,
which can include logged-out runs where a repository pin persists. All three are
one-way SHA-256 digests of Actual-internal ids, so these events never carry the
raw ids. They let Actual group metrics by account, organization, and repository
without putting those ids in the payload.

These hashes are **first-party, identity-linked data — not anonymous**. Because
the CLI computes them deterministically, Actual's backend can match a hash to
the originating account, organization, and repository, and it applies a secret
server-side pepper to maintain that association. Treat logged-in
plan-governance telemetry as attributable to your Actual account:

- **Who can attribute it:** Actual, as the first party, via its authorized
  backend systems and personnel. The raw-id → hash mapping and the pepper live
  only in Actual's backend.
- **Third parties cannot recover raw ids:** the pepper and the mapping are never
  shared, so PostHog cannot reverse `user_id_hash`/`org_id_hash`/`repo_id_hash`
  to raw account, user, or organization ids. This protection covers the peppered
  identity hashes only — the unpeppered, repository-derived keys (`repo_hash`,
  `repo_url_hash`, and the logged-out `org_id`) stay recomputable from a known
  repository, so events can still be correlated to that repository.
- **Purpose:** to attribute product-analytics activity to an account/org/repo so
  Actual can measure and improve governance features per customer.
- **Retention & deletion:** the association is kept for as long as needed to
  provide product analytics for your account and is deleted, or the link
  severed, on account deletion or a verified deletion request.

#### Logged-out runs

Logged-out runs carry no account credential and no `user_id_hash`/`org_id_hash`,
but they are **pseudonymous, not anonymous**, for two reasons. First, they still
send the installation's persistent `distinct_id` — the same id used on logged-in
runs, since logout removes stored credentials but does not reset it — so once an
installation has ever sent a logged-in event, Actual can link that `distinct_id`,
and therefore the installation's logged-out events, back to the account. Second,
they can still carry a `repo_id_hash` (when a repository pin persists) and the
repo-derived `repo_hash`/`repo_url_hash`/`org_id`, which are recomputable from a
known repository.

`actual check-override` reports a cleared rule by sending a dedicated
`plan_governance_rule_override` event (one per cleared rule), distinct from a
real check's completion so overrides are cleanly separable and override rate has
an unambiguous denominator. **The override reason is customer free text, and
the event never carries it raw** -- only `reason_category`, `reason_len`, and a
one-way `reason_hash` (see above).

Each event also carries a `distinct_id`: a random, opaque identifier
generated once on first use and stored locally
(`~/.actualai/actual/telemetry-id`), never derived from a username, email,
hostname, or any other identifying material. It exists so events from the
same installation can be grouped over time. **It persists across login and
logout** — it is written once and reused forever, and logging out removes your
stored credentials but does not reset it — so an installation sends the same
`distinct_id` whether or not you are logged in. It is generated independently of
`repo_hash`/`repo_url_hash` (i.e. not cryptographically derived from them),
but **every plan-governance event carries `distinct_id` and
`repo_hash`/`repo_url_hash` together in the same payload** -- that pairing is
how events are grouped per installation, and it also means an installation
can be correlated with the set of repositories it has run `plan-check` or
`impl-check` against over time. On a logged-in run the `distinct_id` travels in
the same event as the `user_id_hash`/`org_id_hash` above, which Actual's backend
resolves to your account; because the id is stable across logout, Actual can
then attribute that installation's **logged-out** events to the same account
too. Do not read `distinct_id` as making any event anonymous.

Each event also carries an `insert_id`: a fresh random UUID generated per
event, used only so the proxy can drop an accidental duplicate delivery of the
same event. It carries no information about the installation, repository, or
user, and is reused verbatim only if the same event is re-sent from the local
spool (below), so a retry collapses into one server-side rather than
double-counting.

Every plan-governance event, from `plan-check`, `impl-check`, `check-override`
and `rules select` alike, is delivered through a small durable outbox rather
than an in-process send, so no command waits on the network: the event is
first written to a local spool file
(`~/.actualai/actual/scope-telemetry-spool.jsonl`, created readable only by the
owner) and then delivered by a separate best-effort uploader, with the entry
removed once the proxy confirms it, or once the proxy rejects it as invalid
sent on its own. An entry stays queued for a later run while the endpoint
can't be reached, when the request fails on its credentials, its route or its
size, and when the proxy reports that it failed to forward any event in the
same request. The spool holds **only** the same
already privacy-filtered event payload that would go on the wire -- counts, enums,
hashed ids, `distinct_id`, `insert_id` -- never plan text, rule text, file
paths, or any raw identifier. It is bounded in size (the oldest entries are
dropped once it is full) and is never written for an opted-out run.

The one setting under "How to opt out of telemetry" stops plan-governance events
too -- they share the exact same opt-out check as the sync counters, and it runs
before any repo-identity hashing or `distinct_id` creation, not just before the
network send: an opted-out run never touches git for telemetry and never writes
`~/.actualai/actual/telemetry-id`.

### ADR and check events (OpenTelemetry, sent to Actual's API by default; first-party identity-linked when sent there while you're signed in)

The CLI sends OpenTelemetry log events about ADRs on four occasions:

- `actual.adr.detected`, with `actual.detection.method` set to `context`, the
  first time any of an ADR's rules is added to an agent's context during a
  Claude Code session, even when the brief has room for only some of them. It
  comes from the `rules brief --claude-hook` path Claude Code drives, and it
  counts an ADR once per session, whichever agent or subagent it reached and
  however often the context is compacted. A manual `actual rules brief` run
  sends nothing.
- `actual.adr.detected`, with `actual.detection.method` set to the gate,
  `planning` or `implementation`, when `plan-check --claude-hook` or
  `impl-check --claude-hook` judges any of an ADR's rules, whatever the
  verdict. It counts an ADR once per session at each gate.
- `actual.gate.evaluated`, with `actual.gate.outcome` set to `breached` and
  `actual.gate.type` naming the gate, when either gate blocks on one of an
  ADR's rules with a `conflicting` or `requires_decision` verdict, counted the
  same way. A blocking verdict sends both a detected and an evaluated event,
  and a check run outside a Claude Code hook sends neither.
- `actual.gate.evaluated`, with `actual.gate.outcome` set to `overridden`, each
  time a human records an override with `actual check-override`: one event per
  ADR with a rule the command names, every time it runs.

A rule file without a v2 header sends none of them.

Each `plan-check` or `impl-check` run that reaches a verdict also sends two
events about the check itself, whether Claude Code ran it as a hook or someone
ran the command:

- `actual.action.started`, stamped when the check started, before its judge
  was called.
- `actual.action.completed`, once the check has its verdict, with its outcome,
  what made a `warn` one, and how long it took.

They're sent on the same runs as the plan-governance `check_started` and
`check_completed` events, with the same outcome, but only to the outputs
below. A run outside a Claude Code hook sends them with no session or agent.

The events exist to evaluate the CLI: whoever receives them can see which ADRs
each session's agents were given, which ones the gates checked work against and
blocked on, which of them a human overrode, how each check came out and what
made a check warn, and how long it took.

**Release builds send these events to Actual's API by default.** They go to
`/v1/logs` on the address the CLI's other telemetry uses: the config file's
`api_url`, which is `https://api-service.api.prod.actual.ai` unless you've
changed it. Turning telemetry off stops them before anything is built. A debug
build, which is what `cargo build` and `cargo test` make without `--release`,
sends them only to an endpoint you set, so the CLI's own test runs never send
them to Actual's API. Two settings change where they go:

- `telemetry.otlp_logs_endpoint` (or the `ACTUAL_OTLP_LOGS_ENDPOINT` env var):
  an OTLP/HTTP logs URL used as-is in place of the default. Events are sent to
  it as binary protobuf (`Content-Type: application/x-protobuf`). It must use
  HTTPS unless it's localhost, and the CLI doesn't follow redirects, so events
  reach that URL and no other. A URL that breaks those rules sends them
  nowhere, never to the default. The standard `OTEL_EXPORTER_OTLP_*` variables
  aren't read.
- `telemetry.otlp_file` (or `ACTUAL_OTLP_FILE`): an absolute path on your
  machine, unset by default. Each request is appended to it as one line of
  OTLP/JSON, in a file readable only by the owner, and is still sent to the
  endpoint. A file that already exists is restricted to its owner before
  anything is added, and nothing is written to a path that isn't a regular
  file or whose permissions can't be changed.

Set either with `actual config set`. Setting the endpoint to `""` returns it to
the default, so turning telemetry off is the way to stop these events. They
don't go to PostHog.

**Signed in, events sent to Actual's API carry your sign-in.** When the
endpoint has the scheme, host and port of the address `actual advisor` uses
(`ACTUAL_API_URL` when it's set, otherwise
`https://api-service.api.prod.actual.ai`) and you've signed in with
`actual login`, each request carries your sign-in's access token in its
`Authorization` header. The token names your account as well as the organization
you signed in to, and Actual ties its events to that organization. Events you
recorded while signed out, or signed in to another
account or organization, go in a request of their own without the token. A
request Actual refuses with a 401 is sent once more without it. Any other
endpoint, a custom one included, never gets the token. The default endpoint is
at that address unless the config file's `api_url` or `ACTUAL_API_URL` names
another one.

Every event carries the first six attributes below. An ADR event adds
`actual.adr.ref` and `actual.adr.ids`, then its own: `actual.detection.method`
on `actual.adr.detected` events, `actual.gate.outcome` on
`actual.gate.evaluated` events, `actual.gate.type` on breach events and
`actual.gate.denied_types` on override events. A check event adds
`actual.action.type` and `actual.action.mode`, and the completed one
`actual.action.outcome`, `actual.action.warn_reasons` and
`actual.action.duration_ms`. An unknown value is
sent as an empty string. None of them names you, your organization, your
repository or this installation, so an event carries the same fields signed in
or out. Only the token on a signed-in request to Actual's API, described above,
says whose events they are, and it makes every event in that request
first-party identity-linked. Sent signed out or to another endpoint, an event
has only the classes its fields carry below:

| Attribute | What it is | Privacy class |
|-----------|------------|---------------|
| `log.record.uid` | The event's `actual.event.id`, repeated under the name OpenTelemetry tools use to drop a duplicate delivery | anonymous |
| `actual.event.id` | A fresh random UUIDv7 per event | anonymous |
| `actual.event.schema_version` | The version of this attribute set, currently `1` | anonymous |
| `actual.action.id` | A fresh random UUIDv7 per run of the CLI, shared by every event that run sends | anonymous |
| `actual.session.id` | Claude Code's id for the conversation: random, generated by Claude Code, never derived from account data. It groups one conversation's events. An override event carries the id the human passed to `check-override --session`, which the gate's denial printed, and a check run outside a hook sends it empty | first-party identity-linked |
| `actual.agent.id` | Claude Code's id for the subagent that was briefed, or whose plan or diff a gate checked, empty for the main agent, on every override event and on a check run outside a hook: random, generated by Claude Code | first-party identity-linked |
| `actual.adr.ref` | The ADR: its group id from its rule files' header, or its one file's `adr_id` when they name no group | first-party identity-linked |
| `actual.adr.ids` | The `adr_id` of each of the ADR's rule files with at least one rule in the brief that added it; on an event from a gate, with at least one rule the gate judged, or for a breach blocked on; on an override event, with at least one rule the override named | first-party identity-linked |
| `actual.detection.method` | How the ADR was detected: `context` when a brief added it to an agent's context, or the gate that judged its rules, `planning` for `plan-check` and `implementation` for `impl-check` | anonymous |
| `actual.gate.outcome` | What the evaluation came to: `breached` when a gate blocked on the ADR's rules, `overridden` when a human overrode them | anonymous |
| `actual.gate.type` | The gate that blocked on the rules: `planning` for `plan-check`, `implementation` for `impl-check` | anonymous |
| `actual.gate.denied_types` | Which gates, `planning` and `implementation`, had denied any of the overridden rules earlier in the session, and an empty list when the human overrode before any denial | anonymous |
| `actual.action.type` | The check: `plan-check` or `impl-check` | anonymous |
| `actual.action.mode` | How the check ran: `hook` when Claude Code ran it as a hook, `direct` when someone ran the command | anonymous |
| `actual.action.outcome` | What the check came to, as its plan-governance `check_completed` event records it: `allow`, `warn` or `block` | anonymous |
| `actual.action.warn_reasons` | What made a `warn` one, and an empty list for `allow` and `block`: `round_limit` when a hook let a round through because every rule it blocked on had been denied more times than `--max-rounds` allows, `requires_decision` when a run outside a hook judged a rule as needing a decision, `partial_coverage` when the judge saw only some of the rules that apply, and `active_override` when a hook round passed while the session carries an override. A round the round limit let through names only `round_limit`, even when the judge saw only some of the rules or the session carries an override. Any other check that warns for more than one of these names each | anonymous |
| `actual.action.duration_ms` | How long the check took, in milliseconds | anonymous |

The ADR references name ADRs Actual generated for your repository, so Actual
can map them back to that repository and its organization. Each event also
names the CLI, its version, the deployment environment (`production` for
release builds) and the surface (`coding-agent`), records when it happened
(when the ADR entered context, when the gate's judge answered, when the
override was recorded, or when a check started or completed), and, when the
environment carries a W3C `TRACEPARENT`, copies its trace id, span id and flags.
**No event carries rule text, plan or diff text, override reasons, file paths,
raw account identifiers, identity hashes or installation id.**

None of these runs contacts the endpoint itself. Each writes its events to a
local spool (`~/.actualai/actual/otlp-logs-spool.jsonl`, readable only by the
owner, bounded like the scope spool) and starts the same detached uploader,
which sends them in batches for up to five seconds in all, retries included. A
batch still being sent when the time runs out is dropped, and the batches it
hasn't reached stay in the spool for its next run. When the endpoint is at
the address `actual advisor` uses, each spooled event also keeps a hash of
the sign-in it was recorded under, which is never sent, and the uploader
reads your sign-in from
`~/.actualai/actual/credentials.yaml` when it sends, renewing it first when it's
within a minute of expiring, as `actual advisor` does. Turning telemetry off
stops these events before anything is built: an opted-out run reads no ids or
sign-in and writes no spool. Building and sending them reads no installation id
either. Only the plan-governance events a run also builds need it, so a
`plan-check`, `impl-check` or `check-override` run reads it, and creates
`telemetry-id` when it's missing, while a `rules brief --claude-hook` run never
does. What Actual does with the events its API receives, signed in or out, is
covered next. Whoever operates any other endpoint you set decides how long it
keeps them.

#### Signed-in events sent to Actual's API

The events in a signed-in request to Actual's API are first-party,
identity-linked data:

- **Who can attribute them:** Actual, as the first party, via its authorized
  backend systems and personnel. They don't go to PostHog or any other
  third-party analytics service.
- **Purpose:** to tie these events to an organization so Actual can measure and
  improve the CLI's ADR guidance and checks for each customer.
- **Retention & deletion:** they're kept for as long as needed to provide
  product analytics for the organization, and are deleted
  when the organization is deleted or on a verified deletion request for it.

#### Signed-out events sent to Actual's API

A request to Actual's API without your sign-in names no account or
organization, and Actual records its events as signed out. That happens when
you recorded them while signed out or under another sign-in, when you've
signed out by the time they're sent or your sign-in has expired and can't be
renewed, when Actual refused the token, and when the endpoint isn't at the
address `actual advisor` uses, as when the config file's `api_url` or
`ACTUAL_API_URL` names another one. The events still carry the fields in the
table above, with the classes given there:

- **Who can attribute them:** Actual, as the first party, via its authorized
  backend systems and personnel. The ADR references lead to the repository the
  ADRs were generated for and to its organization, and a session that also sent
  a signed-in request links its signed-out events to that request's
  organization. They don't go to PostHog or any other third-party analytics
  service.
- **Purpose:** to measure and improve the CLI's ADR guidance and checks across
  everyone who uses it.
- **Retention & deletion:** they're kept for as long as needed for that purpose.
  They aren't stored with an account or organization, so a deletion request
  can't remove them.

### Project metadata (sent to the Actual API)

When fetching ADRs, the CLI sends a match request containing:

| Field | Description |
|-------|-------------|
| `path` | Relative path of each detected project (e.g. `"."`, `"packages/api"`) |
| `name` | The package name in the project's `package.json`, `Cargo.toml` or `pyproject.toml`, or the last segment of the module path in its `go.mod`, not counting a major-version suffix such as `/v2`. The directory name is the fallback when none of those gives one |
| `languages` | Detected programming languages (e.g. `["typescript", "python"]`) |
| `frameworks` | Detected frameworks with categories (e.g. `[{"name": "nextjs", "category": "web-framework"}]`) |
| `canonical_ir` | What the CLI's built-in analysis rules found in the project: `ir_hash`, a SHA-256 hash of the full result; `taxonomy_version`; and `facets`, each with a category (`facet_slot`), the ids of the rules that matched (`rule_ids`) and a `confidence` score. The request leaves out the matched values and the files they came from |
| `options` | The ADR filters you've set in the config file, if any: `categories` (from `include_categories`), `exclude_categories`, `include_general` and `max_per_framework`. One per request, where the fields above repeat per project |

This metadata is used to match relevant ADRs to your project. **The request
carries no source code or repository URLs.**

### Advisor questions (sent to the Actual API)

`actual advisor` sends your question to Actual's API exactly as you typed it, so
anything you put in it goes too. The request also carries your sign-in's access
token, the raw id of your organization or of the one you named with `--org`,
and, when the question is scoped to a connected repository, that repository's
raw id. These are the ids themselves, not the hashes telemetry uses.

To find the repository, the CLI can fetch your organization's connected
repositories from the API and compare your `origin` remote with that list on
your machine, so the remote's URL isn't sent. The request goes to
`https://api-service.api.prod.actual.ai` unless `--api-url` or `ACTUAL_API_URL`
names another address. It isn't telemetry, so turning telemetry off doesn't stop
it.

### LLM tailoring (sent to your configured AI provider)

During the tailoring step of `actual adr-bot`, the CLI sends prompts through a
runner: the one you set with `--runner` or the `runner` config key, or else one
it detects on your machine. `anthropic-api` and `openai-api` post them to that
provider's API. `claude-cli`, `codex-cli` and `cursor-cli` hand them to the
`claude`, `codex` or `cursor-agent` tool on your machine, which sends them
according to its own settings. `codex-cli` has one exception: if `codex` exits
normally with no answer and an error that reads like a model problem, such as
`model not found` or `does not exist`, and an OpenAI API key is set
(`OPENAI_API_KEY` or the `openai_api_key` config key), the CLI retries through
`openai-api` without asking you again. That runner posts the prompts straight
to OpenAI's API under that key, whatever `codex` is set to use. None of this
passes through Actual's servers.
Refer to your AI provider's privacy policy for how they handle this data:

- [Anthropic Usage Policy](https://www.anthropic.com/policies)
- [OpenAI Privacy Policy](https://openai.com/policies/privacy-policy)
- [Cursor Privacy Policy](https://cursor.com/privacy)

Each prompt holds:

- The ADRs being tailored.
- The detected project: its path, name, languages with their line counts,
  frameworks, package manager and dependency counts.
- The text of the repository's existing output files (`CLAUDE.md`, `AGENTS.md`
  or `.cursor/rules/actual-policies.mdc`, whichever you're generating), with no
  length limit.
- A file tree of up to 200 file and directory names.
- The contents of key files: manifests such as `package.json` and `Cargo.toml`
  and config files such as `tsconfig.json` and `vite.config.ts` in the
  repository root, plus up to five entrypoints such as `main.rs` or `index.ts`
  from anywhere in it. Each file is cut at 200 lines, and together they're
  capped at 32 KB.

Key files go as they are, so a manifest's `repository` field or a `go.mod`
module path tells the provider where your repository lives. The tree and the key
files leave out `.env`, `.npmrc`, `.netrc`, private keys and any file with
`credential` or `secret` in its name. That check reads file names only, so a
secret inside a file the CLI does send goes with it.

Those limits describe what the CLI puts in a prompt, and an agent can read past
them. `claude-cli` gives the model file tools including Read, Glob and Grep for
up to 10 turns, or the `max_turns` you set in the config. `codex-cli` and
`cursor-cli` run as agents in your repository, and the prompt lets all three
read files it left out. Whatever they read goes to their provider.
`anthropic-api` and `openai-api` have no such tools, so their provider gets the
prompt alone.

`actual adr-bot --no-tailor` skips this step, and no prompt is sent.

### Plan and diff checks (sent to your configured AI provider)

`actual plan-check` and `actual impl-check` ask your AI provider whether a plan
or a diff follows the rules that apply to it, and `actual rules select` can ask
it to rank rules against a plan. They use the same runners as tailoring, so
their prompts reach the provider the same way and don't pass through Actual's
servers. Only tailoring makes the `codex-cli` retry. Claude Code runs both
checks through their `--claude-hook` paths, so with those hooks installed this
text goes out as you work, without anyone typing a command.

| Command | What the prompt holds |
|---------|-----------------------|
| `plan-check` | The plan's full text, and each judged rule's document slug, id, level and statement |
| `impl-check` | The diff's full text, including the whole of any new file git doesn't ignore, and the same rule fields |
| `rules select` | The plan, the paths it names or `--file` adds, and each candidate rule's slug, title, scope and up to four path patterns. Sent only when the first pass finds more rules than `--limit` keeps |

`claude-cli` runs these calls with no tools, and the API runners have no file
tools. `codex-cli` and `cursor-cli` still run as agents in your repository.
These prompts don't ask them to open other files, but the CLI doesn't stop them,
and anything they read goes to their provider.

A `plan-check` or `impl-check` you run yourself can make that `rules select`
call first, with the diff standing in for the plan on `impl-check`. The hook
paths never make it, `--no-rank` skips it, and so does `rules select --by-adr`.
`rules eval --rank` can make it for each case in the file you give it. No prompt
goes out when no rule applies or no runner is available. Turning telemetry off
doesn't stop any of these calls.

`actual check-override` keeps the reason you give in two files on your machine:
the session's record under `~/.actualai/actual/plan-check-sessions/` and
`~/.actualai/actual/plan-check-overrides.log`. The reason stays out of every
prompt above, but it does come back to the agent. Later `--claude-hook` rounds
in that session that reach a verdict repeat it, word for word, in what they
return to Claude Code: a round that blocks puts it in the denial the agent
reads, and a round that lets the plan or diff through shows it as a notice. From
there it can reach the provider running the agent.

### Update check (sent to the npm registry)

At most once a day, after a command run at a terminal, the CLI asks the npm
registry for the latest published version of `@actualai/actual`. It is one
HTTPS `GET` of `https://registry.npmjs.org/@actualai/actual/latest` with a
`User-Agent: actual-cli/<version>` header, made by a background process so the
command never waits for it. It carries no account, repository, file or
telemetry data; like any web request, it shows npm your IP address.
`actual version --check` makes the same request when you run it. The answer is
kept in `~/.actualai/actual/update-check.yaml`.

`ACTUAL_NO_UPDATE_CHECK=1`, or `update_check.enabled: false` in the config
file, stops the request entirely. See [docs/UPDATE_CHECK.md](docs/UPDATE_CHECK.md).

## Where data is sent

| Destination | Data | Protocol |
|-------------|------|----------|
| `api-service.api.prod.actual.ai` | Telemetry counters, project metadata, `actual advisor` questions, `actual auth create-token` requests (your sign-in's access token, plus the new token's name and scopes), plan-governance events, and ADR and check events from release builds unless you set another OTLP endpoint | HTTPS (enforced) |
| PostHog (via the `api-service.api.prod.actual.ai` proxy only) | Plan-governance events, forwarded server-side | N/A -- the CLI never contacts PostHog directly |
| `app.actual.ai`, Actual's sign-in server, unless `actual login --api-url`, `actual mint-token --issuer` or `ACTUAL_AUTH_URL` names another address | Sign-in requests, with no repository or project data. `actual login` sends a one-time code and then the access token it gets back, after opening the sign-in page in your browser. `actual logout` sends the token it revokes, and `actual mint-token` sends a signed service-account assertion. Renewing an access token that expires within a minute sends your refresh token, which `actual advisor`, `actual auth create-token` and the telemetry uploader do for you | HTTPS (enforced; plain HTTP only to localhost) |
| `files.pythonhosted.org`, PyPI's file host | A download of the `semgrep-core` scanner: a fixed file for your OS and CPU, requested by `actual adr-bot` when the CLI has no copy of its own, even if you've installed Semgrep yourself, and kept on your machine afterwards. It carries no account or repository data; like any web request, it shows PyPI your IP address. Turning telemetry off doesn't stop it | HTTPS |
| `registry.npmjs.org` | A request for the latest CLI version: at most once a day in the background, and each time you run `actual version --check`; no account or repository data | HTTPS |
| Your AI provider (Anthropic, OpenAI or Cursor) | ADR content + repo context for tailoring, plan or diff text + rule text for `plan-check`, `impl-check` and `rules select`, and, for `actual models`, a request for the model list that holds only your API key | HTTPS for the API runners, including tailoring's `codex-cli` retry through `openai-api`, and for `actual models`; the tool's own connection for `claude`, `codex` and `cursor-agent` |
| The OTLP logs endpoint: `/v1/logs` on `api_url` in release builds, or the one you set | ADR context, use, breach and override events, and check events, plus your sign-in's access token when the endpoint is at the address `actual advisor` uses and you're signed in | OTLP/HTTP with binary protobuf over HTTPS (enforced; plain HTTP only to localhost) |
| The local file you configure (none by default) | ADR context, use, breach and override events, and check events, one OTLP/JSON request per line | Local file, readable only by the owner |

All non-localhost API communication enforces HTTPS. The CLI will reject
non-HTTPS API URLs.

## How to opt out of telemetry

One setting turns telemetry off:

```bash
actual config set telemetry.enabled false
```

It writes this to `~/.actualai/actual/config.yaml`:

```yaml
telemetry:
  enabled: false
```

The setting lasts across shells and applies to every run that reads that file,
including the `--claude-hook` paths Claude Code runs for you.

**This one setting stops every telemetry event**: the counters, the
plan-governance events that reach PostHog through Actual's proxy, and the ADR
and check events, whether they go to Actual's API or to an endpoint or file you
set. You don't need a second switch for any of them.

Events an earlier run left queued in the local spools stay on your machine,
unsent, while telemetry is off, and the uploader sends them if you turn it back
on. An upload already running when you change the setting finishes.

Change the setting when no `actual adr-bot` run is in progress. A run that's
already going still sends its counters, and the config it saves at the end can
put the old value back, which leaves telemetry on.

The setting covers telemetry, not every network request. The update check has
its own switch (see "Update check" above). The requests that do the work you
asked for still go out, such as `actual adr-bot`'s match request and
`semgrep-core` download, `actual advisor` questions, sign-in, and prompts to your
AI provider.

For CI and one-off runs, set `ACTUAL_NO_TELEMETRY` instead. It has the same
effect on the process it's set for and the commands that process starts, and any
non-empty value works:

```bash
export ACTUAL_NO_TELEMETRY=1
```

If you build from source, `cargo build --release --no-default-features` leaves
telemetry out of the binary.

## Data retention

Telemetry counters are aggregated and carry no account identity. Both
`repo_hash` and `repo_url_hash` are one-way SHA-256 hashes -- the original
repository URL cannot be *recovered* from a hash. They are not peppered, though,
so anyone who already knows a candidate repository URL can recompute its hash and
match its events; they identify a repository to someone who can guess it, not a
person directly.

First-party, identity-linked logged-in data is covered above under "First-party
identity data": the raw-id → hash association and the server-side pepper are
held only by Actual's authorized backend systems, are used solely for product
analytics, and are deleted or de-linked on account deletion or a verified
deletion request. Peppering prevents PostHog from reversing the account/org/repo
identity hashes to raw ids, but the unpeppered repository-derived keys reach
PostHog recomputable, as described above. Opting out of telemetry stops these
events at the source, so no identity hashes are produced at all.

Signed-in OpenTelemetry events sent to Actual's API are covered above under
"Signed-in events sent to Actual's API": they're held only by Actual's
authorized backend systems, are used solely for product analytics, and are
deleted when the organization is deleted or on a verified deletion
request for it. Signed-out ones are covered under "Signed-out events sent to
Actual's API": they aren't stored with an account or organization, so a deletion
request can't remove them. Opting out of telemetry stops both before they're
built.

## Reviewing changes to telemetry identity

Any change that adds, removes, or alters a telemetry field or identifier — or
how identifiers are hashed, peppered, or mapped — must re-check this notice
before it ships. The pull-request template carries the checklist; at minimum:

- Every new or changed field is listed above with an explicit privacy class
  (**anonymous**, **pseudonymous**, or **first-party identity-linked**).
  Pseudonymous means the field carries no account identity, but a stable key
  such as an installation id or a repository hash can link events to each other,
  to a known repository, or, once that installation has sent a logged-in event,
  to an account.
- No field Actual can attribute to an account is described as "anonymous" or as
  carrying "no PII"; hashing alone is never treated as anonymization.
- For any identity-linked field, this notice states who can attribute it, the
  purpose, and the retention/deletion behavior.
- Wording is coordinated with the paired backend (`sprintreview`) change, and
  privacy/product approval is obtained for identity-field or classification
  changes.

## Changes to this policy

Material changes to data collection will be documented in this file and
noted in release changelogs.
