# MOOSEDev Reference

This page collects operational details that are useful after the
[quickstart](quickstart.md). For installation and source-build instructions, see
[Installing MOOSEDev](install.md).

## MCP tools

MOOSEDev exposes the following tools over MCP. Knowledge writes are typed and
validated against the loaded ontology. Read tools query the graph instead of
dumping project history into the model context.

### Recall and reasoning

| Tool | Purpose |
| --- | --- |
| `get_relevant_context` | Retrieve current project knowledge by topic, or omit the topic for a broad inventory. History is opt-in. |
| `query` | Ask one focused natural-language question and receive an answer with a symbolic reasoning trace. |
| `sparql` | Run a read-only SPARQL query. `SELECT` and `ASK` return JSON; `CONSTRUCT` and `DESCRIBE` return N-Triples. |
| `get_entity_dossier` | Retrieve the decisions, constraints, lessons, judgments, and observations that govern one code entity. |
| `get_provenance` | Find which agent recorded an item and when. |

For ordinary recall, start with `get_relevant_context`, move to `query` when
relationships need synthesis, and use `sparql` for exact or exhaustive reads.

### Capture and lifecycle

| Tool | Purpose |
| --- | --- |
| `record_important_decision` | Record a typed decision, lesson, constraint, pattern, anti-pattern, or requirement. |
| `supersede_decision` | Replace a record while preserving the prior version and the rationale for the change. |
| `retract_decision` | Deprecate a record that no longer applies and has no replacement. |
| `relate` | Add an ontology-legal relationship between existing records. |
| `link_code` | Link a record to a code entity by source position or stable symbol. |
| `declare_component_paths` | Map repository paths to an existing system component. |

Use supersession or retraction to correct the graph. Do not create silent
duplicates or delete historical knowledge.

### Alignment, policy, and review

| Tool | Purpose |
| --- | --- |
| `align_concepts` | Align a new term to the best matching ontology class, with the deciding sensor and rationale. |
| `suggest_mappings` | Return ranked ontology-class candidates for human review. |
| `suggest_links` | Suggest ontology-legal relationships between records. Suggestions are not written automatically. |
| `evaluate_policy` | Evaluate a host event through the shared symbolic push, gate, and capture policy. |
| `capture_decision_point` | Create a grounded proposed decision and queue its code links for review. |
| `pending_ratifications` | List proposed records and links waiting for human review. |
| `validate_against_architecture` | Validate recorded knowledge against the architecture SHACL shapes. |

### Operations

| Tool | Purpose |
| --- | --- |
| `export_graph` | Export RDF from the live store as N-Quads, N-Triples, or Turtle. |
| `ping` | Check MCP transport health. |

## Server modes

### Generated configuration

The recommended setup is:

```sh
cd /path/to/project
moosedev init
```

`init` creates a project MCP entry in shared `--connect` mode. It can also add
Codex, Zed, VS Code, OpenCode, and Claude Code integrations. Run
`moosedev --help` for the current flags. Repeated runs merge safely; `--force` replaces
existing MOOSEDev entries, and `--stdio` selects the single-client mode.

### Manual stdio configuration

For one client, configure the executable as a plain stdio MCP server:

```json
{
  "mcpServers": {
    "moosedev": {
      "command": "/absolute/path/to/moosedev",
      "args": [],
      "env": {
        "MOOSEDEV_DATA_DIR": "/path/to/project/.moosedev"
      }
    }
  }
}
```

The process opens the project store directly. Because the RocksDB-backed store
has one writer, a second direct process cannot open the same data directory.

### Shared daemon

Use one backend when several agents, editors, or terminals need the same graph:

```sh
MOOSEDEV_DATA_DIR=/path/to/project/.moosedev moosedev --serve
```

Configure each MCP client as a lightweight proxy:

```json
{
  "mcpServers": {
    "moosedev": {
      "command": "/absolute/path/to/moosedev",
      "args": ["--connect"],
      "env": {
        "MOOSEDEV_DATA_DIR": "/path/to/project/.moosedev"
      }
    }
  }
}
```

`--serve` and every `--connect` process must resolve the same
`MOOSEDEV_DATA_DIR`, or an explicitly shared `MOOSEDEV_SOCKET`. The default
socket is `<MOOSEDEV_DATA_DIR>/moosedev.sock`. A `--connect` process starts a
detached backend when none is listening unless `MOOSEDEV_NO_AUTOSPAWN=1` is
set.

The daemon also hosts the Knowledge-LSP and, by default, a web workbench on an
ephemeral loopback port. Use `moosedev --status` to inspect the backend or
`moosedev ui` to open the workbench. `moosedev --serve --open` starts the daemon
and opens the workbench when ready.

## Code indexing

The code layer supports Rust, TypeScript, and Python through SCIP producers.
Build the substrate, preview graph changes, then apply the ones you want:

```sh
moosedev index
moosedev mint
moosedev mint --apply
moosedev classify
moosedev classify --apply
```

`mint` creates durable code entities. `classify` creates role and criticality
proposals, which do not affect authoritative dossiers or debt metrics until a
human ratifies them in the workbench. Both commands are dry runs unless
`--apply` is present. They open the single-writer store even in dry-run mode, so
stop the shared daemon before running them.

Run `moosedev index` after significant source changes. `moosedev init` can
install a post-commit refresh hook. To debug position resolution, use 1-based
line numbers and UTF-8 byte columns:

```sh
moosedev resolve path/to/file.rs 42:9
```

