# MOOSEDev Knowledge-LSP for Zed

This is a thin Zed extension that names the `moosedev lsp` binary. The MOOSEDev
daemon does all language-server work.

## Development install

Install the `wasm32-wasip1` Rust target first:

```sh
rustup target add wasm32-wasip1
```

In Zed, run `zed: install dev extension` and choose this `clients/zed`
directory.

## Project settings

`moosedev init --zed` creates the following project-local `.zed/settings.json`
entry, which enables the optional diagnostics:

```json
{ "lsp": { "moosedev": { "initialization_options": { "diagnostics": { "constraints": true, "staleRationale": true } } } } }
```

Hover and diagnostics reflect saved, indexed state. Run `moosedev index` after
significant changes; `moosedev init` also offers a post-commit hook to automate
that refresh.
