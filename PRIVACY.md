# Privacy & Telemetry

Actual CLI collects minimal telemetry to help us understand usage patterns and
improve the product. How your data is handled depends on whether you are logged
in and on who processes it. This notice is explicit about both:

- **Logged-in events link to your Actual account.** When you are logged in,
  plan-governance events carry one-way `user_id_hash`/`org_id_hash` values.
  Because the CLI computes them deterministically from your account and
  organization ids, Actual's backend resolves them back to your account and
  organization — so logged-in telemetry is first-party, identity-linked data,
  **not** anonymous. The raw ids never leave your machine, but hashing alone
  does not make identity-linked data anonymous.
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

This document describes exactly what data is collected, its privacy class, where
it goes, who can attribute it to you, how long it is kept, and how to opt out.

## What data is collected

### Telemetry counters (anonymous)

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
| `repo_hash` | SHA-256 hash of `(repo_url + commit_hash)` -- the raw URL and commit are **never** sent |
| `repo_url_hash` | SHA-256 hash of the normalized repo URL alone -- stable across commits, used to count unique repos |
| `source` | Always `"actual-cli"` |
| `version` | CLI version string (e.g. `"0.1.2"`) |

The `cli.sync.adrs_recommended` metric also includes:

| Tag | Description |
|-----|-------------|
| `adr_ids` | Comma-separated list of ADR IDs that were matched for this repo (e.g. `"adr-001,adr-002"`) |

ADR IDs are internal Actual AI identifiers and are not sensitive. Repository identity
remains one-way hashed -- raw URLs are never sent.

Telemetry does not include raw usernames, email addresses, IP addresses, file
contents, file paths, or source code. When you are logged in, plan-governance
events include one-way identity hashes that let Actual, as the first party,
attribute activity to your Actual account, organization, and connected
repository; the raw identifiers are never sent by the CLI, but these hashes are
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
| `user_id_hash` | One-way SHA-256 of `"user:"+<user id>`, only when logged in -- the raw user id is **never** sent; peppered server-side before PostHog |
| `org_id_hash` | One-way SHA-256 of `"org:"+<organization id>`, only when logged in -- the raw org id is **never** sent; peppered server-side before PostHog |
| `repo_id_hash` | One-way SHA-256 of `"repo:"+<connected-repo id>`, sent whenever that id is known locally -- **including logged-out runs where a repository pin persists**; the raw id is **never** sent; peppered server-side before PostHog |
| `org_id` | A synthetic, repo-derived org identifier (a UUID from the repo slug), sent **only when not logged in**, so logged-out runs still have an org-shaped grouping key -- derived deterministically from the repo slug and **not** peppered, so recomputable from the repository |
| `reason_category` | `check-override` only: a coarse length bucket of the override reason (`empty`/`short`/`medium`/`long`) -- derived from length, never the text |
| `reason_len` | `check-override` only: the character length of the override reason |
| `reason_hash` | `check-override` only: a one-way SHA-256 of the override reason (peppered server-side before PostHog) -- lets identical reasons group without exposing the text |
| `scope_run_id` | `rules select` only: a random per-run correlation id (not derived from any data) |
| `rules_scanned` / `stage1_candidates` / `selected` | `rules select` only: counts of rules scanned, stage-1 candidates, and rules finally selected -- numbers only |
| `stage2_invoked` / `stage2_status` | `rules select` only: whether the LLM rank ran, and its status/degradation (`applied`/`not-needed`/`unavailable`/…) |
| `cache_hit` | `rules select` only: whether the scope index came from cache |
| `runner` | `rules select` only: the rank runner label when stage 2 ran |

**No plan text, diff content, matched rule file paths, or conflicting
plan/diff spans are ever sent** -- only the rule's internal id/slug and a
coarse allow/warn/block verdict.

#### First-party identity data (logged-in runs)

The `user_id_hash` and `org_id_hash` fields are present only when you are logged
in; `repo_id_hash` is present whenever a connected-repo id is known locally,
which can include logged-out runs where a repository pin persists. All three are
one-way SHA-256 digests of Actual-internal ids: the raw ids never leave your
machine, so the CLI payload on its own does not contain them. They let Actual
group metrics by account, organization, and repository without putting those ids
in the payload.

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
an unambiguous denominator. **The override reason is customer free text and is
never sent raw** -- only `reason_category`, `reason_len`, and a one-way
`reason_hash` (see above) leave the machine.

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

