# MOOSEDev harness

The optional harness is an interactive coding agent for a local model server such
as LM Studio. The daemon owns project memory and policy; the runner enforces
reading, capture, review, and execution gates without asking the model to call MCP
tools. Claude, Codex, and other clients can share the existing daemon.

Build both executables, start your local model server, and launch the conversation:

```sh
cargo build --features harness --bins
target/debug/moosedev-harness
```

Use `--project DIR` to select another project. Launching from a subdirectory uses
the repository root. The full-screen interface opens with a persistent composer,
a chronological transcript, and visible activity. Enter submits; Alt-Enter adds a
newline. Multiline paste is preserved. Escape interrupts active work. Follow-ups
submitted during work queue before the next action; steering an approved task
returns it to planning before further edits.

## Startup and model selection

Interactive startup connects to the project's daemon, checking its project root,
data directory, and harness API. When no daemon is running, it starts the actual
`moosedev` executable, using `--daemon-exe PATH`, an installed sibling, or `PATH`.
It leaves that shared daemon running when the interface exits. A running daemon
with an unavailable or incompatible API must be restarted by the user; startup
does not replace it. `MOOSEDEV_NO_AUTOSPAWN` disables automatic startup.

`--daemon URL` selects a loopback daemon explicitly. The usual discovery address
is `.moosedev/http.addr`; an explicit `MOOSEDEV_DATA_DIR` selects another store.
If the project needs initialization, the interface offers `/init`. This invokes
normal `moosedev init` and can create project configuration, memory directories,
and managed instructions; initialization happens only after that human command.
It does not bootstrap project knowledge or download models.

The default model endpoint is `http://127.0.0.1:1234/v1`. Without a configured or
remembered model, startup discovers the server's model IDs and selects the model
automatically if exactly one is advertised. Otherwise it presents a numbered
list. `/model` refreshes that list; `/model NUMBER` or `/model MODEL_ID` selects one, and
`/model http://HOST:PORT/v1 MODEL_ID` selects an endpoint and model. Non-secret
preferences are remembered in `.moosedev/harness/provider.json`. Explicit
`MOOSEDEV_LLM_BASE_URL` and `MOOSEDEV_LLM_MODEL` environment variables or project
`.env` values take precedence at launch. Model selection affects the harness's
coding session; it does not reconfigure a shared daemon.

`MOOSEDEV_LLM_API_KEY` provides authentication;
`MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS` declares the model context window.
`MOOSEDEV_LLM_STRUCTURED_OUTPUT=auto` uses native structured output when available
and validated JSON fallback otherwise. `required` rejects providers without
native structured output. Complete model actions are validated before execution;
streamed partial text never authorizes an edit or command.
Before the first task generation, the harness verifies both streaming and
nonstreaming structured responses with neutral connection probes. The harness-only
`MOOSEDEV_HARNESS_RESPONSE_POLICY` setting accepts `auto` (default),
`provider-default`, or `reasoning-off`. Auto first tests the provider default; if a
completed response contains only reasoning, it tests `reasoning_effort: "none"`.
The resolved mode must pass both content paths and is recorded in the session.
This setting fixes the observed Qwen MLX response routing on LM Studio; it is not
assumed to work on every provider. Reasoning text never becomes an executable
action. A model/endpoint/settings change invalidates the compatibility cache.

Invalid JSON and repairable action/capture arguments share **three candidate
outputs total** per action decision or capture note. The runner automatically
supplies bounded validation feedback for attempts two and three, then pauses for
human guidance. Retry progress is visible, and the attempt count survives restart,
interruption, and `/continue`. New human guidance permits a fresh repair cycle.
Permission denials and source/knowledge changes still require the applicable
human review; correction never grants approval.

A failed step records `last_error` and a typed `last_error_kind` in the journal:
`model_output` (validation of model output, spends the repair budget),
`daemon_rejection` (daemon HTTP 4xx), `service` (daemon 5xx or transport), or
`other`. The class comes from the error type, never from message text, so study
tooling can separate daemon and transport faults from model faults. Intent
events also mark `edit_applied` (every applied edit) and `repair_exhausted` (the
purpose whose third candidate failed).

## Request usage accounting

