# Reproducible harness evaluation: protocol and pilot

Status: plan approved by the maintainer on 2026-09-07, including the repository-build
requirement below. Rubrics were approved and the pilot completed: 16 selected cells, 19 retained
attempts, with reports and a verified private archive under
`bench/private-evidence/harness-pilot-2026-09-07/`. This is a
protocol/checklist mirror; accepted typed project records remain authoritative.

## 1. Objective and experimental design

Build an auditable pilot measuring whether local models with the MOOSEDev harness
can approach GPT-5.6-sol in Codex without MOOSEDev. Compare complete coding setups;
do not attribute every difference to memory alone.

| Model | Without MOOSEDev | With MOOSEDev |
| --- | --- | --- |
| GPT-5.6-sol | Codex | Codex + MOOSEDev MCP |
| Qwen3.8-27B | OpenCode | MOOSEDev harness |
| Gemma 4 26B-A4B | OpenCode | MOOSEDev harness |
| Gemma 4 E4B | OpenCode | MOOSEDev harness |

Two distinct tracks:

- **Inherited knowledge:** begin with a small curated graph subset. Baselines
  receive equivalent facts, rationale, relationships, and lifecycle status in
  ordinary notes. This measures using inherited knowledge.
- **Knowledge accumulation:** begin with minimal project metadata. Scripted
  episodes supply new evidence; agents preserve and subsequently use what they
  learn. Score newly captured knowledge separately from seeded knowledge.

Pilot: **one development scenario per track × eight setups = 16 runs**, each with
three episodes. Pilot results diagnose feasibility and measurement quality and
are excluded from a later confirmatory study.

## 2. Scenarios and reference answers

Create Python standard-library projects with 3–8 source files and fast tests.
Use MOOSEDev, MOOSE, and Trivyn decision clusters as scenario candidates, retaining
their reasoning without depending on the full applications.

- **Inherited knowledge:** a ruleset-dependent cache, followed by a related feature
  and a fresh-session extension that must preserve the original dependency.
- **Knowledge accumulation:** a retryable operation that must avoid duplicate
  effects, followed by a changed requirement and a fresh-session feature that
  must respect the updated decision.

Each package contains the starting project, visible tests, three fixed prompts,
permitted clarification answers, initial knowledge, hidden behavioral tests, and
an expected-fact rubric. Include expected absences: unsupported rationale,
unnecessary records, and obsolete rules must not receive credit.

Graph records identify candidates, but source evidence or maintainer-confirmed
rationale establishes the reference answer. Preserve the source-to-scenario
mapping, including the frontier-model origin of existing captures. The maintainer
reviews the compact rubric before it is frozen. Private source evidence stays in
the private artifact area; release exports require a separate review.

Start a fresh conversation between episodes, retaining actual code, notes, and
graph state. Archive prior transcripts outside agent access. Never repair work
using the reference implementation. A terminal failure ends the run and marks
dependent episodes unattempted; the run remains in the outcome denominator.

## 3. Execution and controls

Add a separate package under `bench/harness_study/`, reusing suitable NeSy telemetry
and reporting utilities without changing historical results. Provide versioned
scenario/run manifests and `validate`, `preflight`, `run`, `review`, `regrade`,
`report`, and `export` commands. A small Rust benchmark adapter drives the existing
conversational session controller, including ordinary approval and capture
behavior; it must not introduce a second workflow engine.

### Repository-built binaries only

All MOOSEDev executables must originate from
`/Users/jcadam/code/moosedev/target`, never Homebrew or a bare command on PATH.
Use one release build of the pinned source revision with the `harness` feature:

- Daemon and MCP proxy: `target/release/moosedev`.
- Harness: `target/release/moosedev-harness`.
- Session-controller benchmark adapter: its release artifact under the same
  checkout's `target` directory.

Resolve absolute paths and record each binary's SHA-256, build profile, source
revision, and any source patch. Freeze executable copies under
`target/harness-study/bin/<build-id>/` before launching the matrix so an unrelated
rebuild cannot replace the artifacts during execution. Archive their hashes and
copies with the run evidence. These are copies of the repository builds, not an
installed release channel.

The benchmark starts and owns each isolated daemon using that frozen executable.
MCP clients use the matching absolute proxy path and disable autospawn with
`MOOSEDEV_NO_AUTOSPAWN=1`; the session adapter receives the explicit daemon address
and executable. Never reuse the normal project daemon. Missing artifacts, build
mismatches, an unexpected daemon, or paths resolving outside the approved target
tree fail preflight; there is no PATH/Homebrew fallback. Hashing a configured file
alone is insufficient proof of the executing daemon's identity.

This applies the existing Lessons:

