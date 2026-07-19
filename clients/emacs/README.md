# MOOSEDev Knowledge-LSP for Emacs

One file, two registrations: load `moosedev.el` and use whichever LSP client
you already run. All behavior lives in the daemon (`moosedev lsp` is a stdio
relay that autospawns it); the stanzas here contain no logic.

```elisp
;; init.el
(load "/path/to/moosedev/clients/emacs/moosedev.el")
```

The `moosedev` binary must be on `exec-path` (or set `moosedev-lsp-binary`).
Activation is gated by a `.moosedev` directory in the project root; there is
no per-language mode list — the server is silent for files outside its
indexing substrate, so new substrate languages need no client edit.

## Choosing a client

| | eglot (built-in, Emacs 29+) | lsp-mode |
|---|---|---|
| Hover dossiers | ✅ (ElDoc; install `markdown-mode` for rendering) | ✅ |
| Knowledge diagnostics | ✅ Flymake | ✅ |
| Proposal code actions | ✅ `M-x eglot-code-actions` | ✅ |
| Code lenses (badges) | ❌ eglot has no codeLens (GNU bug#73452) | ✅ on by default (9.0+) |
| Runs alongside rust-analyzer etc. | ❌ one server per buffer — MOOSEDev *replaces* the language server (start it deliberately with `M-x moosedev-eglot`) | ✅ registered `:add-on? t` |

If you want the ambient layer (lenses + co-resident language server), use
lsp-mode. If you want a zero-install look at hover/diagnostics/actions,
eglot works out of the box.

## Version floors

- **Pull diagnostics**: eglot ≥ 1.20 (Jan 2026; `M-x package-install eglot`
  upgrades the built-in copy) or lsp-mode ≥ 10. Older clients still get
  diagnostics — the daemon pushes for clients that don't advertise pull.
- **UTF-8 positions**: eglot ≥ 1.12, lsp-mode ≥ 10 (older versions negotiate
  UTF-16; the server falls back automatically).
- Lens/code-action commands (`moosedev.openEntity`) are server-handled: the
  daemon opens the workbench in your browser via `window/showDocument`. Both
  clients support it; nothing to configure.

## Smoke test

```sh
emacs -Q -l clients/emacs/moosedev.el path/to/project/src/main.rs
# eglot:    M-x moosedev-eglot   (root-gated; errors outside a .moosedev project)
# lsp-mode: install lsp-mode first, then M-x lsp
```
