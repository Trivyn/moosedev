# MOOSEDev v1 Specification

**Version:** 1.0  
**Date:** June 2026  
**Status:** v1 Scope Definition

## 1. Positioning & Goals

**MOOSEDev** is a neurosymbolic MCP sidecar that helps coding agents (and eventually humans) maintain accurate, structured understanding of software projects over time.

Its primary purpose is to combat **comprehension debt** — the gradual loss of shared understanding of *why* a codebase is designed the way it is — and to serve as an early example of a **domain-specific, epistemically grounded AI application**.

MOOSEDev is built on the closed-source MOOSE neurosymbolic engine. The MOOSEDev application itself is open source.

### Core Philosophy
- Prioritize symbolic reasoning and structured knowledge over pure generation.
- Maintain a queryable, typed project knowledge graph instead of relying solely on expanding context windows.
- Emphasize auditability, alignment, and user control.
- Reduce dependence on frontier models by grounding agents in explicit architectural knowledge.

## 2. v1 Scope (Prioritized)

### Must-Have (High Impact)
- External symbolic session/project memory
- Typed architecture knowledge capture (`ArchitecturalDecision`, `Lesson`, `Constraint`, `AntiPattern`, etc.)
- Concept alignment to the project model
- Natural language querying with execution traces
- Bootstrap workflow for existing codebases

### Should-Have
- Local SPARQL endpoint
- Lightweight architectural validation

### Out of Scope for v1
- Full ontology generation (remains in Trivyn)
- Heavy code synthesis or generation capabilities
- Programming language-specific ontologies
- Public or authenticated endpoints

## 3. Key Components

- **Session/Project Knowledge Graph**: Persistent, queryable memory built on MOOSE’s Chat pipeline and focus stack.
- **Architecture Ontology**: Ships with general software engineering and architecture ontologies, including typed classes for decisions and lessons.
- **Alignment Subsystem**: Reuses MOOSE’s alignment engine to keep generated concepts consistent with the project model.
- **Execution Traces**: Provides auditability for all reasoning steps.

## 4. Directory Structure
```
moosedev/
├── src/                          # MCP server implementation (Rust)
│   ├── main.rs
│   ├── mcp/
│   │   ├── server.rs
│   │   └── tools/
│   │       ├── memory.rs
│   │       ├── alignment.rs
│   │       ├── query.rs
│   │       └── validation.rs
│   ├── graph/
│   ├── ontology/
│   └── bootstrap/
├── ontologies/
│   ├── software-engineering.ttl
│   └── architecture.ttl
├── skills/
│   └── bootstrap-existing-codebase.md
├── docs/
├── scripts/
│   └── build-release.sh
├── Cargo.toml
├── README.md
└── LICENSE
```
## 5. Core MCP Tools (v1)

| Category     | Tool Name                        | Purpose                                              | Priority |
|--------------|----------------------------------|------------------------------------------------------|----------|
| Memory       | `get_relevant_context`           | Retrieve relevant prior context from the session graph | High     |
| Memory       | `get_focus_stack`                | Return current symbolic focus stack                  | High     |
| Memory       | `search_session_graph`           | Search past decisions, lessons, and knowledge        | High     |
| Memory       | `record_important_decision`      | Record typed decisions, lessons, or constraints      | High     |
| Alignment    | `align_concepts`                 | Align new concepts to the loaded ontologies          | High     |
| Alignment    | `suggest_mappings`               | Propose ontology mappings for new concepts           | Medium   |
| Query        | `query`                          | Natural language query with reasoning traces         | High     |
| Validation   | `validate_against_architecture`  | Perform lightweight architectural validation         | Medium   |

## 6. Bootstrap SKILL

MOOSEDev includes a structured workflow (delivered primarily as documentation and prompt templates) that guides an agent to analyze an existing codebase and populate the project knowledge graph with typed architectural knowledge.

**Purpose**:
- Accelerate onboarding of MOOSEDev on existing projects
- Serve as a powerful internal testing tool
- Validate the typed ontology and recording mechanisms

The workflow is documented in `skills/bootstrap-existing-codebase.md`.

## 7. Open Source & Distribution Model

- **MOOSEDev source code**: Fully open source
- **Core MOOSE engine**: Remains closed-source for now
- **Distribution**: Official releases are distributed as pre-built binaries
- Users can download ready-to-run binaries from GitHub Releases
- Building from source requires access to the private MOOSE repository

## 8. Additional Interfaces

- **Local SPARQL Endpoint**: Enabled by default for power users and tooling integration
- **Simple Local Web UI** (stretch): Read-only interface for inspecting the focus stack, recorded decisions, and project knowledge graph

## 9. Success Criteria for v1

- Agents can maintain coherent understanding across long sessions with significantly reduced context usage
- New concepts introduced during development are aligned to a consistent project model
- The bootstrap workflow successfully extracts and structures useful architectural knowledge from real codebases
- MOOSEDev demonstrates clear differentiation from general-purpose context and memory tools

---

**End of Specification**