Task journals expose `token_usage`, with one current receipt per explicit client
HTTP attempt (internal HTTP redirects are outside this boundary). Receipts
identify the model and sanitized endpoint, request purpose,
repair decision and candidate, status, elapsed time, and provider-reported usage.
Actions, capture notes, and compatibility probes are separate purposes;
schema or optional streaming-usage fallbacks have separate request IDs. Raw usage
is retained alongside nullable token fields. Missing counts stay unknown, and
cache/reasoning detail fields must not be added to input/output totals blindly.

The client requests final streaming usage. A provider that explicitly rejects the
optional usage parameter can be retried without it; this attempt is also recorded.
An absent or interrupted usage report remains an accounting gap. These receipts
measure provider requests, not whether the resulting action or code is correct.

An adjacent `<task-id>.usage.jsonl` file saves started and terminal receipts under
the task's existing lease. Replay merges receipts by request ID. A crash that
loses a terminal receipt leaves an outstanding request with unknown consumption;
old tasks without accounting carry a legacy gap. Persistence errors are recorded
as accounting gaps and surfaced in progress. Usage files are private task data
and are excluded from project-knowledge capture prompts.

The study runner additionally observes agent and daemon/helper proxy traffic,
keeps native-client and proxy measurements separate, and reports observation
coverage. It publishes a field's full total only when every request in that
measurement scope has a final observed value. Native harness journals alone do
not meter separate daemon processes, embeddings, hardware energy, or hosted
billing. Compare resource use alongside task completion and knowledge quality;
short failed runs do not demonstrate better efficiency.

## Conversation and review

Ask questions, discuss code, or describe a change directly in the composer.
Read-only questions can receive answers without a modification plan. Coding work
starts in Plan mode; inspect its file scope and required checks before approving
Auto execution. The runner advances automatically until it needs human input or
reaches a completion gate. Plans, edits, checks, and knowledge proposals appear
in the conversation; exact requests remain available in the task journal.

Use `/approve` to approve the displayed plan or exact policy-gated edit. `/review`
opens outstanding knowledge; `/accept NUMBER` or `/reject NUMBER` reviews an
operation, and omitting the number reviews all displayed operations.
`/no-knowledge` confirms a consolidated no-change assessment. Tab switches views;
Page Up/Down scroll. `/help` lists the controls.

`/plan` returns to planning, `/continue` resumes interrupted work, and `/new`
begins a conversation. `/resume` lists saved conversations; `/resume ID` opens one.
`/connect` retries a failed daemon connection. `/quit` exits while preserving
unfinished work. The CLI can reopen one with `moosedev-harness resume-session ID`.

The runner retrieves project knowledge before planning and affected-file dossiers
before edits. The model can request one unique literal replacement or supply
whole new file content. The runner constructs the edit precondition from the exact
source delivered for that request; ambiguous replacements are rejected. Concurrent
file changes still invalidate approval or fail the executor's exact comparison.
Deletion continues to require human approval. Legacy whole-file edit requests
remain readable with their original strict semantics.

