# Source mapping: ruleset cache

Status: proposed synthetic adaptation; maintainer review pending. Source revision:
`2d6428f5d9b4837110ece588fd38f1750bb2626a`. No MOOSE or Trivyn private code or records are included.

## Public-source anchor

`src/stories/narration/packet.rs`, `NarrationPacket::cache_key`, lines 72–81,
keys narration using contract version, project write generation, model, token
budget, and packet fingerprint. This independently observable implementation
supports the narrow pattern that a cached result's identity covers its producing
inputs and configuration. It does **not** establish the synthetic scoring API,
revision policy, performance requirement, or historical design rationale below.

## cache-identity

Scenario-author proposal: ruleset name/revision identifies immutable behavior;
input plus that identity determines a score. This projects the public cache-key
pattern into a small deterministic program. The reason is directly demonstrable:
input 5 under multiplier 3 gives 15; multiplier 4 gives 20. Omitting the version
can return 15 where 20 is required. `scenario.json#/initial_facts/0` is the full
agent-visible contract and is the normative evidence if approved.

## cache-reuse

Scenario-author proposal: the injected evaluator represents expensive work, so
reuse successful results including zero; preserve retryability after exceptions.
This is supplied to both conditions in `scenario.json#/initial_facts/1`; it is not
inferred from the original graph or asserted as a real product requirement.

## Episode evolution

`scenario.json#/episodes/1/prompt` introduces runtime version switching and reuse
when switching back. `scenario.json#/episodes/2/prompt` introduces bulk processing
without scalar-selection mutation. All newly expected facts are explicitly
stated in these prompts. The hidden tests implement these observable contracts;
the reference trees are examples, not the semantic authority.

The source graph was largely captured by frontier models. This fixture is a
public-source-backed synthetic adaptation, **not an exact extracted graph
subset**. Publish that distinction; inherited-fact scores cannot be counted as
new local-model capture. Do not release pilot results as a graph-reconstruction
study without approving this adaptation first.
