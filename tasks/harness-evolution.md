# Harness evolution: reconciliation and system-owned intent

The user approved implementation of both stages on 2026-09-10, existing batched
human review for non-exact reuse, and six exploratory plus twelve matched local
model cells. No commit, merge, or publication is included. The graph governs:

- Requirement: `https://moosedev.dev/kg/Requirement/4ff3ef62-c7a9-4494-89d3-a91ea110c52a`.
- Decision: `https://moosedev.dev/kg/ArchitecturalDecision/395c4e78-0413-4cb6-90a8-901f44d062f2`.
- Grounded capture: `https://moosedev.dev/kg/Requirement/b6853918-4325-4547-bc50-9b7abaf964c0`.
- Durable recovery: `https://moosedev.dev/kg/Requirement/b31c7834-065b-461b-b1cd-a39f58cf84d7`.

This is a progress mirror, not canonical project knowledge. Prior branch work and
`bench/private-evidence/harness-intent-pilot-v1/` remain preserved.

## Stage 1

- [x] Recall graph, approve exact Requirement replacement, supersede and validate.
- [x] Capture the implementation decision and establish bounded agent ownership.
- [x] Add versioned daemon candidate lookup, immutable reuse dispositions, and freshness checks.
- [x] Add durable runner reconciliation, bounded semantic choices, and batched review.
- [x] Preserve legacy journals and same-operation request identity.
- [x] Derive legal capture relationships from existing SHACL catalogue.
- [x] Emit complete record/link/reuse disposition events exactly once. The final
  provenance audit supports six missing Stage 1 task-link receipts, correcting
  the earlier count of fourteen: eight were setup links. Historical evidence is
  unchanged; explicit correction supplements accompany the Stage 2 archive.
- [x] Independently review and verify lifecycle, recovery, and compatibility regressions.
- [x] Freeze owned binaries/configuration and run six exploratory local cells.
- [x] Grade and retain all Stage 1 results before implementing Stage 2 behavior.

All six cells are sealed and independently graded. Qwen completed all seven
allocated episodes with 400,900 tokens; E4B completed none, consuming at least
3,561,550 tokens across its three attempted episodes. Both maintenance primary
outcomes are false. Raw outcomes, explicit review corrections, physical usage,
timeout classifications and disposition supplements are retained under
`target/harness-evolution-stage1/`. The private archive at
`bench/private-evidence/harness-evolution-stage1-v1/` passed verification of all
31,672 members (archive SHA-256
`4364bd00d71353eee65851887c13f6baabf28ba184b3f80a9b6c313672a70085`).

## Stage 2

- [x] Add default-off `change-level-v2` with incremental semantic purpose selection.
- [x] Derive pre-edit source scope and resolve current entities deterministically.
- [x] Discover post-edit entity candidates and obtain reviewed associations.
- [x] Share candidate facilities between study arms; preserve all applicable constraints.
- [x] Verify empty knowledge, stale/unresolved indexing, scope changes, new helpers and resume.
- [x] Independently review and pass scoped Rust/Python, fmt, clippy and feature checks.
- [x] Freeze one recovery Stage 2 build and run twelve matched cells serially in frozen order.
- [x] Independently grade all sixteen attempted episodes, retaining twelve unattempted dependent episodes.
- [x] Audit all 289 task-owned dispositions against daemon operations: exact equality, no gaps.
- [x] Capture results as typed Lessons, link governing code and validate graph.
- [x] Qualify the separately applied post-campaign repairs and freeze their new build identity.
- [x] Verify native response contracts on both models using the repaired build.
- [x] Finalize the narrative, artifact boundaries and independently verified private archive.

The initial Stage 2 build `13b0e898...` was aborted after one sealed control cell:
MLX rejected the nonempty association schema's `uniqueItems` keyword after Qwen's
code passed all three ledger hidden-check sets. Cells 1–11 were not started.
Evidence remains under `target/harness-evolution-stage2/`; no result is erased or
pooled. The same run exposed 89 unnecessary empty-choice model calls.

Recovery within the approved deterministic-controller boundary:

- [x] Preserve and privately archive the aborted campaign, review and exact provider error.
- [x] Enforce handle uniqueness in the controller with a portable provider schema.
- [x] Journal deterministic no-eligible-choice outcomes without model inference; preserve unresolved evidence gates.
- [x] Probe actual native nonempty purpose/association contracts on both local models before freezing.
- [x] Independently review scoped regressions and freeze a new matched twelve-cell identity.

