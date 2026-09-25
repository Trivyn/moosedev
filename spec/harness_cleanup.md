# Harness cleanup — module boundaries, one action vocabulary, versioned files

**Version:** 0.1 · **Date:** 2026-09-21 · **Status:** draft for approval (not implemented).
Written from the 2026-09-21 architectural review of `moosedev` at `c8d4ff5` (branch `v3`).
When you approve, its Requirements and
Constraints are captured through the approve-spec loop before implementation begins.
Accepted graph records govern this spec if they disagree.


## Why

The harness grew by thrash: 29k lines under `src/harness/` (35% of the crate) written
across four campaigns in three weeks, with correctness fixes layered on correctness fixes.
The design is now settling (symbolic policy only, native tool calls, per-role models, the
permission workflow, the finish guard), so the cost of the accumulated shape is no longer
worth paying. The review found five things worth fixing, in this order:

1. `src/harness/` holds two programs. The server side (`daemon/**`, 6.9k lines, mounted
   by `api/routes.rs`) and the client side (runner, executor, session, TUI) live in one
   folder; the client imports server types across the HTTP boundary and even persists one
   in the task journal (`runner/task.rs:160`); the client links the server through
   `startup.rs → runtime`. The graph's own component model has the same blur: SystemComponent
   `e19120a8` "active-agency policy engine" covers `src/harness/` and `src/policy/` as one.
2. The action vocabulary is written by hand in five places (`model.rs` schema literal,
   `ACTION_MEANINGS`, `PLAN_MODE_ACTION_NAMES`, `tools.rs description()`,
   `actions.rs validate_permission`), joined by string equality, kept honest by three
   consistency tests. Adding one action touches nine files. Prompt copy sits in the same
   file as HTTP transport and stream decoding, so a wording tweak and a provider quirk edit
   the same 1,300 lines.
3. Two of the four persisted harness files have no schema marker and none has a
   load-an-old-file test, so every field added this week is an unmeasured compatibility bet.
4. (Outside the harness.) Four clone modules render graph records as markdown artifacts;
   each new record kind copies the pattern a fifth time.
5. (Outside the harness.) `graph::AppState` is the crate's DI container and cycle hub;
   `graph` imports its own consumers (`stories`, `adrs`, `validation`, `canonical`).

Stages 1–3 are the harness cleanup. Stages 4–5 are independent and may be approved or
deferred separately.

## Governing records (already accepted)

- Lesson `9ceca86a` *Keep harness mechanisms behind focused module boundaries*: preserve
  prompts, journal schemas, side-effect order, security policy and platform distinctions
  during extraction; mechanically compare moved bodies; run the unchanged scoped
  regressions, including real OS probes.
- Lesson `28d0a90c` *Decompose a feature before its module becomes the feature*.
- AD `58ff5625` *Split graph module into responsibility submodules*: preserve the public
  API through re-exports; behaviour-preserving; verified with build, clippy, tests.
- AD `9dcaddeb` *S8: task journal schema 2*: old journals are refused with a named message;
  migration was declined. This spec does not reverse that.
- AD `84dfd153` *Native tool calls are the action contract*; AD `909145d3` *Plan mode
  offers only planning actions in schema and prompt*; AD `4d47f57c` *guidance file*: "output
  format, action meanings and mode actions stay compiled because they must match the schema".
- Constraint `cd9f1a96`; Constraint `39d89d42` (guidance carries no study examples).
- Lessons `6582117b` (artifact logic out of handlers) and ADs `242ae2b2`, `899b9ab9`,
  `603fe674`, `1e260d23` (the artifact family and its deliberate per-kind divergences).

## Rules for every stage

- **R0. Behaviour-preserving.** No stage changes what the model is sent, what the journal
  contains, what the daemon writes to the graph, or what the sandbox allows. Each stage is
  a mechanical move or a derivation that reproduces today's bytes.
- **R1. One stage, one commit series, green at each commit.** `cargo build`,
  `cargo clippy --all-targets` with and without `--features harness`, `cargo fmt`, and the
  scoped tests named in each stage. The full suite runs once at the end of Stage 3 and once
  at the end of Stage 5 (release-acceptance gate).
