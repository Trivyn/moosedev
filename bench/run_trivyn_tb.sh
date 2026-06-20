#!/usr/bin/env bash
# Temporal git-walk bootstrap of the trivyn corpus (private; graph stays local in BENCH_HOME-style
# store, never committed) via gpt-5.4-mini/codex. Large history (~255 decision-bearing episodes),
# so snapshot often and use --resume to chunk it. Launch via `!`.
#   smoke:  bash run_trivyn_tb.sh --limit 20
#   full:   bash run_trivyn_tb.sh            (resume a partial run by appending --resume)
set -euo pipefail
cd "$(dirname "$0")"
exec .venv/bin/python temporal_bootstrap.py \
  --repo "$HOME/code/moosedev_benches/trivyn" \
  --data-dir "$HOME/.moosedev-stores/trivyn-temporal" \
  --backend codex --model gpt-5.4-mini --milestone-every 25 "$@"
