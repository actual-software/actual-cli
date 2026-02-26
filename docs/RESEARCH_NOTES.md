# Research Notes Template: ADR-Worthy Decisions for {Lang} / {Framework}

> This file is a **structural template** — it shows the format and section style
> for research notes. Replace `{Framework}`, `{Lang}`, and placeholder text with
> real findings for the framework under investigation.
>
> Target: **10–15 ADR candidates total**. Cover 6–9 of the categories below. Skip
> categories that don't apply to this framework. Do not add categories.

---

## Sources

List the authoritative sources consulted. Each ADR candidate must be traceable to at least one entry here.

| Source | Version | Content |
|--------|---------|---------|
| {Framework} official docs: {Topic} | {vN.x} | {What it covers} |
| {Framework} official docs: {Topic} | {vN.x} | {What it covers} |
| {Lang} community style guide or RFC | {date/version} | {What it covers} |

---

## Category 1: Project Configuration & Build

> Non-obvious setup choices: config keys, required opt-ins, file conventions, defaults
> that changed between major versions.

### ADR Candidate: {Prescriptive verb phrase — "Use X for Y" or "Prefer X over Y"}

**Source**: {Docs page or RFC URL}

**Decision**: {What to do (or not do), 1–2 sentences. Mention the specific config key, API, or flag.}

**Why it's non-obvious**: {One sentence: what wrong default a developer would use and why.}

**Key details**:
- Applies to: {Framework} {vN}+
- {Specific config key or API name}
- {Any version-change note: "Changed from X to Y in vN"}

---

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {What to do, 1–2 sentences.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- Applies to: {Framework} {vN}+
- {Specific detail}

---

## Category 2: Project Structure & Organisation

> File/folder conventions, co-location rules, module boundaries, naming that
> the framework enforces or strongly recommends.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {What to do, 1–2 sentences.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Detail}
- {Detail}

---

## Category 3: Data Access / State Management

> How to fetch, cache, mutate, and share data within the framework's model.
> Common mistakes with ORM integration, caching layers, or state primitives.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {What to do, 1–2 sentences. Reference the specific API or hook.}

**Why it's non-obvious**: {One sentence: what the wrong default is.}

**Key details**:
- {Specific method or config key}
- {Version note if behaviour changed}

---

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {1–2 sentences.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Detail}

---

## Category 4: Request / Response Handling

> Routing conventions, HTTP method handling, middleware placement, headers,
> cookies. Framework-specific patterns that differ from general {Lang} idioms.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {1–2 sentences.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Specific API or config}
- {Applies to: {Framework} vN+}

---

## Category 5: Error Handling

> Expected vs unexpected errors. Where to catch, what to return, what to throw.
> Decisions that affect user experience and observability.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {1–2 sentences. Be specific: "return an error object" vs "throw".}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Specific pattern or hook}

---

## Category 6: Authentication & Security

> Auth integration patterns, secret handling, input validation, CSRF, rate
> limiting. What the framework provides vs what must be layered on top.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page or security advisory}

**Decision**: {1–2 sentences.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Specific API, middleware, or config key}

---

## Category 7: Environment & Secrets

> How to separate build-time from runtime config, which values are safe to
> expose to the client, and how to validate required variables on startup.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {1–2 sentences. Name the specific mechanism: prefix, env file, runtime API.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Specific prefix, convention, or API}

---

## Category 8: Testing Strategy

> What the framework officially supports for unit, integration, and e2e tests.
> Non-obvious test setup, mocking patterns, or test-isolation pitfalls.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page or community guide}

**Decision**: {1–2 sentences.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Specific test API or config}

---

## Category 9: Performance & Caching

> Opt-in vs opt-out caching, revalidation strategies, bundle size, lazy loading.
> Decisions that are easy to get wrong and only surface under load.

### ADR Candidate: {Prescriptive verb phrase}

**Source**: {Docs page}

**Decision**: {1–2 sentences. Name the specific cache layer or option.}

**Why it's non-obvious**: {One sentence.}

**Key details**:
- {Specific config or API}

---

## Candidate Summary

Total candidates: **N** (aim for 10–15)

### By priority

**Must-have** (prevent common setup mistakes or PR-level bugs):
1. {Title}
2. {Title}
3. ...

**Important** (improve architecture or avoid performance problems):
4. {Title}
5. ...

**Nice-to-have** (good practice, lower impact):
6. {Title}
...