At the final checkpoint the model answers one plain question about what it
learned; the daemon types that note into proposals (see "How the harness
decides"). Existing knowledge is not contemporaneous evidence, and a typed
proposal does not establish that a claim is true: proposals still require human
review. A daemon that does not advertise capture contract 3 and intent contract 2
must be upgraded before a new task is created.

Every edit crosses the execution boundary. Intermediate checkpoints only
journal; the final checkpoint produces the proposals for human review, and
proposals remain outside policy authority until ratified. Completion requires the approved checks to pass,
human acceptance/rejection of outstanding capture operations (or consolidated
no-change confirmation), graph persistence, and validation. Ratification can
invalidate a prior plan approval and require renewed approval and verification.
The task's own accepted final note completes without renewed approval or
re-verification when the daemon proves that its review alone caused the revision
change, counting the code entities and record-entity edges its own links write;
this holds for requirements and constraints too (`final_review_attested`). The
runner asks only after every plan check passed with nothing else pending, and
refreshes first so it never sends a stale expected revision. A supersession or
retraction, a capture that is no longer the final checkpoint (for example after
steering), an unrelated or concurrent knowledge change, or a source change
retains the approval gate.
At final review, operations can be accepted in either order. Governing changes
outside that attested final note still require renewed plan approval before
execution. If review succeeds but the
final checkpoint fails, `/continue` retries completion without repeating capture
or claiming that no knowledge changed.

Automatic index refresh currently supports Python projects with an explicit
absolute `MOOSEDEV_SCIP_PYTHON` launcher; it refuses registry/PATH fallback and mixed
producer refreshes. Source must match its indexed evidence before a derived
association can be reviewed.

Daemon or model-server outages pause work without consuming the model-repair
budget. Restore the connection, then use `/continue` (headless `step` or `run`) to
retry. The pending note and any already-submitted operation ID remain in the
journal; a connection failure does not require new model guidance.

Commands run in a filtered, read-only copy of project source with separate
writable scratch space. Use project-relative paths. The live project, unrelated
home files, protected configuration, symlinks and hardlinks are excluded;
installed runtime/toolchain directories and specific package caches are trusted
read-only inputs. Confinement uses `sandbox-exec` on macOS and requires `bubblewrap` on
Linux (x86-64 or ARM64); unsupported platforms cannot execute commands. Commands
have no network, a clean environment, and bounded output. The default command
timeout is 900 seconds; human configuration `MOOSEDEV_COMMAND_TIMEOUT_SECONDS`
can set it to 1–86400 seconds, including through the project's `.env`.
Dependencies must be available in that view. Sibling path dependencies, including
this repository's `../moose`, are not automatically exposed. Source snapshots
fail explicitly above 512 MiB total, 100,000 entries, or 64 directory levels. Source
edits use a separately gated action; policy-gated edits require human approval.

Source snapshots use a stable task path and preserve file timestamps; each command
refreshes them from the live project. Build artifacts persist in a task-local
cache across commands and normal journal reloads. On macOS each command receives
a fresh backing directory, seeded with distinct file inodes using copy-on-write
clones where supported. Harness-managed aliases keep Cargo's build and registry
paths stable; sandbox writes are granted only to that command's backing paths.
A detached process can outlive the command, but cannot write into a later
command's backing through those aliases or its old file descriptors. Linux uses
its PID namespace and retains the existing build directory. Failed cache copying
falls back to a cold cache; ordinary copying on filesystems without clone support
adds startup work and is bounded.

After the command leader exits, output draining has a 500 ms grace period. If a
child keeps a pipe open, the result retains captured output and reports an
incomplete command instead of waiting for the full command timeout. That result
does not pass a required check. macOS commands cannot set immutable file flags;
cleanup repairs user-set flags only on safely identified owned entries.

Temporary command homes and
files are removed on success, failure, timeout, or interruption. Completion and
cancellation remove the task's source and build scratch; cancelled tasks retain
their journal and can resume with a cold cache. If cleanup fails, cancellation
still takes effect and the task journal records `cleanup_pending` with the cause.
Press Esc again (headless `cancel`) to retry cleanup while staying cancelled, or
use `/continue` (headless `resume`) to retry cleanup before resuming work. Pending
cleanup survives restart and cannot be bypassed by replanning. Managed scratch
parents must be real directories. Scratch reuse and cleanup never follow
filesystem aliases left by commands. Root-level `build`, `dist`, and
`target` are excluded; nested source directories with those names are included.
Directories with a valid [CACHEDIR.TAG](https://bford.info/cachedir/) are excluded
at any depth from navigation and command snapshots. The marker must be an ordinary,
unaliased file with the exact standard signature; invalid markers do not hide source.
Sensitive names such as `.env`, `.git`, `.moosedev`, and credential directories
remain excluded at every depth. Oversized source snapshots fail explicitly rather
than silently omit input files.

Conversation journals and task journals are local operational state under
`.moosedev/harness`, separate from canonical `.moosedev/kg.nq`. They preserve
messages, queued input, exact model requests/responses, execution evidence, and
pending obligations across interruption or restart. Do not edit journals to
bypass gates. Context is bounded; governing knowledge is never silently dropped
to fit the model window.

Repository navigation previews are byte-bounded (up to 8 KB), and optional
conversation history uses only space remaining after current evidence and the
action schema. Omitted paths are disclosed; `search` first returns the accepted
project knowledge matching the query, then examines both paths and contents
throughout the permitted workspace. The configured model ID is supplied
as session metadata so the model can answer identity questions without guessing.
Action observations use bounded previews; `inspect(event,offset)` lets the model
read detailed journal output without repeating a command. The final checkpoint
consumes the whole journal since the last checkpoint in one note; the checkpoint
position persists across interruption. The Journal view displays a compact
index; complete requests and observations remain in the task JSON. Unchanged checkpoints skip redundant
file rewrites; changed checkpoints retain atomic publication and fsync.
Plan summaries are limited to 4000 UTF-8 bytes. If the required context leaves
no room for the note question, increase the configured model window and retry.

The daemon rejects untrusted browser origins and non-address Host headers across
all HTTP routes, including review and checkpoint. Native local clients and the
same-origin web UI remain supported. A separately hosted development UI needs
its exact origin in the daemon's comma-separated `MOOSEDEV_ALLOWED_ORIGINS`
environment variable (for example `http://localhost:5173`). This browser boundary
does not authenticate other programs already running with the user's privileges.
`GET /api/v1/harness/checkpoint` is read-only status and returns `durable: false`;
clients use `POST` to validate and publish a durable checkpoint. This also keeps
legacy browsers without Fetch Metadata from triggering writes through GET.

## How the harness decides

The coding model answers two questions and nothing else: `harness_action` while
working and one plain-prose `harness_capture_note` at the final checkpoint. Every
other decision is derived by the daemon from the approved plan, the diff and the
graph, and journaled as an intent event; none is asked of the model. Task
journals are schema 2. A journal written by an earlier build is refused with
"start a new task", and a new task requires a daemon advertising capture
contract 3 and intent contract 2.

- Obligations. `/approve` derives each plan file's obligations from the direct
  dossier records of its resolved definitions and takes the plan summary as the
  purpose (`obligations_derived`). One human approval; no purpose review. A file
  without governing records is ungoverned, which is journaled, not parked.
- Knowledge. Accepted knowledge, entity dossiers and knowledge returned by
  `search` are the project's authoritative answers: the model is told to act on
  them instead of re-deriving or confirming them from source, and a divergence
  between source and an accepted record is a code defect unless a record chose
  that behaviour. `search(query)` asks the daemon for the query's accepted
  records (an evidence-only context request: complete claims, no inventory or
  dossiers) and returns them before repository matches (`knowledge_search`).
- Linked evidence. Each step's context leads with the governing records a
  deterministic walk reaches from the attached files' code: accepted
  Constraints on the components that code belongs to (by `realizes`, declared
  paths, or the components its linked records concern or constrain), the
  records those linked records are motivated by, the current head of any chain
  superseding them, and the Lessons learned from them. Each record renders its
  header, a `via:` line naming how it was reached, and its complete claim.
  Records the file dossiers already print are left out, and a record reached
  twice is shown once. Every accepted Constraint is listed, the first 24 with
  claims; other kinds stop at eight motivating records, eight supersession
  heads and six Lessons, with one line counting what was left out. Topic
  recall (limit 5, dossier records excluded) is used only when the walk finds
  nothing, under a "Topic evidence (fallback" header.
- Dossiers. A file dossier lists each knowledge-bearing entity's direct records
  with their complete claims, rendered like linked evidence (superseded records
  show only their header line), and its component's records by title: accepted
  Constraints always, other kinds up to twelve, then a count. Each claim and
  each component list appears once per file dossier. The harness requests
  dossiers without a byte bound; context that exceeds the prompt budget fails
  instead of being truncated.
- Scope. An edit outside the plan files is discarded and the task re-enters Plan
  mode naming the file (`scope_escape_replan`, three per task; the fourth parks
  for guidance as `scope_escape_exhausted`). The first no-op edit runs the
  required checks instead of consuming the repair budget
  (`noop_edit_continuation`). A model replan with no edit, command, required
  check result or human answer since approval continues the approved plan
  instead of reopening planning (`replan_continuation`, unbounded); a replan
  while already planning changes nothing (`replan_noop`). A real replan keeps
  the files already read (`model_replan`).
- Checks. Plan checks run verbatim through `/bin/sh`, so each must start with
  an installed program, a shell builtin or a project file. A description in
  place of a command is rejected before plan approval and costs a repair
  attempt (`plan_check_rejected`). A check the shell cannot start at run time
  (exit 126 or 127) is reported as an invalid check, not a failed test
  (`check_unrunnable`). A replan after any check result is a real replan,
  since replanning is how checks change.
- Associations. After `finish`, `POST /api/v1/harness/intent/associate` binds the
  changed definitions to their governing records with the predicate the ontology
  allows (`association_derived`, `association_none`, `association_skipped`). The
  runner sends one range per changed line hunk, in changed and original
  coordinates; past 64 hunks or 2000 changed lines a file's change coalesces to
  one prefix/suffix range per side (`ranges_coalesced`). Each hunk contributes
  its leaf definitions, those it touches that enclose no other kept definition it
  touches, so a method wins over its class. Parameters, type members, locals and
  test paths are skipped; a module-level constant or table is kept. A producer
  that leaves kinds unspecified (scip-python) is read by the SCIP symbol grammar.
  Bindings
  enter the ordinary link review as `DerivedAssociation` cards; an unresolved or
  stale index is journaled (`unresolved_binding`), never parked. Steering during
  that review discards the pending bindings; they are re-derived at the next
  `finish`.
- Capture. Intermediate checkpoints only journal (`capture_deferred`). At the
  final checkpoint the note is journaled (`capture_note`) and
  `POST /api/v1/harness/capture/type` types it: a symbolic decision for the
  change, a symbolic lesson for a check that failed then passed, and, when the
  daemon has an LLM sensor, bounded sensor proposals. Each proposal is scored
  against same-kind accepted records (title 0.5, rank 0.3, overlap 0.2) and
  receives a durable receipt: `restates` (receipt only, no record), `refines`
  (proposal plus a confidence-annotated edge written at capture) or distinct
  (plain proposal). Thresholds are frozen defaults (`MOOSEDEV_RECONCILE_RESTATES`
  0.80, `MOOSEDEV_RECONCILE_REFINES` 0.55,
  `MOOSEDEV_RECONCILE_REFINES_CONTAINMENT` 0.60,
  `MOOSEDEV_RECONCILE_TIEBREAK_BAND` 0.08), overridable only by environment and
  recorded in every receipt. A title collision or daemon rejection retypes the
  same note under fresh operation IDs without a model call (`capture_retyped`,
  three per note, then `capture_retype_exhausted`); a source or knowledge change
  between typing and capture does the same (`capture_note_invalidated`).
- Capture links. Each proposal links to the leaf definitions the hunks of its
  files touch, proven against the loaded index: the changed ranges when the
  index holds the changed source, or the original ranges when it holds the
  original (Rust's index is not refreshed after an edit), keeping only
  definitions whose name no hunk touches. At most eight definitions per file and
  sixteen per proposal, in file, range and symbol order. A file without a
  definition anchor links its module instead (the synthetic whole-file module
  for Rust), and so does a file whose anchors were capped (`anchor_overflow`) or
  whose leaves share one span (`anchor_ambiguous`); an index that proves
  neither side is noted (`index_unproven`). The runner journals the counts once
  per capture (`capture_anchored`). A restated note (a `restates` receipt) links
  its existing record to the same anchors, skipping definitions the record
  already reaches or awaits review for, so a note that only restates still
  submits a capture. That capture is reviewed and attested like any other; the
  attestation excludes exactly the edges its acceptance writes onto the existing
  record.

Human review remains only where a new record or a new code link is written.
Read-only conversations never reach the final checkpoint and therefore capture
nothing: steering text is journaled and surfaces in the final note of the next
completed plan.

## Headless compatibility

Existing headless commands remain available, return JSON, and require a running
daemon. `tui ID` opens an existing task in the conversational interface:

```sh
moosedev-harness new 'Fix the parser regression and verify the result'
moosedev-harness status TASK_ID
moosedev-harness run TASK_ID
moosedev-harness approve TASK_ID
moosedev-harness review TASK_ID accept
moosedev-harness no-knowledge TASK_ID
moosedev-harness tui TASK_ID
```

`step` advances once; `run` advances at most 32 steps and stops at human gates.
Headless tasks require one no-change confirmation at the final checkpoint when
the typed note proposes nothing. They retain individual proposal reviews;
opening a task in the TUI enables conversational batching while preserving its
outstanding obligations.
`approve-policy`, `review ID reject`, `plan`, `cancel`, `resume`, and `answer ID TEXT`
retain their task semantics. Headless `resume ID` resumes a task; interactive
`resume-session ID` resumes a conversation. `--help` lists all commands. Options
precede the command. Errors produce JSON on stderr and a nonzero exit status.