- **R2. Public re-exports keep old paths alive for one release** where a path is used by
  tests or examples, then are removed. Internal `crate::` paths are updated in the same
  commit as the move.
- **R3. Moved bodies are diffed, not retyped.** `git mv` for whole files; for partial
  moves, `git diff --color-moved` must show the body as moved.
- **R4. No new vocabulary.** No ontology change, no new journal fields, no new config keys.
- **R5. Each stage ends by capturing one ArchitecturalDecision (with the rejected
  alternatives below), linking it `concerns` to the components it touches, and running
  `validate_against_architecture`.

---

## Stage 1 — Two programs, one protocol

### Target layout

```
src/api/harness/            server side: the daemon's harness endpoints (was src/harness/daemon/**)
  mod.rs                    the private/public module split of today's daemon.rs, unchanged
  spec.rs, capture_type.rs, ground.rs, intent.rs, context.rs, scope.rs, capture.rs,
  review.rs, reconcile_score.rs, associate.rs, candidates.rs, anchors.rs, journal.rs,
  checkpoint.rs, revision.rs
src/harness/                client side: the agent program; everything feature-gated except protocol
  mod.rs                    doc: "The harness client. `protocol` is the wire contract shared with the daemon."
  protocol/                 ungated leaf: wire types + the two helpers both sides must compute identically
    mod.rs                  (glob re-exports as today)
    capture.rs context.rs spec.rs associate.rs typing.rs scope.rs candidates.rs
    intent.rs               NEW: IntentResolveRequest/Response, IntentEntity, IntentBinding (+from_derived),
                            IntentLinkRequest/Response — moved verbatim from daemon/intent.rs:10-72
    digest.rs               was src/harness/digest.rs (sha256_hex, sha256_json)
    tokens.rs               `tokens` + STOPWORDS, moved from daemon/reconcile_score.rs:17,125-131
  coverage.rs               now #[cfg(feature = "harness")] (its only consumer is runner/symbolic/coverage.rs)
  config.rs executor/ progress.rs response.rs runner/ session.rs startup.rs tui.rs …  (unchanged)
src/runtime/
  mod.rs                    composition root, unchanged content (build_state, serve, relay, autospawn policy)
  backend.rs                NEW: the six path/process helpers with no AppState dependency:
                            socket_path_for, serve_log_path_for, http_addr_file_path_for, read_http_addr,
                            backend_is_live, spawn_detached_backend_with_exe  (from runtime.rs:266-424)
```

### Requirements

- **S1-R1.** `src/harness/` contains only the client program and its shared wire contract.
  After this stage `grep -rn 'crate::harness::' src/api src/graph src/mcp src/lsp
  src/runtime` matches only `crate::harness::protocol` and `crate::harness::CONFIG_FILE_NAME`.
- **S1-R2.** `harness::protocol` is a leaf. Its only in-crate import stays
  `crate::policy::PolicyDecision` (`protocol/context.rs:3`). A test asserts the module
  has no `crate::graph`, `crate::code`, `crate::api`, axum or oxigraph dependency
  (a build-time check: `protocol` compiles in a `cargo check` of a doc example is not
  available; use a source-level test that scans `src/harness/protocol/*.rs` for those
  prefixes — cheap and honest).
- **S1-R3.** The runner imports no server type. The five `daemon::intent` imports
  (`runner/links.rs:5`, `runner/task.rs:160,193`, `runner/symbolic/scope.rs:5`,
  `runner/symbolic/associate.rs:7`) and `harness/coverage.rs:10`
  (`reconcile_score::tokens`) point at `protocol`. Serde shapes are unchanged, so the
  task journal bytes are unchanged (verified by the Stage 3 fixture).
- **S1-R4.** The client's only dependency on the server process is `runtime::backend`.
  `startup.rs` imports nothing else from `runtime`. `runtime/mod.rs` uses `backend` for
  the same helpers so there is one spelling of each path.
