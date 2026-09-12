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
outputs total** per action decision or evidence page. The runner automatically
supplies bounded validation feedback for attempts two and three, then pauses for
human guidance. Retry progress is visible, and the attempt count survives restart,
interruption, and `/continue`. New human guidance permits a fresh repair cycle.
Permission denials and source/knowledge changes still require the applicable
human review; correction never grants approval.

A failed step records `last_error` and a typed `last_error_kind` in the journal:
`model_output` (validation of model output, spends the repair budget),
`controller_invariant` (the runner's own durable state disagreed with itself),
`daemon_rejection` (daemon HTTP 4xx), `service` (daemon 5xx or transport), or
`other`. The class comes from the error type, never from message text, so study
tooling can separate controller faults from model faults. Intent events also
mark `edit_applied` (every applied edit, both policies), `repair_exhausted` (the
purpose whose third candidate failed), and `purpose_missing_rounds_exhausted`
(three consecutive missing-purpose cycles under `change-level-v2`).

## Capture reconciliation

New tasks negotiate capture contract 2 with the daemon. Saved tasks without a
contract version retain the original capture workflow; an incompatible daemon
must be upgraded before starting a new task.

When a proposed record may already exist, the harness presents its claim,
relationships, lifecycle and differences for comparison. Retrieval nominates a
candidate; it does not establish equivalence. The model can recommend reusing the
record unchanged, revising a task-owned pending proposal, or recording distinct
knowledge under a noncolliding title. The daemon resolves identities and preserves
the operation across interruption. Reuse never silently appends new evidence or
rewrites the existing graph record.

Semantic reuse joins the ordinary human review batch. The Knowledge view shows
both claims and the recommendation; governing knowledge still needs review before
dependent edits. Revising a pending proposal pauses behind its original whole-batch
review: rejecting that batch resumes the saved replacement, while accepting it
discards the replacement and reopens the evidence assessment. Another task's pending
records cannot be ratified through this task's review.

When a complete candidate cannot fit the model context, a card may require a human
semantic decision without a model recommendation. The card identifies that source
explicitly; a structural-only study reviewer cannot approve it automatically.
Capture assessments and unresolved reviews remain separate durable obligations,
so an acknowledged evidence page is not repeatedly regenerated and completion
cannot bypass its required review.

When the only reuse candidate exceeds the model's remaining prompt budget, the
runner asks the human to judge it directly. Rejecting that card keeps the
observation unresolved and moves on; it never restarts reconciliation, and a
candidate the human has already rejected is never re-presented as an oversized
card. Model-judged reuse rejections still restart reconciliation with the
rejected candidate excluded.

## Request usage accounting

Task journals expose `token_usage`, with one current receipt per explicit client
HTTP attempt (internal HTTP redirects are outside this boundary). Receipts
identify the model and sanitized endpoint, request purpose,
repair decision and candidate, status, elapsed time, and provider-reported usage.
Actions, capture assessments, and compatibility probes are separate purposes;
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

Capture prompts supply evidence references and typed choices for existing graph
targets. The runner resolves those choices into exact journal evidence and graph
IRIs before daemon submission. Existing knowledge is not contemporaneous evidence,
and selecting a reference does not establish that a claim is true: proposals still
require human review. A daemon without the typed-choice capability must be upgraded.

Every edit crosses the execution boundary. Capture assessments
continue during work and accumulate for human review; proposals remain outside
policy authority until ratified. Completion requires the approved checks to pass,
human acceptance/rejection of outstanding capture operations (or consolidated
no-change confirmation), graph persistence, and validation. Ratification can
invalidate a prior plan approval and require renewed approval and verification.
An accepted final proposal can retain verification only when the daemon proves
that the task's non-governing review alone caused the revision change. Changes to
requirements, constraints, lifecycle, or unrelated knowledge retain the approval
gate.
At final review, operations can be accepted in either order. Governing changes
still require renewed plan approval before execution. If review succeeds but the
final checkpoint fails, `/continue` retries completion without repeating capture
or claiming that no knowledge changed.

The experimental `change-level-v2` policy is off by default. Set
`MOOSEDEV_HARNESS_INTENT_POLICY=change-level-v2` before creating a task to enable
incremental purpose selection and reviewed post-edit associations. The model
selects purpose and obligation roles from supplied current records; the harness
retains those choices while it resolves scope and prepares the ordinary plan.
Roles live in the task journal. Human plan approval binds the selected knowledge,
file scope, source proofs and checks. Genuinely missing governing knowledge must
be captured and accepted before the dependent edit.

Purpose responses offer only decisions valid for the current state: a next page
requires a continuation cursor, completion requires a selected purpose, and a
missing-knowledge judgment requires retrieval to have reached its last page.
A missing-purpose checkpoint drains all evidence pages, reviews and pending
revisions before refreshing accepted knowledge and selecting purpose again.
An empty accepted inventory or three consecutive unsuccessful missing-purpose
judgments pauses for human guidance. Resume preserves the checkpoint, and service
errors do not consume the semantic retry budget.

The harness derives each edit's scope from its exact changes and current source
evidence. Successive harness-applied edits can share the approved purpose and file
scope; external source drift or newly implicated governing knowledge requires
review of the affected approval. All applicable Constraints remain in force.
When an edit first touches an unlinked definition, the harness presents its exact
affected scope for human approval under the selected rationale. A pre-existing
graph link is not required. View the selected claims, rationales and affected
definitions in Knowledge, and the proposed source change in Diff. Subsequent
owned edits within that approved scope do not repeat the scope approval.
After editing, the harness discovers candidates from the refreshed source index.
The model recommends associations using supplied choices, and the human reviews
new links through the existing review flow. Existing links are reused. Unsupported
or stale indexing remains explicit unresolved work; broad file candidates retain
their conservative scope. The model does not need to predict a new helper's name
before writing it.

`MOOSEDEV_HARNESS_POSTEDIT_ASSOCIATIONS=1` enables that same post-edit facility with
the `current` policy, which is how the matched study isolates the pre-edit gate.
New `change-level-v2` tasks include the facility. Policy and association contracts
are persisted; changing the environment does not change a resumed task. Older
journals retain their original behavior. See [the evolution spec](../spec/harness_evolution.md)
for the approved contract and evaluation limits.

Post-edit discovery records structural outcomes without inference when the current
index supplies no linkable entity, or a proven entity has no eligible record
choices. Those outcomes do not assert that governing knowledge is absent or that
a record is semantically irrelevant. Stale or unresolved index evidence remains
explicit. Nonempty choices still require semantic selection and ordinary review;
the controller validates supplied handles and bounded retries before proposing links.
Association prompts carry each eligible record's complete claims once. Candidate
identity and source proofs remain separate from those semantic choices.

The earlier experimental policy remains available for existing tasks. Set
`MOOSEDEV_HARNESS_INTENT_POLICY=change-level` before creating a task to require a
plan mapping from current knowledge to affected existing or planned code entities.
`current` retains ordinary plan approval. The policy is persisted with the task;
changing the environment does not change it on resume. Purpose and obligation
roles are task-journal metadata, not new graph predicates. Related helpers can
share existing requirements or decisions rather than creating a record per helper.
Missing purpose must be captured and accepted before the revised plan can execute.

When the post-edit facility is disabled, `MOOSEDEV_HARNESS_ENTITY_LINKS=1` enables
the earlier optional association action. The harness supplies bounded record/entity
choices, resolves identities through the daemon, and submits existing-record links
for review. Acceptance does not prove semantic correctness, and a changed knowledge
revision can require renewed plan approval. The controlled pilot enables this
capability in both arms.
Automatic index refresh currently supports Python projects with an explicit
absolute `MOOSEDEV_SCIP_PYTHON` launcher; it refuses registry/PATH fallback and mixed
producer refreshes. Source must match its indexed evidence before a precise link
can be reviewed. See [the pilot spec](../spec/harness_intent_pilot.md) for the frozen
indexer setup, study limits, and gate-count definitions.

Daemon or model-server outages pause work without consuming the model-repair
budget. Restore the connection, then use `/continue` (headless `step` or `run`) to
retry. Pending capture evidence and any already-submitted operation ID remain
in the journal; a connection failure does not require new model guidance.

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
action schema. Omitted paths are disclosed; `search` examines both paths and
contents throughout the permitted workspace. The configured model ID is supplied
as session metadata so the model can answer identity questions without guessing.
Action observations use bounded previews; `inspect(event,offset)` lets the model
read detailed journal output without repeating a command. Capture independently
processes all checkpoint evidence in bounded pages, with persisted event and byte
positions across interruption. Completing one page does not clear the remaining
capture obligation. The Journal view displays a compact index; complete requests
and observations remain in the task JSON. Unchanged checkpoints skip redundant
file rewrites; changed checkpoints retain atomic publication and fsync.
Plan summaries are limited to 4000 UTF-8 bytes. If required capture context leaves
no room for evidence, increase the configured model window and retry: that
checkpoint's file set is frozen and cannot be narrowed by replanning.

The daemon rejects untrusted browser origins and non-address Host headers across
all HTTP routes, including review and checkpoint. Native local clients and the
same-origin web UI remain supported. A separately hosted development UI needs
its exact origin in the daemon's comma-separated `MOOSEDEV_ALLOWED_ORIGINS`
environment variable (for example `http://localhost:5173`). This browser boundary
does not authenticate other programs already running with the user's privileges.
`GET /api/v1/harness/checkpoint` is read-only status and returns `durable: false`;
clients use `POST` to validate and publish a durable checkpoint. This also keeps
legacy browsers without Fetch Metadata from triggering writes through GET.

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
Headless tasks require one no-change confirmation per checkpoint, after all its
evidence pages have been assessed. They retain individual proposal reviews;
opening a task in the TUI
enables conversational batching while preserving its outstanding obligations.
`approve-policy`, `review ID reject`, `plan`, `cancel`, `resume`, and `answer ID TEXT`
retain their task semantics. Headless `resume ID` resumes a task; interactive
`resume-session ID` resumes a conversation. `--help` lists all commands. Options
precede the command. Errors produce JSON on stderr and a nonzero exit status.