All three telemetry opt-outs described below disable plan-governance events
identically -- they share the exact same opt-out check as the sync counters,
and are checked before any repo-identity hashing or `distinct_id` creation,
not just before the network send: an opted-out run never touches git and
never writes `~/.actualai/actual/telemetry-id`.

### ADR events (OpenTelemetry, off unless you configure an output)

The CLI can send two OpenTelemetry log events about ADRs:

- `actual.adr.detected`, with `actual.detection.method` set to `context`, the
  first time any of an ADR's rules is added to an agent's context during a
  Claude Code session, even when the brief has room for only some of them. It
  comes from the `rules brief --claude-hook` path Claude Code drives, and it
  counts an ADR once per session, whichever agent or subagent it reached and
  however often the context is compacted. A manual `actual rules brief` run
  sends nothing.
- `actual.gate.evaluated`, with `actual.gate.outcome` set to `overridden`, each
  time a human records an override with `actual check-override`: one event per
  ADR with a rule the command names, every time it runs.

A rule file without a v2 header sends neither. The events exist to evaluate
the CLI: whoever receives them can see which ADRs each session's agents were
given, and which of them a human overrode.

**Nothing is sent, or built, unless you configure an output.** Both are unset by
default:

- `telemetry.otlp_logs_endpoint` (or the `ACTUAL_OTLP_LOGS_ENDPOINT` env var):
  an OTLP/HTTP logs URL, used as-is. Events are sent to it as binary protobuf
  (`Content-Type: application/x-protobuf`). It must use HTTPS unless it's
  localhost, and the CLI doesn't follow redirects, so events reach that URL and
  no other. The standard `OTEL_EXPORTER_OTLP_*` variables aren't read.
- `telemetry.otlp_file` (or `ACTUAL_OTLP_FILE`): an absolute path on your
  machine. Each request is appended to it as one line of OTLP/JSON, in a file
  readable only by the owner. A file that already exists is restricted to its
  owner before anything is added, and nothing is written to a path that isn't
  a regular file or whose permissions can't be changed.

Set either with `actual config set`, and clear it by setting it to `""`. These
events don't go to PostHog or to `api-service.api.prod.actual.ai`, only where
you point them.

Every event carries each of these attributes except `actual.detection.method`,
which only context events carry, and `actual.gate.outcome` and
`actual.gate.denied_types`, which only override events carry. An unknown value
is sent as an empty string. None of them names you, your organization, your
repository or this installation, so an event carries the same fields logged in
or out:

| Attribute | What it is | Privacy class |
|-----------|------------|---------------|
| `log.record.uid` | The event's `actual.event.id`, repeated under the name OpenTelemetry tools use to drop a duplicate delivery | anonymous |
| `actual.event.id` | A fresh random UUIDv7 per event | anonymous |
| `actual.event.schema_version` | The version of this attribute set, currently `1` | anonymous |
| `actual.action.id` | A fresh random UUIDv7 per run of the CLI, shared by every event that run sends | anonymous |
| `actual.session.id` | Claude Code's id for the conversation: random, generated by Claude Code, never derived from account data. It groups one conversation's events. An override event carries the id the human passed to `check-override --session`, which the gate's denial printed | first-party identity-linked |
| `actual.agent.id` | Claude Code's id for the subagent that was briefed, empty for the main agent and on every override event: random, generated by Claude Code | first-party identity-linked |
| `actual.adr.ref` | The ADR: its group id from its rule files' header, or its one file's `adr_id` when they name no group | first-party identity-linked |
| `actual.adr.ids` | The `adr_id` of each of the ADR's rule files with at least one rule in the brief that added it, or, on an override event, with at least one rule the override named | first-party identity-linked |
| `actual.detection.method` | Context events only. How the ADR was detected: `context` | anonymous |
| `actual.gate.outcome` | Override events only. What the evaluation came to: `overridden` | anonymous |
| `actual.gate.denied_types` | Override events only. Which gates, `planning` and `implementation`, had denied any of the overridden rules earlier in the session, and an empty list when the human overrode before any denial | anonymous |

