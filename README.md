# MOOSEDev

**Structured, long-term project memory for coding agents — a neurosymbolic MCP server that fights comprehension debt.**

> Status: **early development.** The MCP server skeleton is working today (build, run, `ping` over stdio); the knowledge-capture and query tools are landing milestone by milestone. See [Status](#status).

---

## What is MOOSEDev?

MOOSEDev is a [Model Context Protocol](https://modelcontextprotocol.io) (MCP) sidecar that gives coding agents — and eventually humans — a reliable, **structured, queryable, long-term understanding** of a software project.

Its purpose is to combat **comprehension debt**: the gradual loss of shared understanding of *why* a codebase is shaped the way it is. Instead of stuffing ever more history into an LLM's context window, MOOSEDev maintains a typed, auditable **project knowledge graph** — architectural decisions, lessons, constraints, anti-patterns — that an agent can record into and reason over symbolically.

MOOSEDev is built on the **MOOSE** neurosymbolic engine, and is the first host application in a planned family of MOOSE-based tools. MOOSEDev itself is open source; the MOOSE engine is closed for now.

## Why it's different

Most agent "memory" tools store free text and retrieve it with embeddings — they optimize for *sounding* relevant. MOOSEDev optimizes for *being* correct:

- **Symbolic layer is primary.** The LLM is a sensor, not the controller. Deterministic mechanisms — typed capture, concept alignment, graph queries, validation — do the load-bearing work.
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
  │ MOOSEDev  (this repo · open source) │
  │   • MCP server (rmcp)               │
  │   • typed knowledge capture + write │
  │   • durable project knowledge graph │──▶ oxigraph (RDF, on-disk)
  │   • SPARQL · architectural validation│
  └─────────────────┬──────────────────┘
                    │ composes
                    ▼
  ┌────────────────────────────────────┐
  │ MOOSE engine  (closed)              │
  │   • NLQ query + execution traces    │
  │   • alignment (3-tier sensor sieve)  │
  │   • focus stack · local embeddings   │
  └────────────────────────────────────┘
```

MOOSE provides the read/reason side — natural-language query, the alignment engine, conversational focus, execution traces, and local embeddings (Snowflake Arctic-Embed-S via Candle). MOOSEDev provides the host side — the MCP server, the durable knowledge graph and its typed **write** path, a SPARQL endpoint, lightweight validation, and the bootstrap workflow.

## v1 tool surface

| Tool | Purpose | Status |
|------|---------|--------|
| `ping` | Health check (transport smoke test) | ✅ working |
| `record_important_decision` | Capture typed decisions, lessons, constraints, anti-patterns | 🚧 M1 |
| `query` | Natural-language query with reasoning traces | 🚧 M1 |
| `get_relevant_context` | Retrieve knowledge relevant to the current focus | 🚧 M1 |
| `search_session_graph` | Search recorded decisions, lessons, and knowledge | 🚧 M1 / M3 |
| `align_concepts` | Align new concepts to the loaded ontologies | ⏳ M2 |
| `suggest_mappings` | Propose ontology mappings for new concepts | ⏳ M2 |
| `validate_against_architecture` | Lightweight consistency validation of recorded knowledge | ⏳ M3 |
| `get_focus_stack` | Return the current symbolic focus stack | ⏳ M4 |

## Status

This is an active, early-stage build. **M0 (foundation) is complete and verified:**

- `moosedev` links and runs against the MOOSE engine and its RDF store.
- The durable knowledge graph persists across restarts (on-disk oxigraph).
- A working **stdio MCP server** (rmcp 1.7) negotiates protocol `2025-06-18` and serves a `ping` tool — verified end-to-end (`initialize` → `tools/list` → `tools/call`).

**In progress:** M1 — the capture → query vertical slice (`record_important_decision`, `query`, `get_relevant_context`). Roadmap and progress are tracked in [`tasks/todo.md`](./tasks/todo.md); the design of record is [`spec/MOOSEDev_design.md`](./spec/MOOSEDev_design.md).

## Getting started

### Use a release binary (recommended)

Pre-built binaries will be published on GitHub Releases. Building from source requires access to the private MOOSE engine (see [Open source & the MOOSE boundary](#open-source--the-moose-boundary)).

### Build from source

Requires a recent Rust toolchain and a checkout of the MOOSE engine at `../moose` (MOOSEDev depends on it via a path dependency, and on its patched `oxigraph` fork).

```sh
git clone <moosedev>            # this repo
# ensure the MOOSE engine is checked out at ../moose
cargo build --release           # add --offline if the oxigraph fork is already cached
```

The default build bundles a CPU embedding backend (`candle-cpu`) and the Arctic-Embed-S model used by the alignment engine.

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

Today the server exposes `ping`; the knowledge tools above come online as M1+ land.

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
clients are lightweight proxies — they never open the store or load the model.
Lifecycle is manual for now (you start `--serve` yourself); transparent
auto-spawn is planned (see `tasks/todo.md`, M5).

### Configuration

MOOSEDev is configured via environment variables (this surface is filling in as features land):

- **LLM endpoint** (for natural-language `query`): an OpenAI-compatible `base_url` / `api_key` / `model` — point it at a local runtime (e.g. Ollama, LM Studio) or a hosted provider. *Local-first by default; cloud is opt-in.*
- **Data directory** (`MOOSEDEV_DATA_DIR`): where the durable knowledge graph and session database live (runtime state, kept out of version control under `data/`).
- **Socket** (`MOOSEDEV_SOCKET`, shared mode): override the per-data-dir Unix socket path used by `--serve` / `--connect`.
- **Ontology directory** (`MOOSEDEV_ONTOLOGY_DIR`): where the shipped ontologies live (defaults to the crate's `ontologies/`).

## Project layout

```
src/            # the MCP server (Rust): mcp/, graph/, ontology/, llm/
ontologies/     # software-engineering + architecture ontologies (.ttl) [forthcoming]
skills/         # bootstrap-existing-codebase workflow [forthcoming]
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

## License

MOOSEDev is licensed under the **Apache License 2.0**. The MOOSE engine is proprietary and separately licensed.