Editor clients connect with `moosedev lsp`. Setup instructions are available
for [Zed](../clients/zed/README.md), [Neovim](../clients/nvim/README.md),
[VS Code](../clients/vscode/README.md), and
[Emacs](../clients/emacs/README.md).

## Bootstrap workflows

A new graph contains no project-specific knowledge. MOOSEDev provides two ways
to seed it.

### Snapshot bootstrap

The snapshot workflow surveys the current codebase and records its architecture,
decisions, constraints, lessons, and patterns as typed, linked knowledge.
`moosedev init` installs the workflow as an agent skill. Ask your coding agent to
"bootstrap this repo's memory into MOOSEDev." Run `moosedev skills` to locate
the shipped workflow documents.

### Temporal bootstrap

Temporal bootstrap walks Git history from oldest to newest and invokes a coding
agent once for each decision-bearing commit. Captures receive the commit's real
date and author, while mechanical commits are filtered out.

```sh
# Preview triage without invoking agents.
moosedev bootstrap --temporal --repo . --dry-run

# Capture a bounded batch, then resume later.
moosedev bootstrap --temporal --repo . --data-dir .moosedev --limit 5
moosedev bootstrap --temporal --repo . --data-dir .moosedev --resume
```

The default agent is `claude`; use `--agent codex` to select Codex. Processing is
sequential, so use `--limit` and `--resume` for larger repositories. Temporal
bootstrap needs exclusive access to the store.

After either workflow, commit `.moosedev/kg.nq` with the code.

## Project memory and graph files

`<MOOSEDEV_DATA_DIR>/kg.nq` is the canonical, sorted N-Quads serialization of
the asserted project graph. Inferred edges are recreated locally. MOOSEDev
exports the file after graph writes and reconciles it with the local store at
startup, so a fresh clone can hydrate its derived cache from version control.

Commit only `kg.nq`. RocksDB, vector indexes, socket files, and other runtime
state are local caches:

```gitignore
/.moosedev/*
!/.moosedev/kg.nq
```

If the file and store both changed since their last synchronization, MOOSEDev
merges them as a union and warns. If `kg.nq` is invalid, including unresolved
merge-conflict markers, startup stops instead of overwriting it. Provenance is
kept in a separate local graph and is not included in the committed project
file.

For an offline backup or transfer, stop the daemon and use the CLI:

```sh
moosedev export backup.nq
moosedev import backup.nq --format nq
```

Export defaults to `--format nq --graph project`. Import defaults to
`--format ttl --graph project --mode patch`; specify `--format nq` for canonical backups.
The supported formats are `nq`, `nt`, and `ttl`. Graph scopes are `project`,
`provenance`, and `all`. Import mode `patch` inserts missing quads, while
`replace` fully restores the selected scope. N-Quads is byte-canonical;
N-Triples is deterministic after graph names are dropped; Turtle is intended
for human reading.

When the daemon is running, export through the `export_graph` MCP tool or the
workbench so the request uses the live store.

## Environment variables

MOOSEDev looks for a `.env` from the current directory upward and stops after
the first project root. The nearest `.env` wins, and explicit process
environment values take precedence. A project root contains `.git`,
`Cargo.toml`, `package.json`, `pyproject.toml`, `setup.py`, `setup.cfg`, or
`requirements.txt`.

| Variable | Purpose |
| --- | --- |
| `MOOSEDEV_DATA_DIR` | Durable store and runtime directory. Keep this identical across a daemon and its clients. |
| `MOOSEDEV_SOCKET` | Override the Unix socket used by `--serve` and `--connect`. |
| `MOOSEDEV_HTTP_ADDR` | Set the workbench bind address. The default is `127.0.0.1:0`. |
| `MOOSEDEV_NO_HTTP` | Disable the web workbench when truthy. |
| `MOOSEDEV_NO_LSP` | Disable the daemon's Knowledge-LSP endpoint when truthy. |
| `MOOSEDEV_NO_AUTOSPAWN` | Prevent `--connect` and `ui` from starting a daemon automatically. |
| `MOOSEDEV_ONTOLOGY_DIR` | Use a custom ontology directory. Release bundles normally find their colocated `ontologies/` directory automatically. |
| `MOOSEDEV_MODEL_DIR` | Use a directory containing `models/` for local Arctic-Embed-S weights. Without a bundled or configured copy, weights download on first use. |
| `MOOSEDEV_SCIP_PRODUCER` | Override the Rust SCIP command. The default is `rust-analyzer`. |
| `MOOSEDEV_SCIP_TYPESCRIPT` | Override the TypeScript SCIP command. The default invokes `npx --yes @sourcegraph/scip-typescript`. |
| `MOOSEDEV_SCIP_PYTHON` | Override the Python SCIP command. The default invokes `npx --yes @sourcegraph/scip-python` and requires Python 3.10 or newer on `PATH`. |
| `MOOSEDEV_LLM_BASE_URL` | Opt into an OpenAI-compatible endpoint for assisted query, chat, and Story narration. Without it, assistance remains symbolic. |
| `MOOSEDEV_LLM_API_KEY` | API key for the configured LLM endpoint. |
| `MOOSEDEV_LLM_MODEL` | Model name sent to the configured endpoint. |
| `MOOSEDEV_LLM_ASSIST_LEVEL` | Select the configured assistance level. |
| `MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS` | Declare the model context capacity. The default is `32768`. |
| `MOOSEDEV_LLM_STRUCTURED_OUTPUT` | Set structured output to `auto`, `required`, or `disabled`. The default is `auto`. |

Setting `MOOSEDEV_HTTP_ADDR` to a non-loopback interface exposes the workbench
to the network. Do so only when that access is intentional.
