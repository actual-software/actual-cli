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
| `plan_governance_check_started` | Built alongside `plan_governance_check_completed` and sent in the same batch, once a run has reached a verdict -- not emitted separately at the actual start of the check |
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

The `rules select` scope-selection event is delivered through a small durable
outbox rather than a single in-process send: because that command is
short-lived, the event is first written to a local spool file
(`~/.actualai/actual/scope-telemetry-spool.jsonl`, created readable only by the
owner) and then delivered by a separate best-effort uploader, with the entry
removed once the proxy confirms it. The spool holds **only** the same
already privacy-filtered event payload that would go on the wire -- counts, enums,
hashed ids, `distinct_id`, `insert_id` -- never plan text, rule text, file
paths, or any raw identifier. It is bounded in size (the oldest entries are
dropped once it is full) and is never written for an opted-out run.

All three telemetry opt-outs described below disable plan-governance events
identically -- they share the exact same opt-out check as the sync counters,
and are checked before any repo-identity hashing or `distinct_id` creation,
not just before the network send: an opted-out run never touches git and
never writes `~/.actualai/actual/telemetry-id`.

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

## Where data is sent

| Destination | Data | Protocol |
|-------------|------|----------|
| `api-service.api.prod.actual.ai` | Telemetry counters, project metadata, plan-governance events | HTTPS (enforced) |
| PostHog (via the `api-service.api.prod.actual.ai` proxy only) | Plan-governance events, forwarded server-side | N/A -- the CLI never contacts PostHog directly |
| Your AI provider (Anthropic/OpenAI) | ADR content + repo context for tailoring | HTTPS |

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
