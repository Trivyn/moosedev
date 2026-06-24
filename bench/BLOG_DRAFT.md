# I put structured project memory head-to-head with mem0 — here's what it actually does, and what it doesn't

> **Draft.** Honest-claims scaffold built from `bench/EVALUATION.md` + the project knowledge graph. Lead
> examples use **codegraph** (a public repo) so every concrete number is checkable; a second corpus is a
> private project, cited only in aggregate.

## The claim, narrowed

> Structured project memory lets a coding agent answer **completeness and history** questions about a
> codebase's decisions — *"list every constraint," "which accepted decisions have no recorded rationale,"
> "what replaced this and why"* — that a vector memory tool like **mem0 cannot answer completely, by
> construction.** It does **not** beat mem0 at what mem0 is *for* (pulling the relevant few memories,
> staying current); there it ties. It's a **superset**, not a better mousetrap. And the long-horizon
> "fights comprehension debt over months" payoff — the actual reason I'm building this — is still a
> hypothesis, not a result.

If that sounds narrow, good. It's falsifiable, it's tested against a real competitor instead of a strawman,
and it survived me actively trying to break it (including, as you'll see, breaking it by accident).

## What I tested

Same raw material to every memory system; each captures and retrieves *its own way*. The contestants:

- **mem0** — the actual, popular vector-memory tool. It ingested codegraph's raw design docs and extracted
  memories its own way (553 of them — I checked it genuinely *stored* the facts, so anything it misses is a
  retrieval limit, not a capture gap). Configured at full strength: semantic + BM25 + entity retrieval.
- **a docs-grep agent** — the team just keeps their decisions in docs and the agent greps them. The realistic
  low-effort baseline.
- **the structured graph** — the same docs bootstrapped into a typed knowledge graph (codegraph: **835
  records** — 392 decisions, 122 constraints, 107 requirements, 58 lessons, 19 anti-patterns) that the agent
  queries with SPARQL / structured recall.
- **cold** — no memory. The floor.

A real coding agent (codex / GPT-5.x) drives each one. I graded with a **strict LLM judge** that credits a
right answer in different words but rejects "same topic, not the same thing" — and, crucially, I *validated
the judge* by requiring it to reproduce the structured arm's exact score before I trusted its numbers on the
competitors. Every run is **re-gradable from its stored transcript**, so when I changed a grader I recomputed
the old runs without re-running an agent.

One honest detour worth admitting up front: my *first* baseline wasn't mem0 — it was my own graph flattened
into text. That's a tidy control, but it quietly hands the baseline all of my structured capture for free
("the graph wearing a markdown hat"). It's not how a real tool works. So I threw it out as the headline and
ran against actual mem0 capturing the raw docs itself. The structured win got *starker*, not weaker.

## What I found

### 1. The win: completeness and history (categorical)

Ask mem0 *"list every constraint in this project."* codegraph has **122**. mem0 returns ~20 loosely-related
facts — it's a top-k relevance retriever; it was never built to enumerate a complete set. Ask *"which accepted
decisions have no recorded rationale"* and it's worse: the *absence* isn't a string you can match. Ask *"what
replaced decision X and why"* and similarity search hands you back things near X.

The graph answers all three exactly, in one query. Fairly judged, on the public corpus:

| question type | mem0 | docs-grep | structured graph |
|---|---|---|---|
| set-completeness (122-item set) | **0.15** | 0.02 | **1.00** |
| negation / "no recorded rationale" | 0.17 | 0.01 | **1.00** |
| supersession ("what replaced X") | 0.15 | 0.15 | **1.00** |

(On a second, private corpus — ~400 decisions over a year — the structured arm pools ~0.94 against ~0.34 for
flat memory. Same shape, different repo.)

This isn't "more relevant results." It's **possible-vs-impossible.** mem0 *holds* the facts (all 553 of them) —
it just can't enumerate them, can't match an absence, and can't traverse a typed link. Top-k tops out at the
top k. The honest boundary: the win scales with *set size*. For a 2–5 item answer or a single lookup, mem0 is
fine and they tie.

### 2. The ties: I don't beat mem0 at mem0's job

This is the part that makes the result credible instead of a sales pitch. On **mem0's home turf**, they tie:

- **Relevance recall** — "why does codegraph do X?" Both systems call their tool and answer from the right
  memory, at comparable cost (~35–40k tokens). The graph isn't cheaper or more accurate here; it's a wash.
- **Currency** — when a decision was reversed, both serve the *current* state (the graph guarantees it via
  supersession links; mem0 manages it with temporal reasoning). Cold memory fails; both real memories pass.

So the picture is **inverse trades.** mem0 is cheap, foolproof, and small-context. The graph is complete,
structured, and auditable. They tie where mem0 is built to win, and the graph adds a structural capability mem0
can't reach. Different tools — one strictly broader on completeness and structure.

### 3. The bug that almost fooled me

For a while my own benchmark told me **mem0 won at relevance** — the structured agent was burning 0.5–1.2M
tokens grepping around while mem0 answered cheaply. I almost wrote that down.

It was a bug *I* introduced. A config change I'd made to load an API key had quietly clobbered the path to the
memory server's binary, so the structured agent's tool **never launched** — and a coding agent with no memory
tool does the natural thing: it greps the filesystem. Every "the model can't use its own tool" and "mem0 wins
relevance" number was an artifact of a dead tool.

I only caught it because one code path *crashed* on the bad path while the others failed *silently*. Fixed it,
re-ran, and the same agent called the memory tool reliably and answered at ~35k tokens — a tie. A confident,
fluent, wrong conclusion almost stuck, and an auditable, re-checkable process caught it. That failure mode —
plausible but wrong — is the exact thing this whole project is betting structured memory can reduce. (It's also
why I'll trust nobody's memory-tool benchmark, including mine, that can't be re-graded from transcripts.)

## What I can't claim (yet)

- **Not better retrieval.** Three separate tests — a dense-vector channel, a 50→634-record scale sweep, a
  "split the blob into typed nodes" decomposition — all came out **≈ parity** with good RAG. If you only need
  *find the relevant record*, mem0 is fine.
- **The edge doesn't compound with size.** The scale sweep killed the "gets better as the codebase grows"
  story: a roughly constant gap, slope ≈ 0, confidence interval through zero. Don't believe anyone (including
  me) who claims compounding without a slope and an interval.
- **Auditability is unproven.** Per-claim provenance and "why did this memory change" are the graph's deepest
  potential edge over mem0 — and I haven't *measured* them yet. By design ≠ demonstrated.
- **Comprehension debt over months is unmeasured.** That's the motivating thesis, and I have *no* evidence for
  it. It needs a longitudinal, in-anger trial. Until then it's a hypothesis.
- **Generalization is thin** — two corpora, one agent family, and the structured win depends on actually
  capturing typed knowledge (friction mem0 doesn't have). On a flat, link-less capture it degrades to parity.

## How to check me

Everything's reproducible: the harness, the typed graphs, and the regrade-from-transcript path. The zero-cost
probes rerun from the stored corpora with no agent; the agentic matrices regrade from logged transcripts; the
verdicts were pre-registered; the competitor is the real mem0, not my approximation of it. Numbers and caveats
live in `EVALUATION.md` and a queryable knowledge graph with provenance.

## What's next

The honest gap is the long game: does this actually reduce the slow loss of *why* a codebase looks the way it
does, used in anger over months? That's the trial worth running. What I have — a real, reproduced capability
that a leading memory tool structurally lacks, *plus* parity where that tool is strong — is what justifies
trying it. It is not a claim that it's already proven. The benchmark's job was to decide whether this is
differentiated enough to earn that expensive trial. It is. So I'm running it.

---
*Build/verify notes (not for publication): private corpus cited in aggregate only. Capability + competitor
result — graph Lessons `243c6c89` (codegraph two-corpus), `440abc78` (competitor-fairness method), `a6529240`
(judge fairness), `e80db883` (corrected balanced matrix), `0a890ea9` (the dead-MCP bug); retrieval nulls —
`7b41be4f`, `8f06f329`; in-anger trial — AD `d3d4d7b1`. All numbers from `EVALUATION.md`.*
