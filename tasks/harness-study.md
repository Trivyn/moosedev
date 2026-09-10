# Harness study implementation

Protocol: [approved pilot](../spec/harness_evaluation_protocol.md). Project graph
records are authoritative; this checklist tracks implementation and evidence.

- [x] Recall prior benchmark knowledge and capture approved protocol records.
- [x] Build immutable artifact storage, offline review/regrade/report/export.
- [x] Author two source-backed scenario packages and reference/negative fixtures.
- [x] Add a JSONL bridge to the real conversational session controller.
- [x] Implement isolated native adapters and explicit repository-binary identity.
- [x] Freeze model/configuration manifests and verify setup independently of runs.
- [x] Verify artifact integrity, graders, parity, isolation and session adapters.
- [x] Present gold review package and receive maintainer approval before pilot runs.
- [x] Execute and retain the 16-run pilot, then report feasibility and limitations.

Pilot setup incident: the first attempt (`0e5eb03f-2470-4407-9927-c347cbb6a6f8`)
was sealed as a preflight failure before inference. LM Studio MLX ignored the
requested 32K load context. Revision `harness-pilot-v2-runtime-context` pins native
runtime capacities separately (Qwen/A4B 262144, E4B 131072), while clients retain
their 32768 setting. The replacement must name the original attempt. Evidence:
`target/harness-study/setup-context-incident-v1.json` and `model-setup-v2.jsonl`.
ArchitecturalDecision: [Pin runtime capacity separately from pilot client context settings](https://moosedev.dev/kg/ArchitecturalDecision/d4eb10ff-b128-408f-9a54-ef5fbd5e92e9).

Sampling incident: replacement `4ebb4136-ad54-46c9-9cda-5ba5e4f54b75` was
interrupted after recorded OpenCode requests omitted the configured temperature.
Revision `harness-pilot-v3-sampling-control` explicitly configures native model
temperature capability and coding/auxiliary agents; the proxy rejects requests
that do not match the frozen temperature. The old run's e1 was attempted despite
its sealed `unattempted` label; `pilot-revision-v3.json` preserves that correction
outside the sealed evidence. New interruption accounting is regression-tested.
ArchitecturalDecision: [Verify pilot sampling on the recorded model request](https://moosedev.dev/kg/ArchitecturalDecision/1688d8dc-765a-4cb9-a094-af866033e9c5).

Completed v3 cells: Qwen/OpenCode passed both scenarios (cells 0 and 1); Gemma
A4B/harness failed retry-ledger capture validation (cell 2), retained without a
retry. Codex+MOOSEDev cell 3 was interrupted for repeated TLS UnknownIssuer
errors. Revision v4 adds only explicit frozen public CA trust for Codex. It will
replace that infrastructure attempt and continue cells 3–15; valid v3 local
outcomes keep their original identities. See `pilot-revision-v4.json` and
ArchitecturalDecision [Freeze explicit public CA trust for confined Codex pilot sessions](https://moosedev.dev/kg/ArchitecturalDecision/114d337d-dc31-4c77-aa5a-1b97707864bf).

Accepted records (2026-09-07):

- Requirement: Harness pilot compares inherited and accumulated project knowledge
  https://moosedev.dev/kg/Requirement/b1b4b815-99f6-4637-a579-ef461cdedcad
- Requirement: Harness study preserves replayable evidence and independent grading
  https://moosedev.dev/kg/Requirement/bc93e612-1077-46b9-9d55-771c21514109
- Constraint: Harness pilot executes only frozen repository-built MOOSEDev binaries
  https://moosedev.dev/kg/Constraint/40d414ce-acd2-4370-b1c6-dfed8834ed74
- Constraint: Harness pilot isolates inputs and fixes simulation conditions
  https://moosedev.dev/kg/Constraint/83c18bb9-6edf-4480-9627-839a9ef3517c
- ArchitecturalDecision: Pilot driver observes native coding sessions and archives sealed run evidence
  https://moosedev.dev/kg/ArchitecturalDecision/be7f7d27-c1d6-44f4-9fec-9087194475c7
- ArchitecturalDecision: Harness pilot uses one enforced sandbox layer per coding setup
  https://moosedev.dev/kg/ArchitecturalDecision/e905638b-f639-4bbf-b4ff-89003a7b4102

The maintainer approved the scenario rubrics on 2026-09-07. The approval receipt
is `target/harness-study/gold-approval.json`; `preflight-approved.json` passes
every setup check. Pilot runs are authorized under the frozen configuration.

Verification evidence is retained under `target/harness-study/`: 134 study tests
passed with platform probes enabled; all 18 reference/negative fixture checks
behaved as expected; four bridge tests and three native harness confinement
probes passed. Build, formatting, warning-denied Clippy and no-default-features
library checks passed. `preflight-pending-gold.json` passes every setup check and
is blocked only on the unissued human gold approval. Frozen repository build:
`e3d359c42d11b04412713af154ee71a604561d8d636b646bb0a6c9020fa9a80a`.

Completed pilot (2026-09-07): all 16 selected cells are sealed, with 8 successful
runs and 8 scored failures. The all-attempt inventory retains 19 attempts, including
three setup/infrastructure predecessors. There are 16 active automated semantic
reviews and one retained superseded review; all 115 expected fact occurrences in
attempted episodes were assessed (88 supported). The final report discloses the
first-error reviewer policy, Qwen response-contract incompatibility, version
selection, and post-hoc semantic adjudication. No scored failures were rerun.

- [Final report](../target/harness-study/pilot-report-final.md)
- [Scientific verification](../target/harness-study/verification-final.json)
- [Durable private archive and inspection guide](../bench/private-evidence/harness-pilot-2026-09-07/README.md)
- [Archive verification](../bench/private-evidence/harness-pilot-2026-09-07/bundle-verification.json)

The 5,804,432,555-byte archive contains 7,109 verified members and lives outside
Cargo's target directory. SHA-256:
`7e82b77cae718f5155324e881bdaa82354eca52022b025f5fb6f312f4f4e64fe`.
The graph captures Lessons 557ff1f2 (response contracts), 3630bb6f (gate recovery),
and 891bc194 (consistent adjudication); validation reports zero violations.
