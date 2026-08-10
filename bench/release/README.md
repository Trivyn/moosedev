# Benchmark artifact release — NeSy 2026 paper

Artifacts backing *"Ontology-Grounded Project Memory for Coding Agents"* (NeSy 2026
Industry Track). This directory is a reviewed, privacy-scoped copy of the benchmark
outputs; the harness itself lives one level up in `bench/`.

## Contents

- `runs/` — immutable agent run transcripts (`*.events.json`) for the public-corpus
  experiments; graded results live in the run indexes below:
  - `codegraph_*` — the full five-condition capability/relevance/currency matrix on the
    neutral public CodeGraph corpus (arms: B0 no-memory, B1-mem0, B1-notes, B1-rag, B2
    typed graph). Backs the capability table (1.00 vs ~0.15) and the relevance tie.
  - `moosedev-temporal_*` — currency reversal-pair runs on this repository's own
    commit-history corpus, including the rank-inverted `glrc` pair behind the paper's
    "1 of 13 trials (8%)" B1-rag result and the B2 13/13.
  - `runs.jsonl` / `runs_regraded.jsonl` — run indexes with judge scores and metrics,
    filtered to the released corpora.
- `corpus/` — corpus exports: `codegraph.json` (the 835 typed records, captured from the
  documentation of [CodeGraph](https://github.com/colbymchenry/codegraph), a third-party
  MIT-licensed code-knowledge-graph tool — thanks to its authors), `moosedev-temporal.json`
  (temporal bootstrap of this repository), `rust-rfcs.json` (scale-study corpus, from the
  public Rust RFCs).
- `scale/` — the scale-degradation study (rust-rfcs, N=50→634; hit@5 0.84 vs 0.60 at
  N=634, constant offset).

## What is withheld

Transcripts and corpora for the two private-project corpora (`trivyn`, `burrow`) are not
released: they contain a private company codebase's and a private project's records.
Aggregate numbers for those experiments appear in `bench/EVALUATION.md` (e.g. the Stage-1
F1 0.94 vs 0.25). This mirrors the paper's method: the headline comparison deliberately
uses the neutral public corpus.

## Escaped and out-of-scope runs; redactions

Thirteen transcripts carry redactions. Eleven are baseline arms — three B0 and eight
B1-rag — that read beyond their assigned condition into private material: private source
trees under the benchmark home, a private project-graph dump in `/tmp`, and
working-directory listings exposing private run and task names. **No B2 run captured
private content**; two B2 oracle runs (`0068a114`, `cae14b26`) ran directory commands that
returned sibling working-directory *names* only (scrubbed), and their answers came from
the pushed oracle context. In the released copies, the captured *output* of every
command that touched private material is replaced with an explicit
`[REDACTED for release: N bytes ...]` marker (about 2.7 MB removed in total); the command
lines themselves are preserved verbatim as the honest record of what each agent did. One
transcript (`...64d40835`) additionally had three echoed private task notes replaced
inline, and one (`...2afc3e5b`) had three private record labels quoted in its final
answer replaced inline — there and in that row's `final_text` in the run indexes.
Transcripts are otherwise verbatim, including local workspace paths.

Affected runs (graded scores retained in the matrix, flagged here rather than dropped):

| run | arm/regime | score |
|---|---|---|
| moosedev-temporal glrc `25086b06`, `6e35142b`, `fb76cdee`, `69b4ab03`, `7388cb15`, `d543a731`, `a7efbaf8` | B1-rag / tooluse | all 0.7, passed |
| moosedev-temporal glrc `b76a28ba` | B1-rag / oracle | 0.0, failed |
| moosedev-temporal glrc `408f1cb1` | B0 / oracle | 0.7, passed |
| codegraph set_accepted_constraints `64d40835`, mh_req_through_constraint `2afc3e5b` | B0 / tooluse | both 0.0, failed |
| moosedev-temporal glrc `0068a114`, `cae14b26` (names-only listings) | B2 / oracle | both 0.7, passed |

**No number reported in the paper depends on a run that captured private content.** The
"1 of 13 trials (8%)" B1-rag currency figure is the push/oracle regime: none of those 13
rows captured private content, the one that ran out-of-scope directory listings
(`b76a28ba`, scrubbed) *failed* — it served the stale answer anyway — and the single
passing trial (`035edd2f`) executed no commands at all. Both escaped codegraph B0 runs
scored 0.0, so "B0 scored zero" stands. The escapes concentrate in memory-poor arms, consistent with the project's recorded
lesson that agents without memory go read source; current harness guidance isolates the
workdir and scores escape as a memory miss — these runs predate it.

## Regrading

Runs regrade without re-executing any agent. Place the release contents where the harness
expects them (`runs/`, `corpus/`, `scale/` under `bench/`, including the `.jsonl` indexes),
then:

- `python3 regrade.py` — recomputes score/passed/metrics for every indexed row from the
  stored `final_text`/artifacts with the current grader, excluding rows listed in a
  sibling `runs_invalid.jsonl`. Run over the released indexes it reproduces the shipped
  `runs_regraded.jsonl` byte-for-byte (verified 2026-08-06).
- `python3 regrade_judge.py` — the LLM-judge recall regrade for capability set tasks
  (needs `OPENROUTER_API_KEY` in `.env`).

**The index files:**

- `runs.jsonl` — append-only raw telemetry for the released corpora (248 rows), kept
  verbatim. It includes 19 codegraph B2 tooluse rows from the 2026-06-23 dead-MCP
  configuration-bug window (`bench/EVALUATION.md` §7): the memory server was unreachable,
  and agents either reported the tool missing or read the raw store directly instead of
  using it.
- `runs_invalid.jsonl` — the explicit invalid-run manifest: those 19 rows, each with a
  mechanical reason (raw-store-access count, or memory-tool-unreachable) derived from its
  own transcript. Only B2 tooluse rows can be invalid on this criterion; baseline arms
  have no memory-tool condition to violate.
- `runs_regraded.jsonl` — the graded matrix (229 rows): every raw row minus the manifest,
  re-scored uniformly on 2026-08-06 with the current deterministic grader (`regrade.py`).
  The regrade produced zero score changes on rows shared with the historical snapshot and
  reproduces the paper's headline checks (glrc B1-rag oracle 1/13; glrc B2 25/25). Table
  1's paraphrase-fair recall columns additionally use `regrade_judge.py`.
- `runs_regraded.snapshot-20260623.jsonl` — the historical point-in-time regrade,
  preserved verbatim for provenance. It predates the final re-runs (it is literally the
  first 224 raw rows) and contains seven rows now classified invalid; the regenerated
  matrix above supersedes it.

See `bench/EVALUATION.md` for the consolidated results and `bench/RESULTS.md` for
currency detail.
