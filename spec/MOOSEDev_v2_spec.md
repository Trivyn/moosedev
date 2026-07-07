# Spec — MOOSEDev v2: Code Layer, Active-Agency Layer, and Knowledge-LSP

> Satisfies `Requirement/350c7f2e-61bd-49e4-b7cd-2fdd3f51f521` — *MOOSEDev v3: standalone
> neurosymbolic coding agent, not an MCP sidecar* (this spec delivers the **v2** slice of that
> trajectory).

**Version:** 0.1 (draft)
**Date:** 2026-07-06
**Status:** Draft for review

This spec operationalizes the accepted v2 decision cluster in the project knowledge graph
(ADs `72e9ca40`, `b0d3e4d9`, `136dbf24`, `7079634f`, `681698b3`, `2501ffa4`, `3b32fb25`,
`145af7e9`, `346bb120`; Constraint `2ba76439`). Where this document and the graph disagree, the
graph wins and this document has a bug. Net-new decisions made *by this spec* (scope cuts,
phasing, client order, substrate strategy) are recorded in the graph alongside it.

---

## 1. Context and motivation

MOOSEDev v1 is a passive MCP memory sidecar: correct symbolic operations
(`get_relevant_context`, `record_important_decision`, `sparql`, `validate_against_architecture`),
invoked only when a host agent chooses to call them. The v1→v2→v3 trajectory
(Requirement `350c7f2e`) makes v2 the step where MOOSEDev gains:

1. a **code layer** — a structural world model of the codebase, joined to the existing record
   layer, and
2. an **active-agency layer** — MOOSEDev acting *into* the loop (push, gate, capture) instead
   of waiting to be called,

with the **knowledge-LSP** as the human-facing surface of both.

The evidence base, from the graph:

- **Push works; pull fails for weak models.** Injecting the relevant record took a local model
  from 0.4 to 1.0 on the cache-gap task — the only condition that moved it (AD `03393c63`,
  Lesson `2dcca5ff`). v2 generalizes push from topical relevance-guessing to entity-exact
  delivery.
- **v1 capture is not anchor-ready.** Measured 2026-07-03: 70 of 674 records (~10%) cite any
  code path, as prose `file:line` that rots; `concerns → SystemComponent` effectively unused
  (AD `3b32fb25`). A position→records resolver has nothing precise to resolve against — which
  is why the LSP was gated to v2 in the first place.
- **Delivery value is capture-gated** (Lesson `56d142f9`): surfaces are only as good as what
  capture put in the graph. v2's capture verb and entity anchoring attack this directly.
- **Structural/symbolic queries are a capability axis** free-text memory cannot express at all
  (Lesson `8a06ad1d`, axis B): the no-LLM CI gate and the why-coverage metric in this spec are
  the demonstrations.

Two tenets govern everything below:

- **One brain** (Constraint `2ba76439`): no surface — LSP server, editor extension, host
  adapter, workbench — may build its own index, cache logic, or policy. All reads go through
  the daemon's queries; all writes route through the ratification gate.
- **v3 is earned by v2's numbers, not assumed** (Consequence `6505ed72`): every push-fire and
  gate-fire is a loggable event, so enforcement value is measurable before v3 commits to
  owning the whole loop.

---

## 2. Architecture overview — what exists, what's added

### 2.1 What already exists

One process, one `Arc<AppState>`, only the backend opens RocksDB:

- **MCP surface** — `src/mcp/mod.rs` (`MooseDevServer`, rmcp). Transports in `src/runtime.rs`:
  `serve_stdio()` for single-client mode, `serve_unix()` for the shared backend on
  `.moosedev/moosedev.sock`, and `--connect` as a thin stdio↔socket proxy
  (`connect_or_spawn`) that MCP clients actually spawn.
- **HTTP surface** — axum, `spawn_http_if_enabled()` (`src/runtime.rs`), routes under
  `/api/v1` (`src/api/routes.rs`), default `127.0.0.1:7474`, addr published to
  `.moosedev/http.addr`. The React workbench (`ui/`) is a pure HTTP client of this surface,
  embedded via `embedded-frontend`.
- **Canonical graph text** — `.moosedev/kg.nq` write-through and hydration
  (`src/canonical.rs`, `src/export.rs`, `src/graph_import.rs`).
- **Host integration precedent** — the opencode push plugin
  (`.opencode/plugins/moosedev-push.ts`) spawns `moosedev --connect` and speaks MCP JSON-RPC
  over the child's stdio; it scans tool args for paths/symbols and injects a knowledge block.
