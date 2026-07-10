# MOOSEDev

MOOSEDev is a neurosymbolic MCP server designed to give coding agents reliable, structured, long-term memory and understanding of software projects. Its goal is to combat comprehension debt — the gradual loss of understanding of why a codebase is structured the way it is — and to serve as an early example of a domain-specific, epistemically grounded AI application.
It is built on top of the closed-source MOOSE neurosymbolic engine. MOOSEDev itself is open source.

---

## Core Philosophy

MOOSEDev exists to push back against the trend of increasingly relying on general-purpose LLMs that optimize for sounding correct rather than being correct. Instead, it emphasizes:

Symbolic reasoning over pure generation
Structured, typed knowledge (not just text)
Auditability and user control
Building a living project knowledge graph over time
Reducing dependence on ever-growing context windows

## Design invariants

These principles should guide all work on MOOSEDev. They reflect both the technical architecture and the deeper intent behind the project.

### 1. Symbolic Layer is Primary
The LLM is a sensor, not the controller. Whenever possible, prefer deterministic symbolic mechanisms (alignment, typed knowledge capture, session graph, focus stack, validation) over relying on the LLM to reason or remember correctly. The strength of MOOSEDev comes from the symbolic layer, not from making the LLM do more work.

### 2. Structured Knowledge Over Free Text
Prefer typed, structured representations (`ArchitecturalDecision`, `Lesson`, `Constraint`, `AntiPattern`, etc.) over unstructured text in markdown files. Free-text notes (like `lessons.md`) are acceptable as a human-readable view, but the source of truth should be structured data in the project knowledge graph.

### 3. Fight Comprehension Debt
A core purpose of MOOSEDev is to reduce the gradual loss of understanding in a codebase. Every major decision, lesson, constraint, or pattern should be captured explicitly so that both agents and humans can recover context quickly and reliably.

### 4. Alignment Prevents Drift
New concepts introduced during development should be aligned to existing models rather than allowed to accumulate as disconnected or slightly inconsistent ideas. Use the alignment tools proactively.

### 5. Memory Should Be External and Queryable
Do not rely on stuffing large amounts of history or context into the LLM’s context window. Use MOOSEDev’s session/project knowledge graph as the primary long-term memory. Prioritize tools like `get_relevant_context`, `search_session_graph`, and `get_focus_stack`.

### 6. Auditability Matters
Reasoning should be traceable. When possible, surface or preserve execution traces, decision rationales, and the path that led to a conclusion. Transparency builds trust.

### 7. Domain-Specific > General-Purpose (for now)
v1 focuses on software engineering and architecture. While the long-term vision includes other domains, resist scope creep into general-purpose capabilities. Depth in one domain is more valuable than breadth.

### 8. Leverage Existing MOOSE Capabilities
MOOSEDev should primarily expose and compose capabilities that already exist in MOOSE (Chat pipeline, alignment subsystem, symbolic memory, etc.). Avoid reimplementing core neurosymbolic logic inside MOOSEDev.

### 9. User Control and Local Operation
MOOSEDev should remain fully usable locally and under user control. Features that require cloud services or remove user agency should be avoided or clearly marked as non-goals.

### 10. Bootstrap and Evolution
MOOSEDev should be able to help recover understanding from existing codebases (via the bootstrap workflow), not just accumulate knowledge from new work. The system should get more valuable over time as the project knowledge graph grows.

### 11. Open Source Boundaries
MOOSEDev (the MCP server, tools, prompts, ontology, and documentation) is open source. The core MOOSE engine remains closed for now. When designing features, keep this boundary in mind — contributions to the open parts are welcome; deep changes to MOOSE behavior are not in scope for external contributors at this stage.

## Long-term Vision
MOOSEDev is the first step toward domain-specific, trustworthy AI applications that run locally under user control. The long-term goal is to build systems that help maintain accurate understanding of complex domains rather than replacing human (or collective) judgment with highly plausible but ungrounded output.

## Development practices