- **S1-R5.** `api/routes.rs` mounts the fifteen harness handlers from `crate::api::harness`.
  Handler signatures, the `{handler, pure_fn}` pairs that `tests/harness_daemon.rs` calls
  directly (`associate_page`, `candidate_page`, `capture_type_operation`, `ground_edit`,
  `load_receipt`, `record_receipt`, `ScoreReceipt`), and the daemon's private/public
  module split are preserved.
- **S1-R6.** The graph's component model follows the code: `active-agency policy engine`
  (`e19120a8`) is re-scoped to `src/harness/` + `src/policy/`; a new SystemComponent
  "harness daemon endpoints" covers `src/api/harness/` (via `declare_component_paths`).
  Existing records that `concerns e19120a8` are not re-pointed; the new component starts
  empty and the Stage 1 AD concerns both.

### Rejected alternatives (recorded with the AD)

- A top-level `src/daemon/`: misleading — MCP, LSP and the HTTP API are the daemon too;
  these files are specifically HTTP handlers and already have the `api/handlers` shape.
- Leaving `daemon/` in place and only moving the intent types: keeps the `api ↔ harness`
  import cycle and the folder lie that produced Lesson `9ceca86a`.
- A separate `moosedev-protocol` crate: correct in principle, but a workspace split is a
  bigger change than the problem and would move `policy::PolicyDecision` too.

### Mechanics (order matters)

1. `git mv src/harness/daemon src/api/harness`; `git mv src/harness/daemon.rs
   src/api/harness/mod.rs`; fix the self-reference at `spec.rs:1736`; update
   `api/mod.rs`, `api/routes.rs` (15 sites), `tests/harness_daemon.rs` (`moosedev::api::harness`).
2. Cut `intent.rs:10-72` into `protocol/intent.rs`; `pub use intent::*` from
   `protocol/mod.rs`; leave `LinkOperation` and the handlers in `api/harness/intent.rs`
   importing from protocol. Repoint the five runner imports and `coverage.rs`.
3. `git mv src/harness/digest.rs src/harness/protocol/digest.rs`; add `protocol/tokens.rs`;
   repoint the 13 `harness::digest` importers (6 server, 6 client, `tests/harness_runner/mock.rs:17`)
   — keep `pub use protocol::digest` in `harness/mod.rs` for one release (R2).
4. Gate `coverage.rs`.
5. Split `runtime.rs` into `runtime/mod.rs` + `runtime/backend.rs`; make `read_http_addr`
   and `http_addr_file_path_for` `pub` (they are `pub(crate)` today — fine inside the
   crate, but state the visibility explicitly); repoint `startup.rs:69-137,920-924`.
6. Update `harness/mod.rs` and `api/mod.rs` doc comments; `docs/harness.md` has no
   module paths to fix (checked: only `docs/floor_study_protocol.md:307` names
   `src/harness/response.rs`, which does not move).

### Verification

`cargo test --features harness --test harness_daemon --test harness_runner
--test harness_session --test harness_executor_recovery`; `cargo test --lib harness::`;
`cargo test` without the feature for `api`, `policy`, `lsp_*`; `cargo clippy` both ways;
`examples/harness_study_session.rs` builds. `git diff --color-moved=zebra` over the stage
shows the daemon bodies as pure moves. The S1-R1 grep and the S1-R2 source test are the
acceptance checks.

---

## Stage 2 — One action vocabulary; prompt copy out of the transport file

### Design

`src/harness/runner/actions.rs` becomes the single source of truth:

```rust
pub struct ActionSpec {
    pub name: &'static str,             // serde tag of model::Action, e.g. "request_permission"
    pub modes: &'static [Mode],         // where the model may choose it (AD 909145d3)
    pub params: &'static [Param],       // (name, ParamKind) — the tool's argument schema
    pub description: &'static str,      // one line, the tool definition's description (tools.rs:34-53 today)
    pub meaning: &'static str,          // the per-action semantics paragraph (ACTION_MEANINGS today)
}
pub const ACTIONS: &[ActionSpec] = &[ /* inspect, reply, read, search, plan, replace, write,
                                        command, request_permission, question, replan, finish
                                        — the order of model.rs:979 today */ ];
```

