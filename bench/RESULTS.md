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
