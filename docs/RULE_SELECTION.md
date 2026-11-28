# Selecting rules for a plan

`actual rules select` answers which committed rule documents govern a proposed
change — a plan, one or more `--file` paths, or both — and says why for each
one. It runs in two stages. This document records what each stage is for, when
the second one is paid for, what happens when it cannot run, and where the
design is weak.

The retrieval stage is described in [SCOPE_INDEX.md](SCOPE_INDEX.md); this
document covers the selection built on top of it.

## Two stages

**Stage 1 is the deterministic prefilter.** The scope index ranks every rule
document against the plan using path globs (a v2 header's declared `paths`, else
the `### Verify` operands), the prose scope sentence, the aspect slug and the
title. Decisions a v2 header marks inactive are not indexed, so neither stage
sees them; see [SCOPE_INDEX.md](SCOPE_INDEX.md#adr-file-header-v2). No model, no network,
sub-millisecond against a built index. It is the whole answer whenever it can be.

**Stage 2 is a runner-backed rank over what stage 1 retrieved.** It is asked one
question — how strongly each of these candidates governs this change — and it is
asked only when stage 1 hands back more candidates than the caller may keep
*and* the query has plan prose to judge against. A path-only query
(`--file` with no `PLAN`) stops after stage 1: there is nothing for the model
to read.

That trigger is the latency contract. Below the cap there is nothing to discard,
so a model call would buy nothing. Above it, something has to go, and a lexical
score is not a good enough reason to choose. Calling only over the surplus makes
the model cost proportional to the ambiguity actually present, which is what
makes the command usable inside a synchronous hook rather than only in a webhook.

Stage 2 reuses the five existing runners — claude-cli, anthropic-api, openai-api,
codex-cli, cursor-cli — through `StructuredRunner`, a trait lifted out of the
tailoring path. Each runner's structured-output plumbing is shared between the
two callers rather than duplicated; only the schema and the prompt differ.

**The answer is one score per candidate, and nothing else.** The prompt numbers
the candidates and asks for a score from 0 to 9 for each, keyed by its number.
Almost all of a rank's time is the model writing its answer: asked for a slug,
a verdict and a one-sentence reason for each of 30 candidates, a model wrote
about 2,400 tokens and took about 16 seconds; asked for a score each, it writes
about 150 tokens and takes about 4. Each selected rule's reason is stage 1's
evidence for it, the same reason `--no-rank` prints. A rank call also asks for
low reasoning effort and no extended thinking, on the backends that take either
(`--effort low` and `alwaysThinkingEnabled: false` for claude-cli,
`model_reasoning_effort=low` for codex-cli): a model left to think wrote
thousands of tokens over a list it then scored the same way.

**Default models.** When neither `--model` nor the config names one, a rank uses
`claude-sonnet-5-5` on claude-cli and anthropic-api and `gpt-6.2-sol` on
codex-cli. The other calls that resolve a runner — the conformance judge,
`adr adopt` — keep their own defaults.

**In the gates.** `plan-check` and `impl-check` rank before they judge, with
the plan or the turn's diff as the query, and so do the plugin's plan and
implementation gates, which run them from hooks. A hook's rank keeps to a
35-second deadline instead of 90, so the conformance judge keeps the rest of
the hook's timeout, and a rank that misses it falls back to stage 1 like any
other failure. The gates retrieve more candidates than `rules select` and
keep more of them: see [The gates' defaults](#the-gates-defaults). `--no-rank` skips the rank in every mode, and the plugin passes
it when `ACTUAL_GATE_RANK=off`. The hooks used to skip stage 2, when a rank
took about 16 seconds; the measurements below show why they no longer do.

## What the model is allowed to do

Three constraints, each enforced in code rather than requested in the prompt.

**It may only judge, never invent.** The prompt carries a fixed, numbered
candidate list, and the answer is a score per number. A number outside the list
is ignored. A hallucinated rule file is impossible by construction, not merely
unlikely.

**It orders only by its score.** Each candidate's score is collapsed into one
of three verdicts: 7 to 9 is `governs`, 3 to 6 is `related`, and 0 to 2 is
`unrelated`. `unrelated` is dropped; the rest go highest score first, and equal
scores keep stage 1's order. A language model is far better calibrated on "does
this rule govern this change" than on "is this rule the third or the fourth
most relevant", so the score is coarse on purpose. On a labelled set of 25
merged pull requests, keeping stage 1's order inside each verdict picked
governing rules for `rules select` as well as ordering by the score did; at the
gates' larger limits, ordering by the score put 0.2 to 1.4 points more of the
governing rules in front of the judge, so the score order is what ships.

**It may not empty the result.** A rank that keeps nothing is treated as a failed
rank, and the caller falls back to stage 1. Returning nothing is never more
useful than returning the deterministic prefilter.

## Degrading

Every path produces an answer. No runner configured, the runner down, the runner
returning the wrong shape, the runner scoring only numbers it was never given,
the runner rejecting everything: each degrades to the stage-1 answer and records
why in the `Stage 2` line. None of them is an error, and none of them costs the
caller a reason — every selected rule's reason is the deterministic one built
from the index's own attribution, ranked or not.

```
Stage 2: unavailable — no runner available: claude-cli: binary not found …
Stage 2: failed — connection refused
Stage 2: not needed — the prefilter returned 3 candidate(s), inside the cap
Stage 2: no plan — no plan prose to rank against
Stage 2: ranked 10 candidate(s): 1 governs, 3 related, 6 unrelated, 0 unjudged
```

`--no-rank` asks for stage 1 alone: offline, and exactly reproducible. A
path-only query also skips stage 2, but that is recorded as `no plan` rather
than `not requested` — the caller did not opt out; there was nothing to rank
on.

## Reproducibility

Everything under the tool's control is a deterministic function of the plan and
the rule set: the candidate set, the order it is presented in, the prompt bytes,
and the ordering rule applied to whatever comes back. A test asserts that two
runs of both stages produce byte-identical prompts and equal selections.

What is not under the tool's control is the model. The design confines that:
the model partitions a fixed list into three buckets, and everything else about
the answer is computed. Two runs that get the same verdicts return the same
selection; two runs that get different verdicts differ only in which candidates
were kept, never in what was on the list or how the survivors were ordered.

`--no-rank` is the exactly-reproducible mode, and it is what the CI regression
gate measures.

## Measurements

The two-stage selector is scored on the same golden set, at the same cap, as the
two offline selectors:

```bash
cargo run -- rules eval --golden tests/fixtures/scope_corpus/golden.json \
  --repo tests/fixtures/scope_corpus --limit 5 --rank
```

`--rank` is opt-in because it costs one runner call per case. The default run is
the offline comparison, which is what CI can afford on every commit.

`--rank` measures the shipped command, trigger included: it reaches stage 2
through the same `Prefiltered::rank_with` that `rules select` uses, so a case
whose prefilter already fits the cap is scored without a model call, exactly as
the command would answer it. That matters because an ungated measurement would
be free to trim a set the command returns whole, and would report a precision
the command never achieves.

**Fixture corpus** — 42 rule files, 19 plans, cap 5, stage 2 on `claude-cli`
with `claude-sonnet-5-5`:

| selector | micro P | micro R | micro F1 |
|---|---|---|---|
| two-stage | 0.91 | 0.82 | **0.86** |
| scope index | 0.62 | 0.89 | 0.73 |
| filename scan | 0.58 | 0.63 | 0.60 |

Precision is where the rank pays, and it pays where the design says it should:
0.91 against the index's 0.62, with recall at 0.82 against 0.89. The earlier
prompt, which asked for a verdict and a reason per candidate, scored F1 0.87 on
the same model and 0.89 on `claude-sonnet-4-6`, at about four times the
latency.

**Merged pull requests** — 25 recent pull requests from one repository with 755
rule files, each labelled by two judge models (Opus and a GPT model) reading the
diff, cap 10, scored on the 23 with at least one governing rule:

| selector | coverage@5 | coverage@10 | median latency |
|---|---|---|---|
| two-stage, `claude-cli` | 0.52 | 0.50 | 4.0 s |
| two-stage, `codex-cli` | 0.56 | 0.52 | 8.6 s |
| earlier prompt, `claude-cli` (Sonnet) | 0.55 | 0.50 | 15.7 s |
| scope index | 0.38 | 0.42 | 0.1 s |

coverage@k is the governing rules among the first k, over the number that fit
in k. Against the earlier prompt on the same pull requests, the difference in
coverage@5 is −0.03 [−0.11, +0.03] on `claude-cli` and +0.00 [−0.07, +0.07] on
`codex-cli` (95% intervals, resampling pull requests).

**The gates' queries** — the same pull requests and labels, with each query the
gates would send. Neither gate passes a list of paths: a plan is matched on its
own text and the paths written in it, and a diff on its text and the paths its
file headers name. The PR's title and description stand in for a plan; the
labelling judges also read the description, which may flatter every selector a
little but none more than another. The diff is the whole PR's, where the
implementation gate sees one turn's.

| query | selector | coverage@5 | coverage@10 | precision@10 | p95 latency |
|---|---|---|---|---|---|
| title and description | two-stage, `claude-cli` | 0.50 | 0.41 | 0.49 | 5.3 s |
| title and description | two-stage, `codex-cli` | 0.50 | 0.43 | 0.49 | 11.2 s |
| title and description | scope index | 0.34 | 0.31 | 0.30 | 0.2 s |
| diff | two-stage, `claude-cli` | 0.53 | 0.51 | 0.51 | 6.0 s |
| diff | two-stage, `codex-cli` | 0.57 | 0.56 | 0.55 | 11.0 s |
| diff | scope index | 0.41 | 0.36 | 0.34 | 0.5 s |

Against the scope index on the same query, the rank adds +0.10 [+0.04, +0.17]
to coverage@10 on a description and +0.15 [+0.07, +0.23] on a diff with
`claude-cli`, and +0.13 [+0.05, +0.20] and +0.20 [+0.12, +0.29] with
`codex-cli`. The scope index does worse on both queries than on the title with
the PR's paths (0.42): a description names few paths, and a diff names them all
but buries them in the words of its code.

Two further properties are asserted in `tests/integration_scope_select.rs`
against a perfectly calibrated fake ranker, which measures the plumbing rather
than any model: the rank never loses a true positive the prefilter had, and
pooled precision never falls. Both follow from the design — stage 2 only removes
and promotes, it never adds — and the tests exist so a later change cannot
quietly break them.

## The gates' defaults

The plan and implementation gates send far more rules to their judge than
`rules select` returns, because the judge now reads them in concurrent calls
of at most 10 and its time barely grows with their number.

| | Candidates (`--candidates`) | Documents kept (`--limit`) | Rules judged (`--max-rules`) |
|---|---|---|---|
| `plan-check` | 60 | 40 | 40 |
| `impl-check` | 150 | 60 | 60 |

They were chosen on 25 merged pull requests from one repository, labelled with
the rules that govern each, with each PR's title and description standing in
for the plan and its diff for the turn's diff.

- **For a diff, the candidates matter most.** Stage 1 finds 17% of the
  governing rules in its top 30, 46% in its top 100 and 61% in its top 150.
  Ranking 150 and keeping up to 60 put 46% of the governing rules in front of
  the judge with codex-cli's rank, and 36% with claude-cli's, which keeps
  fewer; the earlier defaults (30, 20, 40) put 15%. On 8 of the pull
  requests, the judge then found about three times as many governing rules
  applying, and flagged under 0.1 rules per diff that were not labelled as
  governing.
- **For a plan, more rules mean more false alarms.** A plan says little about
  what the change will touch, so stage 1 and the rank both find fewer of the
  governing rules, and the judge reads a rule the plan does not mention as one
  it leaves out. Sending stage 1's top 100 found more than twice as many
  governing rules applying on codex-cli, but flagged 1.6 rules per plan that
  were not labelled as governing, against 0.4 at these defaults, which reach
  20% of the governing rules instead of 16%. Some of those may be rules the
  labels missed; until that is checked by hand, the plan gate stays small.
- **The rank's time bounds the candidates.** A rank takes about 4 seconds plus
  0.13 a candidate on codex-cli: at 150 candidates, 25 seconds at the median
  and 26 at the 95th percentile. claude-cli took 7 and 9. The hooks' deadline
  of 35 seconds covers both, and with the judge's 150 seconds it stays inside
  the plugin's 200-second gate timeout. A rank that misses the deadline falls
  back to stage 1's top 60, which still reaches 28% for a diff.
- **The judge's time.** At 60 to 80 rules the judge took 9 to 18 seconds at the
  median and at most 27, over 8 pull requests on both runners, in calls of 20,
  where one call over the same rules took 76 to 101 seconds. Calls of 10 cut it
  again: end to end on the 25 pull requests, `impl-check`'s judge took 18
  seconds at the median and 26 at the 95th percentile on codex-cli, and 9 and
  16 on claude-cli. A chunk that fails, or runs past 75 seconds, is made once
  more.
- **The whole check.** `impl-check` end to end with these defaults took 44
  seconds at the median and 52 at the 95th percentile on codex-cli, and 17 and
  25 on claude-cli, with no call failing. `plan-check` took 23 and 32 on
  codex-cli, and 11 and 24 on claude-cli; one codex-cli check failed open when
  both tries of its only call came back as cut-off JSON.

Not yet measured: a held-out set of pull requests, any
second repository, and whether the plan gate's extra flags are rules the
labels missed.

## Cost

Stage 1 is sub-millisecond against a built index. Stage 2 is a single model
call, and it is the whole cost of the command.

It is bounded two ways, and only one of them binds everywhere.

**Time.** A 90-second wall-clock deadline enforced in `rules::scope::rank`, not
by the runner's own timeout. A runner timeout is an
*inactivity* timer that resets on every streamed event, so a backend that keeps
talking is never cut off by it: a measured `claude-cli` call under the earlier
prompt ran 218 seconds under a 60-second runner timeout. On the 25 pull requests
above, a rank took 4.0 seconds at the median and 4.3 at the 95th percentile on
`claude-cli`, and 8.6 and 9.6 on `codex-cli`. Exceeding the deadline degrades to
the prefilter like any other failure. In the gates' hooks the deadline is 15
seconds, which the 95th percentile on both queries above stays inside on both
backends.

**Money, on one backend.** A rank inherits `max_budget_usd` from the config, so
a selection cannot outspend what tailoring is allowed. Only the Claude CLI
enforces it: the other four backends accept the value and drop it, because their
APIs offer no per-request cap. Treat it as a real bound on one runner rather
than as a spend control, and use the deadline as the bound that applies to all
five.

## Honest limits

- **The plan is trusted input.** It is interpolated into the prompt as prose,
  fenced in a delimiter derived from its own bytes so an imitated section header
  cannot close the block. That raises the cost of a blind injection; it does not
  end it, and a plan can still bias verdicts. What it cannot do is add a rule,
  since a score for a number outside the list is ignored — so the worst case is
  mis-ranking documents that genuinely govern the repository. Anyone feeding
  `rules select` a plan from an issue body, a webhook or a pull-request
  description is crossing that boundary and needs a defence at their own layer.
- **A rank need not be a full judgement.** A model that returns one score and
  omits the rest still produces a capped answer: the unjudged candidates are
  kept at their prefilter rank, below everything the ranker affirmed. The status
  line says `partly ranked` and gives the count when that happens, and each
  padded rule keeps `stage: prefilter` with no verdict in the JSON.
- **Stage 2's quality is the model's quality.** The tests bound what the
  orchestration can do to a good answer; they say nothing about whether a given
  model gives one. The verdict printed beside each rule is what makes a bad one
  visible; the reason line under it is stage 1's evidence, not the model's.
- **The score cutoffs were set on two small sets.** 7 for `governs` and 3 for
  `related` were chosen on 25 merged pull requests from one repository and on
  the fixture golden set, with Sonnet and a GPT model. The `related` cutoff is
  the sensitive one: at 5, fixture recall fell from 0.91 to 0.60, because
  models score many rules that do apply at 2 to 4. A model whose scores cluster
  differently — Haiku and the smallest GPT models scored almost everything 0 or
  9 — fits them poorly.
- **Recall is stage 1's ceiling.** Stage 2 can only re-rank what the prefilter
  retrieved. A rule the lexical index never surfaced cannot be promoted, however
  obviously it applies. Raising `--candidates` widens the window at the cost of
  prompt size; it does not remove the ceiling.
- **A very small rule set ranks poorly, by construction.** Inverse document
  frequency measures how a term separates one document from the others, so it
  has less and less to say as the corpus shrinks. At one document it has nothing
  to say at all — every term is on every document — and the index falls back to
  weighting each known term equally, which answers "does this rule match" rather
  than "which rule matches best". Between two and roughly a dozen near-duplicate
  documents there is no fallback and none is wanted: shared wording genuinely
  does not separate them, and only the terms that differ carry rank. A freshly
  synced repository is therefore ranked more crudely than a mature one, and the
  numbers below were measured on corpora of 42 and 425 files.
- **The `related` bucket is a judgement call.** A rule that constrains a change
  indirectly can land in either bucket, and where it lands decides whether it
  survives the cap. The verdict is printed for exactly this reason.
- **One call, no retry.** A rank that fails degrades rather than retrying,
  because a second call spends the latency budget the trigger exists to protect.
- **Stage-2 latency is a model call's.** A rank now typically answers in 4 to
  10 seconds, but it is still a network call to a model, and the deadline is
  what bounds the worst case. A caller that cannot tolerate a 90-second wait
  should pass `--no-rank`. The gates' hooks wait at most 35 seconds.
- **The gates' defaults were chosen on the same 25 pull requests.** See
  [The gates' defaults](#the-gates-defaults) for what they rest on and what is
  not yet measured.
- **Each round of a gate ranks afresh.** A plan the agent revises after a
  denial is ranked again, so every round of the revision loop costs a rank as
  well as a judgement. No scores are reused between rounds.
- **The fixture corpus is synthetic.** It is the CI regression gate, not
  evidence on its own. See the limits section of
  [SCOPE_INDEX.md](SCOPE_INDEX.md).

## Commands

```bash
actual rules select [<PLAN>...] [--repo PATH] [--file PATH]... [--limit N]
                              [--candidates N] [--no-rank] [--runner NAME]
                              [--model NAME] [--explain] [--json]
actual rules eval --golden FILE [--repo PATH] [--limit N] [--ablate SIGNAL]...
                                [--rank] [--candidates N] [--runner NAME]
                                [--model NAME] [--json]
```

One of `PLAN` or `--file` is required. `--file` alone is a valid query — a
path and no plan — and skips stage 2 because it ranks against plan prose.

`rules select --json` lists every stage-1 candidate under `selection.considered`,
in stage-1 order, with its stage-1 `score` and, when stage 2 scored it, its
`rank_score` from 0 to 9 — the unrelated and the capped too — so another limit
or cutoff can be tried on the same answer without asking the model again.

`plan-check` and `impl-check` select the same way before they judge, and take
`--limit`, `--candidates` and `--no-rank` with the same meaning, plus
`--max-rules N` (default 40), the most individual rules one check judges. The
judge reads them in calls of at most 10, sent at once, so its time follows the
largest call rather than the whole list.
`--selection-only` prints, as JSON, what the judge would be handed — the
selection with every candidate's scores, the gathered rules, whether the cap
cut any, and how long the rank took — and stops before the judge. It is for
measuring and tuning selection; a hook never uses it.

`--runner` and `--model` steer stage 2 only, and both fall back to the same
config fields `actual adr-bot` uses.

Which of them widens the search and which narrows it follows one rule: a loose
preference yields a list, a named backend is a pin.

- **`--model`, or `model:` in the config**, is loose. It yields an ordered list
  of the backends that can serve that model, tried until one works.
- **`--runner`, or `runner:` in the config**, names a backend. Either one is the
  whole list. Asking for a backend that is not installed says so rather than
  quietly using another one — which would also mean stage 2 ran on a model you
  did not choose.
- **Neither set** falls back to the default order `adr-bot` uses: claude-cli,
  then anthropic-api.

`runner:` being a pin is deliberate, and it matches `adr-bot`, which uses the
configured runner as-is and auto-detects only when neither the flag nor the
config field is set. One config key has to mean one thing in both commands. The
cost is that a pinned backend which is missing yields no stage 2 even when
another is available, so the message says as much and names the config key:

```
Stage 2: unavailable — no runner available: claude-cli: binary not found …
`runner: claude-cli` in the config pins stage 2 to that backend, so no other
was tried — pass --runner to override it for this run, or unset it to let the
model choose.
```
