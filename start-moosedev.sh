#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

: "${MOOSEDEV_DATA_DIR:="$ROOT/.moosedev"}"
: "${MOOSEDEV_ONTOLOGY_DIR:="$ROOT/ontologies"}"
: "${MOOSEDEV_LLM_BASE_URL:="http://localhost:1234/v1"}"
: "${MOOSEDEV_LLM_API_KEY:="lm-studio"}"
: "${MOOSEDEV_BIN:="$ROOT/target/release/moosedev"}"
: "${MOOSEDEV_LLM_MODEL:="gemma-4-26b-a4b-it-mlx"}"

if [[ -z "${MOOSEDEV_LLM_MODEL:-}" ]]; then
  cat >&2 <<'EOF'
MOOSEDEV_LLM_MODEL is required.

Set it to the exact model ID loaded by your OpenAI-compatible server, for example:
  MOOSEDEV_LLM_MODEL=<loaded-model-id> ./start-moosedev.sh

Defaults:
  MOOSEDEV_LLM_BASE_URL=http://localhost:1234/v1
  MOOSEDEV_LLM_API_KEY=lm-studio
EOF
  exit 2
fi

if [[ ! -x "$MOOSEDEV_BIN" ]]; then
  cat >&2 <<EOF
MOOSEDev binary not found or not executable:
  $MOOSEDEV_BIN

Build it first with:
  cargo build --release

Or set MOOSEDEV_BIN to another executable path.
EOF
  exit 2
fi

export MOOSEDEV_DATA_DIR
export MOOSEDEV_ONTOLOGY_DIR
export MOOSEDEV_LLM_BASE_URL
export MOOSEDEV_LLM_API_KEY
export MOOSEDEV_LLM_MODEL

exec "$MOOSEDEV_BIN" --serve "$@"
