# Selecting rules for a plan

`actual rules select` answers which committed rule documents govern a proposed
change, and says why for each one. It runs in two stages. This document records
what each stage is for, when the second one is paid for, what happens when it
cannot run, and where the design is weak.

The retrieval stage is described in [SCOPE_INDEX.md](SCOPE_INDEX.md); this
document covers the selection built on top of it.

## Two stages

**Stage 1 is the deterministic prefilter.** The scope index ranks every rule
document against the plan using path globs from `### Verify` operands, the prose
scope sentence, the aspect slug and the title. No model, no network,
sub-millisecond against a built index. It is the whole answer whenever it can be.

**Stage 2 is a runner-backed rank over what stage 1 retrieved.** It is asked one
question — which of these candidates govern this change, and why — and it is
asked only when stage 1 hands back more candidates than the caller may keep.

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
Stage 2: ranked 10 candidate(s): 1 governs, 3 related, 6 unrelated, 0 unjudged
```

`--no-rank` asks for stage 1 alone: offline, and exactly reproducible.

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

**Fixture corpus** — 38 rule files, 16 plans, cap 5, stage 2 on `claude-cli`
with `claude-sonnet-4-6`:

| selector | micro P | micro R | micro F1 |
|---|---|---|---|
| two-stage | 0.86 | 0.87 | **0.86** |
| scope index | 0.68 | 0.87 | 0.76 |
| filename scan | 0.63 | 0.63 | 0.63 |

Precision is where the rank pays, and it pays where the design says it should:
0.86 against the index's 0.68, with recall unchanged at 0.87. Stage 2 cannot
retrieve a rule stage 1 missed, so it does not move recall; what it does is stop
returning the ones stage 1 got wrong.

All sixteen cases leave more candidates than the cap — between 6 and 19 — so
stage 2 ran on every one, and the trigger does not separate these numbers from
what `rules select` returns.

Two of those sixteen cases degraded to the prefilter mid-run — one runner
timeout, one runner error — and were scored as prefilter results. The 0.86 is
therefore a floor rather than a ceiling, and it is also a live demonstration of
the fallback: a partial measurement across sixteen plans is worth more than no
measurement because the ninth timed out.

Two further properties are asserted in `tests/integration_scope_select.rs`
against a perfectly calibrated fake ranker, which measures the plumbing rather
than any model: the rank never loses a true positive the prefilter had, and
pooled precision never falls. Both follow from the design — stage 2 only removes
and promotes, it never adds — and the tests exist so a later change cannot
quietly break them.

## Cost

Stage 1 is sub-millisecond against a built index. Stage 2 is a single model
call, and it is the whole cost of the command.

That call is bounded by a 90-second wall-clock deadline enforced in
`rules::scope::rank`, not by the runner's own timeout. A runner timeout is an
*inactivity* timer that resets on every streamed event, so a backend that keeps
talking is never cut off by it: a measured `claude-cli` call ran 218 seconds
under a 60-second runner timeout. Two runs of the same plan took 6 and 218
seconds, which is the real spread. Exceeding the deadline degrades to the
prefilter like any other failure.

## Honest limits

- **Stage 2's quality is the model's quality.** The tests bound what the
  orchestration can do to a good answer; they say nothing about whether a given
  model gives one. The reason line is what makes a bad verdict visible.
- **Recall is stage 1's ceiling.** Stage 2 can only re-rank what the prefilter
  retrieved. A rule the lexical index never surfaced cannot be promoted, however
  obviously it applies. Raising `--candidates` widens the window at the cost of
  prompt size; it does not remove the ceiling.
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
actual rules select <PLAN>... [--repo PATH] [--file PATH]... [--limit N]
                              [--candidates N] [--no-rank] [--runner NAME]
                              [--model NAME] [--explain] [--json]
actual rules eval --golden FILE [--repo PATH] [--limit N] [--ablate SIGNAL]...
                                [--rank] [--candidates N] [--runner NAME]
                                [--model NAME] [--json]
```

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