### 1.  Plan Mode Default
- Enter plan mode for ANY non-trivial task (#+ steps or architectural decisions)
- If something goes wrong, STOP and re-plan immediately - don't keep pressing on.
- Use plan mode for verification steps, not just building
- Write detailed specs upfront to reduce ambiguity

### 2. Subagent Strategy
- Use subagents liberally to keep main context window clean
- Offload research, exploration, and parallel analysis to subagents
- For complex problems, throw more compute at it via subagents
- One task per subagent for focused execution

### 3. Self improvement Loop
- After ANY correction from the user: record a typed `Lesson` in the graph
  (`record_important_decision`, `kind: Lesson`) — the graph is the source of truth (invariant
  #2). Optionally mirror it to `tasks/lessons.md` as a human-readable view.
- Capture the Pattern and a rule that prevents the same mistake; recall-first (a **list-all**
  `get_relevant_context`) before recording so you don't duplicate an existing Lesson.
- Ruthlessly iterate until the mistake rate drops.
- Review prior Lessons at session start via `get_relevant_context` / `query`.

### 4. Verification before Done
- Never mark a task complete without proving it works
- Diff behavior between main and your changes when relevant
- Ask yourself: "Would a staff engineer approve this?"
- Run tests, check logs, demonstrate correctness

### 5. Demand Elegance (Balanced)
- For non-trivial changes: pause and ask "is there a more elegant way"
- If a fix feels hacky: "Knowing everything I know now, implement the elegant solution."
- Skip this for simple, obvious fixes -- don't over-engineer
- Challenge your own work before presenting it.

### 6. Autonomous Bug fixing
- When given a bug report: Just fix it.  Don't ask for hand-holding
- Point at logs, errors, failing tests - then resolve them
- Zero context switching required from the user
- Go fix failing CI tests without being told how

## Task management

The durable **project knowledge graph is the source of truth**; markdown files
(`tasks/todo.md`, `tasks/lessons.md`) are optional human-readable mirrors, never the canonical
record (invariant #2). See "Dogfooding MOOSEDev" for the tool loop.

1. **Recall First**: surface prior decisions/lessons/constraints from the graph
   (`get_relevant_context` / `query` / `sparql`) before non-trivial work — and show the queries.
2. **Plan First**: write the plan (the plan file, or `tasks/todo.md`) with checkable items.
3. **Verify Plan**: check in before starting implementation.
4. **Capture as Typed Records**: record durable decisions/requirements/constraints/patterns as
   you go (`record_important_decision`); `align_concepts` before coining a new term; always
   report what was written (kind/title/IRI). Correct with `supersede_decision` (replacement) or
   `retract_decision` (deprecate) — never a silent duplicate.
5. **Track Progress & Explain Changes**: mark items complete; give a high-level summary at each step.
6. **Validate**: run `validate_against_architecture` after capturing.
7. **Capture Lessons**: after corrections, record a typed `Lesson` (see Self improvement Loop).

## Core Principles
- **simplicity first**: Make every change as simple as possible.  Impact minimal code.
- **No Laziness**: Find root causes. No temporary fixes.  Senior developer standards.
- **Minimal Impact**: Changes should only touch what's necessary. Avoid introducing bugs.

## Dogfooding MOOSEDev (self-hosted memory)

This repo uses **itself** for long-term memory: a local `moosedev` MCP server exposes the durable
project knowledge graph. When its tools are available, prefer them over re-deriving context (invariant #5).

- **Recall first** — before non-trivial work, use `get_relevant_context` or `query` to surface prior
  decisions, lessons, and constraints.
- **Dossier before editing code** — topic recall depends on choosing good words; a position does not.
  Before modifying a specific function/type/module, call `get_entity_dossier` (file + 1-based
  line/col, SCIP symbol, or entity IRI) on it. Treat any `Constraint` in the result as a hard rule
  for the edit; `realizes` + the "via component" section tell you whose architectural rules apply.
  The no-recorded-knowledge reply means nothing is linked there — it does not replace topic recall.
- **Link decisions to the code they govern** — when a captured record is really about a *specific*
  entity (not a whole component), attach it with `link_code(record_iri, predicate, file/line/col |
  symbol)`; `constrains` for Constraints, `concerns` otherwise. Private entities are minted lazily
  by the call. That link is what makes the dossier (and editor hover) find it later.
- **Keep questions to MOOSEDev short** — the `query` (NLQ) tool wants one focused question per call, a
  single sentence ideally, not a paragraph.
- **Capture as you go** — record durable knowledge with `record_important_decision` (`kind`:
  `ArchitecturalDecision`, `Lesson`, `Constraint`, `Pattern`, `AntiPattern`, `Requirement`); capture the
  decision and its rationale, not transient chatter.
- **Anchor code-touching records** — link records to their `SystemComponent` with
  `relations: [{predicate: "concerns", target: "<component name>"}]`; list components with `sparql`
  if unsure.
- **Align new concepts** with `align_concepts` before introducing a new term, so the model graph does
  not drift (invariant #4).
- **Verify** with `validate_against_architecture` after capturing; use `sparql` for precise,
  deterministic reads of the graph.

The graph's committed source of truth is `.moosedev/kg.nq` — canonical N-Quads, re-exported
automatically on every write and reconciled at startup (a fresh clone hydrates from it). The
RocksDB store and vector DBs under `.moosedev/` are a derived, gitignored local cache. The graph
is version-controlled with the code and grows more valuable over time (invariant #10).
~

<!-- moosedev:begin — MOOSEDev project-memory workflow. Managed by `moosedev init`; edit around this block freely, or delete the whole begin…end block to opt out. -->
> This project uses the **MOOSEDev** MCP server for durable, structured, long-term memory.
> The typed **project knowledge graph is the source of truth** for architectural decisions,
> lessons, constraints, requirements, and patterns — **not** markdown files. Free-text notes
> (e.g. `tasks/lessons.md`, `tasks/todo.md`) are optional human-readable mirrors, never canonical.

## Working with project memory (MOOSEDev)

When the `moosedev` MCP tools are available, prefer them over re-deriving context from scratch.
The loop:

1. **Recall first.** Before non-trivial work, surface prior decisions/lessons/constraints from
   the graph — and show the queries you ran. "Recall first" means a **list-all**
   `get_relevant_context` (no `topic`), not only a topic probe: a topic-scoped empty result
   means nothing cleared the relevance floor, **not** that the graph is empty.
2. **Capture as typed records.** Record durable knowledge as you go with
   `record_important_decision` (pick the right `kind`). Capture the decision **and its
   rationale** (and the rejected alternative), not transient chatter. Always report what was
   written (kind / title / returned IRI) — no silent writes.
3. **Align before coining.** Run `align_concepts` (or `suggest_mappings`) before introducing a
   new term, so the graph doesn't drift.
4. **Correct, don't duplicate.** `supersede_decision` when there is a replacement;
   `retract_decision` to deprecate one without a successor. Never silently duplicate — recall
   (list-all) first to confirm a record is genuinely new.
5. **Validate.** Run `validate_against_architecture` after capturing; resolve violations.

### Tool-selection ladder (cheap → precise)
- `get_relevant_context` — fast, deterministic, **shallow** lexical anchor/browse. Start here.
- `query` — walk-planned, synthesized natural-language answer **with a reasoning trace**. Use
  when you need reasoning over relationships, not just a label match. Keep questions short and
  focused (one question per call).
- `sparql` — exact, deterministic structural reads of the graph. Use for precise listings.

### Capture kinds
`ArchitecturalDecision` (the default — a choice + why + what was rejected) ·
`Constraint` (a hard limit/invariant) · `Requirement` (a goal/need) ·
`Pattern` (a deliberate recurring approach) · `AntiPattern` (something to avoid, + why) ·
`Lesson` (a non-obvious learning/gotcha).
<!-- moosedev:end -->