Derived from `ACTIONS`, replacing hand-written copies:

| Today | After |
|---|---|
| `model.rs:979` one `json!` literal for `action_schema(mode)` | `action_schema(mode)` = `oneOf` over `ACTIONS.iter().filter(mode)`; the `variant()` helper stays |
| `model.rs:51 PLAN_MODE_ACTION_NAMES` | `ACTIONS.iter().filter(|a| a.modes.contains(&Mode::Plan)).map(name)` |
| `model.rs:47 ACTION_MEANINGS` | signature list rendered from `name(params…)` + concatenated `meaning`s; must reproduce today's bytes exactly |
| `model.rs:53-54 PLAN_MODE_ACTIONS` / `AUTO_MODE_ACTIONS` "Allowed actions now: …" | names derived; the mode-specific nudge sentences stay as two `const`s in `prompt.rs` |
| `tools.rs:34-53 description(name)` string match with an orphan `edit` arm and a `_ => "A harness action."` fallback | `ActionSpec::description`; an unknown name is a bug, not a fallback |
| `actions.rs:58-88 validate_permission` mode gate | `spec.modes.contains(&task.mode)` (the in-plan-file scope checks stay hand-written — they are not vocabulary) |
| three consistency tests (`model.rs:1211-1282`) | one: every `ACTIONS.name` round-trips through `model::Action`'s serde tag and every `Action` variant has a spec (`edit` is the legacy alias and is asserted absent from `ACTIONS`) |

Prompt copy moves to `src/harness/runner/prompt.rs`: `ROLE_OPENING`, `ROLE_BOUNDARY`,
`RULES_HEADER`, the four `*_OUTPUT` consts, `JOB`, the two mode nudges, `project_rules`,
`plan_rule_echo`, and the assembly fns `mandatory_prompt` (`model.rs:578-640`),
`observations_prefix`, `history_tail`, `navigation_context`. `model.rs` keeps the budget
consts, `Generated`, role/client selection, `model_json`, `decode_tool_completion`,
streaming and partial-JSON salvage — i.e. it becomes the transport file its name suggests.

### Requirements

- **S2-R1.** For a fixed task state, the prompt bytes and the tool definitions the harness
  sends are identical before and after this stage, in both modes and both action contracts
  (`tools`, `json_schema`). Verified by a golden fixture captured *before* the refactor
  (`tests/fixtures/harness/prompt_{plan,auto}_{tools,json}.txt`, produced by a test
  helper that renders `mandatory_prompt` and `tools::definitions` for the
  `test_support` planned runner). The fixtures stay as the standing regression for
  prompt drift; a deliberate wording change updates them in the same commit.
- **S2-R2.** Adding an action is one entry in `ACTIONS`, one `Action` variant, one
  `dispatch.rs` arm, and its symbolic handling. Nothing else lists action names.
  `grep -rn '"request_permission"' src/harness` after the stage matches only
  `actions.rs` and tests.
- **S2-R3.** Prompt copy and control logic are in different files. `model.rs` contains no
  `const … &str` longer than one line after the stage.
- **S2-R4.** Compiled stays compiled (AD `4d47f57c`). The copy lives in a Rust module, not
  in `templates/`, so nothing next to the user-editable `GUIDANCE.md` looks editable.

### Rejected alternatives

- Prompt copy as `templates/harness/prompts/*.md` via `include_str!`: separates copy from
  code just as well, but puts compiled text beside `GUIDANCE.md`, the one file the user
  *may* edit; the distinction AD `4d47f57c` draws would then be invisible in the tree.
- A derive/proc-macro on `Action`: the schema needs per-field constraints
  (`maxLength`, `["string","null"]`, integer minimums) that a macro would have to grow
  attributes for; a table is smaller and readable.
- Deleting the `json_schema` contract to simplify derivation: it is the recorded fallback
  for runtimes without tool support (AD `84dfd153`).

