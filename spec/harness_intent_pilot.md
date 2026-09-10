# Change-level intent pilot

Status: implementation requested; the two new Requirement records below and the
maintenance rubric await explicit approval under AGENTS.md:140–147. This document
is a review proposal, not an accepted graph record or completed implementation.

## Requirements proposed for the project graph

| Kind / title | Exact proposed description |
| --- | --- |
| Requirement — Change-level intent precedes experimental harness edits | Provide an experimental, default-off harness policy requiring an approved change-level intent mapping before source mutations. This mapping is plan metadata persisted in the task journal: it assigns purpose and obligation roles to record references and identifies affected existing or planned code entities and verification. It is distinct from graph intent links (realizes, satisfies, embodies); neither the mapping nor its roles introduces an ontology class or predicate, and graph writes use only existing validated predicates with their existing meanings. Reuse relevant current typed knowledge; related entities and helpers may share one justification. When required intent is genuinely missing, present grounded proposed knowledge during plan review and require its acceptance before execution. Keep entity identity resolution, minting, and validated link plumbing in MOOSEDev. Knowledge/source/scope changes invalidate affected approval; interruption and resume preserve the obligation. Structural approval and a valid reference do not establish semantic correctness. |
| Requirement — Controlled local intent-enforcement pilot | Compare current and change-level policies in one frozen local harness using Qwen3.8-27B and Gemma E4B on the existing cache and ledger scenarios plus one approved synthetic maintenance-helper scenario: twelve runs, at most twenty-eight episodes, one run per cell. On approval, place the maintenance package in the committed scenarios directory, bind approval to its package digest, and allow its single episode explicitly in the loader without a private-path exception. Keep functioning indexing, initial knowledge, runtime settings, task budgets, and simulated review rules matched across policies. Preserve the ledger's empty initial knowledge. Measure correctness, semantic knowledge and link quality, progress, repeated work, complete resource usage, and approval overhead separately; do not reward record count or link density. Pre-register the primary outcome and count approval overhead in gate decisions and approval cycles. Retain all attempts and replayable evidence under new identities. This diagnostic protocol measures friction, blockage, and link quality under scope-only simulated auto-acceptance, not semantic protection or statistically established benefit. |

Link the first to the active-agency policy engine; link the second to the benchmark
harness. Existing accepted requirements for current knowledge delivery
(`0145baa7-1479-4290-b93f-ceaf8b53d58a`), grounded capture
(`b6853918-4325-4547-bc50-9b7abaf964c0`), durable resume
(`b31c7834-065b-461b-b1cd-a39f58cf84d7`), and replayable study evidence
(`bc93e612-1077-46b9-9d55-771c21514109`) remain governing; do not duplicate them.

## Implementation proposal

- Persist the experimental policy with each task and expose it in study identity.
  Existing journals and default operation use the current policy. Treatment-only
  schema/prompt additions expose bounded record/entity choices; models do not
  generate graph IRIs or a separate specification for each helper.
- Extend the approved plan with one coherent intent mapping. Reuse the existing
  planning → capture → review → approval sequence. Any proposed record supplying
  intent must be accepted before execution, including decisions ordinarily left
  for later review. Rejected/missing intent returns to usable planning with the
  existing bounded recovery budget, rather than an endless approval loop.
- Distinguish a record's role as purpose or obligation; do not require a fresh
  Requirement when existing typed knowledge already supplies the needed intent.
  Selecting a record never exempts a change from other applicable constraints.
  These roles live only in journal plan metadata. Graph intent links retain the
  meanings in AD `7079634f-057d-4738-b752-a9cf51f2cba0`; no role predicate is added.
  Alignment of "change-level intent mapping" remained ambiguous (top 0.426),
  so it is explicitly defined here as metadata, not asserted as an ontology type.
- Use indexed source proofs and daemon-resolved symbols for affected targets.
  New entities remain explicit planned targets until current-source indexing
  resolves them. Reuse reviewed link proposals; do not infer that every touched
  function serves every cited record or silently replace precise links with
  module links. Ratified intent associations are not proof of implementation.
- Record intent reuse/proposal, invalidation, unresolved bindings, rejection,
  and approval cycles as observable events. Retain source CAS, current-knowledge
  approval checks, confinement, and human review. Account for all model requests.

## Study setup and maintenance rubric

Use an explicit intent-study mode with selected scenarios and policy in schedule,
manifest, comparison, and approval identities. Freeze unchanged cache/ledger
packages plus a separately hashed, identical indexing overlay for both policies.
On approval, copy the entirely synthetic maintenance package (no private source)
into `bench/harness_study/scenarios/display_labels_maintenance/`, the committed
scenarios directory. Bind the gold approval to that package's digest and the
selected scenario set. Add an explicit single-episode allowance for this scenario;
keep legacy three-episode validation and archived approval contracts. No private
path bypass is permitted; hidden/gold files stay outside runtime agent inputs.

