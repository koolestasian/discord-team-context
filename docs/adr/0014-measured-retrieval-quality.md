# 0014. Measured retrieval quality

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D13, §9, §9.1, §11, §14, §18 (M2)

## Context

Search and `/ask` retrieval decide whether the product is useful. Reddit practitioners
disagree on whether adding embeddings to keyword search helps: some report big
accuracy gains, others "barely improved" (plan §9.1; a lead to verify, not a
measured fact). Agentic search, where an agent runs several keyword searches, is
reported to be replacing single-shot vector retrieval. Embeddings add a dependency
and binary size; ranking tweaks without measurement can make results worse
unnoticed.

## Decision

Measure retrieval instead of guessing.

- `fixtures/demo/eval.toml` holds at least 40 questions about the fictional demo team.
  Each lists the message and entry ids that answer it. The set includes paraphrases
  (for example "what DB did we pick?" when the messages say "SQLite"), reversed
  decisions, names and dates.
- `cargo xtask eval` reports **recall@10** and **MRR** for `search`, `context_for` and
  the `/ask` retrieval step.
- A stored baseline is recorded in M2, and CI fails if the metrics drop below it.
- Any change to search or ranking, and the query-expansion step of `/ask` (§11), is
  accepted only if the eval supports it.
- Embeddings (`sqlite-vec`) and ranking changes are accepted only if they raise the
  eval meaningfully without hurting latency or binary size.

Embeddings are out of v0.1 (plan non-goals) and are a v0.3 option (§18).

## Alternatives considered

- **Add embeddings from the start.** Handles paraphrases well in principle, but adds a
  model or API dependency and index size, and reports on its value conflict.
- **Judge quality by hand.** Cheap, but not repeatable, and regressions go unnoticed.
- **LLM-as-judge scoring.** Can score free-form answers, but it needs a model in CI
  and makes the gate non-deterministic; the plan keeps real APIs out of CI (§14).
- **A larger external benchmark.** Broader, but it would not reflect chat with
  decisions, reversals and names.

## Consequences

### Good

- Search changes are justified with numbers, and regressions fail CI.
- The demo dataset doubles as the eval corpus, so anyone can reproduce results.

### Bad

- The eval measures a small fictional corpus; scores may not match a real team's chat.
- Writing and maintaining 40 or more good questions is real work.
- "Meaningfully" is not defined as a number in the plan; it must be agreed when the
  first baseline exists.

### Follow-ups

- M2: write the eval set, `xtask eval` and the baseline.
- Compare query expansion and any later bounded search loop on the same eval.