### Tests to rewrite (they pin today's literals)

`model.rs:1011-1029`, `:1211-1259`; `tests/harness_runner/tools.rs:44-112`;
`tests/harness_runner/symbolic.rs:325-367` (the sentence "Allowed actions now: …");
`tests/harness_runner.rs:542-593` (section order + two `ACTION_MEANINGS` substrings).
Each becomes an assertion against `ACTIONS` or against the golden fixture rather than a
retyped literal. Behavioural tests in `tools.rs:115-359` stay.

### Verification

S2-R1 fixtures equal; `cargo test --features harness --lib harness::runner` and
`--test harness_runner`; `docs/harness.md:138-145` and `:600-605` re-read against the
table (no per-action table is added to the docs — the table is the code).

---

## Stage 3 — Every persisted harness file is versioned and has a load test

### Current state (verified)

| File | Marker | Refusal | Fixture test |
|---|---|---|---|
| `.moosedev/harness/tasks/{id}.json` | `Task.schema: u32 = 2` (`runner.rs:62,264`) | yes, named message (AD `9dcaddeb`) | none |
| `.moosedev/harness/conversations/{id}.json` | `Conversation.schema = 1` (`session.rs:45,70`) | yes (silent: `load` returns None) | none |
| `.moosedev/harness/provider.json` | none (`startup.rs:309 RememberedProvider`) | n/a | none |
| `moosedev.toml` `[harness]` | none; unknown keys rejected (`config.rs:114-118`) | yes — but the message does not say *why* | parse tests only |

### Requirements

- **S3-R1.** Every file the harness writes under `.moosedev/harness/` carries a `schema`
  field. `RememberedProvider` gains `#[serde(default)] schema: u32` written as `1`; a file
  with a different value is ignored and re-discovered (the file is a cache, so ignoring is
  correct — no refusal).
- **S3-R2.** One committed fixture per file, produced by *this* build and never edited by
  hand: `tests/fixtures/harness/task-schema-2.json` (a planned task with one edit, one
  permission grant, one intent event, `last_failure` set, `pending_spec` set — the fields
  added this month), `conversation-schema-1.json`, `provider-schema-1.json`,
  `moosedev-full.toml` (every known key at both role levels). A test loads each through
  the real loader (`Runner::load`, `Conversation::load`, the provider reader,
  `ModelFile::parse`) and asserts the parsed value equals a constructed one. A second test
  asserts `task-schema-1.json` (a copy with `schema: 1`) is refused with the recorded
  message.
- **S3-R3.** Bumping any `schema` constant requires regenerating its fixture in the same
  commit; the fixture test names the constant so the failure says which.
- **S3-R4.** `moosedev.toml` stays strict (typo safety, per AD `18e1e455`), but the
  unknown-key error names the build and the keys it knows:
  `unknown key harness.index_refresh; this build (0.11.1) knows harness.{model, index_refresh}`.
  That turns a version skew from a mystery into a message.
- **S3-R5.** `IntentLinkRequest` (Stage 1) lands in the journal fixture, so its move is
  proven byte-neutral.

### Rejected alternatives

- Migrating old journals: declined in AD `9dcaddeb`; nothing here changes that.
- Relaxing `moosedev.toml` to ignore unknown keys: reverses a deliberate typo guard for a
  problem a better message solves.

### Verification

`cargo test --features harness --test harness_runner --test harness_session --lib
harness::config --lib harness::startup`. The fixture directory is listed in
`docs/harness.md` under Persistence with the one-line rule from S3-R3.

---

## Stage 4 — One artifact generator (outside the harness)

Decided 2026-09-21: the HTTP JSON stays byte-identical; ADRs stay on their own renderer and
share helpers; lessons, constraints and requirements move onto one engine.

### What is genuinely shared (verified)

