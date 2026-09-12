# Local change-level intent diagnostic

This is a new controlled baseline, governed by `spec/harness_intent_pilot.md` and
accepted Requirement `59378083-19c8-4814-a63c-8256342f6fac`. It does not revise or
pool the original pilot or recovery runs. Twelve cells comprise at most 28
episodes: Qwen3.8-27B and Gemma E4B, current and change-level policies, three
scenarios. Dependent episodes remain unattempted after failure.

Use `evaluation_mode: "local-intent-pilot"`, explicit `scenario_ids` containing
`ruleset_cache`, `retry_ledger`, and `display_labels_maintenance`, and exactly
those two local model identities. `intent_policies` defaults to
`["current", "change-level"]`. Keep the previously frozen context, temperature,
helper, deadline, and runtime capacity settings. Every arm enables the shared
optional entity-association action; only treatment requires approved change intent.

Before inference, freeze an offline SCIP producer, dependency tree, launcher,
and Node executable. `indexing.verify_indexer` validates the explicit manifest;
`build --indexer-manifest PATH` embeds it in the new build identity. No npx,
installation, PATH-based producer fallback, or scored network discovery is used.
Preflight also records `/usr/bin/python3`, its resolved executable, both SHA-256
hashes, and interpreter version; each run verifies and retains the same identity.
Set `config.indexer_manifest` to that manifest and `config.binary_manifest` to
the newly frozen build. Create the approved identity with
`approve-gold --config CONFIG --reviewer NAME --output FILE`; this records an
actual human approval, not permission inferred by the command. It binds scenario
package/gold hashes and the design in `intent.design_identity()`.

Preflight verifies the producer and a confined disposable index→mint→link→dossier
canary. Its knowledge never enters scored workspaces. Both arms index before each
episode, receive identical reviewed cache/maintenance seed associations, and use
shared source refresh for later entity bindings. Ledger starts with an empty
knowledge graph; readiness distinguishes expected absence of knowledge from a
missing index or unresolved source entity. Indexer identities, operations, source
proofs, dossiers, and final substrate artifacts are retained with each run.

Run cells serially in the frozen randomized schedule. The simulated reviewer
checks only ordinary scope and structural validity. It accepts in-scope record
and link proposals without semantic truth assessment and never consults gold or
hidden tests. Approval does not earn semantic credit.

The primary outcome is maintenance completion within budget, passing behavior
and extraction checks, meaningful helper links to both original seed records,
and no redundant or unsupported accepted knowledge. Additional justified claims
are allowed. Zero new records and record counts are secondary measurements.
Each normal evidence-bound semantic review may include `intent_assessment`:

```json
{
  "helper_links_both_seeds": true,
  "no_redundant_or_unsupported_accepted_knowledge": true,
  "new_record_count": 0,
  "rationale": "Explain the source and graph evidence independently.",
  "evidence": [{"path": "sealed-artifact.txt", "start_line": 1, "end_line": 3}]
}
```

`report` and `regrade` combine this assessment with objective checks. Missing or
conflicting semantic assessments remain unknown. Normal claim judgments remain
required, and corrections explicitly supersede earlier reviews.

Durable native event IDs deduplicate resumed/repeated snapshots. Reports preserve
plan approval attempts, each record/link disposition, approval cycles, accepted
revision changes, and invalidations separately. Source reads, unchanged rereads,
and repeated command requests measure observable repetition, not automatically
waste. Keep correctness, matched episode exposure, graph quality, progress,
token coverage, and total campaign cost separate: early failure is not efficiency.
The scope-only reviewer and n=1 per cell limit conclusions to diagnostic friction
and resulting quality; they do not establish semantic protection or statistical benefit.
