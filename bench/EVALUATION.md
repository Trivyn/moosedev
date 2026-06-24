# MOOSEDev — State of the Evidence (consolidated evaluation, 2026-06-23)

A session-long, multi-instrument evaluation of one question: **does structured (typed-graph) project
memory beat good free-text / vector memory for a coding agent?** Every instrument is reuse-first,
pre-registers a strict verdict, grades against primary or source-derived ground truth, and is
reproducible from immutable transcripts. The **source of truth is the project knowledge graph** (typed
Lessons, linked by IRI); this file is the human-readable mirror (CLAUDE.md invariant #2).
Private-corpus note: `trivyn` is private, only aggregate numbers appear; `rust-rfcs` and `codegraph` are public.

---

## Executive summary

> **MOOSEDev's demonstrable value is completeness/structure + currency — NOT retrieval precision.** Against
> a *real* competitor (mem0) on a neutral public corpus, structured memory answers **completeness, negation,
> and traversal** questions that vector/free-text memory **structurally cannot** (B2 1.00 vs mem0 ~0.15), while
> **tying** mem0 on its home turf (relevance recall, currency). It is a **superset**, not a sidegrade. The
> longitudinal "fights comprehension debt over months" thesis, the project's actual purpose, remains
> **unmeasured**.

| dimension | instrument | result | status |
|---|---|---|---|
| Retrieval precision | hybrid A/B, scale-degradation, decomposition | constant offset, no compounding, saturates on clean data | **≈ parity** (3 nulls) |
| **Capability** (completeness / negation / traversal) | Stage 0–2 + **real-competitor test vs mem0** | B2 sweeps; mem0 ~0.15, real-docs-grep ~0.05; reproduced on 2 corpora | **categorical win** |
| Relevance + currency (vs mem0) | balanced competitor matrix | both arms call their tool reliably at ~35–40k tok; equal coverage | **tie** (superset) |
| Currency / anti-staleness | H4 reversal pairs | B2 100% current; free-text collapses on rank-inverted reversals | **proven** (rare-trigger) |
| Longitudinal (comprehension debt / months) | — | the actual thesis | **untested** |

**Verdict (see §9): don't kill it — *earn the thesis.*** The evidence rules out the kill signal ("structured
≈ vector memory but costlier → redundant") and earns the expensive trial.

---

## 1. How to trust this (method)

**The arms ladder**, and how it *evolved* under criticism (each step a recorded correction):
- **B0** cold (no memory) · **B1-md** rationale-as-markdown (agent grep) · **B1-rag** a `recall()` tool · **B2** the MOOSEDev typed graph (`get_relevant_context` / `query` / `sparql`).
- **B1-rag/B1-md were content-parity** — the B2 graph *flattened to text* (AD `47f3f038`). A valid representation-only control, but "the graph wearing a markdown hat" (AD `b3205dcb`): it hands free-text MOOSEDev's capture+curation for free.
- So the headline baseline moved to **ecological / real competitors**: **B1-notes** (agent greps the project's real docs) and, this session, **B1-mem0** (the actual mem0 tool, capturing the raw docs its own way) — Lesson `440abc78`.

**Discipline:** pre-registered verdicts; `$0` probes that rerun from the stores with no agent; **regrade from
immutable transcripts** after any grader change; a **strict LLM-judge** for cross-vocabulary fairness, *validated
by reproducing the structured arm's score*; ground truth from **primary source / SPARQL-derived sets**, never an
arm's own prose. **Corpora:** trivyn (private, ~416 records, 42 supersession chains), rust-rfcs (public, decision-scale),
codegraph (public, neutral, not our doc style — the external-validity workhorse).

---

## 2. Retrieval precision — PARITY (three nulls)

Structured retrieval ≈ good BM25/hybrid. If you only need "find the relevant record," RAG is fine.

- **Hybrid dense⊕BM25F (AD `b933bd10`):** a *head-of-ranking re-ranker*, not a recall expander — recovers nothing BM25F misses within k=10 but promotes the current record (recall@3 +0.216 on hard old-framing) and spends **−19% agent tokens**. Accuracy near-ceiling for both arms. Lesson `410d6c73`.
- **Scale-degradation (rust-rfcs, N=50→634):** B2 beats B1-rag at *every* N (hit@5 0.84 vs 0.60 at N=634) — a real **constant** offset — but the gap does **not** widen: slope β = −0.033 [95% CI −0.124, +0.059], spans 0. **The "edge compounds with scale" thesis is KILLED.** Lesson `7b41be4f`.
- **Decomposition (split blob → typed nodes):** clean corpora are retrieval-*saturated* (blob hit@5 = 1.00), so decomposition has no gap to fill. Lesson `8f06f329`.

**Read:** retrieval precision is not the differentiator, and does not justify the project on its own.

---

## 3. Capability — the categorical win (the core result)

A **possible-vs-impossible** axis, not a shared accuracy axis (Lesson `8a06ad1d`): *enumerate a complete typed
set*, *find an absence*, *traverse a supersession/multi-hop link*. Top-k retrieval cannot do these at any k< all;
a typed graph returns them in one query.

**Ladder of evidence:**
- **Stage 0 — tool ceiling ($0, trivyn):** for answer sets ≥16, negations, and traversals, B1 BM25 recall@5 ≈ 0.00–0.11 (needle in a 416-record haystack); reaching completeness needs ingesting the whole ~63K-token corpus; B2 returns the exact set in one SPARQL. Lesson `392855d0`.
- **Stage 1 — agentic (trivyn, N=3):** pooled hard-class **B2 F1 0.94 / recall ~0.99** vs B1-rag 0.25 vs B0 ~0. Lesson `6a70b02e`.
- **Stage 2 — neutral public corpus (codegraph, N=2):** **reproduces** — pooled **B2 0.89** vs B1-rag 0.34 vs B0 0, on someone else's docs with a richer graph than ours. Lesson `243c6c89`.

**The definitive test — vs a REAL competitor (this session, codegraph).** Replaces the graph-derived baseline with
the *actual* tools, captured **whole-system**: B1-mem0 = mem0 ingesting the raw docs its own way (553 memories,
verified faithful); B1-notes = the agent grepping the real shipped docs. Graded by a **B2-validated LLM judge**
(credits paraphrase, rejects same-topic-not-same-rule):

| class | B2 | B1-mem0 (fair) | B1-notes (fair) |
|---|---|---|---|
| set-completeness | **1.00** | 0.15 | 0.02 |
| negation | **1.00** | 0.17 | 0.01 |
| supersession | **1.00** | 0.15 | 0.15 |

mem0 surfaces ~15% of a large set (its top-k slice) and **never the whole** — and it's **capture-clean** (mem0
*holds* the 553 facts; it just can't enumerate/negate/traverse them). This is the strongest external-validity
result we have: the categorical gap survives a real tool, on a public corpus, fairly graded.

---

## 4. The mem0 comparison — the balanced matrix

Testing only B2's turf would be a strawman, so the matrix also tests **mem0's home turf** (relevance recall +
currency), with mem0 at full multi-signal strength (semantic + BM25 + entity):

| dimension | B2 | mem0 | verdict |
|---|---|---|---|
| completeness / negation / supersession | 1.00 (via `sparql`, ~78–167k tok) | ~0.15 | **B2 categorical** |
| simple relevance | cov 0.82 (`get_relevant_context` 4/4, ~35k tok) | cov 0.67–0.90 (~40k tok) | **tie** |
| currency (real CHANGELOG reversal) | 1.00 | 1.00 | **tie** (cold 0.00) |

**Synthesis — inverse trades.** mem0 = cheap, foolproof, small-context; MOOSEDev = complete, structured,
auditable. They **tie where mem0 is built to win**; MOOSEDev is a **strict superset on the structural axis**,
which top-k cannot reach. *"Different tools for different jobs — one strictly broader on completeness/structure."*

*(Multi-hop omitted from the table: size-1 expected sets make recall-only coverage degenerate; needs a
precision-aware metric.)*

---

## 5. Currency — proven (rare-triggering)

When a decision is later reversed, does memory serve the **current** answer or the **stale** one? On four real
reversal pairs (×4 models, ×2 delivery regimes), **B2 is 100% current (25/25)**. Free-text is current on 3 of 4
pairs but **collapses on the rank-inverted pair** (`glrc`, where the superseded record out-ranks the current one
in BM25): in the push/oracle regime B1-rag serves the stale answer **1/13 current (8%)** vs B2 **13/13**.
Structured current-only retrieval is a **guarantee**; free-text currency is a content/rank/regime-dependent
**gamble**. Real, but rare-triggering — rich capture suppresses how often reversals bite. Rationale `51338ce8`,
`abe11031`; Lesson `9fc21dc0` (grading). Full detail: `bench/RESULTS.md`.

---

## 6. Efficiency

Mixed / parity — **accuracy is the win, not tokens.** Hybrid retrieval saves −19% agent tokens on the currency
corpus. MOOSEDev's *read* cost is moderate and **query-mechanism-dependent**, not fixed: targeted
`get_relevant_context` (~35k) ≈ mem0's `recall`; completeness via SPARQL is ~78–167k for a 122-set agentic run —
a capability mem0 lacks at any cost. (An alarming "1.68M token" figure turned out to be a harness bug, §7.)

---

## 7. Honesty trail — why the numbers are trustworthy

The evaluation repeatedly caught and corrected its *own* errors; that auditability is the point.
- **A false +20pp "drift" win** was overturned by a free regrade over immutable transcripts — a grading artifact (a `stale`-substring flag + a token-balloon "escape" proxy). A grader can *manufacture* a signal, not only hide one. Lesson `a03dbe13`.
- **The LLM judge caught its own format bug** — it scored B2 0.00 on tasks B2 strict-scored 1.00 (B2 formats sets as fenced "title | IRI" blocks a bullet-extractor misses). The B2-validation check flagged it; the judge now reads the full answer. Lesson `a6529240`.
- **A dead-MCP bug nearly produced a wrong conclusion.** A `.env` loader I added clobbered `MOOSEDEV_BIN` with an unexpanded `$ROOT`, so the moosedev MCP never launched and *every* B2 agent fell back to grepping the store (0.5–1.2M tokens). This masqueraded as "mem0 wins relevance" / "B2 thrashes" / a weak-model tool-calling gap. With the bin fixed, gpt-5.4-mini calls `get_relevant_context` **4/4 at ~35k tokens** and B2 **ties** mem0 on relevance. The fix also retired the "agent query-competence" story. Lesson `0a890ea9` (supersedes the turf and efficiency claims → `e80db883`, `4d34f8c6`).

The meta-point: a plausible-but-wrong conclusion almost stuck, and an auditable, re-checkable process caught it —
which is precisely the failure mode (confident, fluent, wrong) the whole project is betting it can reduce.

---

## 8. Honest scope — what is NOT shown

- **Governance / auditability** (per-claim provenance, supersede-not-overwrite, validation) is MOOSEDev's deepest potential edge over mem0 — but it's **by design, unmeasured**.
- **Comprehension debt over months** — the *actual reason the project exists* — has **zero evidence**. The capability win is the point-in-time retrieval slice, not the longitudinal payoff.
- **Conditional on structured capture.** The win requires you to *capture* typed knowledge — friction mem0 doesn't have. On a flat capture, B2 degrades to free-text parity (Lesson `0b2a0f80`).
- **Single agent family** (codex / gpt-5.x); relevance markers were prior-contaminated (cold B0 0.33–0.80), so the relevance *tie* rests on tool-usage + token parity, not coverage — cleaner project-specific relevance tasks are a follow-up.

---

## 9. Verdict — don't kill it, earn the thesis

The kill signal would have been *structured memory ≈ vector/free-text memory on what matters, but costlier →
redundant.* We looked for it against a real competitor and found the **opposite**: a categorical, reproduced,
capture-clean capability (completeness / negation / traversal) that mem0 **cannot** match, with **parity** where
mem0 is strong — i.e. a superset. That clears the bar to keep investing.

It does **not** prove the big claims (governance, comprehension-debt-over-months); those stay hypotheses. So the
benchmark did its job: it ruled out "redundant RAG" and **earned the months-long in-anger trial** (AD `d3d4d7b1`)
— which is the real test of the thesis, to be instrumented up front (pre-registered "recover the why" probes, a
firing tally, a kill condition). Killing MOOSEDev now would be killing it right before the test that matters, on
evidence that already shows a real, alternative-beating capability.

---

## 10. Artifacts & reproduce

- **Graph (source of truth):** capability — Lessons `392855d0`, `6a70b02e`, `243c6c89`, `8a06ad1d`; competitor method — `440abc78`; grading — `a6529240`, `a03dbe13`, `9fc21dc0`; the dead-MCP correction — `0a890ea9`, `e80db883`, `4d34f8c6`; retrieval nulls — `7b41be4f`, `8f06f329`, `410d6c73`. ADs `47f3f038` (bench), `b3205dcb` (ecological arms), `b933bd10` (hybrid), `d3d4d7b1` (in-anger trial). Query via the `moosedev` MCP.
- **Harness (`bench/`, regrade-safe):** retrieval — `hybrid_*`, `scale_*`, `run_hybrid_ab.sh`, `run_scale_degradation.sh`; capability — `capability_build.py`, `grade_set.py`, `run_capability.sh`; competitor — `mem0_build.py` (mem0 ingests raw docs → on-disk qdrant), `mem0_mcp/server.py`, `regrade_judge.py` (B2-validated LLM judge); config — `config.py`. Currency detail: `RESULTS.md`.
- **Reproduce:** `$0` probes rerun from the stores with no agent; agentic matrices regrade from logged transcripts; competitor build needs `OPENROUTER_API_KEY` in `.env`, then `mem0_build.py --corpus codegraph --reset` and `CORPUS=codegraph ARMS="B2 B1-notes B1-mem0" ./run_capability.sh`.