The ADR references name ADRs Actual generated for your repository, so Actual
can map them back to that repository and its organization. Each event also
names the CLI, its version, the deployment environment (`production` for
release builds) and the surface (`coding-agent`), records when it happened
(when the ADR entered context, or when the override was recorded), and, when
the environment carries a W3C `TRACEPARENT`, copies its trace id, span id and
flags. **No rule text, plan text, override reasons, file paths, raw account
identifiers, identity hashes or installation id are sent.**

Neither the hook nor `check-override` contacts the endpoint itself. Each writes
its events to a local spool (`~/.actualai/actual/otlp-logs-spool.jsonl`,
readable only by the owner, bounded like the scope spool) and starts the same
detached uploader, which sends them in batches for up to five seconds in all,
retries included. A batch still being sent when the time runs out is dropped,
and the batches it hasn't reached stay in the spool for its next run. All three
opt-outs below stop these events before anything is built: an opted-out run
reads no ids and writes no spool. Building and sending them reads no
installation id either. Only the plan-governance events a run also builds need
it, so a `check-override` run reads it, and creates `telemetry-id` when it's
missing, while a `rules brief --claude-hook` run never does. Whoever operates
the endpoint you configure decides how long it keeps the events.

### Project metadata (sent to the Actual API)

When fetching ADRs, the CLI sends a match request containing:

| Field | Description |
|-------|-------------|
| `path` | Relative path of each detected project (e.g. `"."`, `"packages/api"`) |
| `name` | Project directory name |
| `languages` | Detected programming languages (e.g. `["typescript", "python"]`) |
| `frameworks` | Detected frameworks with categories (e.g. `[{"name": "nextjs", "category": "web-framework"}]`) |

This metadata is used to match relevant ADRs to your project. **No source
code, file contents, or repository URLs are sent to the Actual API.**

### LLM tailoring (sent to your configured AI provider)

During the tailoring step, ADR content and a bundled summary of your
repository structure are sent to your configured AI provider (Anthropic,
OpenAI, or a local runner). This data is sent directly to the provider's
API -- it does not pass through Actual servers. Refer to your AI provider's
privacy policy for how they handle this data:

- [Anthropic Usage Policy](https://www.anthropic.com/policies)
- [OpenAI Privacy Policy](https://openai.com/policies/privacy-policy)

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
| `api-service.api.prod.actual.ai` | Telemetry counters, project metadata, plan-governance events | HTTPS (enforced) |
| PostHog (via the `api-service.api.prod.actual.ai` proxy only) | Plan-governance events, forwarded server-side | N/A -- the CLI never contacts PostHog directly |
| `registry.npmjs.org` | A request for the latest CLI version: at most once a day in the background, and each time you run `actual version --check`; no account or repository data | HTTPS |
| Your AI provider (Anthropic/OpenAI) | ADR content + repo context for tailoring | HTTPS |
| The OTLP logs endpoint you configure (none by default) | ADR context and override events | OTLP/HTTP with binary protobuf over HTTPS (enforced; plain HTTP only to localhost) |
| The local file you configure (none by default) | ADR context and override events, one OTLP/JSON request per line | Local file, readable only by the owner |

All non-localhost API communication enforces HTTPS. The CLI will reject
non-HTTPS API URLs.

## How to opt out of telemetry

There are three independent ways to disable telemetry:

### 1. Environment variable (recommended)

```bash
export ACTUAL_NO_TELEMETRY=1
```

Any non-empty value disables telemetry. Add this to your shell profile for
a permanent opt-out.

### 2. Config file

Add to `~/.actualai/actual/config.yaml`:

```yaml
telemetry:
  enabled: false
```

### 3. Compile-time

Build from source without the telemetry feature:

```bash
cargo build --release --no-default-features
```

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
PostHog recomputable, as described above. Opting out of telemetry (any method
above) stops these events at the source, so no identity hashes are produced at
all.

## Reviewing changes to telemetry identity

Any change that adds, removes, or alters a telemetry field or identifier — or
how identifiers are hashed, peppered, or mapped — must re-check this notice
before it ships. The pull-request template carries the checklist; at minimum:

- Every new or changed field is listed above with an explicit privacy class
  (**anonymous**, or **first-party identity-linked**).
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