`lessons.rs`, `constraints.rs`, `requirements.rs` (485/489/502 lines) carry the same type
family (`{X}GenerationOptions/Set/Summary/Document/Warnings/Meta`) and the same function
list with byte-parallel bodies for `count_*`, `enumerate_*` (slug de-dup, `{:04}` ordinals,
`ORDER BY ?ts ?x`), `fetch_related_*` batching, `zip_archive`, `summary()`, `render_index`,
`summarize_warnings`, `filename`, `slugify`. `adrs.rs` differs in kind: a 7-predicate
cluster, five sections, no description in `Meta`, a generation memo, and two private copies
of the shared status helpers (`render_plain_status` `adrs.rs:628` duplicates
`artifacts.rs:128`; `adr_link`/`render_status_label` duplicate `render_lifecycle_status`).

### Design

`src/artifacts.rs` (already the shared home) gains a kind-parameterised engine:

```rust
pub(crate) struct ArtifactKind {
    pub class: &'static str,          // "Lesson" — resolved via state.resolve_class (Constraint 19bb4d8a)
    pub prefix: &'static str,         // "LSN" | "CST" | "REQ"
    pub noun: &'static str,           // slug fallback + section heading ("lesson")
    pub related: RelatedQuery,        // see below
    pub related_heading: &'static str,
    pub unlinked_warning: &'static str, // "unlinked_lessons" — a name, because the UI counts it
    pub empty_index_text: &'static str,
    pub addressed: bool,              // requirements only: the "Addressed" column and meta line
}
pub(crate) enum RelatedQuery {
    Inbound  { predicate: &'static str, subject_class: &'static str },   // requirements: ?ad isMotivatedBy ?req
    Either   { forward: &'static str, inverse: &'static str },            // lessons: learnedFrom ∪ yieldsLesson
                                                                          // constraints: constrains ∪ isConstrainedBy
}
```

plus one generic `ArtifactSet`/`ArtifactDocument`/`ArtifactSummary`/`ArtifactWarnings` and
`generate_set(state, &ArtifactKind, options)`. The three per-kind modules shrink to a
`pub const KIND: ArtifactKind` and their public type aliases; `api/models.rs` keeps the
four `{X}ListResponse`/`{X}DetailResponse` structs and gains `From<&ArtifactSet>` mappings
that emit today's field names (`related_sources`, `graph_lessons`, `lesson_files`, …).

Divergences the engine must reproduce, not normalise (each is asserted by an existing
golden test): both-direction de-dup for lessons and constraints (one mechanism — the
`HashSet<(record,target)>` from `constraints.rs:286` — replaces the linear scan at
`lessons.rs:293`; same output); constraints wrap the related title in `not_recorded`
(`constraints.rs:390`) while the other two print it raw — the engine does what constraints
does only if the golden tests for lessons/requirements still pass; otherwise `ArtifactKind`
gains a one-bit flag (decide at implementation, record which); requirements' `addressed`
derivation (`requirements.rs:473`, the only artifact use of `graph::is_retired`); the index
column sets; the per-kind empty-set text.

ADRs: delete `render_plain_status`, `render_status_label`, `adr_link` in favour of the
`artifacts.rs` equivalents (`artifacts::render_plain_status` additionally maps
`"superseded"`; confirm `tests/adrs.rs` output is unchanged or record the one-word
difference). The cluster renderer, memo and warnings stay.

### Requirements

- **S4-R1.** Generated markdown, index, ZIP and JSON for all four kinds are byte-identical
  before and after. `tests/{lessons,constraints,requirements,adrs}.rs` pass unchanged; a
  new test renders each kind for a fixed fixture graph and compares against captured
  files (`tests/fixtures/artifacts/*.md`) so future edits to the engine show every kind
  they touch.
- **S4-R2.** Adding a record kind to the artifact family is a new `ArtifactKind` const, a
  DTO pair in `api/models.rs`, and the three routes. No new `src/<kind>.rs` clone.
- **S4-R3.** `artifacts.rs` becomes `pub mod artifacts` (it is `mod` today, `lib.rs:11`) so
  integration tests can drive the engine directly; the per-kind modules keep their public
  names (`generate_lesson_set` etc.) as thin wrappers for one release (R2).
