#!/usr/bin/env bash
# Launch the burrow temporal git-walk bootstrap into an isolated store, capturing per-episode via
# gpt-5.4-mini in the codex harness (Lesson c0aa3509). Resumable: append --resume to skip episodes
# already in temporal-applied.log. All long paths are baked in so this can't be mangled by paste.
set -euo pipefail
cd "$(dirname "$0")"
exec .venv/bin/python temporal_bootstrap.py \
  --repo "$HOME/code/burrow" \
  --data-dir "$HOME/.moosedev-stores/burrow-temporal" \
  --backend codex --model gpt-5.4-mini --milestone-every 10 "$@"