- **Client config generation** — `src/init.rs` writes `.mcp.json` and `.codex/config.toml`.
- **LLM-optional core** (AD `346bb120`): without `MOOSEDEV_LLM_BASE_URL` the engine is pinned
  to PureSymbolic. Everything in this spec except LLM-assisted capture extraction (§5.4) runs
  PureSymbolic.

### 2.2 What v2 adds

```
                        ┌──────────────────────────────────────────────┐
                        │            moosedev daemon (one process)     │
   editors  ──LSP──────▶│  LSP surface ──┐                             │
   (Zed, nvim)          │                ├──▶ dossier/policy queries   │
   agents   ──MCP──────▶│  MCP surface ──┤      (one query layer)      │
   (CC, opencode)       │                │            │                │
   workbench ─HTTP─────▶│  HTTP surface ─┘            ▼                │
                        │                     project KG (RocksDB,     │
   host hooks ─────────▶│  policy engine ◀───  kg.nq canonical)        │
   (push/gate/capture)  │       │                     ▲                │
                        │       ▼                     │                │
                        │  fire log (.moosedev/) substrate index       │
                        │                        (.moosedev/substrate/)│
                        └──────────────────────────────────────────────┘
```

New components, all inside the existing crate:

| Component | Location (proposed) | Nature |
|---|---|---|
| Substrate index + resolver | `src/code/substrate/` | derived cache, per-commit, gitignored |
| KG code skeleton + minting | `src/code/skeleton.rs` | KG writes, canonical in `kg.nq` |
| Entity dossier query | `src/code/dossier.rs` | one read function, all surfaces |
| Policy engine | `src/policy/` | reads graph, returns typed decisions |
| Knowledge-LSP server | `src/lsp/` | third surface; `moosedev lsp` shim |
| Host adapters | `.opencode/plugins/` (upgrade), `.claude/hooks/` (new) | thin, disposable |
| Ratification queue + inbox | daemon + `ui/src/pages/RatificationsPage.tsx` | write path gate |

**The one-query-layer rule is enforced by construction:** `get_entity_dossier(entity)` is a
single function; the LSP hover, the MCP push payload, and the HTTP/workbench view all call it.
The human's hover and the agent's pushed dossier are the identical world model because they are
the identical query — not because two implementations are kept in sync.

---

## 3. Code layer

### 3.1 Two-tier granularity (AD `72e9ca40`)

- **Substrate index** — the full AST/symbol graph, consumed from commodity tooling, stored
  under `.moosedev/substrate/` (derived, gitignored, regenerated per commit). No IRI-stability
  promises. Structural queries needing full resolution run here.
- **KG skeleton** — the addressable subset that gets IRIs in the project graph:
  - **always minted:** modules (logical) and the exported/public surface;
  - **lazily minted:** any other entity, on first attachment of a judgment, observation, or
    record link;
  - **never minted:** statements and expressions.

Pouring the AST into the KG is rejected (Alternative `a5360220`): walk-planning precision
depends on a curated graph (Rationale `262a87b8`).

### 3.2 Substrate strategy: SCIP canonical, tree-sitter fallback

**Decision (this spec):** SCIP is the canonical substrate format; tree-sitter is the fallback
resolver for languages without a SCIP producer. Both ship in the first slice.

- **SCIP mode.** First producer: rust-analyzer (`rust-analyzer scip` emit) — dogfooding on
  moosedev/moose themselves. Later languages are additional producers (scip-typescript,
  scip-python, scip-java, …), zero resolver changes. The SCIP symbol grammar is already the
  recorded identity scheme (AD `136dbf24`).
- **tree-sitter mode.** Per-language grammar, syntactic entities only.

**Degradation semantics (normative).** Fallback mode must degrade *honestly* — reduced
features, never reduced precision presented as precise:

| Capability | SCIP mode | tree-sitter fallback |
|---|---|---|
| Entity identity | SCIP symbol (cross-file, semantic) | `ts:<lang>:<path>:<kind>:<qualified-name>` |
| Position → entity | definition & references resolve | enclosing declaration only |
| Topology (calls/uses/implements) | derived | **absent** — never approximated |
| Fan-in/out, blast radius | derived | **absent** |
| Refactor continuity | rename/move edges via symbol identity | same-path+same-name only; renames orphan (flagged stale) |
| Hover dossier | full | full (dossiers are KG-side) |
| Constraint diagnostics | full | entity-local only |

