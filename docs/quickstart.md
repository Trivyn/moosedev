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

Add `--codex` to also write `.codex/config.toml`, `--opencode` to install the
opencode push plugin (`.opencode/plugins/moosedev-push.ts` — proactively injects
memory for local models that under-call MCP tools), or `--stdio` for a
single-client (non-shared) config.

## 3. Reload your MCP client

Restart Claude Code (or run `/mcp` to reconnect). The `moosedev` tools —
`get_relevant_context`, `record_important_decision`, `query`, `sparql`, … — are
now available. No LLM or API key is required; MOOSEDev is pure-symbolic by
default.

## 4. Seed the graph

An empty graph has nothing to recall. Seed it from your existing codebase by
asking your agent to run MOOSEDev's bootstrap skill,
`skills/bootstrap-existing-codebase.md` — it walks the agent through recording
the architectural decisions, constraints, and lessons behind the code as typed,
linked, queryable records. From there, capture decisions as you work.

## 5. Commit the memory

```sh
git add .moosedev/kg.nq .mcp.json .gitignore CLAUDE.md
git commit -m "Add MOOSEDev project memory"
```

`.moosedev/kg.nq` is the committed source of truth — canonical, sorted N-Quads
that diff and merge cleanly. A teammate who clones the repo and boots MOOSEDev
gets the project's accumulated memory hydrated automatically.

## Optional: the web UI

The shared backend serves a graph-browsing UI. Find and open it with:

```sh
moosedev --status   # shows the URL
moosedev ui         # opens it in a browser
```
