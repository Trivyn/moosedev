#!/usr/bin/env bash
#
# Build a release MOOSEDev binary with the web UI embedded.
#
# WHY THIS SCRIPT EXISTS
# ----------------------
# The web UI is embedded via the `embedded-frontend` cargo feature, which uses a
# compile-time `include_dir!("$CARGO_MANIFEST_DIR/ui/dist")` (see
# src/api/handlers/static_files.rs). That feature is intentionally NOT a default:
# a fresh checkout has no `ui/dist`, and the macro would fail to compile.
#
# The consequence is a footgun. A plain `cargo build --release` (no feature)
# compiles the `#[cfg(not(feature = "embedded-frontend"))]` fallback, which serves
# `"MOOSEDev UI is not embedded in this build"` and 404s the whole web UI — with no
# build-time error. You only notice when the UI is blank at runtime.
#
# This script does the two steps in the required order (build the UI, THEN build
# the binary with the feature) so you always get a working, UI-embedded binary.
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

echo "==> Building release binary with the embedded-frontend feature …"
cargo build --release --features embedded-frontend

echo "==> Done: ./target/release/moosedev (web UI embedded)."
