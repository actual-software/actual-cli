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
- **Discovery is flat.** Only `.md` files directly under `.actual/rules/` are
  indexed; a subdirectory is skipped without comment. The directory is
  generated and written one level deep, so this avoids governing a repository
  by a vendored copy or an editor backup that happened to land there. A
  hand-organised `.actual/rules/auth/` is ignored.
- **Retrieval is lexical.** A plan that paraphrases its whole domain without
  naming it will not match, and no amount of weighting fixes that.
- **Inverse document frequency needs documents to compare.** It scores a term by
  how well it separates one document from the rest, so it says less as the
  corpus shrinks and nothing at all at a single document, where every term is on
  every document and `ln(1/1) = 0`. Below two documents the index weights each
  known term equally instead — the same fallback would be wrong at scale, since
  it is exactly the zero on a ubiquitous term that retires the dead
  `cross-cutting` prefix. Both numbers above are from corpora far past that
  boundary.

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
actual rules select [<PLAN>...] [--repo PATH] [--file PATH]... [--limit N] [--min-score S] [--by-adr] [--explain] [--json]
actual rules eval --golden FILE [--repo PATH] [--limit N] [--min-score S] [--ablate SIGNAL]... [--rebuild] [--json]
```

One of `PLAN` or `--file` is required. `--file` alone is a valid query — a
path and no plan, the hook case — and is the whole answer from this stage;
stage 2 is skipped then because it ranks against plan prose. `--explain`
prints, per hit, which signal carried it and on which terms, the globs that
matched a named path, the terms the corpus made worthless, and what the
filename scan would have chosen instead at the same `--limit` — so a wrong
selection is diagnosable rather than mysterious. `--no-rank` holds
`rules select` to this stage alone even when a plan is present; the stage-2
flags are documented in [RULE_SELECTION.md](RULE_SELECTION.md).

`--min-score` is the floor a document must reach to be returned at all.
Without one a selection returns its cap whenever anything scored above zero,
so a file no rule governs still gets the least bad few — for a hook, that is a
brief about rules that do not apply. The floor is what lets a selection answer
"nothing". It rides on the query, so every path applies it: the command, the
prefilter feeding stage 2, the grouped search and `rules eval`. It defaults to
the `rules_min_score` config key, and the panel prints it whenever one is in
force, so an empty answer is attributable to the floor rather than looking
like an index that found nothing.

Persist a corpus-specific floor with the validated config interface:

```bash
actual config set rules_min_score 1.5
```

Scores are sums of weighted 0..1 coverages, so a useful floor is
corpus-dependent and belongs in config rather than in a constant. Measured on
the 425-document reference corpus over the edited files of 35 merged pull
requests, at cap 5 by document:

| floor | precision | recall | files with no governing rule that still get a brief |
|---|---|---|---|
| none | 31% | 30% | 19 of 33 |
| 1.5 | 40% | 30% | 13 of 33 |
| 2.0 | 34% | 23% | 10 of 33 |
| 2.5 | 45% | 12% | 2 of 33 |

A floor of 1.5 buys the abstention for nothing: recall is unchanged. Past it,
recall is what is being spent. Combined with `--by-adr` at two decisions, the
same corpus gives 53% precision at 56% recall.

`--by-adr` changes the unit: it groups the result by the decision each
document belongs to, ranks decisions at their best document's score, and
counts `--limit` in decisions rather than documents. A generated rule set
splits one decision across many near-identical documents that share their
verify paths, so a document-level cap can spend itself on aspects of a single
subject while the next relevant decision never appears. The decision name is
the title prefix before the last colon, derived at index time so colons within
the decision title remain part of its identity; a document
whose title has no colon names no decision and forms its own group rather
than pooling with other unnamed ones. Decisions appear where their best
document ranked, and documents retain their relative rank within each
decision. Making each group contiguous can change the flattened document
order when decisions were interleaved. Grouping is stage 1 only, since what
is being capped is decisions while the rank judges documents. Measured on the
425-document reference corpus, over the edited files of 35 merged pull
requests: the top two decisions reach 38% precision at 56% recall, against
31%/30% for the top five documents.

Because grouped selection is stage 1 only, `--runner`, `--model`, and
`--candidates` conflict with `--by-adr` instead of being silently ignored.
`--no-rank` remains valid and explicit. `--explain` shows each grouped
document's signal attribution and path evidence, then compares it with the
filename scan regrouped and capped in decisions so both sides use the same
unit.
