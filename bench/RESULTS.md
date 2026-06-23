# Benchmark results

Human-readable view of benchmark findings. The **source of truth is the project knowledge graph**
(typed `ArchitecturalDecision` / `Rationale` / `Lesson` records, linked below by IRI); this file
mirrors them for quick reading (CLAUDE.md invariant #2). Numbers here are reproducible from the
immutable run transcripts via `python regrade.py` — no agent re-runs required.

---

## H4 — Currency / anti-staleness (the structural kill-shot)

**Question.** When a recorded decision has been *superseded* (the project reversed an earlier
choice), does memory serve the **current** answer or the **stale** one? This is where structured
memory should have a mechanism free text lacks: supersession chains + current-only retrieval
(`get_relevant_context`) vs BM25 over a currency-blind text export.

**Design.** Four *reversal pairs* on the `moosedev-temporal` corpus — each a real
superseded→accepted decision in MOOSEDev's own history. Arms differ only in memory: **B0** cold,
**B1-md** markdown+grep, **B1-rag** BM25 over the content-parity export (currency-blind),
**B2** the live MOOSEDev graph. Two delivery regimes: **oracle** (top-k context pushed into the
prompt) and **tooluse** (agent decides whether/how to query). Models: `gpt-5.3-codex-spark`,
`gpt-5.4-mini`, `gpt-5.5`, `qwen3.6-35b-a3b`.

### Grading (corrected)

Currency is measured by **coverage of the CURRENT answer's markers** (`must_include_any`) — this is
robust and is what `score` already uses (`0.7·coverage + 0.3·cited`). The earlier `stale` keyword
diagnostic was **unreliable** and is now a non-authoritative hint only: a *correct* current answer
naturally names the old state to deny it ("it is **not** a *temporary* stub anymore", "you do
**not** need to start `--serve` *yourself*"), so naive substring matching false-flagged it. The
matcher is now negation- and markdown-aware (`grade.py`), but keyword staleness still cannot
distinguish a current answer that *narrates* history ("the old stub was intentionally temporary,
now replaced") — hence coverage, not the marker list, is the verdict. See Lesson `9fc21dc0`.

### Result — currency rate (answer asserts the current state), all models pooled

| task (reversal pair) | B0 | B1-md | B1-rag | **B2** |
|---|---|---|---|---|
| `glrc_currency` — get_relevant_context: "pure-symbolic" → BM25-backed | 1/2 | 2/2 | **11/24 (46%)** | **25/25 (100%)** |
| `connect_daemon_currency` — `--connect`: manual `--serve` → auto-spawn | — | — | 5/5 (100%) | 5/5 (100%) |
| `ontology_currency` — architecture.ttl: temp stub → generated production | — | — | 5/5 (100%) | 5/5 (100%) |
| `supersede_scope_currency` — supersede: ArchDecision-only → InformationRecord | — | — | 5/5 (100%) | 5/5 (100%) |

**B2 is 100% current in every cell** (25/25, all 4 models, both regimes). Free-text is current on
3 of 4 pairs but collapses on `glrc`.

### Why `glrc` collapses — and only in the push regime

`glrc` is the rank-inverted pair: the **superseded** "pure-symbolic" record **outranks the current
record in BM25**. Splitting that task by delivery regime makes the mechanism explicit:

| regime | B1-rag (free-text) | **B2 (structured)** |
|---|---|---|
| **oracle** (top-k *pushed* into prompt) | **1/13 (8%)** — catastrophic | **13/13 (100%)** |
| **tooluse** (agent runs its own search) | 10/11 (91%) — recovers | 12/12 (100%) |

In **oracle/push**, free text hands the agent the top BM25 chunk — which is the *stale* one — and
the agent has no currency signal to override it. In **tooluse**, the agent issues its own queries
and can dig past the stale top hit. Structured memory needs neither escape hatch: current-only
retrieval is a **guarantee**, not a rank- or regime-dependent gamble.

### Conclusion

Structured memory **guarantees** currency (B2 100% on every pair, model, and regime). Free-text
currency is a **content/rank/regime-dependent gamble**: fine when the current record ranks well and
self-announces (3 pairs), **catastrophic** when the superseded record outranks it *and* context is
pushed rather than pulled (`glrc`, oracle). This is the un-confounded re-run that overturned the
earlier "structured representation never wins" finding.

- Graph (source of truth): Rationale `51338ce8` (clean currency A/B), Rationale `abe11031`
  (4-pair generalization), Lesson `9fc21dc0` (currency-grading pitfall), AD `47f3f038`
  (benchmark design). Scope: Lesson `8a06ad1d` (the bench measures the retrieval slice only).
- Artifacts: `runs/runs.jsonl` (raw, append-only) → `runs/runs_regraded.jsonl` (`python regrade.py`);
  per-run transcripts `runs/<run_id>.events.json`; tasks `tasks_public/moosedev-temporal/*_currency.json`.

### Honest scope

This is the **retrieval slice** only: point-in-time currency + delivery. It does *not* measure
MOOSEDev's longitudinal, capability, or trust/governance advantages, which need their own
instruments (Lesson `8a06ad1d`). A null elsewhere must never be read as "structured memory does
not help."

---

## Hybrid retrieval — does the dense channel earn its keep?

**Question.** `get_relevant_context` seeds with a hybrid BM25F⊕dense retriever (AD `b933bd10`). Does
the dense channel improve end-to-end answers over pure BM25F, or just cost tokens? A clean A/B on the
**same binary and graph** — only `MOOSEDEV_DENSE_FLOOR` differs (0.50 hybrid vs 0.99 pure-BM25F).
Corpus: a **private** 416-record temporal graph (GROWL-enriched — Pattern `613cb623`); agent
gpt-5.4-mini via codex. Per-decision content stays private; only methodology + aggregates here.

### Instrument A — seed-recall (retrieval slice, deterministic, $0)

Over 37 currency chains, hybrid is a **head-of-ranking re-ranker, not a recall expander**: within
k=10 it recovers nothing BM25F misses (0 recovered), but it **promotes the current record higher in
19/37 chains** and lifts the hard old-framing **recall@3 by +0.216** (0.622→0.838) and MRR +0.03, at
a small tail-recall cost. The lift sits at the top of the list — where a pushed top-k decides the answer.

### Instrument B — end-to-end (110 cells), graded by coverage (Lesson `9fc21dc0`)

| pooled drift | BM25F | hybrid |
|---|---|---|
| oracle (top-k pushed) | 17/20 | **20/20** (+15pp) |
| tooluse (agent queries) | 18/20 | 19/20 (tie) |

- **Efficiency: hybrid −19% agent tokens** (5.66M→4.58M) — finds the current record faster, less
  thrash. Grading-independent; the solid headline.
- **Accuracy: near-ceiling for both arms.** Hybrid's one decisive edge is the **hardest-to-retrieve
  drifted item** (full coverage 5/5 vs 2/5) — where the current record's vocabulary has diverged far
  from the query, so lexical BM25F cannot reach it. Tooluse is a tie; every cell hybrid ≥ BM25F.

### Why these numbers are trustworthy: a correction caught by a free regrade

The first Instrument B report claimed a "drift/tooluse **+20pp**" win. A regrade over the **immutable
transcripts** (no agent re-run) **overturned it** — that gap was a **grading artifact**, not a
retrieval win. Two bugs in `hybrid_ab_report.py`: (1) `mem%` folded in a `stale`-substring flag that
false-positives on a correct answer *narrating* the old state to deny it (backtick-wrapped negations
defeat the negation window) and on stale markers overlapping the cited decision's own title →
suppressed BM25F (tooluse 10/20→18/20 once fixed); fix: currency = coverage only. (2) an escape proxy
(tokens > threshold ⇒ "read source") fired on `materialize_tree=false` pure-memory tasks with **no
source to escape to**; fix: gate escape on `materialize_tree`. A grading artifact can **manufacture**
a false signal, not only hide a true one — always regrade after a grader change (Lesson `a03dbe13`).

### Honest scope

Single model, single corpus, n=5 drift / 3 currency (Wilson intervals overlap on the tooluse tie).
This corpus is **near-ceiling by coverage** for both arms — a *weak accuracy discriminator*; the
differentiation lives in the 1–2 hardest-retrieval tasks. Testing the dense channel's accuracy
properly needs more hard-drift tasks, not more N on easy ones.

- Graph (source of truth): Lesson `410d6c73` (regraded finding), Lesson `a03dbe13` (grading-artifact
  methodology, extends `9fc21dc0`), Pattern `613cb623` (re-serve modernization), AD `b933bd10`
  (hybrid retrieval). Correction trail: supersede chain `51932e6f→75f622d7→410d6c73`.
- Reproduce (private corpus — artifacts under `BENCH_HOME`, never the open repo): `hybrid_ab_report.py`
  over `runs_{bm25f,hybrid}.jsonl` (regrade-safe, no agent re-run); seed-recall `seed_recall/*.json`.
