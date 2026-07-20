# MOOSEDev

**Structured, long-term project memory for coding agents — a neurosymbolic daemon that fights comprehension debt.**

> Status: **v2.x scope complete; active pre-production development.** See [Status](#status).

*NOTE*: MOOSEDev is in **very** early development, and is not to be considered production ready.

---

## What is MOOSEDev?

MOOSEDev is a local project-memory daemon that gives coding agents and humans a reliable, **structured, queryable, long-term understanding** of a software project. Agents reach it through the [Model Context Protocol](https://modelcontextprotocol.io) (MCP), editors through a knowledge-focused LSP, and humans through the web workbench.

Its purpose is to combat **comprehension debt**: the gradual loss of shared understanding of *why* a codebase is shaped the way it is. Instead of stuffing ever more history into an LLM's context window, MOOSEDev maintains a typed, auditable **project knowledge graph**: architectural decisions, lessons, constraints, anti-patterns that an agent can record into and reason over symbolically.

MOOSEDev is built on the **MOOSE** neurosymbolic engine. MOOSEDev itself is open source; the MOOSE engine is closed (for now, plans to open source when it's ready).

## Why it's different

Most agent "memory" tools store free text and retrieve it with embeddings; they optimize for *sounding* relevant. MOOSEDev optimizes for *being* correct:

- **Symbolic layer is primary.** The LLM is a sensor, not the controller. Deterministic mechanisms: typed capture, concept alignment, graph queries, validation — do the load-bearing work.
- **Structured knowledge over free text.** Decisions and lessons are typed instances in an RDF graph (`ArchitecturalDecision`, `Lesson`, `Constraint`, `AntiPattern`, …), not markdown blobs.
- **Alignment prevents drift.** New concepts are aligned to the project's ontology rather than accumulating as inconsistent one-offs.
- **Auditability.** Queries carry execution traces, so reasoning is inspectable rather than opaque.
- **Local and under your control.** Runs locally; no required cloud services.

The full set of design invariants lives in [`AGENTS.md`](./AGENTS.md) and [`CLAUDE.md`](./CLAUDE.md).

## How it works

```
  Coding agents ── MCP ───────┐
  Editors ─────── Knowledge-LSP├──▶ MOOSEDev shared daemon
  Host hooks ──── policy events┘       │
                                       ├── code substrate (SCIP + tree-sitter)
                                       ├── typed capture + ratification queue
  Human workbench ◀── HTTP ────────────┤
                                       ├── oxigraph project knowledge graph
                                       └── MOOSE engine
                                           • symbolic NLQ + traces
                                           • alignment + local embeddings
```

MOOSE provides the read/reason side: natural-language query, the alignment engine, conversational focus, execution traces, and local embeddings (Snowflake Arctic-Embed-S via Candle). MOOSEDev provides the host side: one shared daemon, the durable knowledge graph and typed **write** path, the code substrate and entity dossiers, the host-independent policy engine, MCP/LSP/HTTP surfaces, SPARQL, validation, and bootstrap workflows. All surfaces use the same graph queries and policy engine; editor and host adapters remain thin clients rather than second sources of truth.

## MCP tool surface

All of the tools below are live. They speak MCP over stdio; the LLM acts only as a sensor — the symbolic layer does the load-bearing work.

| Tool | Purpose |
|------|---------|
| `ping` | Health check (transport smoke test) |
| `record_important_decision` | Capture a typed decision / lesson / constraint / pattern / anti-pattern / requirement into the durable graph |
| `supersede_decision` | Record a type-preserving replacement that supersedes a prior item: links `supersedes`, captures *why*, marks the old one superseded (never deleted) |
| `retract_decision` | Deprecate a recorded item that no longer applies (no replacement); captures *why* and preserves it as history |
| `relate` | Link two recorded items with a typed, ontology-legal relationship so memory can be *traversed*, not just searched |
| `link_code` | Link a typed record to a code entity by source position or stable symbol, lazily minting private entities when needed |
| `declare_component_paths` | Declare the repo paths owned by a system component so code entities can realize the right architecture |
| `get_entity_dossier` | Return the authoritative decisions, constraints, lessons, judgments, and observations linked to one code entity |
| `evaluate_policy` | Evaluate push, gate, or capture events through the shared symbolic policy engine |
| `capture_decision_point` | Create a grounded, proposed decision from a completed episode and queue its code links for human ratification |
| `pending_ratifications` | Report proposed records and links awaiting human review without turning judgment classification into a nudge |
| `suggest_links` | Suggest ranked, ontology-legal links between records (suggest-only; confirm with `relate`); can scan for under-linked records |
| `query` | Natural-language query over the graph, with a symbolic reasoning trace |
| `get_relevant_context` | Retrieve current, authoritative knowledge relevant to a topic (symbolic, no LLM); omit the topic to list everything, or explicitly opt into lifecycle history |
| `get_provenance` | Retrieve provenance for an item — which agent recorded it, and when — by IRI |
| `align_concepts` | Align a new concept to the best-matching ontology class (keyword + embedding sensors), with rationale or ranked candidates |
| `suggest_mappings` | Propose ranked ontology-class mappings for a new concept, for human review |
| `sparql` | Read-only SPARQL over the local store (SELECT/ASK → JSON; CONSTRUCT/DESCRIBE → N-Triples) |
| `export_graph` | Export the knowledge graph as RDF text (canonical N-Quads by default; `nq`/`nt`/`ttl`, scope `project`/`provenance`/`all`) |
| `validate_against_architecture` | Validate recorded knowledge against the loaded architecture SHACL shapes (symbolic, on-demand) |

## Status

The complete **v2.0–v2.3 product scope is implemented and acceptance-tested on this branch**. MOOSEDev remains early, pre-production software; “v2” describes the delivered product phase, not a claim of production maturity. The source tree is version `0.7.0`; the install script and Homebrew formula deliver the latest published release, which can lag the branch until it is tagged.

The v1 memory foundation remains intact: typed capture and lifecycle management, symbolic query and recall, alignment, SPARQL, SHACL validation, graph import/export, shared multi-client operation, bootstrap workflows, generated documentation, and a loopback web workbench.

v2 adds the code-aware and active layers:

- **v2.0 — code-aware read path:** SCIP indexing (Rust, TypeScript, Python) with a tree-sitter fallback, stable code-entity identity, deterministic position resolution, component path coverage, entity dossiers, and honest-silence hover/diagnostics.
- **v2.1 — ambient understanding:** knowledge LSP code lenses, constraint and stale-rationale diagnostics, per-component why-coverage debt, the workbench ratification inbox, and pending-review nudges.
- **v2.2 — active agency:** one graph-driven policy engine for entity-exact push, edit-time gates, and grounded decision capture; thin Claude Code and opencode adapters; best-effort fire telemetry in `.moosedev/fires.jsonl`. Automatic session checkpoints only journal telemetry; deliberate `capture_decision_point` calls are the path that proposes graph records.
- **v2.3 — ratified editor writes:** LSP code actions propose record links, roles, and criticality; every change goes through the ratification queue, with no direct LSP graph-write path. Push/pull diagnostic parity and the real Neovim conformance client are covered by tests.

The phase definitions and acceptance criteria are in [`spec/MOOSEDev_v2_spec.md`](./spec/MOOSEDev_v2_spec.md). Delivered editor clients: Zed, Neovim, VS Code, and Emacs (eglot/lsp-mode) — all thin stanzas over the same server.

## Getting started

### Use a release binary (recommended)

Pre-built, self-contained binaries are the supported path for everyone (building from source requires access to the private MOOSE engine — see [Open source & the MOOSE boundary](#open-source--the-moose-boundary)). Install with the script or Homebrew:

```sh
# macOS (Apple Silicon) / Linux (x86-64)
curl -fsSL https://raw.githubusercontent.com/Trivyn/moosedev/main/scripts/install.sh | sh

# or Homebrew
brew install Trivyn/moosedev/moosedev
```

Each downloads a binary bundled with its `ontologies/`, `skills/`, and `templates/` — no toolchain needed. See **[docs/install.md](docs/install.md)** for platforms, checksums, upgrades, and the macOS signing note; binaries are also on [GitHub Releases](https://github.com/Trivyn/moosedev/releases).

### Build from source

Requires Rust 1.89 or newer and a checkout of the MOOSE engine at `../moose` (MOOSEDev depends on it via a path dependency, and on its patched `oxigraph` fork).

```sh
git clone https://github.com/Trivyn/moosedev.git
cd moosedev
# ensure the MOOSE engine is checked out at ../moose
scripts/build-release.sh
```

The default build bundles a CPU embedding backend (`candle-cpu`) and the Arctic-Embed-S model used by the alignment engine.

The human-facing web UI is embedded in the Rust binary by default, so generated frontend assets must exist before a normal Cargo build:

```sh
cd ui
npm install
npm run build
cd ..
cargo build --release
```

`ui/dist/` is generated output from Vite and is intentionally not tracked in Git.

For a backend-only build that does not require `ui/dist/`, use the explicit headless feature:

```sh
cargo build --release --no-default-features --features headless
```

### Run as an MCP server

The quickest way to wire MOOSEDev into a project is:

```sh
cd /path/to/your/project
moosedev init
```

This writes a ready-to-use `.mcp.json` (shared `--connect` mode), the `.gitignore` memory rule, a `CLAUDE.md` template, and agent skills under `.claude/skills/` — non-destructively (it won't clobber an existing `CLAUDE.md` or other MCP servers) — then reload your MCP client. Add only the integrations you use:

```sh
moosedev init --codex          # Codex MCP config + .agents/skills/
moosedev init --zed            # project-local Knowledge-LSP settings
moosedev init --vscode         # project-local VS Code settings (extension in clients/vscode)
moosedev init --opencode       # active-agency opencode adapter
moosedev init --claude-hooks   # Claude Code push/gate/capture hooks
```

Flags compose, and repeated runs merge safely. `--stdio` opts out of the default shared daemon; `--force` replaces existing MOOSEDev entries. See the [Quickstart](docs/quickstart.md) for the full flow and `moosedev --help` for the complete option list.

<details>
<summary>Manual configuration</summary>

MOOSEDev speaks MCP over **stdio**; `init` just generates this for you. To configure a client by hand, point it at the binary:

```json
{
  "mcpServers": {
    "moosedev": {
      "command": "moosedev",
      "args": []
    }
  }
}
```

Use an absolute path to the binary if `moosedev` isn't on the client's `PATH`. To share one graph across several clients, use `"args": ["--connect"]` — see [Shared mode](#shared-mode-multiple-clients--agents) below.
</details>

All tools in the [MCP tool surface](#mcp-tool-surface) are available over this transport.

### Index code and enable the Knowledge-LSP

v2's code-aware surfaces need a substrate index and minted public code entities. Preview the graph-changing steps before applying them:

```sh
moosedev index
moosedev mint
moosedev mint --apply
moosedev classify
moosedev classify --apply   # optional role/criticality proposals
```

`mint` and `classify` open the single-writer store directly, including in dry-run mode, so stop the shared daemon before running them. Both are dry-run unless `--apply` is present. Re-run `moosedev index` after significant source changes; `moosedev init` can also install a post-commit refresh hook. `moosedev resolve FILE LINE:COL` is the deterministic debugging path for position resolution.

The Knowledge-LSP runs through `moosedev lsp` and shares the daemon's graph. It provides entity-dossier hover, role/criticality and debt code lenses, Information/Hint constraint and staleness diagnostics (pull for capable clients, push otherwise), and proposal-only code actions; lens clicks open the workbench via `window/showDocument`, handled server-side so no client needs custom glue. Clients are thin stanzas: [Zed](clients/zed/README.md) (dev extension), [Neovim](clients/nvim/README.md) (plain lspconfig entry; also hosts the executable conformance suite), [VS Code](clients/vscode/README.md) (LSP-client extension, local `.vsix`), and [Emacs](clients/emacs/README.md) (eglot + lsp-mode registrations in one file).

Classification only creates proposals. Judgments and editor code actions do not affect authoritative dossiers or debt metrics until a human ratifies them in the web workbench.

### Seed the graph (bootstrap)

A fresh graph has nothing to recall — **bootstrap** it from your existing codebase so agents start
with real context, not a blank store. Two complementary paths:

- **Snapshot bootstrap** *(agent skill)* — a one-shot survey of the codebase's *current* design
  rationale (architecture, decisions, constraints, lessons), captured as typed, linked records.
  `moosedev init` installs it as an auto-discoverable agent skill, so you just tell your coding
  agent: *"bootstrap this repo's memory into MOOSEDev."* (`moosedev skills` lists the shipped
  workflow docs.)

- **Temporal bootstrap** *(built-in command)* — replays your git history oldest→newest, capturing
  each decision-bearing commit with its **real commit date and author**, so the graph carries an
  accurate timeline and honest `supersedes` chains as decisions evolved:

  ```sh
  moosedev bootstrap --temporal --repo . --dry-run                        # preview the triage — no agents
  moosedev bootstrap --temporal --repo . --data-dir .moosedev --limit 5   # capture the first few commits
  moosedev bootstrap --temporal --repo . --data-dir .moosedev --resume    # …then finish the rest
  ```

  It drives a coding agent (`claude` by default, or `--agent codex`) **once per decision-bearing
  commit** — sequential, so pace larger histories with `--limit` / `--resume`. Mechanical commits
  (fmt / bump / wip) are triaged out, and each commit's date + author are stamped server-side rather
  than left to the LLM.

Either way, commit the resulting `.moosedev/kg.nq` to version your project's memory with the code.
See the **[Quickstart](docs/quickstart.md)** for the end-to-end flow.

### Shared mode (multiple clients / agents)

The durable graph is RocksDB-backed, which is **single-writer**: only one process
can hold a project's store open at a time. With the default stdio mode each MCP
client spawns its own server, so a **second** client (e.g. Codex alongside Claude
Code) on the same project fails to start. To let several clients share one live
graph **concurrently**, run a single shared backend and point the clients at it:

```bash
# 1) Start one backend per project (it owns the store; keep it running).
MOOSEDEV_DATA_DIR=/path/to/project/.moosedev moosedev --serve
```

```jsonc
// 2) Configure every client to connect through a thin proxy instead of
//    spawning their own server. Same MOOSEDEV_DATA_DIR as the backend.
{
  "mcpServers": {
    "moosedev": {
      "command": "/absolute/path/to/moosedev",
      "args": ["--connect"],
      "env": { "MOOSEDEV_DATA_DIR": "/path/to/project/.moosedev" }
    }
  }
}
```

The backend listens on a Unix socket derived **per data dir**
(`<MOOSEDEV_DATA_DIR>/moosedev.sock` by default, or `MOOSEDEV_SOCKET`), so
separate projects each get their own backend with no cross-talk. `--connect`
clients are lightweight proxies; they never open the store or load the model.
When no backend is listening, `--connect` auto-spawns a detached `--serve`
backend for the same resolved socket unless `MOOSEDEV_NO_AUTOSPAWN=1` is set.

### Web UI

A `--serve` backend also exposes the human-facing web UI on a **loopback-only,
ephemeral port** by default, the OS picks a free port, so per-project backends
never collide. Besides browsing the graph, the workbench is where proposed
records and links are **ratified** — classifier judgments and editor code-action
proposals stay pending in its inbox until a human accepts them. The backend writes the resolved address to
`<MOOSEDEV_DATA_DIR>/http.addr`; discover or open it with:

```bash
moosedev --status   # is a backend running? where is its web UI?
moosedev ui         # open the web UI in a browser (auto-spawns a backend if needed)
moosedev --serve --open   # start the backend and open the UI once it is up
```

`--status` and `ui` are socket-only — they never open the store. Set
`MOOSEDEV_HTTP_ADDR` for a stable port or to expose the UI on a network
interface (e.g. `0.0.0.0:7474`), or `MOOSEDEV_NO_HTTP=1` to disable it. A UI bind
failure never takes down the MCP backend.

### Version-controlled memory (`kg.nq`)

The committed source of truth for a project's memory is a canonical text serialization:
`<MOOSEDEV_DATA_DIR>/kg.nq` (sorted N-Quads, asserted knowledge only — reasoner-inferred edges are
excluded and re-derived locally). MOOSEDev maintains it automatically:

- **every write** to the project graph re-exports `kg.nq`, so it is always ready to commit —
  `git diff` shows new records as added lines, reviewable in a PR like any other change. Rapid
  successive writes (bulk imports, bootstrap replays) are coalesced: the burst skips per-write
  exports and a single export lands once it goes quiet;
- **on startup** the file and the local store are reconciled: a fresh clone (or a `git pull` that
  changed `kg.nq`) hydrates the local store from the text; an existing store with no `kg.nq` yet
  exports one (adoption);
- the RocksDB store and the vector DBs are a **derived, gitignored local cache** — never commit
  them, only `kg.nq`:

```gitignore
/.moosedev/*
!/.moosedev/kg.nq
```

If both the text and the store changed since the last sync (e.g. a hand-edited `kg.nq` beside
unsynced local writes), MOOSEDev merges them as a union and warns. If `kg.nq` cannot be parsed
(e.g. leftover merge-conflict markers), the backend refuses to start rather than risk clobbering
the file — resolve it and restart. The provenance graph (who recorded what, when) stays local and
is not committed.

### Export / import the graph

A project's memory is just an RDF graph, so it can also be backed up and restored explicitly as
text. These subcommands run directly against the on-disk store and need **no running backend**:

```bash
# Back up the project graph as canonical N-Quads …
moosedev export backup.nq

# … and restore it (N-Quads is not the import default, so name the format).
moosedev import backup.nq --format nq
```

`export` takes `--format nq|nt|ttl` (default `nq`) and `--graph project|provenance|all` (default
`project`). `import` takes `--format ttl|nt|nq` (default `ttl`), the same `--graph` scopes, and
`--mode patch|replace` (default `patch` — `patch` inserts only missing quads; `replace` fully
restores the selected scope). **N-Quads is the canonical version-control format** (deterministic);
Turtle is human-readable but not byte-canonical. While a backend is running, use the `export_graph`
MCP tool or the web UI instead, so the operation goes through the live store.

### Configuration

MOOSEDev is configured via environment variables (this surface is filling in as features land).
At startup, `moosedev` also reads a repo-root `.env` when present; explicit environment variables
from the shell or MCP client config take precedence. This applies to `--connect` too, so an
auto-spawned backend inherits the resolved configuration.

- **LLM endpoint** (optional, for assisted NLQ/chat): set `MOOSEDEV_LLM_BASE_URL` to an OpenAI-compatible endpoint, plus optional `MOOSEDEV_LLM_API_KEY` / `MOOSEDEV_LLM_MODEL` / `MOOSEDEV_LLM_ASSIST_LEVEL` (how aggressively the LLM sensor assists). When no base URL is configured, MOOSEDev pins LLM assistance to pure-symbolic mode; `get_relevant_context`, `sparql`, capture, validation, and symbolic `query` remain available. *Local-first; cloud is opt-in.*
- **Data directory** (`MOOSEDEV_DATA_DIR`): where the durable knowledge graph and session database live. Runtime state is kept out of version control except `kg.nq`, the committed canonical serialization of the project graph (see "Version-controlled memory").
- **Socket** (`MOOSEDEV_SOCKET`, shared mode): override the per-data-dir Unix socket path used by `--serve` / `--connect`.
- **Web UI address** (`MOOSEDEV_HTTP_ADDR`, shared mode): bind address for the human-facing web UI. Defaults to an ephemeral loopback port (`127.0.0.1:0`); set a fixed `host:port` for a stable URL or network exposure. `MOOSEDEV_NO_HTTP=1` disables the UI entirely.
- **Knowledge-LSP** (`MOOSEDEV_NO_LSP`): disable the daemon's editor endpoint when set to a truthy value.
- **Code index producers** (`MOOSEDEV_SCIP_PRODUCER`, `MOOSEDEV_SCIP_TYPESCRIPT`, `MOOSEDEV_SCIP_PYTHON`): override the Rust, TypeScript, and Python SCIP producer commands used by `moosedev index`. Rust defaults to `rust-analyzer`; TypeScript defaults to `npx --yes @sourcegraph/scip-typescript`; Python defaults to `npx --yes @sourcegraph/scip-python` (needs Python 3.10+ on PATH; activate the project's virtual environment for cross-package references).
- **Ontology directory** (`MOOSEDEV_ONTOLOGY_DIR`): where the shipped ontologies live. By default MOOSEDev looks for an `ontologies/` directory next to the running binary (the layout of the released tarball), then falls back to the crate's `ontologies/` for `cargo run`. Set this only to load ontologies from a custom location. *Keep the unpacked release bundle together so the binary can find its `ontologies/`.*

## Project layout

```
src/            # shared daemon and CLI (Rust)
src/code/       # SCIP/tree-sitter code substrate, resolver, minting, observations
src/lsp/        # thin Knowledge-LSP transport and presentation surface
src/policy/     # host-independent push/gate/capture policy engine and fire telemetry
ui/             # the human-facing web UI (Vite/React); built to ui/dist/ and embedded in the binary
clients/        # thin Knowledge-LSP clients and conformance fixtures (Zed, Neovim, VS Code, Emacs)
.claude/hooks/  # Claude Code active-agency adapters installed by init
.opencode/      # opencode active-agency adapter installed by init
ontologies/     # software-engineering + architecture ontologies + SHACL shapes (.ttl)
skills/         # workflow docs: bootstrap, temporal capture, ADR generation
templates/      # CLAUDE.md template for projects adopting MOOSEDev as memory
spec/           # v1/v2 specifications + design of record + upstream engine asks
tasks/          # build checklist / roadmap
tests/          # integration tests
```

## Open source & the MOOSE boundary

- **MOOSEDev** (this repo — the MCP server, tools, ontologies, prompts, docs) is **open source**.
- **The MOOSE engine** remains **closed** for now. Building MOOSEDev *from source* therefore requires access to the private MOOSE repository and its `oxigraph` fork; published **binaries** are the route for everyone else.

Contributions to the open parts of MOOSEDev are welcome. Deep changes to MOOSE behavior are out of scope for external contributors at this stage — but capabilities MOOSEDev needs from the engine are surfaced openly in [`spec/core-moose-asks.md`](./spec/core-moose-asks.md).

## Documentation

- [`spec/MOOSEDev_spec.md`](./spec/MOOSEDev_spec.md) — v1 scope definition
- [`spec/MOOSEDev_v2_spec.md`](./spec/MOOSEDev_v2_spec.md) — v2 code layer, active-agency layer, Knowledge-LSP, and acceptance criteria
- [`spec/MOOSEDev_design.md`](./spec/MOOSEDev_design.md) — design of record (architecture, decisions, milestones)
- [`spec/core-moose-asks.md`](./spec/core-moose-asks.md) — capabilities to upstream into core MOOSE
- [`docs/quickstart.md`](./docs/quickstart.md) — installation, initialization, bootstrap, and first recall
- [`clients/zed/README.md`](./clients/zed/README.md) / [`clients/nvim/README.md`](./clients/nvim/README.md) / [`clients/vscode/README.md`](./clients/vscode/README.md) / [`clients/emacs/README.md`](./clients/emacs/README.md) — delivered Knowledge-LSP clients
- [`AGENTS.md`](./AGENTS.md) / [`CLAUDE.md`](./CLAUDE.md) — design invariants and development practices

## How this was built

MOOSEDev has been written iteratively with AI coding agents under human architectural direction and review. 

## License

MOOSEDev is licensed under the **Apache License 2.0**. The MOOSE engine is proprietary and separately licensed.