Every resolution result carries its mode; surfaces may show a subtle "syntactic anchor" marker
in fallback mode but must never invent topology from lexical guesses — that is exactly the
relevance-guessing the entity layer exists to eliminate (AD `3b32fb25`).

**Index runtime:** `moosedev index` subcommand runs producers and rebuilds the substrate;
the daemon detects a stale substrate (HEAD moved since last index) and marks resolutions
stale-but-served. No file watcher in v2.0 (simplicity first); a git `post-commit` hook line is
offered by `moosedev init`. Watcher support is an open question (§8).

### 3.3 Entity identity (AD `136dbf24`)

- A code entity is a **continuant**: symbol path + kind, persisting through edits. Judgments,
  contracts, and intent links attach here.
- Volatile facts (content hash, line span, metrics) live on **commit-anchored snapshots**,
  minted lazily — only when an observation needs to attach. Observations attach to snapshots.
- Refactor continuity is typed edges: `renamedFrom` / `movedFrom` / `splitInto` / `mergedInto`
  (code-level supersession, reusing lifecycle machinery; deprecated ≈ superseded).
- **Nothing in the graph is anchored by line number.** Line spans are substrate/snapshot data,
  recomputed per commit.

### 3.4 Three strata (AD `b0d3e4d9`)

| Stratum | Authority | Lifecycle | Examples |
|---|---|---|---|
| 1. **Structure** | derived from source, never hand-asserted | regenerated per commit | kind; logical *and* physical containment (crossed by `declaredIn`); topology; exposure (public API / FFI / network entry) |
| 2. **Judgments** | proposed from evidence, human-ratified | lifecycle-managed | role; **criticality (orthogonal axis)**; attention policy = f(role, criticality, observations); domain-concept links; contracts (reusing `Constraint`, `constrains` widened to code entities) |
| 3. **Observations** | measured, instrument-provenanced | append-only, commit-anchored | perf/profile, churn & authorship concentration (per-region bus factor), defect attribution, verification state, human-vs-agent authorship |

Discipline (Consequences `44229453`, `9c452470`):

- Roles are **playing-relations** (`playsRole`, carrying status + provenance), never
  subclasses. An entity *is* a Function and *plays* CoreAlgorithm.
- SHACL shapes differ per stratum: observations append-only; judgments require ratification
  provenance.
- **Humans are never asked to assert what stratum 1 derives or stratum 3 measures** — the
  ratification-fatigue guard is structural, not procedural.
- Ontology terms (CodeEntity, CodeSnapshot, CodeRole, Criticality, AttentionPolicy,
  Observation, AuthorshipProvenance, CodeFile) passed the alignment check 2026-07-03
  (all genuinely new; mint under the Component/Module lineage). Confirmed reuses: Interface,
  Module, Constraint, lifecycle machinery, `dependsOn`/`implements`/`uses`/`accesses`/`exposes`.
- Role taxonomy per AD `b5b4762b` (core-algorithm / domain-logic / boundary / glue /
  boilerplate / generated) — ratified `accepted` 2026-07-06 (maintainer), satisfying the
  role-badge gate (§7, phase v2.1).

### 3.5 Intent links (AD `7079634f`)

Code entities link into the record layer — one graph, not two:

- `realizes` (CodeEntity → SystemComponent): records concerning a component transitively
  concern its realizing code.
- `satisfies` (→ Requirement), `embodies` (→ Pattern); `constrains` / `violates` widened to
  code entities.

This closes two loops:

1. `validate_against_architecture` gains **denotation** — constraints become checkable against
   the actual entities realizing a component: edit-time diagnostics (§6.4) and a **no-LLM CI
   gate** (`moosedev validate --code`, exit-code contract for CI).
2. **Drift detection, both directions** — a decision whose realizing code was deleted flags
   stale; a core-role, high-criticality entity with no linked rationale is a comprehension-debt
   hotspot with an address. **Why-coverage** (fraction of core entities with linked rationale)
   is a SPARQL-able, per-component metric — the product claim, served via HTTP for the
   workbench debt view.

### 3.6 Path→SystemComponent map — in the graph, not a config file

**Decision (this spec):** the v1-deliverable path map (Consequence `54937887`) is stored **in
the project graph** as path-glob literals on `SystemComponent` (property to be minted through
`align_concepts` at implementation — working name `hasPathGlob`), not as a new config file
under `.moosedev/`.

