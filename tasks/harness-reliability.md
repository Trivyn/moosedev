# Harness reliability implementation

Implements the approved small-model repair plan under the graph's existing
mandatory-reading, evidence-grounded-capture, and mediated-execution requirements.
The graph remains authoritative; this file tracks implementation and verification.

- [x] Negotiate and persist a usable structured response mode before generation.
- [x] Resolve model evidence and target references into validated capture requests.
- [x] Materialize safe edits from controller-owned source snapshots.
- [x] Share a durable three-candidate repair budget across parsing and validation.
- [x] Verify compatibility, recovery, approval, persistence, and execution regressions.
- [x] Run six separately versioned local harness development cells with repository builds.
- [x] Retain and archive new evaluation evidence without modifying the original pilot.
- [x] Capture implementation decisions, link governing code, and validate the graph.

Implementation decision: https://moosedev.dev/kg/ArchitecturalDecision/e8b72f20-1c7f-4c5d-b043-958cb445fd23

Development results: six cells, two complete scenarios; Qwen and A4B each 1/2,
E4B 0/2. Independent review supports 32/37 attempted-episode fact assessments
(14/14 seeded, 18/23 newly introduced), with five missing and one duplicate.
See `target/harness-recovery-development-v1/report-final.md`.

Private retention: `bench/private-evidence/harness-recovery-development-v1/`.
All 1,959 archive members verified; grading, analysis, and summary replay match.
The adjacent corrected replay script disables Python bytecode writes inside
sealed evidence. The original pilot archive remains unchanged.
