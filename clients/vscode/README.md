# MOOSEDev Knowledge-LSP for VS Code

A thin "picture frame" extension (spec §5.7): it launches `moosedev lsp` and
registers it as an LSP client. All behavior — hover dossiers, knowledge
diagnostics, code lenses, proposal code actions — lives in the daemon and is
identical across editors.

## Requirements

- VS Code ≥ 1.91 (`vscode-languageclient` 10.x floor)
- The `moosedev` binary on PATH (or set `moosedev.serverPath`)
- A workspace with a `.moosedev` directory (the extension activates on it)

## Build and install locally

```sh
cd clients/vscode
npm install
npm run package          # builds dist/extension.js and produces moosedev-0.1.0.vsix
code --install-extension moosedev-0.1.0.vsix
```

For development: open `clients/vscode` in VS Code and press F5 (Extension
Development Host). `npm run check` typechecks; `npm run build` bundles.

Marketplace publication is deliberately out of scope for now.

## Settings

| Setting | Default | Effect |
|---|---|---|
| `moosedev.serverPath` | `""` | Binary path; empty resolves `moosedev` from PATH |
| `moosedev.diagnostics.constraints` | `true` | Information diagnostics on constrained entities |
| `moosedev.diagnostics.staleRationale` | `true` | Hint diagnostics for stale rationale |
| `moosedev.codeLens` | `true` | Knowledge badges above declarations |
| `moosedev.nudge` | `true` | Once-per-session pending-ratifications notice |
| `moosedev.trace.server` | `off` | LSP traffic tracing |

Settings changes restart the client (initialization options are read once at
initialize). `moosedev init --vscode` seeds `.vscode/settings.json` with the
defaults for discoverability (skipped with a note when the file uses JSONC
comments, which strict merging cannot preserve).

## Notes

- Diagnostics are pulled (`textDocument/diagnostic`) on open, change, and
  save; the server suppresses its push path for pull-capable clients, so
  there is no double reporting.
- Code-lens clicks (`moosedev.openEntity`) are handled server-side: the
  daemon opens the workbench page via `window/showDocument`. No commands are
  registered by the extension.
- VS Code negotiates UTF-16 positions only (vscode-languageserver-node
  #1224); the server falls back from UTF-8 automatically.
- No language list is baked in: the client attaches to all files under the
  knowledge folder and the server stays silent outside its indexing
  substrate (`src/code/substrate/lang/`).
- Multi-root workspaces get one client per `.moosedev` folder, each scoped
  to its own root's daemon; folders added or removed at runtime start and
  stop their clients accordingly.
