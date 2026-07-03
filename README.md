# MOOSEDev

**Structured, long-term project memory for coding agents — a neurosymbolic MCP server that fights comprehension debt.**

> Status: **active development.** See [Status](#status).

*NOTE*: MOOSEDev is in **very** early development, and is not to be considered production ready.

---

## What is MOOSEDev?

MOOSEDev is a [Model Context Protocol](https://modelcontextprotocol.io) (MCP) sidecar that gives coding agents — and eventually humans — a reliable, **structured, queryable, long-term understanding** of a software project.

Its purpose is to combat **comprehension debt**: the gradual loss of shared understanding of *why* a codebase is shaped the way it is. Instead of stuffing ever more history into an LLM's context window, MOOSEDev maintains a typed, auditable **project knowledge graph**:  architectural decisions, lessons, constraints, anti-patterns that an agent can record into and reason over symbolically.

MOOSEDev is built on the **MOOSE** neurosymbolic engine. MOOSEDev itself is open source; the MOOSE engine is closed (for now, plans to open source when it's ready).

## Why it's different

Most agent "memory" tools store free text and retrieve it with embeddings; they optimize for *sounding* relevant. MOOSEDev optimizes for *being* correct:

- **Symbolic layer is primary.** The LLM is a sensor, not the controller. Deterministic mechanisms: typed capture, concept alignment, graph queries, validation — do the load-bearing work.
- **Structured knowledge over free text.** Decisions and lessons are typed instances in an RDF graph (`ArchitecturalDecision`, `Lesson`, `Constraint`, `AntiPattern`, …), not markdown blobs.
- **Alignment prevents drift.** New concepts are aligned to the project's ontology rather than accumulating as inconsistent one-offs.
- **Auditability.** Queries carry execution traces, so reasoning is inspectable rather than opaque.
- **Local and under your control.** Runs locally; no required cloud services.

The full set of design invariants lives in [`CLAUDE.md`](./CLAUDE.md).

## How it works

```
  Coding agent (Claude Code, Claude Desktop, …)
        │  MCP — JSON-RPC over stdio
        ▼
  ┌────────────────────────────────────┐
  │ MOOSEDev  (this repo)              │
  │   • MCP server (rmcp)              │
  │   • typed knowledge capture        │
  │   • durable knowledge graph        │──▶ oxigraph (RDF, on-disk)
  │   • SPARQL · arch validation       │
  └─────────────────┬──────────────────┘
                    │ composes
                    ▼
  ┌────────────────────────────────────┐
  │ MOOSE engine  (closed)             │
  │   • NLQ query + execution traces   │
  │   • alignment                      │
  │   • focus stack · local embeddings │
  └────────────────────────────────────┘
```

MOOSE provides the read/reason side: natural-language query, the alignment engine, conversational focus, execution traces, and local embeddings (Snowflake Arctic-Embed-S via Candle). MOOSEDev provides the host side: the MCP server, the durable knowledge graph and its typed **write** path, a SPARQL endpoint, lightweight validation, and the bootstrap workflow.

## v1 tool surface

All of the tools below are live. They speak MCP over stdio; the LLM acts only as a sensor — the symbolic layer does the load-bearing work.

| Tool | Purpose |
|------|---------|
| `ping` | Health check (transport smoke test) |
| `record_important_decision` | Capture a typed decision / lesson / constraint / pattern / anti-pattern / requirement into the durable graph |
| `supersede_decision` | Record a type-preserving replacement that supersedes a prior item: links `supersedes`, captures *why*, marks the old one superseded (never deleted) |
| `retract_decision` | Deprecate a recorded item that no longer applies (no replacement); captures *why* and preserves it as history |
| `relate` | Link two recorded items with a typed, ontology-legal relationship so memory can be *traversed*, not just searched |
| `suggest_links` | Suggest ranked, ontology-legal links between records (suggest-only; confirm with `relate`); can scan for under-linked records |
| `query` | Natural-language query over the graph, with a symbolic reasoning trace |
| `get_relevant_context` | Retrieve recorded knowledge relevant to a topic (symbolic, no LLM; superseded entries excluded); omit the topic to list everything |
| `get_provenance` | Edit provenance for an item — which agent recorded it, and when — by IRI |
| `align_concepts` | Align a new concept to the best-matching ontology class (keyword + embedding sensors), with rationale or ranked candidates |
| `suggest_mappings` | Propose ranked ontology-class mappings for a new concept, for human review |
| `sparql` | Read-only SPARQL over the local store (SELECT/ASK → JSON; CONSTRUCT/DESCRIBE → N-Triples) |
| `export_graph` | Export the knowledge graph as RDF text (canonical N-Quads by default; `nq`/`nt`/`ttl`, scope `project`/`provenance`/`all`) |
| `validate_against_architecture` | Validate recorded knowledge against the loaded architecture SHACL shapes (symbolic, on-demand) |

## Status

This is an active build with a working v1 surface. **Live today and exercised by integration tests:**

- **Typed knowledge capture** into a durable, on-disk graph that persists across restarts — decisions, lessons, constraints, patterns, anti-patterns, requirements (`record_important_decision`) — plus the full edit-and-link lifecycle: `supersede_decision`, `retract_decision`, `relate`, `suggest_links`.
- **Natural-language query** with a symbolic reasoning trace (`query`); symbolic, no-LLM **context recall** (`get_relevant_context`); and edit **provenance** (`get_provenance`).
- **Concept alignment** to the project ontology (`align_concepts`, `suggest_mappings`).
- **SPARQL** reads (`sparql`) and **SHACL validation** of recorded knowledge against the architecture shapes (`validate_against_architecture`).
- **Graph export / import** for backup and version control (CLI plus the `export_graph` tool).
- A **shared multi-client backend** (one `--serve` process, thin `--connect` proxies) so several agents share one live graph concurrently, and a **loopback web UI** for humans.
- The **history-walking bootstrap workflow** for recovering knowledge from an existing codebase (`skills/bootstrap-existing-codebase.md`).
- A **graph→docs workflow** that renders captured decisions as a standard ADR set (`skills/generate-adrs-from-graph.md`).
- A **stdio MCP server** (rmcp 1.7, latest negotiated protocol) verified end-to-end (`initialize` → `tools/list` → `tools/call`), and persistence across restarts (on-disk oxigraph).

Against the roadmap in [`tasks/todo.md`](./tasks/todo.md): M0–M3 are complete, M4 is partial (the bootstrap workflow shipped; the `get_focus_stack` tool is deferred), and the M5 shared-backend core is complete.

## Getting started

### Use a release binary (recommended)

Pre-built binaries are published on [GitHub Releases](https://github.com/Trivyn/moosedev/releases) for macOS (Apple Silicon) and Linux (x86-64). This is the supported path for everyone: building from source requires access to the private MOOSE engine (see [Open source & the MOOSE boundary](#open-source--the-moose-boundary)).

### Build from source

Requires a recent Rust toolchain and a checkout of the MOOSE engine at `../moose` (MOOSEDev depends on it via a path dependency, and on its patched `oxigraph` fork).

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

MOOSEDev speaks MCP over **stdio**. Point an MCP client at the binary. For example, in an `.mcp.json` (or your Claude Desktop config):

```json
{
  "mcpServers": {
    "moosedev": {
      "command": "/absolute/path/to/moosedev",
      "args": []
    }
  }
}
```

All tools in the [v1 tool surface](#v1-tool-surface) are available over this transport.

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
never collide. The backend writes the resolved address to
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
  `git diff` shows new records as added lines, reviewable in a PR like any other change;
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
- **Ontology directory** (`MOOSEDEV_ONTOLOGY_DIR`): where the shipped ontologies live. By default MOOSEDev looks for an `ontologies/` directory next to the running binary (the layout of the released tarball), then falls back to the crate's `ontologies/` for `cargo run`. Set this only to load ontologies from a custom location. *Keep the unpacked release bundle together so the binary can find its `ontologies/`.*

## Project layout

```
src/            # the MCP server (Rust): mcp/, graph/, ontology/, alignment/, llm/, api/, …
ui/             # the human-facing web UI (Vite/React); built to ui/dist/ and embedded in the binary
ontologies/     # software-engineering + architecture ontologies + SHACL shapes (.ttl)
skills/         # workflow docs: bootstrap, temporal capture, ADR generation
templates/      # CLAUDE.md template for projects adopting MOOSEDev as memory
spec/           # specification + design of record + upstream engine asks
tasks/          # build checklist / roadmap
tests/          # integration tests
```

## Open source & the MOOSE boundary

- **MOOSEDev** (this repo — the MCP server, tools, ontologies, prompts, docs) is **open source**.
- **The MOOSE engine** remains **closed** for now. Building MOOSEDev *from source* therefore requires access to the private MOOSE repository and its `oxigraph` fork; published **binaries** are the route for everyone else.

Contributions to the open parts of MOOSEDev are welcome. Deep changes to MOOSE behavior are out of scope for external contributors at this stage — but capabilities MOOSEDev needs from the engine are surfaced openly in [`spec/core-moose-asks.md`](./spec/core-moose-asks.md).

## Documentation

- [`spec/MOOSEDev_spec.md`](./spec/MOOSEDev_spec.md) — v1 scope definition
- [`spec/MOOSEDev_design.md`](./spec/MOOSEDev_design.md) — design of record (architecture, decisions, milestones)
- [`spec/core-moose-asks.md`](./spec/core-moose-asks.md) — capabilities to upstream into core MOOSE
- [`CLAUDE.md`](./CLAUDE.md) — design invariants and development practices

## How this was built

MOOSEDev has been written iteratively with AI coding agents under human architectural direction and review. 

## License

MOOSEDev is licensed under the **Apache License 2.0**. The MOOSE engine is proprietary and separately licensed.