- **S4-R4.** Lesson `6582117b` holds: handlers stay 54-line thin wrappers.

### Rejected alternatives

- Unifying the JSON: alters `/api/v1/{lessons,constraints,requirements}` and needs a UI
  pass for a saving of four DTO structs.
- Folding ADR into the engine: the cluster/memo/reciprocal-warning machinery would make
  the descriptor carry most of ADR's complexity anyway.

### Verification

`cargo test --test lessons --test constraints --test requirements --test adrs --test api`;
the S4-R1 fixture comparison; `ui/` unchanged (`git diff --stat ui/` is empty).

---

## Stage 5 — `AppState` stops importing its consumers (outside the harness)

### What was verified

`AppState` (`graph/state.rs:100-185`) holds, besides the store and vocabularies: the LLM
client, the substrate cache and reload lock, `stories::StoryCheckRegistry` and
`StoryNarrationCache` (`:139,:141`, read only from `src/stories`), `adrs::AdrSetMemo`
(`:179`, invalidated under lock inside `note_project_write` `:355-363`), the canonical
write throttle, and the in-process HTTP address (`:184`, written by `runtime.rs:146,162`,
read by `graph/dossier.rs:1060,1085`). `graph` therefore imports `stories`, `adrs`,
`canonical`, `validation` (`graph/links.rs:346` → `validation::run_project_shacl`) and
`llm`; each of those imports `graph` back. `temporal.rs` (1,061 lines, std-only imports)
is declared only in `main.rs:43` and is unreachable from `tests/`.

### Changes, smallest first

- **5a. Story registries belong to stories.** New `stories::Registries { checks:
  Mutex<StoryCheckRegistry>, narrations: StoryNarrationCache }`, owned by the HTTP server
  state (5b), not by `AppState`: every reader (`stories/checks.rs:541,552,653`,
  `stories/narration/mod.rs:99`) runs inside a request. If a reader turns out to need them
  from a non-HTTP path, fall back to a `Box<dyn Any>` extension slot on `AppState` — record
  which at implementation.
- **5b. The ADR memo is an HTTP-layer cache, not graph state.** `api::ServerState { app:
  Arc<AppState>, adr_memo: Arc<AdrSetMemo>, stories: Arc<stories::Registries> }` with
  `axum::extract::FromRef` impls, so every existing `State<Arc<AppState>>` handler compiles
  unchanged and only `api/handlers/adrs.rs` and the story handlers extract the extra
  fields. `note_project_write` shrinks to: mark inferred stale, bump
  `project_write_generation` under a graph-owned `generation_lock`, note the canonical
  write. `AdrSetMemo` validates and stores under that same lock via a new
  `AppState::with_write_generation(|gen| …)`; the guarantee "no stale memo survives after
  `note_project_write` returns" is retained and its tests (`tests/adrs.rs:164`,
  `src/adrs.rs:718-759`) must pass unchanged. `graph` no longer imports `adrs` or `stories`.
- **5c. `canonical` and `validation` are graph.** They are read/write plumbing over the
  store with mutual imports; the folder should say so: `git mv src/canonical.rs
  src/graph/canonical.rs`, `git mv src/validation.rs src/graph/validation.rs`, with
  `pub use` at the old paths for one release (R2). No behaviour change. (The SystemComponent
  `graph/store layer` `11e01ea2` already covers both paths.)
- **5d. `temporal` joins the library.** `pub mod temporal;` in `lib.rs`, `use
  moosedev::temporal` in `main.rs`; zero body edits (it has no `crate::` imports). Enables
  `tests/temporal.rs` later; none is written in this stage.
- **5e. One name for the bound address.** Rename the private `runtime::http_addr()`
  (`runtime.rs:248`, the env-derived *desired* bind address) to `desired_http_bind_addr()`
  so it cannot be confused with `AppState::http_addr()` (the actual bound address, the
  in-process source of truth per the doc at `graph/state.rs:180-183`). The addr *file*
  stays a cross-process hint laundered through `verify_http_addr` — unchanged.
