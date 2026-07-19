# MOOSEDev Knowledge-LSP — Neovim client

A plain LSP registration (spec §5.7) — no plugin. Neovim attaches multiple
servers per buffer, so MOOSEDev runs **alongside** rust-analyzer /
typescript-language-server: they keep language intelligence; MOOSEDev adds
the knowledge layer.

## Install

1. Put the `moosedev` binary on your `PATH` (`cargo install --path .` from the
   repo root, or point `cmd` at a build).
2. Add the registration from [`moosedev.lua`](./moosedev.lua) to your config
   (Neovim 0.11+ shown; a classic `nvim-lspconfig` form is in the comments).

The server attaches in repos with a `.moosedev/` directory and autospawns the
shared daemon on first use. There is no `filetypes` list on 0.11+: the server
is silent for files outside its indexing substrate, so new substrate
languages need no client edit.

## What you get

- **Hover** (`K`): the entity dossier — linked decisions/constraints/lessons,
  ratified judgments, churn observations.
- **Code lenses**: role/criticality badges + per-kind record counts +
  no-rationale hotspots (`vim.lsp.codelens.refresh()` to populate). Running a
  lens (`vim.lsp.codelens.run()`) opens the entity in the workbench — the
  command is server-handled via `window/showDocument` (0.10+), no client glue.
- **Diagnostics**: constraint reminders and stale-rationale hints. Neovim
  0.10+ pulls (`textDocument/diagnostic`) and the daemon suppresses its push
  path for pull-capable clients, so nothing double-reports; 0.9 gets push.
  UTF-8 positions negotiate on 0.11+ (0.10 uses UTF-16 — handled server-side).
- **Code actions** (`gra` / `vim.lsp.buf.code_action()`): the v2.3 write path.
  “Link … to this entity”, “Propose role: …”, “Propose criticality: …” — every
  choice files a **proposal** into the ratification queue; nothing touches the
  knowledge graph until a human ratifies it in the workbench inbox.

## Conformance suite

This directory doubles as the scripted conformance client (spec §7):

```sh
clients/nvim/conformance.sh            # builds moosedev, runs headless nvim
```

`conformance.lua` drives a real Neovim LSP client against the daemon —
capabilities, a required non-null dossier hover, code lens, pull diagnostics,
push suppression for pull-capable clients, the server-handled
`moosedev.openEntity` → `window/showDocument` round-trip, code action, and an
executeCommand write round-trip (idempotency included) — exiting
non-zero on any failure. The default target is the public, substrate-covered,
knowledge-bearing `propose_link` definition. Custom `rel_file line col` targets
must likewise be public, substrate-resolved, and knowledge-bearing; an honestly
silent hover is valid product behavior but fails this conformance fixture. The
runner SKIPs cleanly when `nvim` is not installed. It runs against a **scratch
copy** of fixture data, so it never writes proposals into your real project
graph.
