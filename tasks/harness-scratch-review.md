# Harness scratch and recovery review

Bug fixes under the accepted conversational harness requirements and decision
30b7dcc9. The graph remains canonical.

- [x] Reuse task build artifacts safely across commands and normal journal reloads; keep source snapshots current at stable paths; clean scratch at terminal task lifecycle boundaries.
- [x] Fix process-group cleanup and PATH/configuration error handling.
- [x] Make final-review ordering and post-review completion failures recoverable without false capture confirmations.
- [x] Bound capture failures and plan text; saturate context arithmetic; journal discarded policy edits; consolidate headless page confirmations.
- [x] Remove checkpoint GET side effects and clarify snapshot exclusions/size limits.
- [x] Record and link the build-lifecycle Lesson; validate the graph.
- [x] Run scoped executor/runner/session/daemon regressions, explicit macOS sandbox tests, build, clippy, formatting, and no-default-features check.

Verification must prove actual Cargo cache reuse, source refresh/deletion, timeout
and cancellation safety, task cleanup, hostile scratch entries, and review recovery.

Recorded Lesson: **Verification gates need a usable build lifecycle**,
https://moosedev.dev/kg/Lesson/b0f00e6c-342d-4fa7-98ba-e56f8f94ce7b.
Linked to the active-agency component, governing AD30b7dcc9, and
`command_with_progress` / `run_command`. Graph conforms: zero violations, 177
existing advisory links.

Implementation:

- Stable task source/build paths and source mtimes preserve Cargo fingerprints.
  Source is freshly copied before execution; command Cargo configuration and
  temporary homes are recreated and removed. No host secrets are exposed.
- Scratch is locked during execution; safe cleanup ignores aliases, preserves
  internal Cargo hardlinks, removes external aliases, repairs owned directories
  with poisoned permissions, and uses constant descriptors for deep cleanup.
- Command timeout defaults to 900 seconds and accepts human configuration
  `MOOSEDEV_COMMAND_TIMEOUT_SECONDS=1..86400`. Completion/cancellation release
  scratch; a cancelled task resumes from its durable journal with a cold cache.
- Checkpoint GET is read-only (`durable: false`); POST publishes. Clients must
  restart both daemon and harness with matching rebuilt binaries.
- Pending completion is persisted separately from human review, so checkpoint
  failures retry completion. Governing review still invalidates execution
  approval while leaving other proposals reviewable.
- Model capture failures stop after three failed assessment attempts, each with
  at most three JSON attempts. Human guidance is required to reset that budget.

Verification:

- Harness library: 41 passed before the final cleanup regression; all 17 executor
  tests then passed on the final implementation, including all five OS probes and
  the added 600-level inaccessible-directory cleanup case.
- Runner: 27 passed, 2 normally ignored; confined completion also passed explicitly.
- Session: 6 passed; daemon: 16 passed; harness CLI: 4 passed.
- Real Cargo regression proves a second check reports `Fresh harness-check`,
  changed source recompiles and fails its check, deleted source disappears, and
  task cleanup removes the retained source/cache.
- `cargo build --features harness --bins`,
  `cargo check --no-default-features --lib`,
  `cargo clippy --features harness --all-targets -- -D warnings`,
  `cargo fmt --all --check`, and `git diff --check` passed. Clippy reports no warnings.

Remaining limits: Linux confinement has not been run on this Mac; sibling path
dependencies remain outside the command view. Snapshots have a 512 MiB total,
100,000-entry and 64-level limit. Journals retain complete evidence; this change
bounds failed generation growth but does not prune audit history.

## Nested cache follow-up

- [x] Exclude valid CACHEDIR.TAG directories before navigation or snapshot traversal.
- [x] Preserve untagged nested source, and reject invalid/aliased/special markers.
- [x] Prove a real command on this repository fits the snapshot budget while
  excluding clients/zed/target; run scoped executor checks.
- [x] Fold the finding into the existing Lesson using explicit supersession.

All 19 executor tests passed, including the six explicitly enabled OS/current-repo
probes. The real-repository command snapshot measured 85,143,220 bytes (~81.2 MiB)
with clients/zed/target absent. Tests also cover two tagged 600 MiB caches, preserved
untagged source, exact marker signatures, symlinks, hardlinks, FIFOs and sockets.
All 27 runner regressions passed (two optional probes left ignored). Build,
no-default-features library check, formatting, and Clippy with warnings denied passed.