The recovery campaign closed on 2026-09-11 at 18:49:08 UTC with build
`2f108e95e5bc6588bcbe3052e55ad4788385661d87528ce539bf8df7364f49c2`.
All twelve cells are sealed and reviewed. Qwen/current completed six of seven
allocated episodes (two of three scenarios), with correct code in all seven;
its final cache workflow timed out during association work. E4B/current and both
experimental groups completed zero of seven allocated episodes each, attempting
three each. All four maintenance primary outcomes are false. The corrected
Qwen/current maintenance judgment preserves passing code and helper links but
penalizes irrelevant accepted graph links; zero new records is secondary.

Observed token counts are Qwen/current at least 1,096,867 (one request without
final usage), E4B/current at least 2,946,960 (two missing), Qwen/v2 49,195 and
E4B/v2 60,232. Early treatment failure is not efficiency. Reports and explicit
review corrections are under `target/harness-evolution-stage2-recovery-v1/`.

The post-campaign patch addresses duplicate association payloads, state-dependent
purpose choices with decision-first wire ordering, and missing-purpose checkpoint
scheduling/recovery. It was not part of the measured build. Association granularity,
unchanged replan/read cycles and cumulative study-state serialization remain
documented follow-up findings; no additional scored campaign is implied.

The separately qualified source tree `9d620f227088d0ff...` passed 63 runner tests
(2 ignored), 8 session tests, 40 daemon tests, 81 harness-library tests (8 ignored),
5 example tests, Clippy with warnings denied, the no-default-features check, 220
Python tests (7 skipped), and a release build. Frozen build `364140e4a4f0d645...`
then passed five hard native response-format contracts and five diagnostic semantic
matches on each model. Four earlier qualification attempts and one setup-only native
preflight failure remain retained. These source, test, documentation and graph
changes occurred after the measured build; the policy remains experimental and
default-off, and no performance or experimental benefit is claimed.

The private recovery archive contains 68,285 verified files. Archive
`stage2-recovery.tar.gz` is 5,451,182,475 bytes with SHA-256
`dc55da81180bcdfd287e1769b825dec810c559248cbcaa5dd93878812bd825f9`;
its manifest SHA-256 is `551613ecc83380e45fe287c5570ffe330b904dfb479d6cba8c8072bfa3a870fd`
and verification SHA-256 is
`bf4b4bf3cca5baa44af6c56f9909252436a2fa5bd666ba246e1d97398ef11f9d`.

Accepted, code-linked Lessons (predecessors retained by supersession):

- `https://moosedev.dev/kg/Lesson/de41a272-47ea-409c-9642-7bfcd30dfab0`
  — Intent gates can move local-model failure into workflow machinery.
- `https://moosedev.dev/kg/Lesson/861c54c7-8260-4bf9-a3b7-66fc1feae97d`
  — Probe response compatibility and controller transitions separately.

Graph validation: twenty shapes, zero violations; 178 pre-existing advisories.

## Stage 2 recovery (in progress, 2026-09-11)

Decision `https://moosedev.dev/kg/ArchitecturalDecision/3c719e56-7707-457d-8c6e-a9616ad6b436`;
Constraint `https://moosedev.dev/kg/Constraint/d2569270-2688-4afd-aeda-deae69b6b8c3`;
Lessons `263173a8`, `00c61123`, `0f673164` (code-linked). Pre-registration:
`bench/harness_study/RECOVERY.md`.