- **5f. Writers cannot forget the write note.** Every project-graph write outside
  `src/graph` (`mcp/mod.rs` ×6, `api/harness/{spec,capture,review}.rs` ×7,
  `api/handlers/export.rs:78`) goes through `AppState::project_write(|store| …)`, which
  runs the closure and then `note_project_write`. A source-level test asserts no file
  outside `src/graph` calls `note_project_write` directly. The exact closure signature
  follows whatever those thirteen sites do today (transaction vs. direct insert) — settle
  at implementation, record in the AD.

Not in this stage: the substrate lifecycle (`graph/state.rs:373-550`) stays in `AppState`
— it is the one place that owns the cache and reload lock, and `code::substrate` must
not learn about `AppState`. `AppState.llm` stays — `stories`, `capture_type` and
`graph/query` reach the model through it and there is no second owner to hand it to yet.
Splitting `kg` into retrieval / ratification / code-linking submodules is a later spec.

### Requirements

- **S5-R1.** After the stage, `src/graph/**` imports nothing from `stories`, `adrs`,
  `canonical` (now inside graph), `validation` (now inside graph) — a source-level test
  scans for `crate::stories`/`crate::adrs` under `src/graph/`.
- **S5-R2.** No handler signature changes except the three that extract the new
  `ServerState` fields. `AppState::bootstrap*` signatures are unchanged (≈40 test callers).
- **S5-R3.** All behaviour tests pass unchanged: `tests/adrs.rs`, `src/stories/tests.rs`,
  `tests/canonical_text.rs`, `tests/validation.rs`, `tests/lsp_diagnostics.rs`.

### Rejected alternatives

- A `Box<dyn Any>` extension map on `AppState` for everything: hides the dependency
  instead of removing it; kept only as the 5a fallback.
- Inverting `adrs`'s invalidation to pure generation polling without a shared lock: changes
  the tested lock-ordering guarantee.
- Moving `llm` out of `AppState` now: needs an owner that does not exist.

### Verification

`cargo test` (full suite — this stage is the second acceptance gate); `cargo clippy
--all-targets` both feature sets; the S5-R1 source test; `moosedev --serve` + workbench
smoke: ADR page renders and refreshes after a `record_important_decision`.

---

## Graph writes on approval (approve-spec loop), and per stage

On approval: capture this spec's Requirements (S1-R1…S5-R3) and the standing rules R0–R5 as
`Requirement`/`Constraint` records (`concerns` `e19120a8`, `19df26dc`, `11e01ea2`,
`402e5c0b`), so each stage's AD has hubs to be `isMotivatedBy`. Each stage then ends with
one `ArchitecturalDecision` carrying the rejected alternatives listed above, `link_code`
to the entry points it creates (`ACTIONS`, `ArtifactKind`, `runtime::backend`,
`api::ServerState`), and `validate_against_architecture`. Stage 1 also re-scopes
`e19120a8` and declares the new `harness daemon endpoints` component (S1-R6).

---

## Order, sizing, and what is not in scope

| Stage | Size | Risk | Depends on |
|---|---|---|---|
| 1 modules | large diff, mechanical | low (moves) | clean tree — satisfied at `c8d4ff5` |
| 2 vocabulary | medium | medium (prompt bytes) | golden fixture captured first |
| 3 versions | small | low | 1 (fixture includes moved type) |
| 4 generators | medium | low | none |
| 5 AppState | medium | medium (cycles) | 1 (5f names `api/harness` paths) |

Each stage is approvable on its own; 1–3 are the harness cleanup and are meant to land
together before the next harness feature.

Not in scope: splitting `tui.rs` (its 900 lines of gate text are a separate spec once the
gates stop changing); the `.env` loader unification (three loaders — worth its own small
decision); the two model-settings resolvers with drifted defaults (`llm/mod.rs:22` vs
`startup.rs:316`) — flagged for a follow-up under AD `18e1e455`; the JSON-scanner and
`sha256_hex` duplicates outside the harness (Stage 1 fixes the harness copies).
