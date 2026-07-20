#!/usr/bin/env bash
# MOOSEDev Knowledge-LSP conformance runner (spec §7): headless Neovim as the
# scripted conformance client, against a SCRATCH copy of this repo so the
# write-path round-trip never files a proposal into the real project graph.
#
#   clients/nvim/conformance.sh [rel_file line col]
#
# Defaults to the `propose_link` definition in src/graph/proposals.rs — a
# public, substrate-covered entity with a rich record neighborhood.
set -euo pipefail

if ! command -v nvim >/dev/null 2>&1; then
  echo "SKIP: neovim not installed (brew install neovim)"
  exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Target file: the default position is resolved from the immutable scratch
# snapshot below, so source, line numbers, index, and binary all agree.
REL_FILE="${1:-src/graph/proposals.rs}"
if [[ $# -ge 3 ]]; then
  LINE="$2"
  COL="$3"
fi

# Scratch copy: the current tracked + untracked, non-ignored working tree
# (including .moosedev/kg.nq). Derived caches are rebuilt in scratch; copying
# them could combine the current source with an older index. The source repo is
# never indexed or otherwise mutated by this conformance run.
SCRATCH="$(mktemp -d /tmp/moosedev-conformance.XXXXXX)"
DAEMON_PID=""
cleanup() {
  local pidfile="$SCRATCH/repo/.moosedev/moosedev-serve.pid"
  if [[ -f "$pidfile" ]]; then
    kill "$(cat "$pidfile")" 2>/dev/null || true
  elif [[ -n "$DAEMON_PID" ]]; then
    kill "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$SCRATCH"
}
trap cleanup EXIT

mkdir -p "$SCRATCH/repo"
git -C "$REPO_ROOT" ls-files --cached --others --exclude-standard -z \
  | tar -C "$REPO_ROOT" --null -T - -cf - \
  | tar -x -C "$SCRATCH/repo"

if [[ ! -f "$SCRATCH/repo/$REL_FILE" ]]; then
  echo "FAIL: target file is not part of the working-tree snapshot: $REL_FILE" >&2
  exit 1
fi
if [[ $# -lt 3 ]]; then
  LINE="$(grep -n "^pub fn propose_link(" "$SCRATCH/repo/$REL_FILE" | head -1 | cut -d: -f1)"
  COL=8 # the identifier after `pub fn `
  if [[ -z "$LINE" ]]; then
    echo "FAIL: could not locate propose_link in $REL_FILE" >&2
    exit 1
  fi
fi

# Give index production a stable HEAD without touching the source repository.
git -C "$SCRATCH/repo" init -q
git -C "$SCRATCH/repo" -c user.email=conformance@local -c user.name=conformance add -A
git -C "$SCRATCH/repo" -c user.email=conformance@local -c user.name=conformance \
  commit -q -m snapshot

echo "building current working-tree binary and indexing scratch snapshot…"
# Build from the source worktree so Cargo can resolve MOOSEDev's intentional
# sibling `../moose` path dependency. The scratch source is an exact snapshot
# of this same worktree, so the binary, positions, and freshly produced index
# still describe one coherent artifact.
cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --quiet
export PATH="$REPO_ROOT/target/debug:$PATH"
# rust-analyzer also resolves Cargo metadata while producing the scratch SCIP
# index. Recreate the build's sibling dependency layout without copying or
# mutating that dependency.
ln -s "$REPO_ROOT/../moose" "$SCRATCH/moose"
(cd "$SCRATCH/repo" && CARGO_NET_OFFLINE=true moosedev index >/dev/null)

# These embedding caches are reconciled against the scratch graph at startup,
# so a stale or incomplete copy is safe and still avoids a multi-minute cold
# embedding pass for the versioned project graph. The code substrate is not
# copied: it must always be rebuilt from the scratch source above.
for cache in instance-vectors.db ontology-vectors.db ontology-vectors.db.fingerprint; do
  if [[ -f "$REPO_ROOT/.moosedev/$cache" ]]; then
    cp "$REPO_ROOT/.moosedev/$cache" "$SCRATCH/repo/.moosedev/$cache"
  fi
done

# Pre-warm the daemon: first-start hydration of kg.nq outlives the stdio
# shim's autospawn timeout, so start it explicitly and wait for the LSP
# socket before Neovim connects.
#
# MOOSEDEV_HTTP_ADDR is pinned to an ephemeral port: the binary's dotenv
# fallback loads the SOURCE repo's .env (compile-time manifest dir), and a
# fixed port there would collide with the real daemon. Explicit env wins
# over .env, and openEntity's showDocument round-trip needs a live HTTP UI.
echo "starting scratch daemon (kg.nq hydration)…"
cd "$SCRATCH/repo"
MOOSEDEV_HTTP_ADDR="127.0.0.1:0" nohup moosedev --serve >"$SCRATCH/serve-stdout.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 240); do
  if [[ -S "$SCRATCH/repo/.moosedev/moosedev-lsp.sock" ]]; then
    break
  fi
  sleep 1
done
if [[ ! -S "$SCRATCH/repo/.moosedev/moosedev-lsp.sock" ]]; then
  echo "FAIL: scratch daemon never opened the LSP socket" >&2
  tail -20 "$SCRATCH/repo/.moosedev/moosedev-serve.log" 2>/dev/null || tail -20 "$SCRATCH/serve-stdout.log" >&2 || true
  exit 1
fi
# The openEntity round-trip needs the workbench address; HTTP binds alongside
# the LSP listener, so this wait is normally instant.
for _ in $(seq 1 30); do
  if [[ -s "$SCRATCH/repo/.moosedev/http.addr" ]]; then
    break
  fi
  sleep 1
done
if [[ ! -s "$SCRATCH/repo/.moosedev/http.addr" ]]; then
  echo "FAIL: scratch daemon never published the HTTP addr file (openEntity needs it)" >&2
  grep -i "http" "$SCRATCH/serve-stdout.log" >&2 || tail -20 "$SCRATCH/serve-stdout.log" >&2 || true
  exit 1
fi

echo "conformance target: $REL_FILE:$LINE:$COL (scratch: $SCRATCH)"
exec_status=0
nvim --headless -l "$SCRIPT_DIR/conformance.lua" "$SCRATCH/repo" "$REL_FILE" "$LINE" "$COL" \
  || exec_status=$?
exit "$exec_status"
