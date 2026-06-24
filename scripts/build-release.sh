#!/usr/bin/env bash
#
# Build a release MOOSEDev binary with the web UI embedded.
#
# WHY THIS SCRIPT EXISTS
# ----------------------
# The default build embeds the web UI via the `embedded-frontend` cargo feature,
# which uses a compile-time `include_dir!("$CARGO_MANIFEST_DIR/ui/dist")` (see
# src/api/handlers/static_files.rs). A fresh checkout has no `ui/dist`, so the
# macro fails to compile until the frontend has been built.
#
# This script does the two required steps in order (build the UI, THEN build the
# default binary) so you always get a working, UI-embedded release binary. For a
# backend-only binary that does not require frontend assets, build with the
# `headless` feature.
#
# Usage: scripts/build-release.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "==> Building web UI into ui/dist …"
if [ ! -d ui/node_modules ]; then
  echo "    ui/node_modules missing — installing UI dependencies first …"
  npm --prefix ui install
fi
npm --prefix ui run build

echo "==> Building release binary with the default embedded frontend …"
cargo build --release

echo "==> Done: ./target/release/moosedev (web UI embedded)."