Freeze scip-python's version, entry point, package/dependency tree hashes, and the
Node executable's absolute path, version, and SHA-256 in the binary manifest.
Set `MOOSEDEV_SCIP_PYTHON` to a frozen launcher that invokes those exact absolute
paths; hash the launcher too. Preflight verifies these identities and a confined
indexing probe, failing on missing or changed inputs. No `npx` fetch, installation,
PATH fallback, or network discovery is permitted during scored execution.

Index current source before each episode and before resolving post-edit anchors,
with identical maintenance in both arms. The seed writer currently emits only
component links: add explicit machinery to index, resolve/mint intended entities,
and assert the approved seed-to-entity associations using existing predicates.
For maintenance, mint both public functions and link both seed records to each;
verify their expected record IRIs through dossiers before either arm begins.
Use an equally frozen, reviewed association map for the inherited cache scenario.
This creates a new control baseline; do not pool it with September 7 pilot cells.
For the initially empty ledger, prove index→mint→link→dossier
in a disposable canary, then verify real source resolution and honestly empty
knowledge. No canary record or hidden fact enters the agent workspace. Readiness
must distinguish missing index, unresolved entity, and absent knowledge.

The maintenance case asks for duplicated display-name normalization in
`render_name(name)` and `render_names(names)` to be extracted into one private
module-level helper, called once per name by both paths. Preserve surrounding
whitespace stripping and `"(unnamed)"` for empty names. Preserve signatures,
iteration order, duplicates, Unicode, empty-input behavior, input immutability,
and propagation of iterator errors. Input names are strings. Existing seeded
knowledge supplies this intent. No new Requirement or ArchitecturalDecision is
required; meaningful
links to existing knowledge suffice. Judge supported, useful new discoveries on
their merits rather than applying an arbitrary zero-record quota. The concrete
review package is `bench/private-evidence/harness-intent-study-review-v2/maintenance/`.
The extraction check intentionally requires the helper's result to pass through
unchanged. This narrow extraction contract excludes post-processing refactors;
it is not a general test of all behavior-preserving refactorings.

Qwen3.8-27B and Gemma E4B retain the common-size and small-model cases while
limiting this first diagnostic to twelve runs. A4B is deferred to expansion to
bound runtime and analysis scope, not excluded on a claim of inferior capability.

Pre-registered primary outcome: for each model/policy, maintenance completes
within its frozen budget, passes behavioral and extraction checks, links the
new helper correctly to both existing seed records, and introduces no redundant
or unsupported accepted knowledge. Score this conjunction as one binary outcome;
retain each constituent result. Use independent evidence-bound semantic grading
for link relevance and redundancy. A justified new discovery does not fail the
outcome; report the stricter zero-new-record result and all new-record counts
secondarily. This avoids turning a node quota into a quality rule.

Approval overhead units: count each record/link review disposition and each plan
approval attempt (accepted or blocked) as separate gate decisions, with types
reported separately. An approval cycle starts on entry to plan review and ends
on approval, rejection/replan, or termination; resume does not start a new cycle.
Count accepted-revision changes and their subsequent plan approval attempts
explicitly. Missing intent necessarily incurs record review plus approval against
the updated revision, even if both occur within one cycle. Report counts per
episode and totals, alongside time; event IDs prevent retry/resume double counting.

## Verification and decision rule

Test default compatibility, reused/missing/rejected intent, shared helper intent,
stale references, source/scope changes, planned target resolution, and cancellation
and resume. Prove the maintenance reference and alternate valid helper names pass;
unchanged duplication and semantic-regression variants must fail relevant checks.
Run scoped Rust/Python tests, formatting, clippy, no-default-features check, build
identity checks, and real confined indexing/dossier readiness before inference.

Run the twelve cells serially in a frozen randomized order, retaining failures;
dependent episodes remain unattempted after terminal failure. Simulated review
uses public task scope and structural validity only, never gold or hidden tests.
It auto-accepts proposals passing those checks and cannot reject a semantically
wrong but structurally valid in-scope claim. Thus n=1 per cell measures friction,
blockage, and resulting link quality under that reviewer, not the gate's semantic
protective value. Testing that value requires a separate semantic-review study.
Independent grading checks semantics and relevance of proposed/accepted links.
Report per-scenario progress and matched exposure alongside campaign cost so early
failure cannot masquerade as efficiency. Diagnose maintenance blockage and graph
noise before expansion; a promising pilot warrants repeated trials, not automatic
default enablement. Retain complete checksummed evidence outside target before
declaring the study complete. No commits or publication are included.