Rationale: the map *is* project knowledge (invariant #2 — structured over free text); in-graph
it is captured/superseded/audited with the existing tools, exported in `kg.nq`, and readable by
every surface through the one query layer (no second brain). `.moosedev/` stays a data dir,
which it is today (exploration confirmed no config machinery exists there to extend).

### 3.7 Bootstrap migration

The 70 v1 records with prose `file:line` citations are parsed as seeds: citation → substrate
lookup at the *commit the record was authored at* (git history) → continuant match in the
current index → proposed `concerns`/intent link, queued for ratification. Unresolvable seeds
are reported, not silently dropped.

---

## 4. Active-agency layer

### 4.1 Policy engine

One host-independent engine in the daemon (`src/policy/`), reading only the graph. Its policy
vocabulary **is** the code layer — role, criticality, attention policy, contracts (AD
`681698b3`: the two halves of v2 are one design). Input: an event (entity touched, edit
proposed, decision point). Output: a typed `PolicyDecision`:

```
PolicyDecision =
  | Inject { dossier }                          -- push
  | Warn   { diagnostic }                       -- degraded gate
  | Gate   { Deny | RequirePlan | RequireRatification, reason, records }
  | CaptureTrigger { extraction spec }          -- capture
```

Policies degrade gracefully per host capability: gate where blocking exists, warn-and-inject
where only observation exists.

### 4.2 The three verbs

1. **PUSH** — entity-exact dossiers on touch. Extends the proven push plugin (AD `03393c63`,
   0.4→1.0) from topical guessing to: host reports touched file/position → resolver →
   continuant → `get_entity_dossier` → inject. Same query as hover (§2.2).
2. **GATE** — host hooks that **block**: deny / require-plan / require-ratification on
   hand-tuned or critical entities. The first literal symbolic-controller moment: the LLM
   cannot talk its way past a hook the way it can past a system prompt. Turns
   `validate_against_architecture` operative at edit time.
3. **CAPTURE** — extract typed records from diff + transcript at decision points (grounded —
   never interrogate the LLM into confabulating, AD `145af7e9`); constrain `isMotivatedBy` to
   existing Requirements; stamp authorship provenance (human vs agent/session) for free; queue
   for human ratification. LLM-assisted extraction is optional per AD `346bb120`; PureSymbolic
   mode still captures diff-derived structure and provenance.

### 4.3 Adapters: thin and disposable

Hook-surface dependence is the platform-risk zone (Consequence `0c447d67`). Adapters contain
**zero policy**: they observe host events, call the daemon, and enact its decision.

- **opencode** — upgrade `.opencode/plugins/moosedev-push.ts` from lexical symbol-scanning to
  resolver-backed entity push; add gate via the plugin's blocking hooks where available.
- **Claude Code** — new hooks (`.claude/hooks/`): `PreToolUse` on Edit/Write for gate
  (deny/ask), `PostToolUse` for push and capture triggers. Precedented mechanism; hooks call
  `moosedev --connect` exactly like the opencode plugin does.

**v2 acceptance test (AD `681698b3`): the same policy drives both hosts.**

### 4.4 Fire-event telemetry — outside the KG

**Decision (this spec):** every push-fire and gate-fire is logged (`[trial-fire]` pattern,
Consequence `6505ed72`) to an append-only JSONL at `.moosedev/fires.jsonl` (gitignored):
`{ts, verb, host, entity, decision, records_cited}`. Fires are operational telemetry, **not**
knowledge — they do not enter the project graph. Aggregates (e.g. "gate prevented N
constraint violations in month 2") are captured as typed records during trial reviews, by a
human. This keeps `kg.nq` reviewable while making v3's case measurable.

---

## 5. Knowledge-LSP surface

### 5.1 What it is, and is not

An LSP server that **annotates symbols and never parses** (AD `2501ffa4`). Language
intelligence stays with rust-analyzer/tsserver/etc.; editors run multiple servers per buffer.
Capabilities declared:

- `hoverProvider` — the dossier (§5.3)
- `codeLensProvider` — role/criticality/debt badges (§5.5, phase v2.1)
- diagnostics — constraint proximity + staleness (§5.4); `publishDiagnostics` push first
  (universal client support), pull diagnostics later
- `codeActionProvider` — the write path (§5.6, phase v2.3, last)

Explicitly **not** declared, ever: completion, go-to-definition, references, rename,
formatting, semantic tokens. Zero contention with the language server.

### 5.2 Transport and process model

`moosedev lsp` subcommand: a thin stdio shim that connects to the shared backend's Unix socket
— the LSP analogue of the proven `--connect` proxy (`src/runtime.rs`), so editors spawn a
short-lived client process while the one daemon serves every surface. Inside the daemon the
LSP handler is spawned alongside HTTP (mirror of `spawn_http_if_enabled`), sharing
`Arc<AppState>`. Crate: `tower-lsp` or `async-lsp` — net-new dependency either way
(exploration: none present); selection criteria are maintenance activity and fit with the
existing tower/tokio stack; decide at implementation (§8).

Resolution flow: `(URI, position)` → substrate symbol → continuant IRI → dossier/policy query.
A miss is **silence** (honest empty state) — no fuzzy fallback, no lexical guess.

### 5.3 Hover: the dossier

Markdown, served from `get_entity_dossier` (the same function push injects):

1. Identity line: kind, logical path, `[syntactic anchor]` marker if fallback-resolved.
2. Judgments: role + criticality badges **with lifecycle status and ratification provenance**
   (`core-algorithm · critical — ratified 2026-07-12 by james`). Proposed-but-unratified
   judgments render as proposals, visually distinct.
3. Linked records: title, kind, status, date — decisions, constraints (contracts first),
   lessons — each addressable (workbench URL).
4. Observations digest: churn, defect attribution, last-verified, authorship concentration.
5. Why-coverage flag when the entity is core/critical with no linked rationale.

Entities with nothing recorded produce **no hover response at all** — the knowledge-LSP is
silent on 95% of the file, by design (lazy skeleton = sparse annotations = no wallpaper).

### 5.4 Diagnostics: severity discipline (normative)

The alarm-fatigue lesson (AD `3b32fb25`) applied: a knowledge server that red-squiggles gets
uninstalled in week one and never reinstalled.

| Condition | Severity | Example |
|---|---|---|
| Editing an entity with attached contracts/constraints | `Information` | "Constrained by 'No line-number anchoring' (AD 136dbf24) — require-plan policy applies" |
| Linked rationale predates N subsequent changes | `Hint` | "Rationale for this function predates its last 14 changes" |
| Validated `violates` edge (SHACL-confirmed) | `Warning` | "Violates accepted Constraint 19bb4d8a" |
| — | `Error` | **never emitted, no exceptions** |

Each category is individually disableable via LSP initialization options. Diagnostics are
recomputed on didOpen/didSave only (not per keystroke) in v2.0.

### 5.5 Code lens (phase v2.1)

Badges above declarations, only where knowledge exists or is conspicuously missing:
`core-algorithm · 3 decisions · 1 constraint`, or `⚠ core entity — no linked rationale`
(the why-coverage hotspot, in situ). Lens commands open the workbench at the entity. Lens
density is self-limiting via the lazy skeleton. **Gate:** role badges require AD `b5b4762b`
ratified (§3.4).

### 5.6 Code actions (phase v2.3 — last)

Lightbulb on a resolved entity: "Link decision to this entity…", "Propose role…", "Record
constraint…", "Mark rationale stale". Every action files a **proposal into the ratification
queue** — the LSP write path has no direct graph access (Constraint `2ba76439`: no side
doors). The ClarificationCard flow (typed question → typed answer, durably recorded)
generalizes from NLQ grounding to this inbox.

### 5.7 Client matrix and onboarding

| Client | Phase | Integration |
|---|---|---|
| **Zed** | v2.0 | settings-based LSP registration, no extension (per AD `2501ffa4`; exact config keys verified at implementation). `moosedev init` generates the stanza — extending the existing `.mcp.json`/`.codex` generation in `src/init.rs`. MCP already covers Zed's agent panel. |
| **Neovim** | v2.3 | plain lspconfig entry; doubles as the scriptable conformance client |
| **VS Code** | post-v2 | thin "picture frame" extension (LSP client + workbench webviews), when a buyer-facing demo warrants it |

### 5.8 Pending-ratifications nudge (Consequence `09ce698d`)

If ratification lives only in a workbench nobody opens, the judgment stratum starves. The
high-frequency surfaces carry the nudge: LSP `window/showMessage` (info) once per session when
the pending queue is non-empty, and the MCP surface exposes the pending count for host status
lines. Never per-edit, never a diagnostic.

---

## 6. Ratification and fatigue guards

- **Structural guard** (Consequence `9c452470`): strata 1 and 3 are never presented for
  human approval — there is nothing to ratify about a derived or measured fact.
- **Confidence gate** (Consequence `be097082`): judgments are default-classified from evidence
  with a confidence score; only low-confidence or high-stakes (critical-entity)
  classifications escalate to the human. High-confidence, low-stakes proposals auto-hold at
  `proposed` and take effect only in advisory surfaces until ratified.
- **Queue mechanics:** proposals are ordinary graph records at `proposed` with full
  provenance; ratification transitions lifecycle to `accepted` (reusing existing lifecycle
  machinery — no new state store). Inbox = a workbench page (`ui/`) listing `proposed`
  judgments/captures with accept / edit / reject; reject records the rejection (auditability,
  invariant #6).
- **Boilerplate stays audited** (Consequence `e56f95cb`): "out of my face" never means
  unaudited — machine checks + anomaly escalation apply precisely to the code no human reads.

---

## 7. Phasing and acceptance tests

Sequencing per AD `2501ffa4` (read-only slice → lens → write path), extended to the full v2
scope. Each phase has hard done-criteria; a phase is not done until they demonstrably pass
(no green-run acceptance — verify on the artifact).

### v2.0 — Read-only vertical slice
Substrate index (SCIP/rust-analyzer + tree-sitter fallback) · KG skeleton + minting rule ·
in-graph path map (§3.6) · resolver · `get_entity_dossier` · LSP hover + Information/Hint
diagnostics · Zed onboarding via `moosedev init` · fire log.

**Accept when:** on the moosedev repo itself, hover over ≥10 distinct entities with linked
records renders correct dossiers in Zed; resolver spot-check ≥95% correct on a 40-position
sample (both modes); a file with no KG entities produces zero hovers/diagnostics; no
diagnostic above Information; substrate rebuild after a rename preserves continuant IRIs in
SCIP mode.

### v2.1 — Ambient layer + debt metric
Code lens · why-coverage SPARQL metric + HTTP endpoint + workbench debt view · ratification
inbox page · pending nudge · bootstrap migration of the 70 citation seeds.

**Accept when:** why-coverage is queryable per component and matches manual count on a sample;
lens appears only on knowledge-bearing/hotspot entities; migration report accounts for every
seed (linked or explained). **Entry gate:** AD `b5b4762b` (role taxonomy) ratified —
satisfied 2026-07-06.

### v2.2 — Active agency
Policy engine · entity-exact push (opencode upgrade) · gate via Claude Code `PreToolUse` +
opencode blocking hooks · grounded capture prototype (diff + transcript → proposed records).

**Accept when:** **one policy file drives both hosts** — a scripted scenario (edit a
constrained entity) produces gate-fire on both, logged to `fires.jsonl`; push injects the same
dossier bytes hover shows; capture produces only `proposed` records with provenance, never
auto-accepted.

### v2.3 — Write path + conformance
Code actions through the ratification queue · Neovim conformance client · pull diagnostics.

**Accept when:** a code-action proposal round-trips (editor → queue → workbench ratify →
visible in next hover); Neovim scripted conformance suite passes; grep proves no graph-write
call path originates in `src/lsp/` except queue submission.

---

## 8. Non-goals and open questions

**Non-goals (v2):**
- No dedicated editor/IDE (Alternative `ad738979` — rejected).
- No lexical/BM25 position-binding fallback (Alternative `272c2ce0` — rejected; misses stay
  silent).
- No language-server features (completion/def/refs/rename) — ever, not just v2.
- No AST-in-KG (Alternative `a5360220` — rejected).
- No cloud dependency; LLM remains optional (AD `346bb120`).
- No per-editor logic beyond picture frames; no policy in adapters.

**Open questions:**
1. **LSP crate** — tower-lsp vs async-lsp (maintenance vs API fit). Decide at v2.0
   implementation start.
2. **File watcher** vs on-demand + git-hook indexing (v2.0 ships the latter).
3. **tree-sitter rename continuity** — accept orphaning, or content-hash similarity matching?
   v2.0 accepts orphaning + stale flag.
4. **Observation retention** — append-only observations will grow `kg.nq`; rollup/compaction
   policy needed before observations ship at volume (v2.2).
5. **Zed config keys** for extension-less LSP registration — verify against current Zed at
   implementation; if Zed has regressed to requiring an extension, the fallback is a minimal
   Zed extension that only registers the binary (still no logic — Constraint `2ba76439`).
6. **ACP-style host portability** for the active layer — recorded as future option
   (AD `145af7e9`), not a v2 prerequisite.
