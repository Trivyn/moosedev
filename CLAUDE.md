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
- After ANY correction from the user: update `tasks/lessons.md` with the Pattern
- Write rules for yourself that prevent the same mistake
- Ruthlessly iterate on these lessons until mistake rate drops
- Review lessons at session start for relevant project

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

1. **Plan First**: Write plan to 'tasks/todo.md' with checkable items
2. **Verify Plan**: Check in before starting implementation
3. **Track Progress**: Mark items complete as you go
4. **Explain Changes**: High level summary at each step
5. **Document Results**: Add review section to `tasks/todo.md`
6. **Capture Lessons**: Update `tasks/lessons.md` after corrections

## Core Principles
- **simplicity first**: Make every change as simple as possible.  Impact minimal code.
- **No Laziness**: Find root causes. No temporary fixes.  Senior developer standards.
- **Minimal Impact**: Changes should only touch what's necessary. Avoid introducing bugs.

## Dogfooding MOOSEDev (self-hosted memory)

This repo uses **itself** for long-term memory: a local `moosedev` MCP server exposes the durable
project knowledge graph. When its tools are available, prefer them over re-deriving context (invariant #5).

- **Recall first** — before non-trivial work, use `get_relevant_context` or `query` to surface prior
  decisions, lessons, and constraints.
- **Keep questions to MOOSEDev short** — the `query` (NLQ) tool wants one focused question per call, a
  single sentence ideally, not a paragraph.
- **Capture as you go** — record durable knowledge with `record_important_decision` (`kind`:
  `ArchitecturalDecision`, `Lesson`, `Constraint`, `Pattern`, `AntiPattern`, `Requirement`); capture the
  decision and its rationale, not transient chatter.
- **Align new concepts** with `align_concepts` before introducing a new term, so the model graph does
  not drift (invariant #4).
- **Verify** with `validate_against_architecture` after capturing; use `sparql` for precise,
  deterministic reads of the graph.

The graph persists in a local, gitignored store and grows more valuable over time (invariant #10).
~
