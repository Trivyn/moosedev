# Harness review fixes

The accepted conversational harness decision and requirements govern these bug
fixes. The graph remains canonical; this file tracks implementation and evidence.

- [x] Verify review findings against source and governing knowledge.
- [x] Bound action observations and paginate capture evidence without losing obligations.
- [x] Restrict command reads and prove secrets outside the allowed view are inaccessible.
- [x] Protect browser-facing mutations and make daemon review preflight exact and recoverable.
- [x] Fix completed-task interruption, final-review freshness, and input delivery defects.
- [x] Address TUI rendering, panic reporting, and startup/recovery defects.
- [x] Capture Lessons, validate graph, run focused regressions and required build/check gates.

Verification must exercise large files and command output, populated knowledge,
accepted final proposals, adversarial browser requests, protected reads, and
interruption/recovery—not only successful no-proposal smoke tests.

Recorded Lessons:

- Capture evidence needs durable paging, not just a prompt size check:
  https://moosedev.dev/kg/Lesson/f718ec89-cbc1-468e-9473-38ff955b8464
- Read-only shell confinement does not protect private data:
  https://moosedev.dev/kg/Lesson/f242a075-a3bb-42d7-923e-e827df8152e4

Both Lessons are linked to their code entities (`capture`, `snapshot_source`).
Architecture validation: conforms, zero violations; 177 existing advisory links.

Implementation notes:

- Capture pages use exact event/UTF-8 byte cursors, frozen checkpoint ends and
  observed file sets. Cursor commits follow successful assessment and durable
  proposal acknowledgment. A full checkpoint remains pending until every page
  is processed; headless human review and interactive import preserve offsets.
- Action observations have bounded head/tail previews; required graph evidence
  and current source remain complete. `inspect(event,offset)` reads journal
  detail without rerunning commands. Repair space is reserved in every packet.
- Final non-governing review can preserve checks only with the daemon's frozen
  before/after revision attestation. Missing proof or unrelated writes retain
  the conservative renewed-approval gate.
- Byte-identical journal checkpoints skip redundant writes/fsyncs. Changed
  transitions still atomically publish and sync the full task journal; no
  pre-effect intent or durability boundary was removed.

Verification (2026-09-06):

- Harness library: 39 passed; five OS-gated cases also passed explicitly on macOS.
- Runner: 19 passed; confined completion also passed explicitly. The optional
  live-model smoke test was not rerun during this review fix.
- Session: 6 passed. Real daemon: 15 passed, plus focused reruns proving frozen
  rejection remains possible after injected lifecycle edges or a vanished target.
- Existing proposal and supersession suites: 13 and 15 passed. Browser policy
  units: 2 passed. Link acceptance/replay exercises the corrected lock ordering.
- `cargo build --features harness --bins`,
  `cargo clippy --features harness --all-targets -- -D warnings`,
  `cargo check --no-default-features --lib`, `cargo fmt --all --check`, and
  `git diff --check` passed.

Operational limits: Linux confinement was updated but not executed on this Mac.
Filtered command snapshots do not include sibling path dependencies such as
`../moose`. Both daemon and harness must restart with rebuilt binaries to use
the updated HTTP boundary and review protocol.
