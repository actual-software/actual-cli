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

## What the model is allowed to do

Three constraints, each enforced in code rather than requested in the prompt.

**It may only judge, never invent.** The prompt carries a fixed, numbered
candidate list, and the answer is a score per number. A number outside the list
is ignored. A hallucinated rule file is impossible by construction, not merely
unlikely.

**It may not reorder within a verdict.** Each candidate's score is collapsed
into one of three verdicts: 7 to 9 is `governs`, 3 to 6 is `related`, and 0 to 2
is `unrelated`. `unrelated` is dropped; the rest keep stage 1's order inside
their verdict. A language model is far better calibrated on "does this rule
govern this change" than on "is this rule the third or the fourth most
relevant", so the design takes only the judgement it is good at. On a labelled
set of 25 merged pull requests, collapsing scores this way picked governing
rules as well as ordering by the score itself did.

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

Two further properties are asserted in `tests/integration_scope_select.rs`
against a perfectly calibrated fake ranker, which measures the plumbing rather
than any model: the rank never loses a true positive the prefilter had, and
pooled precision never falls. Both follow from the design — stage 2 only removes
and promotes, it never adds — and the tests exist so a later change cannot
quietly break them.

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
the prefilter like any other failure.

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
  should pass `--no-rank`.
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