- [x] Capture the recovery decision, constraint and three wedge Lessons; validate.
- [x] Fix the parked-edit invalidation after a governing detour (treatment only).
- [x] Refresh the purpose-selection revision after a post-edit link review (treatment only).
- [x] Refetch candidates after a rejected reuse; replay a persisted reconciliation disposition (shared).
- [x] Typed `last_error_kind`, `edit_applied`, `repair_exhausted`, `purpose_missing_rounds_exhausted`.
- [x] Driver: typed reviewer cause, `cause.classify`, first-edit marker, gate-repeat counter, metrics once, `episode_limit`, build-id pooling guard, recovery design identity.
- [x] Regression gate against the mock daemon (Rust and Python) green: 70 runner, 82 lib, 8 session, 5 example, 236 Python; clippy/fmt clean.
- [x] Frozen build `86dcd22618223c7eb686388ecea7607bd04dea4669115089fd671f823b109e69` against the archived engine (tree `b410d543…`, unchanged since the sealed campaigns); qualification on the isolated source passed all eight gates (`target/harness-evolution-stage2-recovery-v2/rust-qualification/receipt.json`); study `harness-evolution-stage2-recovery-v2`, design `763ccc52…`, pre-registration sha `4a3c1ea0…`; preflight ready 14/14 with native probes on both models.
- [x] Twelve serial cells closed 2026-09-12T01:43Z; independently graded (12/12 reviewed). Treatment primary 2/6 true (cells 10, 11); zero controller-class terminals, zero unknowns; all four maintenance conjunctions false on unsupported parameter/test links. Four treatment stops are the obligation-only purpose coercion (Lesson `f34b953c`), an empty inventory, and a shared plan-scope error; no repaired path failed. Results: `target/harness-evolution-stage2-recovery-v2/final-results.md`, `final-audited-summary.json`.
- [x] Private archive `bench/private-evidence/harness-evolution-stage2-recovery-v2/stage2-recovery-v2.tar.gz`: 63,131 verified files, 4,343,583,138 bytes, SHA-256 `0cea247e54f55660875539f23ea2ed1d399ad5ef271b576676851f03e23752be`. Post-run Lessons `f34b953c` (obligation-only purpose coercion) and `10d76078` (association granularity arm-independent), code-linked; graph validation zero violations.

Next direction (pre-registered branch): narrow or remove the purpose gate (make `done` legal with obligation-only selections), reorder terminal-cause rows 10/11, record phase-at-deadline, self-transition on plan-scope escape, then a deterministic candidate kind/test-path filter experiment. No default enablement of `change-level-v2`.

Deferred by decision: reuse reviews blocking while a proposal page is open;
missing-round stop durability across restart.

## Three-arm baseline (in progress, 2026-09-12)

Decision `https://moosedev.dev/kg/ArchitecturalDecision/f19ce43b-456a-4949-9f8d-8eb4728090b1`.
Pre-registration: `bench/harness_study/BASELINE.md`. Mode
`local-harness-evolution-stage2-baseline`: 2 models × 3 packages × 3 arms
(harness current, harness change-level-v2, OpenCode without MOOSEDev), 18 cells,
`episode_limit` 1. Question: does the harness cost task capability relative to
the native agent at episode 1; is the purpose gate worse than current.

- [x] Capture the decision; write the pre-registration.
- [x] Driver: baseline mode, 18-cell schedule, opencode arm in evolution runs, native terminal causes, native first-edit marker, baseline review validation; 250 Python tests. Also fixed: opencode launch read the node path from a Codex entry absent in non-pilot preflights.
- [x] Frozen build `5cdf8bd6c2f85532d3d675e309824f3f5d9730ca7031016edb77ca39e52ea75a`: every Rust, Cargo and embedded-asset input identical to `86dcd226…` and the engine tree identical (`build-equivalence.json`; binary bytes differ only by embedded build path). Qualification eight gates green on the isolated source. Study `harness-evolution-stage2-baseline-v1`, design `c0e50535…`, pre-registration sha `16ce6dbb…`; preflight ready 15/15 with OpenCode 1.17.8 fingerprinted.
- [x] Attempt v1 (`target/harness-evolution-stage2-baseline-v1/`): 7 cells sealed, then cell 7 (Gemma/current/cache) looped on a rejected human-required reuse card (2,512 rejects, 43.5 GB events) and the driver was killed by the OS at sealing; retained unsealed. Lesson `7f341f01`.
- [x] Runner fix: rejected human-required card keeps the observation unresolved; oversized branch never re-presents a rejected candidate.
- [x] Driver guards: streaming seal, 8 GiB evidence cap (`evidence_limit`), reviewer reject-loop cap of 5 (`reviewer_reject_loop`, controller class); 258 Python tests, 72 runner tests; recovery identity `763ccc52…` unchanged.
- [x] Freeze baseline-v2: build `abe95d92da7994ec9a801073899e480d58a492f5987e1957fff67fa45a6e1fb6` (Rust diff vs `86dcd226…`: `capture_resolution.rs` and its tests only; engine identical), qualification eight gates green, study `harness-evolution-stage2-baseline-v2`, design `17205d5a…`, pre-registration sha `65862780…`, preflight ready.
- [x] Eighteen cells closed 2026-09-12 (19 sealed runs; cell 7 attempt 1 was a provider stall, retained and replaced under the same identity). Capability table: Gemma harness-current 0/3 vs native 2/3 (two harness-fails/native-passes pairs, one both-fail); Qwen 3/3 both ways under current, 0/3 under change-level-v2. Every harness failure on a natively passable package is a human-parking halt (purpose gate ×5, plan-scope escape ×2, no-op-edit repair budget ×1, idle gate ×1). Results: `target/harness-evolution-stage2-baseline-v2/final-results.md`. Lesson `Harness failures on natively passable tasks are unattended workflow halts`.
- [x] Independent grading 19/19 reviewed (`report-reviewed.json`), audited summary `final-audited-summary.json`; all four harness maintenance conjunctions false (parameter/test links or broken code); Gemma's two native passes lack tests; Qwen native ledger is the only run capturing all three retry facts.
- [x] Private archive `bench/private-evidence/harness-evolution-stage2-baseline-v2/stage2-baseline-v2.tar.gz`: 69,852 verified files, 5,913,305,757 bytes, SHA-256 `0c4fa95c5550073dad621393a91f6ac4ecc268165c82789972c4d0bcaf9e2a72`.
- [ ] v2→v3 graph migration: backfill the new record relations (`restates`, `refines`) across an existing graph as proposed, provenance-marked edges; frontier/large model for the judgment step, like bootstrap; replayable and idempotent. Requirement recorded 2026-09-12 (see graph); not yet scheduled.
- [ ] Review the recovery change set: `tasks/harness-recovery-changeset.md` (six guarded runner transitions; purpose gate and scope escape first).

