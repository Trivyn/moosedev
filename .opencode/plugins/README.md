# MOOSEDev Push Plugin for opencode

`moosedev-push.ts` proactively injects relevant MOOSEDev project-memory records into opencode's model context. It is intended for local/self-hosted models that often do not call MCP memory tools on their own.

## Install

Keep the plugin in this directory:

```text
.opencode/plugins/moosedev-push.ts
```

Run opencode normally. Do not use `--pure`; opencode disables plugins in pure mode.

## Configuration

Set the project memory store explicitly:

```sh
export MOOSEDEV_DATA_DIR=/path/to/project/.moosedev
```

Optional:

```sh
export MOOSEDEV_BIN=/absolute/path/to/moosedev
```

If `MOOSEDEV_BIN` is unset, the plugin runs `moosedev` from `PATH`.

The plugin calls:

```sh
moosedev --connect
```

with:

```sh
MOOSEDEV_DATA_DIR=/path/to/project/.moosedev
MOOSEDEV_NO_AUTOSPAWN=1
```

Start a shared backend separately when needed:

```sh
MOOSEDEV_DATA_DIR=/path/to/project/.moosedev moosedev --serve
```

## How It Works

- After file-oriented tools run, the plugin records a rolling working set of recently touched project files.
- Before each model turn, it builds a short topic from that working set and calls MOOSEDev `get_relevant_context` with `{ topic, limit: 5 }`.
- New records are prepended to the system prompt under:

```text
## Relevant recorded project knowledge (MOOSEDev)
```

- Records are deduplicated by IRI across turns, so the injected block does not grow with repeated context.
- Missing config, missing binary, down backend, parse errors, and timeouts are graceful no-ops with a one-time warning.

## Validation

1. Run opencode without `--pure` against a local model on a project with a `.moosedev` store.
2. Touch files related to a recorded constraint by reading or editing them.
3. Inspect the outgoing system prompt or debug transcript and confirm the MOOSEDev block is injected.
4. Confirm the model acts on the record without calling a memory tool.
5. Unset `MOOSEDEV_DATA_DIR` and confirm the session continues unchanged with a warning.
6. Repeat a second turn over the same files and confirm previously injected IRIs are not repeated.
