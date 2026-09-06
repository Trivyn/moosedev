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
before edits. Every edit crosses the execution boundary. Capture assessments
continue during work and accumulate for human review; proposals remain outside
policy authority until ratified. Completion requires the approved checks to pass,
human acceptance/rejection of outstanding capture operations (or consolidated
no-change confirmation), graph persistence, and validation. Ratification can
invalidate a prior plan approval and require renewed approval and verification.
An accepted final proposal can retain verification only when the daemon proves
that the task's non-governing review alone caused the revision change. Changes to
requirements, constraints, lifecycle, or unrelated knowledge retain the approval
gate.

Commands run in a filtered, read-only copy of project source with separate
writable scratch space. Use project-relative paths. The live project, unrelated
home files, protected configuration, symlinks and hardlinks are excluded;
installed runtime/toolchain directories and specific package caches are trusted
read-only inputs. Confinement uses `sandbox-exec` on macOS and requires `bubblewrap` on
Linux (x86-64 or ARM64); unsupported platforms cannot execute commands. Commands
have no network, a clean environment, bounded output, and a 120-second limit.
Dependencies must be available in that view. Sibling path dependencies, including
this repository's `../moose`, are not automatically exposed. Source snapshots
fail explicitly above 64 MiB per file, 512 MiB total, or 100,000 entries. Source
edits use a separately gated action; policy-gated edits require human approval.

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

The daemon rejects untrusted browser origins and non-address Host headers across
all HTTP routes, including review and checkpoint. Native local clients and the
same-origin web UI remain supported. A separately hosted development UI needs
its exact origin in the daemon's comma-separated `MOOSEDEV_ALLOWED_ORIGINS`
environment variable (for example `http://localhost:5173`). This browser boundary
does not authenticate other programs already running with the user's privileges.

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
Headless tasks retain individual capture checkpoints; opening a task in the TUI
enables conversational batching while preserving its outstanding obligations.
`approve-policy`, `review ID reject`, `plan`, `cancel`, `resume`, and `answer ID TEXT`
retain their task semantics. Headless `resume ID` resumes a task; interactive
`resume-session ID` resumes a conversation. `--help` lists all commands. Options
precede the command. Errors produce JSON on stderr and a nonzero exit status.
