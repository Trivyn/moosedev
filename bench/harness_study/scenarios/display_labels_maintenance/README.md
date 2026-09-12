# Draft maintenance rubric — approval required

**Task:** Extract repeated name normalization into one private module-level
helper used by both public entry points. Helper naming is unrestricted.
This one-episode synthetic case measures unnecessary gate friction on a small
behavior-preserving change. It is a review artifact, not an approved live case.

**Inputs:** Two accepted seed records fully specify existing normalization,
public API compatibility, batch behavior, and the maintenance objective.
No new Requirement or ArchitecturalDecision is needed. Seeds will be identical
and entity-linked in both study arms; helper links can reuse their intent.
This requires new seed machinery: index source, resolve/mint both public
functions, assert accepted associations from both seed records to each function
using existing predicates, then verify expected record IRIs in their dossiers.
The resulting control is a new baseline, not the September 7 pilot cells.

**Correctness:** Preserve surrounding-whitespace trimming, empty fallback,
internal whitespace/case, Unicode, iterable order and duplicates, list output,
input immutability, and iterator-error propagation. Both public APIs must route
each name through the same private helper. The hidden extraction check patches
candidate private module functions to observe shared delegation; it does not
require a particular helper name or exact source text. Hidden checks are never
provided to the agent or simulated approver.
The check intentionally requires each API to return the helper result unchanged
(or collect those unchanged results in the batch list). A refactor that
post-processes the result can preserve behavior but fail this narrow extraction
contract. This is intended strictness, not a general refactoring equivalence test.

**Knowledge:** Grade both seeded claims for retained meaning. Penalize duplicate,
unsupported, or obsolete claims and irrelevant links. Additional supported,
useful, nonduplicate knowledge is not automatically wrong. Record/link counts
are not success targets. All semantic grades require evidence-bound review.

**Controls:** The original project must pass behavior and fail extraction. The
reference, renamed-helper reference, and annotated public-functions reference
must pass all checks; annotations do not change the required API shape. Negative controls
cover no extraction, omitted trimming, deduplication, swallowed iterator errors,
and empty-result API regression. All 12 verification expectations pass;
verification logs and file hashes are retained here.

**Study context:** Twelve scenario runs comprise up to 28 episodes across two
models and two policies. Ledger keeps its original empty accumulation graph;
a separate disposable canary proves index→mint→link→dossier readiness, while
ledger readiness verifies fresh resolution and honestly empty initial dossiers.

**Primary outcome:** Per model/policy, complete maintenance within the frozen
budget, pass behavior and extraction checks, correctly link the new helper to
both seed records, and add no redundant or unsupported accepted knowledge.
Grade this conjunction as one binary outcome and retain its constituent results.
Independent evidence-bound semantic review judges link relevance and redundancy.
Report zero-new-record completion secondarily; a supported useful discovery does
not itself fail the primary outcome. With one run per cell and a scope-only
simulated reviewer, this measures friction, blockage, and link quality under
auto-acceptance, not semantic protection or statistically established benefit.

**Approval overhead:** Report record/link review dispositions and plan approval
attempts as typed gate-decision counts, plus approval cycles (entry to plan
review through approval, rejection/replan, or termination). Resume preserves the
cycle. Acceptance that changes the knowledge revision adds review plus approval
against the updated revision; report these events even within a single cycle.

**Placement and freeze:** This package is entirely synthetic with no private
source. Upon approval, place it in the committed
`bench/harness_study/scenarios/display_labels_maintenance/` directory, bind gold
approval to its digest and the selected scenario set, and add an explicit
single-episode loader allowance. There is no private-path exception. Freeze the
scip-python package/dependency hashes, entry point and version, the Node absolute
path/version/hash, and the absolute-path launcher supplied by
`MOOSEDEV_SCIP_PYTHON`. Preflight verifies the identities and confined indexing;
no npx fetching or PATH fallback is permitted.

**Model scope:** Qwen3.8-27B and Gemma E4B retain common-size and small-model cases.
A4B is deferred to bound the first diagnostic's runtime and analysis scope,
not because this study has established inferior capability.