- [Benchmark identity must bind the executing artifact](https://moosedev.dev/kg/Lesson/3ea68212-0e5b-4307-a118-f2c8620bb9c6).
- [Trial daemon binary is decided by a client spawn race, not by config](https://moosedev.dev/kg/Lesson/2d4ad53a-35c5-431a-8780-e178830cac66).

The older live-trial choice to use Homebrew remains scoped to that separate trial;
this harness pilot explicitly evaluates repository builds.

### Run configuration

- Drive Codex and OpenCode through their JSON event interfaces; preserve native
  session exports where available. Codex invocation must match the installed CLI
  and [official non-interactive interface](https://learn.chatgpt.com/docs/non-interactive-mode).
- Resolve and record exact model identifiers, local weight hashes/quantization,
  runtime versions, context settings, thinking/sampling settings, and effective
  client configuration. Fail on a model mismatch; never substitute silently.
- Isolate workspaces, runtime/configuration directories, and daemon stores.
  Disable unrelated MCP servers, plugins, personal instructions, and persistent
  agent memories.
- Keep gold, hidden tests, original repositories, and other runs inaccessible to
  agent tools. Permit only required model/daemon connections; fixture commands
  require no network.
- macOS permits one Seatbelt layer: retain the harness's native executor boundary;
  confine Codex/OpenCode externally and disable Codex's incompatible inner sandbox.
  Daemons are separately confined. Record this setup difference (AD e905638b).
- Use identical task wording and factual starting information. Baselines may
  create and revise notes. Freeze capability-specific usage guidance before runs.
- Pin installed Gemma E4B as the daemon language-model helper across MOOSEDev
  conditions, and freeze the local embedding configuration. Log helper calls and
  resources separately. If E4B is inadequate, document the failure and propose a
  new pilot version using matched local helpers; never silently pool versions.
- The scripted reviewer approves in-scope plans and structurally valid proposals
  without consulting gold or correcting semantic mistakes. Log simulated
  approvals and direct graph writes. This measures unguided capture, not human
  curation quality.
- Limit each episode to 20 minutes, excluding setup/model loading. Run local
  inference serially. Record the randomized schedule; pair each model's conditions
  under the same frozen configuration.

Native context management remains part of each setup. Record differences rather
than claiming identical internal prompts or treating raw token totals as directly
comparable across caching schemes and tokenizers.

## 4. Evidence, scoring, and reproducibility

Keep complete run bundles in a dedicated artifact root outside agent-visible
workspaces. Retention is mandatory; remove disposable execution scratch only
after its evidence is archived and verified.

Preserve:

- Protocol/scenario versions, source commits, binary/model fingerprints, sanitized
  configuration, hardware/runtime information, timestamps, and run identity.
- Exact prompts, scripted answers/approvals, raw CLI events, stdout/stderr, exposed
  model requests/responses, tool calls/results, and daemon logs.
- Initial and episode-boundary source snapshots, patches, notes, canonical graph
  exports, task/conversation journals, proposals, and available intermediate
  mutation evidence.
- Hidden-test versions/results, claim-level grading decisions and supporting
  spans, failures, exclusions, retries, and links to replacement runs.

Save all observable evidence; mark provider-internal material and unavailable
usage fields as unavailable. Never store credentials. Use checksummed immutable
artifacts and append-only run/grading indexes. Corrections create new records.

Report separate outcomes:

1. **Task success:** required hidden tests and behavioral constraints pass.
2. **Knowledge quality:** supported-fact precision/recall, unsupported claims,
   duplicates, and treatment of changed knowledge. Credit equivalent baseline
   prose; valid graph structure is not proof of semantic correctness.
3. **Subsequent use:** fresh-session behavior respects prior knowledge.
4. **Workflow reliability:** malformed actions, stalls, retries, capture omissions,
   and simulated gate counts.
5. **Resources:** elapsed time and available token/cache/helper usage, disclosing
   missing fields.

Use deterministic graders for executable/structural checks and blinded claim-level
review for semantic equivalence. Retain each judgment and its evidence span.

Separate offline `regrade`/`report` from a fresh model run. Offline commands must
reproduce tables from frozen evidence and judgments without models or network.
A fresh rerun gets a new ID; disclose stochastic variation and unavailable
historical hosted-model versions.

Classify preflight/configuration failures, infrastructure outages, and agent task
failures separately. Retain every attempt; do not selectively rerun poor outcomes.

## 5. Acceptance and delivery

- [x] Present new protocol Requirements/Constraints for acceptance under the
  graph workflow; reuse existing benchmark records.
- [x] Obtain maintainer approval of source-backed scenario rubrics.
- [x] Prove reference implementations pass and defective implementations fail.
- [x] Test note/graph content parity, episode resets, gold/sibling isolation,
  simulated approvals, timeout capture, and interrupted artifact writes.
- [x] Test binary selection with a conflicting Homebrew/PATH executable,
  unexpected live daemon, and rebuilt source artifact after freezing binaries.
- [x] Test graders with paraphrases, valid alternatives, omissions, invented
  rationale, duplicate claims, and stale decisions.
- [x] Check adapter conformance and actual agent/helper identities.
- [x] Run and preserve the 16-run pilot.
- [x] Deliver complete evidence bundles, frozen protocol, offline reproduction
  commands, and a feasibility/failure/runtime/grading report.

Recommend the size and controls of a later held-out study from the pilot. Freeze
that study separately with independent scenarios, repetitions, hypotheses,
exclusions, and uncertainty analysis. Pilot rankings and the expected GPT +
MOOSEDev lead are not established conclusions.