Version of the same Lesson recorded for this follow-up:
https://moosedev.dev/kg/Lesson/9d0f114d-6de8-4842-9d95-acf5d0b59b8e.
It supersedes b0f00e6c, preserving the prior description and links while adding
the nested-cache evidence and rule. The replacement is also linked to
snapshot_source; graph validation reports zero violations.

The subsequent critical pass below addresses the execution/recovery findings that
remained after this nested-cache follow-up.

## Critical execution and recovery pass

User-approved scope under AD30b7dcc9 and the existing accepted requirements:

- [x] Bound command shutdown/output draining and contain detached macOS writers.
- [x] Validate the managed scratch parent without following aliases; prevent or
  safely recover immutable scratch entries and report actionable cleanup errors.
- [x] Persist failed cancellation cleanup and expose a retry across journal reloads.
- [x] Separate infrastructure outages from invalid capture assessments; retain
  checkpoint evidence and allow recovery without invented model guidance.
- [x] Verify focused executor, runner, and session regressions, including real
  macOS confinement/cancellation probes; run formatting, build, Clippy and the
  no-default-features check.
- [x] Update the existing typed lifecycle Lesson and documentation, then validate
  the graph.

Automatic abandoned-cache expiry and old-glibc compatibility remain follow-up
work. Linux runtime probes have not been run on this Mac.

Implementation and evidence:

- macOS uses distinct writable build generations with stable harness-managed
  Cargo aliases and canonical-only sandbox grants. Copy-on-write preserves
  artifacts without sharing writable inodes. Real OS probes cover silent detached
  writers, held file descriptors, cross-generation hardlink attempts, lingering
  output, and cancellation followed by cleanup and reuse of the same task path.
  This is write isolation; detached processes can still survive until stopped.
- Output draining has a 500 ms grace after leader exit. Incomplete output retains
  observed evidence and fails the check. All three probed immutable flag setters
  (chflags, fchflags, setattrlist) are denied; cleanup repairs only safely identified
  owned entries and leaves external hardlinked flags unchanged.
- Scratch parents are opened without following managed aliases. Enumeration,
  cache inspection/copying and cleanup share bounded work/time budgets. Cache
  failures fall back to an empty generation without first requiring old-cache
  disposal. Large-directory cleanup deletes batches so explicit retries progress.
- Cancellation and cleanup_pending are persisted before deletion. Failed cleanup
  remains visible after restart; Esc/cancel retries cleanup, continue/resume first
  cleans up then restores work. Service failures retain capture offsets and
  operation IDs without exhausting malformed-model retries.

ArchitecturalDecision: **Isolate macOS command writes while retaining warm build paths**,
https://moosedev.dev/kg/ArchitecturalDecision/31d3f5b6-e599-417c-9b47-778b212213b4.
Linked to run_command, rotate_build, the active-agency component and the governing
execution/recovery requirements and constraint.

Current Lesson: **Verification gates need a usable build lifecycle**,
https://moosedev.dev/kg/Lesson/4ae844d2-6121-4175-a3af-12d89098ca2b.
Explicitly supersedes 9d0f114d, carrying every prior semantic link and description
forward, with additional capture/cancel links. Graph conforms with zero violations
and 177 existing advisories.

Final verification:

- Executor library: 25 passed, including all six explicitly enabled OS/repository
  probes and the warm-Cargo regression. Run with `--include-ignored --test-threads=1`.
- New macOS executor recovery integration probes: 4 passed with
  `--ignored --test-threads=1`.
- Runner: 30 passed, 2 optional probes ignored; session: 7 passed.
- Build (`--features harness --bins`), Clippy (`--features harness --all-targets --
  -D warnings`), formatting, no-default-features library check and `git diff --check`
  passed. No warnings or graph violations.
- One parallel executor run encountered a transient scratch-lock conflict; the
  isolated regression and full serial run passed. Explicit OS verification uses
  serial execution to avoid overlapping process lifecycle probes. Busy scratch
  locks remain fail-closed, with cleanup_pending retaining the retry obligation.

No commit or merge was performed.
