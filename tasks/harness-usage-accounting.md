# Harness request usage accounting

Implements the user's approval of accounting (#1) before further harness behavior
changes. Existing Requirement bc93e612-1077-46b9-9d55-771c21514109 governs replayable
resource evidence; Lesson e23cb3ec-faa0-4fb2-aa58-9a4b5c79b0e9 records the observed
streaming and normalization gaps. The graph remains authoritative.

- [x] Recall broad project inventory and governing usage/study records.
- [x] Record the implementation decision under the existing requirements.
- [x] Record each physical HTTP attempt with native usage, attribution, status,
      latency, and nullable normalized token fields; request streaming usage.
- [x] Persist native harness action/capture/probe receipts through interruption
      and restart without treating telemetry as project capture evidence.
- [x] Aggregate proxy/native study usage without double counting; include helper,
      fallback, failed, and repair requests, and explicit observation coverage.
- [x] Verify client, harness, study, legacy compatibility, and security regressions.
- [x] Probe installed local streaming usage on neutral inputs with repository code.
- [x] Document accounting scope, retain validation evidence, link code, and validate
      the graph.

No capture/edit/approval/recovery policy changes, new scored model matrix, edits to
historical sealed evidence, or monetary estimates are part of this pass.

Decision: ArchitecturalDecision `06530a8c-54b5-4b8b-9495-9e835dec055e`,
"Account for physical model requests with explicit usage coverage", linked to the
client, runner, durable ledger, and study aggregator. Architecture validation:
conforms, zero violations (177 pre-existing advisories).

Verification: 431 library tests passed (8 gated tests ignored); harness daemon /
runner / session: 17 / 38 / 8 passed (2 runner tests ignored). Study: 179 tests,
7 gated skips, zero failures; final observer cleanup: 12 passed. Formatting,
clippy with warnings denied, no-default-features check, and release bins passed.
Neutral probes: all three installed models reported streaming and nonstreaming
usage, 7/7 physical attempts measured including one failed compatibility attempt.
Offline replay verified all six prior development run seals and reproduced
71/174 historical request coverage without fabricating missing totals.

Evidence: `bench/private-evidence/harness-usage-accounting-v1/` (private source
snapshots, executable identities, native receipts, replay report, logs, and
integrity manifest); working logs in `target/harness-usage-verification/`.
