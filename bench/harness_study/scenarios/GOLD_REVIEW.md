# Pilot scenario rubric review

**Status: pending maintainer approval. Do not run the pilot matrix yet.**

These are synthetic adaptations grounded in public MOOSEDev source, not exact
curated excerpts from the MOOSE/MOOSEDev/Trivyn graphs. No private repository
content is included. This distinction needs explicit approval before freezing
the study. Existing frontier-generated records are discovery aids, not gold.

| Scenario | Independent public anchor | Proposed facts to approve |
| --- | --- | --- |
| `ruleset_cache` (inherited) | Narration packet cache key includes configuration and source generation. | Cache identity covers input + immutable ruleset name/revision; successful values including zero are reused, exceptions remain retryable; switching retains old entries; bulk evaluation preserves scalar selection. |
| `retry_ledger` (accumulation) | Lost-response harness regression preserves operation identity and rejects mismatched retry payloads. | Durable receipt/effect persistence; original-receipt replay; conflict rejection; global IDs become account-scoped; old database migrates to default; importer commits a prefix and stops on conflict. |

Each package's `SOURCE_MAPPING.md` distinguishes original evidence from authored
choices and identifies the source revision. Full prompts and machine-readable
claims live in `scenario.json` and `gold.json`. Review those claims, source
mappings, and the explicit exclusions below; reference code is only one valid
implementation. Approval should record reviewer identity, timestamp, scenario
bundle digest, and any rubric revisions in immutable benchmark artifacts. Do not
silently change `review_status` to approved because the broader plan was approved.

## Scoring rules

- Evaluate each episode only against its `expected_fact_ids`. References to future
  requirements earn no credit. Seeded cache facts are inherited knowledge, never
  newly captured local knowledge. Retry starts with no seeded knowledge records.
- `retry-global` is current in episode 1. In later episodes the account policy
  replaces it; retaining it as clearly obsolete history is valid, presenting it
  as current is a stale-knowledge error. `retry-account` should be linked to or
  explicitly explain replacement of the former policy; exact record counts and
  exact ontology wording are not required.
- Credit semantically equivalent project notes and graph records. Judge rationale
  from agent-visible facts and prompts, not from whether code matches a reference.
- Each `forbidden_claims` entry is a check for an unsupported/stale assertion, not
  a string blacklist. Discussing a rejected alternative as rejected is valid.
- Failure recovery, empty input, zero/negative values, arbitrary string IDs, and
  migration tests enforce explicitly provided API contracts. Concurrent callers,
  arbitrary external exactly-once delivery, and cross-process cache persistence
  are outside scope. Do not infer those obligations from a graph's terminology.
- Hidden tests retain prior observable API checks across episodes. The episode-1
  SQLite trigger probe is specific to its required original schema and is not
  carried across the permitted episode-2 schema migration. Later tasks preserve
  earlier behavior except the explicitly replaced identity rule. `project/` alone goes
  into agent workspaces; `reference/`, `hidden/`, `negative/`, gold, prompts for
  later episodes, and these review documents stay outside agent access.

## Fixture verification commands

For each episode, copy its complete `reference/eN/` into a disposable workspace,
then run `python3 -m unittest discover -s tests -v` there and run the absolute
`hidden/eN.py` path with the workspace as cwd. Reference files contain no hidden
tests or gold. For each `negative_checks` item, copy `base_reference`, overlay the
specified directory, and require the same episode's hidden test to fail. Do not
run negative overlays in place on references or agent workspaces.

Six authored negative checks cover value-only caching, treating zero as a miss,
bulk mutation of scalar selection, absent durable retry deduplication, obsolete
global identity, and a batch path that drops account scope. Additional valid
implementations and semantic grading controls belong in the shared study tests.

## Open approval decision

Approve or revise the above synthetic adaptations and claim-level rubric before
any model execution. If exact inherited real-graph subsets are required instead,
replace the inherited scenario seed and mapping first and freeze a new bundle;
do not relabel these fictional ruleset records as recovered project history.
