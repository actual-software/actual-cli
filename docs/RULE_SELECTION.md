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
question — which of these candidates govern this change, and why — and it is
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

## What the model is allowed to do

Three constraints, each enforced in code rather than requested in the prompt.

**It may only judge, never invent.** The prompt carries a fixed candidate list.
A verdict naming anything outside it is dropped. A hallucinated rule file is
impossible by construction, not merely unlikely.

**It may not reorder within a verdict.** Each candidate gets one of `governs`,
`related` or `unrelated`. `unrelated` is dropped; the rest keep stage 1's order
inside their verdict. A language model is far better calibrated on "does this
rule govern this change" than on "is this rule the third or the fourth most
relevant", so the design takes only the judgement it is good at.

**It may not empty the result.** A rank that keeps nothing is treated as a failed
rank, and the caller falls back to stage 1. Returning nothing is never more
useful than returning the deterministic prefilter.

## Degrading

Every path produces an answer. No runner configured, the runner down, the runner
returning the wrong shape, the runner naming only invented rules, the runner
rejecting everything: each degrades to the stage-1 answer and records why in the
`Stage 2` line. None of them is an error, and none of them costs the caller a
reason — the deterministic reason built from the index's own attribution stands
in wherever the ranker has not supplied one.

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
with `claude-sonnet-4-6`:

| selector | micro P | micro R | micro F1 |
|---|---|---|---|
| two-stage | 0.86 | 0.88 | **0.87** |
| scope index | 0.62 | 0.89 | 0.73 |
| filename scan | 0.58 | 0.63 | 0.60 |

Precision is where the rank pays, and it pays where the design says it should:
0.86 against the index's 0.62, with recall level at 0.88 against 0.89.

Two further properties are asserted in `tests/integration_scope_select.rs`
against a perfectly calibrated fake ranker, which measures the plumbing rather
than any model: the rank never loses a true positive the prefilter had, and
pooled precision never falls. Both follow from the design — stage 2 only removes
and promotes, it never adds — and the tests exist so a later change cannot
quietly break them.

### The full-corpus arm

`--full-corpus` scores the alternative the two stages are measured against: a
model shown every indexed document, with no prefilter, asked which govern the
change. It answers whether selection needs a lexical stage at all. It is a
measurement, not a shipped path, and it costs one runner call per case, each
carrying the whole rule set.

It describes the change in stage 2's words and asks for the same verdicts, so
the two arms differ only where the comparison needs them to:

- **Every document is listed**, so stage 1's recall is not its ceiling.
- **The model names only the rules that apply.** A rule it leaves out is not
  selected. Stage 2 pads a partial answer with unjudged candidates; padding
  here would score stage 1's answer under this arm's name.
- **Nothing is an answer.** Stage 2 treats an empty rank as a failure and falls
  back to the prefilter. Here an empty answer stands, and the false-injection
  rate (see [SCOPE_INDEX.md](SCOPE_INDEX.md#measurements)) is what it is
  measured on.
- **A path-only case is still judged**, because the hook this would serve
  usually knows only the file.
- **A failed call scores empty and is counted.** The summary line ends
  `failed N`, and the case's JSON carries a `failure` reason, so a run that
  mostly failed cannot pass for one that mostly abstained. A two-stage case
  that degraded to the prefilter is marked the same way.

Within a verdict, stage 1's order still breaks ties, which decides only what is
cut when more rules apply than the cap allows.

The same fake ranker bounds this arm's plumbing in the same test file: it finds
every expected rule the cap has room for, and it abstains on every plan in
`golden_ungoverned.json`, where the two-stage selector still injects the
prefilter's answer.

## Cost

Stage 1 is sub-millisecond against a built index. Stage 2 is a single model
call, and it is the whole cost of the command.

It is bounded two ways, and only one of them binds everywhere.

**Time.** A 90-second wall-clock deadline enforced in `rules::scope::rank`, not
by the runner's own timeout. A runner timeout is an
*inactivity* timer that resets on every streamed event, so a backend that keeps
talking is never cut off by it: a measured `claude-cli` call ran 218 seconds
under a 60-second runner timeout. Two runs of the same plan took 6 and 218
seconds, which is the real spread. Exceeding the deadline degrades to the
prefilter like any other failure.

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
  since a verdict naming a non-candidate is dropped — so the worst case is
  mis-ranking documents that genuinely govern the repository. Anyone feeding
  `rules select` a plan from an issue body, a webhook or a pull-request
  description is crossing that boundary and needs a defence at their own layer.
- **A rank need not be a full judgement.** A model that returns one verdict and
  omits the rest still produces a capped answer: the unjudged candidates are
  kept at their prefilter rank, below everything the ranker affirmed. The status
  line says `partly ranked` and gives the count when that happens, and each
  padded rule keeps `stage: prefilter` with no verdict in the JSON.
- **Stage 2's quality is the model's quality.** The tests bound what the
  orchestration can do to a good answer; they say nothing about whether a given
  model gives one. The reason line is what makes a bad verdict visible.
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
- **Stage-2 latency is not predictable.** The measured spread on one plan was 6
  to 218 seconds. The deadline bounds the worst case; it does not make the
  typical case fast, and a caller that cannot tolerate a 90-second wait should
  pass `--no-rank`.
- **The fixture corpus is synthetic.** It is the CI regression gate, not
  evidence on its own. See the limits section of
  [SCOPE_INDEX.md](SCOPE_INDEX.md).

## Commands

```bash
actual rules select [<PLAN>...] [--repo PATH] [--file PATH]... [--limit N]
                              [--candidates N] [--no-rank] [--runner NAME]
                              [--model NAME] [--explain] [--json]
actual rules eval --golden FILE [--repo PATH] [--limit N] [--ablate SIGNAL]...
                                [--rank] [--full-corpus] [--candidates N]
                                [--runner NAME] [--model NAME] [--json]
```

One of `PLAN` or `--file` is required. `--file` alone is a valid query — a
path and no plan — and skips stage 2 because it ranks against plan prose.

`--runner` and `--model` steer stage 2, and the full-corpus arm in `rules eval`,
and both fall back to the same config fields `actual adr-bot` uses.

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
