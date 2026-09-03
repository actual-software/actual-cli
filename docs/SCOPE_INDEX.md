# Local scope resolution

This is the retrieval half of `actual rules select`: an offline, model-free
ranking of every committed rule document against a plan. It records how the
index decides, what it was measured against, and where it is weak.

It is also stage 1 of the two-stage selector. When it retrieves more candidates
than the caller may keep, a runner-backed rank judges the surplus; see
[RULE_SELECTION.md](RULE_SELECTION.md). Everything below is the stage that needs
no model and no network, and that stays the whole answer whenever a runner is
absent.

## The problem

At plan time there is no diff, so file-path matching cannot be the primary
selector. What a repository does have is its committed rule set — and until now
nothing indexed it usefully.

- **The filename topic segment is dead weight.** In the reference corpus all 425
  files under `.actual/rules/` are prefixed `cross-cutting-`, so the segment
  meant to carry topic carries nothing.
- **The status quo is an honour-system scan.** `CLAUDE.md` instructs the agent to
  eyeball filenames, match aspect slugs by judgement, and read at most five.
  Nothing verifies the selection or records why a file was chosen.
- **Real applicability is present but unindexed**, in two places the filename
  scan never opens: the prose scope sentence under the title, and the `### Verify`
  block, whose grep and test operands name concrete paths.

## What is indexed

Five signals per document, each weighted separately so `--explain` can attribute
a rank to one of them.

| Signal | Source | Default weight |
|---|---|---|
| `path` | path globs from `### Verify` operands | 3.0 |
| `scope` | the prose applicability sentence | 2.0 |
| `path-terms` | words inside those path operands | 1.0 |
| `title` | the `#` heading | 1.0 |
| `slug` | the aspect segment of the filename | 0.5 |

Weights are set by principle — evidence the rule author had to make executable
outranks prose, which outranks the filename — and are **not** fitted to the
golden set. `--ablate <signal>` re-scores with one switched off.

`title` is a deliberate addition beyond the three signals the ticket named. It
is free, it states the subject, and it measurably helps; it is also the signal
most entangled with the reference corpus's ground truth, so a test asserts the
index still beats the filename scan with `title` switched off.

Two properties do the real work.

**Inverse document frequency retires the dead prefix by itself.** `cross-cutting`
appears in 425 of 425 documents, so `ln(425/425) = 0` and it contributes nothing
to any score — with no stopword entry, no threshold and no per-corpus tuning. A
segment stops counting exactly when it stops discriminating. `rules index` prints
which terms fell out this way.

**Term frequency separates a domain term from an ordinary word.** IDF alone
cannot: in a corpus of this size `value` is exactly as rare as `jwks` if each
occurs once. Each matched term contributes `idf(t) × tf/(tf + 1.5)`, so how often
a document says a word counts alongside how unusual the word is. The factor
saturates, so a document cannot buy rank by repetition.

**Path containment is directional both ways.** A plan naming
`infra/terraform/lambda/reports.tf` matches a rule globbing
`infra/terraform/lambda/**`, and a plan naming `infra/terraform/` matches it too.
Both are scored by *depth* rather than by the fact of matching, because a `**`
glob matches everything beneath it — only depth separates the rule governing the
exact directory from the one governing the whole tree. Partial agreement is
discounted a further 25%.

## Measurements

Both selectors are scored on the same cases at the same cap; giving the index a
larger budget would prove nothing. Micro pools hits across cases, so a case
expecting many rules counts for more; macro averages the per-case scores, so
every plan counts the same.

Reproduce with:

```bash
cargo run -- rules eval --golden tests/fixtures/scope_corpus/golden.json --repo tests/fixtures/scope_corpus --limit 5
```

**Fixture corpus** — 38 rule files, 12 topic clusters, 16 plans, cap 5:

| selector | micro P | micro R | micro F1 |
|---|---|---|---|
| scope index | 0.68 | 0.87 | **0.76** |
| filename scan | 0.63 | 0.63 | 0.63 |

**Reference corpus** — the private 425-file rule set, 10 plans:

| cap | index F1 | scan F1 | index P | scan P |
|---|---|---|---|---|
| 5 | **0.25** | 0.20 | 0.80 | 0.64 |
| 10 | **0.41** | 0.26 | 0.77 | 0.49 |
| 20 | **0.56** | 0.31 | 0.67 | 0.39 |

Recall is low at cap 5 on the reference corpus because its expected sets run to
73 files; both selectors face that ceiling equally.

Precision at the status-quo cap is the number to read: **0.80 against 0.64**. Of
five rule files an agent is allowed to open, the index wastes one and the
filename scan wastes nearly two.

## Honest limits

- **The fixture corpus is synthetic**, written to reproduce the shapes of the
  reference corpus. It is the CI regression gate, not evidence on its own. The
  reference-corpus numbers above are the independent check, and they were
  produced from a golden set whose ground truth is defined by ADR title.
- **That ground truth is partly circular for the `title` signal**, which is why
  the no-title ablation is asserted separately.
- **The prose scope sentence underperforms its weight on the reference corpus.**
  Ablating it *improves* F1 there by 0.005, because near-duplicate ADRs share
  near-identical scope sentences and it cannot separate them. It helps on the
  fixture corpus. The weight was left alone rather than fitted to ten cases.
- **The `path` signal only fires when the caller names paths.** At plan time
  most callers do not, so it contributes nothing to most of these numbers. It is
  what makes the index sharp once a plan does name a file.
- **Retrieval is lexical.** A plan that paraphrases its whole domain without
  naming it will not match, and no amount of weighting fixes that.

## Cost

Building an index over 425 files takes about 130 ms; a query against a built
index is sub-millisecond. The index is cached under the user's config directory —
never inside the repository, where a derived artifact beside committed source
would get committed with it — and keyed by a stat-only fingerprint of every rule
file's name, size and modification time. Any edit, add, remove or rename
invalidates it. Every cache operation is best-effort: an unreadable or
unwritable cache degrades to a rebuild. `actual rules index --clear` removes
every cached index — including those left by other repositories — and rebuilds
this one.

## Commands

```bash
actual rules index [PATH] [--rebuild] [--clear] [--json]
actual rules select <PLAN>... [--repo PATH] [--file PATH]... [--limit N] [--explain] [--json]
actual rules eval --golden FILE [--repo PATH] [--limit N] [--ablate SIGNAL]... [--rebuild] [--json]
```

`--explain` prints, per hit, which signal carried it and on which terms, the
globs that matched a named path, the terms the corpus made worthless, and what
the filename scan would have chosen instead at the same `--limit` — so a wrong
selection is diagnosable rather than mysterious. `--no-rank` holds
`rules select` to this stage alone; the stage-2 flags are documented in
[RULE_SELECTION.md](RULE_SELECTION.md).
