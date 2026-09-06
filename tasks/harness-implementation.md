# Harness implementation

Behavioral source: [MOOSEDev Harness v0.1](../spec/MOOSEDev_harness_spec.md).
This file is a review/checklist mirror, not canonical project knowledge.

## Status and authorization

- [x] Save the conversational spec as a short behavioral draft.
- [x] Recall current knowledge and check for existing Requirements/Constraints.
- [x] Extract the new records below without writing them to the graph.
- [x] Obtain explicit approval to capture the seven records below as accepted.
- [x] Capture approved records, report kind/title/IRI, and validate the graph.
- [x] Resolve implementation choices against these records and capture decisions
  with their motivating Requirement/Constraint links before code changes.
- [x] Implement and verify the phases below.

The user explicitly approved these records with “Approve” on 2026-09-06.
All seven were captured as accepted before implementation decisions and code.

## Existing records to reuse

- Requirement: [MOOSEDev v3: standalone neurosymbolic coding agent](https://moosedev.dev/kg/Requirement/350c7f2e-61bd-49e4-b7cd-2fdd3f51f521).
- Constraint: [Every surface is a thin client of the one policy engine](https://moosedev.dev/kg/Constraint/2ba76439-e146-425d-b18a-4d46f7418cb8).
- Constraint: [Proposed judgments must never influence the gate](https://moosedev.dev/kg/Constraint/4879b25c-4410-4c88-b22c-fd187a31b143).
- Lesson: [Evaluate harness foundations by enforced reading and capture](https://moosedev.dev/kg/Lesson/6046ab5f-84da-456f-bd92-c4df301d3630).

These existing records are dependencies, not candidates for duplicate capture.

## Accepted records

| Record | Canonical graph IRI |
| --- | --- |
| R1 · Requirement | [Standalone harness with headless and TUI operation](https://moosedev.dev/kg/Requirement/4c7530e7-ec40-418d-ac4c-e8a5bd98a6a3) |
| R2 · Requirement | [Harness defaults to Plan and executes approved work in Auto](https://moosedev.dev/kg/Requirement/8d27fa75-1858-4a1f-a7c9-9a76c9f72c7a) |
| R3 · Requirement | [Harness enforces current knowledge delivery before work](https://moosedev.dev/kg/Requirement/0145baa7-1479-4290-b93f-ceaf8b53d58a) |
| R4 · Requirement | [Harness enforces evidence-grounded capture checkpoints](https://moosedev.dev/kg/Requirement/b6853918-4325-4547-bc50-9b7abaf964c0) |
| R5 · Requirement | [Harness completion requires verification and human knowledge review](https://moosedev.dev/kg/Requirement/c9d06b61-92a6-47a7-a7a4-efdf8decb92c) |
| R6 · Requirement | [Harness resumes without losing obligations or repeating writes](https://moosedev.dev/kg/Requirement/b31c7834-065b-461b-b1cd-a39f58cf84d7) |
| C1 · Constraint | [Harness mediates every model-requested edit and command](https://moosedev.dev/kg/Constraint/d3268de1-50e3-4f55-8766-1b753cb8735f) |

### R1 · Requirement · Standalone harness with headless and TUI operation

Provide an additional standalone, local-first harness for general coding work,
with a headless runner and thin TUI sharing start, inspect, approve, mode-change,
cancel, and resume operations. Retain daemon compatibility with existing MCP,
HTTP, and LSP clients and use a configurable local model endpoint. The runner
invokes daemon and model operations programmatically. OpenCode integration is
outside the initial scope.

### R2 · Requirement · Harness defaults to Plan and executes approved work in Auto

New tasks start in Plan: read-only project exploration and reviewable planning,
with planning knowledge captured as proposals. Auto executes an approved plan
within user permissions and daemon policy. It pauses for required ratification,
conflicts, destructive actions, or actions outside the authorized workspace.
Material objective or approach changes return to planning. Mode changes retain
evidence, task state, and capture obligations.

### R3 · Requirement · Harness enforces current knowledge delivery before work

Retrieve current requirements, constraints, decisions, and lessons before
planning, and affected entity/component knowledge before proposing edits. Newly
discovered targets require fresh retrieval. Supply bounded model context and
record source/knowledge versions. Scope or governing-evidence changes refresh
context and invalidate affected approvals. Verify observable compliance rather
than treating model acknowledgment as proof of understanding.

### R4 · Requirement · Harness enforces evidence-grounded capture checkpoints

At decision checkpoints and final review, preserve contemporaneous instructions,
choices, conversation, changes, and verification evidence. Reconcile with existing
knowledge and propose warranted ArchitecturalDecision, Requirement, Constraint,
Lesson, Pattern, and AntiPattern records with evidence and appropriate links.
Use explicit supersession/retraction for changed knowledge. Activity journaling
does not discharge capture; neither forced new records nor invented rationale
are permitted. Proposed knowledge cannot independently authorize actions.

### R5 · Requirement · Harness completion requires verification and human knowledge review

Mark a task complete only when its requested outcome is met, required checks pass,
the user has accepted/rejected its knowledge proposals or confirmed that no durable
knowledge changed, resulting graph writes/links are persisted, and graph validation
passes. Display execution-finished/knowledge-review-pending separately from complete.

### R6 · Requirement · Harness resumes without losing obligations or repeating writes

Persist operational mode, plan, action outcomes, evidence references, approvals,
and unresolved obligations outside the canonical graph. Cancellation/interruption
preserves resumable state. Resume refreshes evidence and prevents repeated completed
edits and duplicate proposals. Required daemon failures block dependent work.

### C1 · Constraint · Harness mediates every model-requested edit and command

All model-requested edits and commands must pass through the runner's execution
boundary. Shell execution cannot bypass workspace permissions, Plan restrictions,
or daemon edit policy. Validate proposed model actions before execution and bound
repair attempts for malformed actions.

Recorded metadata: `kind` as shown; `status: accepted`; exact titles and descriptions
above; `concerns` the existing “active-agency policy engine” component for all seven
records, additionally “MCP tool surface & server runtime” for R1.

Implementation ArchitecturalDecision:
[Standalone harness executes through daemon-backed task gates](https://moosedev.dev/kg/ArchitecturalDecision/2c3a1def-3756-4a1d-9cd6-1c25c44e0e66).
It links its motivating records and chooses an optional Rust runner/Ratatui binary,
daemon HTTP adapters, an operational journal, explicit file scope and checks,
confined commands, and human capture review. Component path coverage now includes
`src/harness/` under the active-agency policy engine and
`src/bin/moosedev-harness.rs` under the MCP runtime.

## Implementation phases and verification

1. [x] **Execution contract:** Define task state transitions, approval provenance,
   evidence freshness, operation identity/reconciliation, and the enforced process
   boundary. Choose daemon transport, local-model response interface, supported
   execution platforms, and TUI library. Preserve the single daemon policy authority.
2. [x] **Headless runner:** Add the independent binary and programmatic daemon client,
   task journal, Plan/Auto transitions, context assembly, validated actions,
   mediated editing/commands, verification, cancellation, and resume.
3. [x] **Capture and review:** Extend deliberate typed proposals where necessary;
   persist evidence and links, support accept/reject/no-change review, and enforce
   completion against actual daemon lifecycle and validation results.
4. [x] **Thin TUI:** Expose the same runner operations with plans, evidence, progress,
   diffs, pending reviews, and explicit completion state. No second controller.
5. [x] **Acceptance:** Drive a model fixture that never calls memory tools or volunteers
   capture through the real loop. Test first-edit/new-target recall, stale evidence,
   Plan/shell write prevention, policy denial, malformed actions, required-check
   failures, pending/rejected/accepted knowledge, daemon outages, and crash/resume
   around writes. Run daemon compatibility regressions and a local-model smoke.

Implemented seams: `/api/v1/harness/context` composes graph recall, dossiers, and
the existing typed policy verdicts. `/harness/capture` and `/harness/review` use
the daemon's graph lifecycle primitives for all six capture kinds; `/harness/checkpoint`
validates and durably exports the graph. The existing `/api/v1/capture` remains
telemetry. Policy does not claim to verify arbitrary patch semantics.

Implementation hazards found during read-only review:

- Daemon autospawn uses `current_exe()`; the new binary requires an already-running
  daemon and discovers its HTTP address rather than invoking that path.
- Successful graph writes can precede deferred canonical export. Completion now
  requires an explicit durable checkpoint; serialized exports prevent older snapshots
  from overwriting newer ones.
- Capture operations now journal stable record/link identities before graph writes.
  Uncertain failures retry the same operation; definitively unpersisted invalid
  proposals can be reassessed with bounded retries.
- Commands use macOS `sandbox-exec` or Linux `bubblewrap` plus socket confinement.
  Source remains read-only and only per-command scratch is writable; unsupported
  confinement fails closed. Edits use a separate path-safe, compare-and-swap operation.

## Verification results (2026-09-06)

- `cargo build --features harness --bins`: both daemon and harness build.
- `cargo check --no-default-features --lib`: daemon builds without harness/TUI dependencies.
- Full library suite: 353 passed, with three confinement tests initially ignored.
  The subsequently added command-commit recovery unit test also passed.
- All four current executor OS tests explicitly passed on macOS, including
  source/graph/network/socket denial, scratch writes, real Rust compilation and
  tests, and descendant termination on cancellation/timeout.
- Harness daemon integration: 9 passed. Scripted runner integration: 3 passed.
  CLI parser: 2 passed; built binary help output checked.
- Real confined-check runner integration explicitly passed: successful verification
  still requires the human no-change review before reaching Complete.
- Live local-model smoke explicitly passed against `google/gemma-4-26b-a4b-qat`
  at LM Studio on port 1234. With a fixture daemon it reached enforced plan/capture
  review with source unchanged. This verifies model interoperability, not a
  coding-quality benchmark or a full real-daemon/model task evaluation.
- Existing HTTP API (40), canonical-text lifecycle (1), grounded capture (8),
  policy (11), proposal (13), and supersession (15) regressions passed.
- Project graph validation: 20 shapes, zero violations; 176 existing nonblocking
  link advisories. Linux confinement is implemented but was not exercised on this Mac.

## Recall performed

`get_relevant_context(limit: 100)` without a topic, followed by exact SPARQL over
the project graph for relevant typed record titles/descriptions:

```sparql
SELECT ?record ?kind ?title ?description WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    ?record a ?kind ; <http://www.w3.org/2000/01/rdf-schema#label> ?title .
    FILTER(REGEX(STR(?kind), "(Requirement|Constraint)$"))
    OPTIONAL {
      ?record ?p ?description .
      FILTER(STRENDS(STR(?p), "#hasDescription"))
    }
    FILTER(REGEX(CONCAT(STR(?title), " ", COALESCE(STR(?description), "")),
      "harness|standalone|ratification|plan mode|task state|local operation|policy engine", "i"))
  }
}
ORDER BY ?title
```
