# Conversational harness implementation

The accepted existing harness requirements govern this implementation; this is
a progress mirror, not a second source of project knowledge.

User choices: full-screen conversation, queued messages at action boundaries,
integrated startup, batched human capture review, existing offline sandbox.

- [x] Recall existing requirements; inspect runner, TUI, provider and startup seams.
- [x] Record correction Lesson and supersede the implementation decision.
- [x] Extend runner with conversational input, live events and batched capture.
- [x] Add streaming model/command output without executing partial actions.
- [x] Add durable sessions, responsive conversation TUI and integrated startup.
- [x] Verify scripted gates/recovery, terminal interaction and real local-model task.
- [x] Update short spec and usage documentation; validate graph and final diff.

Lesson: [Harness TUI means a conversational coding-agent interface](https://moosedev.dev/kg/Lesson/57cc867d-9b9b-4efe-ab4c-bbf1a0229753).
Decision: [Conversational harness with daemon-backed task gates](https://moosedev.dev/kg/ArchitecturalDecision/30b7dcc9-4c3c-4c02-b046-75974c825ee4),
superseding `2c3a1def-3756-4a1d-9cd6-1c25c44e0e66` with its motivating requirement
and constraint links explicitly retained.

## Populated-project context regression

- [x] Reproduce reported context overflow; recall governing records and inspect dossiers.
- [x] Bound optional navigation/history by bytes and reserve action-schema space;
  retain complete governing evidence and expose configured model identity.
- [x] Test a large repository and populated graph, including both reported prompts.
- [x] Rebuild the harness, validate the graph, and document the fix.

Lesson: https://moosedev.dev/kg/Lesson/6db3740a-ce02-49bb-9602-9bd617d79f26

The uncapped first-2,000-path preview caused the immediate failure when combined
with 40–48 KB of daemon recall. Action assembly now reserves complete required
evidence and schema first, then spends remaining bytes on up to 12 KB of recent
conversation and an 8 KB navigation preview. Search covers path names and file
contents beyond the old 2,000-file cutoff. The prompt includes the configured
model ID. No daemon restart or context-window increase is required for this fix.

Validation: 14 runner integration tests, four controller integration tests, and
four runner unit tests passed. New cases cover ~49 KB of complete knowledge plus
~100 KB of paths, both reported prompts, oversized required evidence rejection,
Unicode budget boundaries, and discovery beyond the preview. The rebuilt TUI
also answered both prompts against this populated MOOSEDev project using LM
Studio's Gemma: `Hello` used a 51,912-byte action request and the identity question
used 54,947 bytes, including schemas; both mandatory capture assessments finished
without proposals or errors. The answer identified `google/gemma-4-26b-a4b-qat`.
Terminal restoration passed. Graph validation remains zero violations.

## Initial implementation verification (2026-09-06)

- Harness library: 29 passed; LLM library: 10 passed; runtime: 8 passed;
  CLI: 3 passed. Runner/session/daemon integration: 11/4/9 passed.
- Explicit macOS confinement: all four executor tests and the confined-command
  completion integration test passed outside the nested test sandbox. They cover
  source/graph/network denial, writable scratch, Rust checks, timeout cleanup,
  cancellation cleanup, and the final human-review gate. Linux was not exercised.
- `cargo build --features harness --bins`,
  `cargo check --features harness --all-targets`,
  `cargo check --no-default-features --lib`, formatting and diff checks passed.
- Graph validation: 20 shapes, zero violations; 177 non-blocking link advisories.
- Real full-screen PTY test used LM Studio at `127.0.0.1:1234/v1`, serving
  `google/gemma-4-26b-a4b-qat`, and an isolated temporary project. The TUI initialized
  the project, started its daemon, discovered/selected the model, obtained plan
  approval and governing Requirement ratification, changed `hello` to `welcome`
  in `code.txt`, ran `/usr/bin/grep -qx welcome code.txt`, and withheld completion
  until human no-change confirmation. The task then reached `Complete` with a
  durable graph checkpoint. Restart/resume retained the conversation and edit.
- A follow-up in that same conversation read `code.txt` and answered
  `code.txt currently contains 'welcome'.` using `reply`, with no plan or edits.
  Quitting restored the terminal. Test journals remain in
  `/private/tmp/moosedev-interactive-smoke-6ce41221`; conversation
  `232d154f-337c-4cfc-b4aa-73dae8a8cd8a` links both tasks. The isolated test daemon
  was stopped after verification.

The live test exposed and drove fixes for an empty startup context query,
redundant approval after knowledge ratification, frozen conversational context,
and historical intentions overshadowing completed work. Interactive action
schemas now exclude actions invalid for the current mode, and prompts end with
current source and deterministic execution evidence. Earlier model attempts to
replace an approved plan or replay an edit were blocked. This is a focused local
smoke test, not a model-quality benchmark or a claim of broad coding-agent parity.