## Symbolic policy (2026-09-12, in the small)

Mirror of graph Requirement `9ae68a19` (symbolic intent policy), AD `dfc80535`
(daemon-owned association, scoring and typing) and Constraint `9936d96d`
(frozen reconciliation thresholds). Plan: `~/.claude/plans/humble-baking-babbage.md`.

- [x] S0 `IntentPolicy::Symbolic`, env `symbolic`, mandatory association contract, symbolic job text, bench allow-list.
- [x] S1 Obligations derived at approval from direct dossier records; purpose = plan summary; `obligations_derived`.
- [x] S2 Scope escape replans naming the file (3 per task, then park); first no-op edit runs checks.
- [x] S3 `intent/associate`: kind-filtered, innermost-definition, legal-predicate bindings; runner derives and ratifies through the existing link review.
- [x] S4 `relate_with_confidence` (RDF 1.2 reification, `trivyn:confidence`), `reconcile_score` with frozen thresholds and receipts.
- [x] S5 `capture/type`: symbolic decision + lesson, optional LLM sensor, three dispositions; `KnowledgeProposal.reconciled` validated against receipts and annotated at capture.
- [x] S6 Runner note flow: one `harness_capture_note`, typed proposals into the ordinary review; restart-safe; negative proof (only `harness_action` and `harness_capture_note` reach the model).
- [x] S7 Bench: `symbolic` accepted; `symbolic_*` metrics; docs.
- [x] S8 (2026-09-12) The symbolic policy is the only harness. C1 removed every
  model-decided mechanism (`current`, `change-level`, `change-level-v2`, purpose
  selection, sensor capture targets, model reconciliation, the `associate` action,
  the v1 capture route and six candidate/reconcile routes); task journals are
  schema 2 and older journals are refused. Two latent bugs fixed on the way:
  abandoning a link review left a dead association (finish could never resolve),
  and a rejected typed capture resubmitted itself unchanged (now a bounded
  retype). C2 deduped (shared digest, daemon journal/revision helpers, graph
  liveness and predicate helpers, `Step` dispatch) and fixed steering during a
  link review and typing invalidation. C3–C5 split runner, daemon and protocol;
  C6 consolidated the runner test scaffolding; C7 added the symbolic TUI panels;
  C8 this note, the docs and the graph (AD "S8: the symbolic policy is the only
  harness"). Deleted-test ledger, by the invariant each covered: model
  reconciliation loop (7 runner tests; Lessons `0f673164` / `7f341f01` remain as
  history), closed-enum sensor targets (2), read-only capture and byte paging
  (2), unreachable governing-refresh branch (1), two concurrent capture reviews
  (1), a constant (1), purpose selection / change-level mapping / planned
  targets / association handles / governing detour (all 22 in `intent.rs`),
  daemon reconciliation journal (4), purpose candidates (2), capture targets (1),
  duplicate post-edit candidate test (1), deleted-scope audit projection (2),
  in-source schema/prompt/paging tests (20). Every surviving invariant was ported
  to a symbolic-flow test of the same name or a renamed one (see commit
  `ee83f22`).
- [x] Matched campaign: one harness arm (`symbolic`) beside `opencode-without` (mode `local-harness-symbolic-baseline`, AD `231b4702`, pre-registration `bench/harness_study/SYMBOLIC.md`).
  - [x] Attempt v1 (`harness-symbolic-baseline-v1`, build `9aa4f75c`, 2026-09-13): stopped by James after cell 1. Gemma wrote plan checks as English sentences (exit 127) and replanned them as "environmental errors" until the deadline. Fix: runnable-command guard at plan validation (`d39f31e`). Lessons `1f6233e3` (diagnose before spending cells), `5ebdbe53` (check guard). Retained unscored.
  - [x] Attempt v2 (`harness-symbolic-baseline-v2`, build `a0fcebde`): Gemma block only (native passed all three packages; harness failed the ledger at the clarification cap and halted on cache and maintenance after passing hidden checks). The guard worked; the halts were a harness regression: a captured final note was reset by the S8/C2 typing invalidation after its governing proposals were accepted, re-captured, and its capped title's collision qualifier was truncated away (`capture_retype_exhausted`). Fix `706d3ad`, Lesson `bb1f94da`. Retained unscored; block 2 not run.
  - [x] Attempt v3 (`harness-symbolic-baseline-v3`, build `1cd75b0b`, 2026-09-13 overnight): all twelve cells sealed (Gemma block, journal review, Qwen block), no stops or replacements, no unknown terminals. Harness-fails-native-passes 0 for both models; unattended halts 0 of 6; structured model decisions 0. Qwen 3/3 both ways (harness 627 s, 42 requests, 109k provider tokens vs native 738 s, 31, 288k). Gemma 2 both-pass (cache, maintenance) and the ledger failed both ways (harness at the clarification cap after 19 replans). Gemma's harness runs cost 2.7x native tokens (replan-after-approval loop, repeated no-change edits). Lesson `1aa42909`. Results: `target/harness-symbolic-baseline-v3/analysis-notes.md`, `final-summary.json`. Private archive `bench/private-evidence/harness-symbolic-baseline-v3/symbolic-baseline-v3.tar.gz`: 38,229 files, 4,233,574,721 bytes, SHA-256 `c9d0bbfa15f2bb4b88a4f7b0355a6b7a98729f9fdfdc1fb528e80db6802d9477` (manifest `78dacabc…`, verification `96a04682…`).

## Fixed boundaries

Only replay of the same persisted operation and unchanged request is automatically
idempotent. Candidate similarity does not prove equivalence. Non-exact reuse
preserves the existing record unchanged, retaining new evidence/differences in the
task journal and a human disposition in the existing review batch. Governing
decisions require earlier review. External pending operations cannot be ratified,
rejected or imported through this task's review authority. Candidate freshness
includes proposed-state changes, not merely accepted knowledge revision.

No ontology vocabulary or core engine rewrite is authorized. Model relation choices
use the existing SHACL-derived catalogue. Any demonstrated missing shape invariant
requires a separately documented contract change. Purpose/obligation/scope and reuse
dispositions are journal metadata. Existing graph predicates retain their meaning.

The current execution, source CAS, sandbox, durable intent, graph review and
completion gates remain authoritative. Old journals retain their contract versions.
New helpers are discovered from indexed source, never invented from predicted names.
Related files can share a rationale without inheriting semantic graph assertions.

## Evaluation

Use Qwen3.8-27B and Gemma E4B, three unchanged scenario packages (cache, ledger,
maintenance), existing gold/rubric and seed associations, 32768 client context,
temperature zero and 1200 seconds per episode. Stage 1 has six exploratory cells;
Stage 2 has twelve matched current versus change-level-v2 cells (both arms receive
the same reconciliation and post-edit candidate facilities). Each stage receives a
new design/build/configuration identity. Native builds must come from this checkout's
target directory and use frozen indexers. No mid-campaign fixes or reruns that erase
failure; new behavior requires new retained identities.

Retain the registered maintenance conjunction and its constituents. Report source
correctness, completion, knowledge/link quality, reconciliation accuracy, tokens and
requests by purpose, repetition, and individual dispositions versus review cycles.
Scope-only simulated review and n=1 per cell do not establish semantic protective
value or statistical benefit. Lower cost from early failure is not efficiency.
Archive all run inputs/outputs and independently verified checksums outside target;
private material remains private and historical campaigns are never pooled.
