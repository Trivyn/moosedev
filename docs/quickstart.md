# Quickstart

Get MOOSEDev running as durable, structured memory for a project in about five
minutes. This is the happy path; see [install.md](install.md) for details and
options.

## 1. Install

Pick one:

```sh
# Script (macOS Apple Silicon, Linux x86-64)
curl -fsSL https://raw.githubusercontent.com/Trivyn/moosedev/main/scripts/install.sh | sh

# or Homebrew
brew install Trivyn/moosedev/moosedev
```

Both download a self-contained binary — no build, no toolchain. Confirm it's on
your PATH:

```sh
moosedev --help
```

If the script warns that its bin dir isn't on your `PATH`, add the line it prints
to your shell profile and re-open the terminal.

## 2. Initialize your project

From the root of the project you want MOOSEDev to remember:

```sh
cd /path/to/your/project
moosedev init
```

This is non-destructive — it never overwrites an existing `CLAUDE.md` or clobbers
other MCP servers in your `.mcp.json`. It writes:

- **`.mcp.json`** — registers the `moosedev` MCP server (shared `--connect` mode,
  so Claude Code, Codex, and the web UI share one live graph).
- **`.gitignore`** — ignores the derived store/vector cache but keeps the
  canonical `.moosedev/kg.nq` under version control.
- **`CLAUDE.md`** — the memory-workflow template (only if you don't have one).
- **`.moosedev/`** — where the graph lives.

Flags compose, and repeated runs merge safely. Add only the integrations you use:

- `--codex` — also write `.codex/config.toml` for the Codex CLI
- `--opencode` — install the opencode push plugin (proactively injects memory
  for local models that under-call MCP tools)
- `--claude-hooks` — install the Claude Code push/gate/capture hooks
- `--zed` / `--vscode` — project-local Knowledge-LSP editor settings (see
  [step 6](#6-enable-the-code-layer--editors))
- `--stdio` — single-client (non-shared) config
- `--binary PATH` / `--data-dir DIR` / `--force` — pin the binary path, choose
  the data dir, or replace existing MOOSEDev entries

## 3. Reload your MCP client

Restart Claude Code (or run `/mcp` to reconnect). The `moosedev` tools —
`get_relevant_context`, `record_important_decision`, `get_entity_dossier`,
`query`, `sparql`, … — are now available. No LLM or API key is required; MOOSEDev is pure-symbolic by
default.

## 4. Seed the graph

An empty graph has nothing to recall. `moosedev init` installs the bootstrap
skill into `.claude/skills/` — so just ask your agent to bootstrap this repo's
memory. It will walk through recording the architectural decisions, constraints,
and lessons behind the code as typed, linked, queryable records. From there,
capture decisions as you work.

## 5. Commit the memory

```sh
git add .moosedev/kg.nq .mcp.json .gitignore CLAUDE.md
git commit -m "Add MOOSEDev project memory"
```

`.moosedev/kg.nq` is the committed source of truth — canonical, sorted N-Quads
that diff and merge cleanly. A teammate who clones the repo and boots MOOSEDev
gets the project's accumulated memory hydrated automatically.

## 6. Enable the code layer + editors

The steps above give you the memory core. The code layer additionally ties that
memory to your **source code** — records pinned to the functions and types they
govern — and surfaces it in your editor. Index the code, then mint the public
entities into the graph (both preview by default; `mint` opens the single-writer
store directly, so stop a shared backend first):

```sh
moosedev index          # build the code substrate (Rust, TypeScript, Python)
moosedev mint           # preview the public-entity skeleton
moosedev mint --apply   # write it
```

With that in place, the **Knowledge-LSP** (`moosedev lsp`, sharing the same
daemon) gives editors dossier hovers, role/criticality lenses, knowledge
diagnostics, and proposal code actions. Wire up your editor with
`moosedev init --zed` / `--vscode`, or the stanzas in
[Zed](../clients/zed/README.md), [VS Code](../clients/vscode/README.md),
[Neovim](../clients/nvim/README.md), and [Emacs](../clients/emacs/README.md).

Optionally, `moosedev classify` (then `--apply`) proposes role and criticality
judgments from code evidence. Proposals — classifier or editor-originated —
change nothing until you ratify them in the workbench inbox (below).

## Optional: the web UI

The shared backend serves the human workbench: browse the graph, and review the
**ratification inbox** where proposed records and links await acceptance. Find
and open it with:

```sh
moosedev --status   # shows the URL
moosedev ui         # opens it in a browser
```
