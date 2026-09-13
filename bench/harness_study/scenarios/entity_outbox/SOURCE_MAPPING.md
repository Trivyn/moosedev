# Source mapping: entity outbox

Status: fully synthetic; maintainer gold review pending. No private or public
MOOSEDev source is adapted. The search indexer, its versions and its contract
are fictional.

## What each episode introduces

- e1 states the indexer contract with its reason: per-entity sequences that
  stay contiguous for as long as an ID has ever existed, because the indexer
  keeps the last seq per ID forever; a global autoincrement is rejected.
- e2 adds deletion and says IDs may be created again, without mentioning
  numbering.
- e3 adds acknowledgement and compaction, without mentioning numbering.
- e4 replaces lifetime numbering with indexer epochs and says only that the
  existing sequence rules still apply within an epoch.
- e5 adds rename, which can create a previously deleted ID again.

`DEPENDENCY_MAP.md` lists, for every later probe, the earlier sentence that
decides it. Reference code carries no comments explaining reasons.
