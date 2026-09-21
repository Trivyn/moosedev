# MOOSEDev Harness — Initial Spec

**Version:** 0.2 · **Date:** 2026-09-06
**Status:** Conversational implementation of the accepted harness requirements.
The accepted graph records linked below govern this spec if they disagree.
Record extraction and implementation checklist: [harness implementation](../tasks/harness-implementation.md).
Conversational decision and verification: [interactive implementation](../tasks/harness-interactive.md).

## Purpose and architecture

Build a standalone, local-first coding agent that makes project-memory reading and
knowledge capture mandatory parts of execution. It supports general coding work:
repository exploration, bug fixes, features, refactoring, and verification.

Retain the existing daemon and its MCP, HTTP, and LSP interfaces. Existing coding
agents and the harness share the same project knowledge. The implementation has
three parts:

- **Daemon:** Owns the graph, code index, retrieval, symbolic reasoning, policy,
  and knowledge ratification.
- **Headless runner:** Maintains task state and executes the workflow, invoking
  the daemon and model programmatically.
- **Standalone TUI:** Presents tasks, plans, progress, evidence, diffs, approvals,
  and captured knowledge.

The runner and TUI may ship as one additional binary, but execution must work
without the TUI. Reuse existing MOOSE capabilities; centralize policy and
knowledge interpretation in the daemon. OpenCode integration is outside scope.

## Conversational interface

Launching `moosedev-harness` opens a full-screen conversation with a persistent
multiline composer. Users ask questions, request changes, and continue with
follow-ups; no task ID or manual stepping is required. Stream assistant prose
and command output, with readable plans, diffs, and knowledge-review cards.
User messages queue durably before the next action; explicit interruption stops
current work and preserves recovery obligations. A conversational reply does not
declare a coding task complete or require a fictional edit plan.

Connect to the verified project daemon or start the actual daemon executable.
Offer initialization and discover local models, preserving explicit configuration.
With no selected model, use a sole discovered model or present a numbered choice.
Never automatically download models or start the model server. Conversation
journals retain full transcripts and linked tasks outside the canonical graph;
bounded prompt context does not replace queryable project memory.

## Operating modes

**Plan is the default.** Explore the repository and graph, clarify requirements,
and prepare a reviewable plan. Read-only inspections are allowed; project code
cannot be modified. Planning decisions enter capture as proposals.

**Auto executes an approved plan.** Routine exploration, edits, and checks proceed
within user permissions and daemon policy. Pause for required ratification,
unresolved conflicts, destructive actions, or actions outside the authorized
workspace. Material changes to the approved objective or approach return to planning.

Mode changes preserve the plan, retrieved evidence, task state, and capture
obligations. Auto does not disable reading, capture, or completion gates.

## Execution and enforced reading

The runner owns navigation, context assembly, action dispatch, verification, and
completion. The model supplies interpretations, proposed actions, and code; it
need not initiate MCP calls or remember the memory workflow.

Before planning, retrieve current project requirements, constraints, decisions,
and lessons. Before proposing target changes, resolve affected code and retrieve
its entity and component knowledge. Newly discovered targets trigger retrieval
before editing.

Each model request receives bounded context: objective, relevant source, governing
knowledge, and applicable diagnostics. Record the source and knowledge versions
supplied. Changes to scope or governing evidence invalidate affected approvals
and require refreshed context.

All model-requested edits and commands pass through the runner's execution
boundary. Shell execution cannot bypass workspace permissions or edit policy.
Validate model responses before execution; bound malformed-action repair attempts.
Commands read a filtered source snapshot and allowlisted installed tools/caches;
unrelated host files and filesystem aliases are inaccessible. Browser-origin and
Host checks protect HTTP ratification and other project mutations.

In Auto mode, the model may request the minimum additional filesystem or network
access required by an exact command. The runner must durably pause before that
command, display its justification and normalized read/write/network capabilities,
and require explicit human approval or denial. Approval applies only to the
current task, is inherited by its later commands and required checks, survives
restart/resume, remains auditable and revocable, and expires on completion.
Pending requests do not survive replanning or changed evidence as executable
authority. The sandbox must reject grants that expose the live workspace, task
scratch, or another route around gated edits; it must not offer full-host,
ambient credential/environment-secret, GUI, or arbitrary privilege grants.

Retrieval and delivery are enforceable. Assess understanding through observable
results, tests, and checkable constraints; model acknowledgment is not proof.

## Enforced capture and completion

Capture at decision checkpoints during work and at final review. Preserve
contemporaneous evidence: user instructions, explicit choices, relevant
conversation, source changes, and verification results.

Reconcile evidence with existing knowledge and prepare warranted typed proposals:
ArchitecturalDecision, Requirement, Constraint, Lesson, Pattern, or AntiPattern.
Include supporting evidence and appropriate component/entity links. Changed
knowledge uses explicit supersession or retraction rather than silent duplication.

A checkpoint need not produce a new record. Accumulate proposals and no-change
assessments during work for consolidated human review before completion. Pause
earlier for new governing requirements, constraints, or knowledge replacements.
Do not manufacture rationale to satisfy a capture quota.
Page full journal evidence with durable event/byte cursors; bounded action
previews must not discard capture obligations or force command re-execution.

Use the daemon's deliberate proposal and ratification machinery, extending it for
the required record kinds. Activity journaling alone does not satisfy capture.
Proposals remain distinct from accepted knowledge and cannot independently
authorize actions.

**Completion requires all of the following:**

- The requested outcome is met and required checks pass.
- The user has accepted or rejected knowledge proposals, or confirmed that no
  durable knowledge changed.
- Resulting graph writes and links are persisted and graph validation passes.

Distinguish “execution finished—knowledge review pending” from “complete.”

## Persistence, interfaces, and acceptance

Maintain a durable operational task journal outside the canonical graph: mode,
plan, action outcomes, evidence references, approvals, and unresolved obligations.
Interruption or cancellation preserves state. Resume refreshes evidence and avoids
replaying completed writes or duplicating proposals. Required daemon failures
block dependent work.

Use programmatic daemon calls and a configurable local model endpoint. Headless
clients and the TUI share task operations: start, inspect, approve, switch mode,
cancel, and resume.

Models are configurable per role from one local, untracked file in the project
root. Planning and implementation may use different models, each with its own
endpoint, context window, output contract and timeouts; the role follows the task
mode, and a role without its own settings inherits the default. The file holds no
project knowledge and no credential. A variable set in the real environment
overrides it; the project `.env` does not. Every model request journals the role
and resolved settings that answered it.

Acceptance tests must demonstrate:

- A model that never requests memory or volunteers capture still receives
  required knowledge and reaches mandatory capture review.
- Plan cannot edit code; Auto respects policy across patches and shell commands.
- Scope changes trigger fresh retrieval; superseded knowledge cannot govern new actions.
- Unreviewed capture, failed persistence, or failed checks prevent completion.
- Resume loses no obligations and repeats neither edits nor knowledge writes.
- Existing daemon clients continue working against the same graph.

Build and operating instructions: [Harness guide](../docs/harness.md).
The initial implementation uses Ratatui and programmatic daemon HTTP endpoints.
