# Field check (`local-harness-field-check`)

This document is hashed into the field-check design identity. Editing it
changes the identity and requires a new human approval.

## Status

A field check is exploratory. It runs selected coding models from the model
table through the symbolic harness arm and the native OpenCode arm, episode 1
only, with one discarded evidence store per cell, on reviewed packages: the
intent pilot packages, the long-horizon packages and exploratory probe
packages (`long_horizon.EXPLORATORY`), which never enter the long-horizon
campaign.
Its runs are never scored, never semantically reviewed, never pooled with any
other study, and are not evidence for any study claim. The grading code
refuses reviews of field-check runs and refuses reports that mix them with
another study.

## What it inherits

- The symbolic baseline's arms (`harness`/`harness`/`symbolic` and
  `opencode`/`without`), evidence byte limit and reject-loop limit, by
  reference; the design identity records the symbolic baseline design hash.
- For intent packages, the sealed intent overlay (seed associations and
  resolution targets). Long-horizon and exploratory packages use their
  long-horizon tables instead; the design identity binds those tables for every
  selected package outside the intent set, because package hashes do not cover
  them. Every harness cell still checks the frozen intent design.
- Preflight's indexer probe covers the intent packages plus every selected
  package. Whether a harness cell's first episode must start with no project
  knowledge follows the package track (accumulation starts empty; inherited
  starts seeded), not the package name.
- The local Gemma E4B model as the daemon helper for capture typing
  (Constraint 83c18bb9), fingerprinted and re-verified like every other model.
- Client context 32768 tokens, 1200-second episodes, temperature 0.
- The harness arm pins `harness_response_policy: reasoning-off`. The native
  arm sends no reasoning option and inherits LM Studio's model setting, which
  is a disclosed difference between the arms.

## Model table

`model_table.py` lists every admissible local model with its weights
directory, the runtime context LM Studio loads it at, and its pinned weights
fingerprint. A field-check configuration's `local_models` must equal the
table entries for the selected coding models plus the helper, and preflight
refuses weights whose fingerprint differs from the table. Changing a row is a
new design identity and needs a new approval. The long-horizon mode will
select its models from the same table (AD 68697d8d, whose consequence named
this table); a field check is not that comparison.

## Approval flow

1. `init-field-check` derives the configuration from a ready symbolic-baseline
   preflight whose own approval still verifies. It may reuse the parent's
   frozen build; sealed symbolic predecessor builds are refused.
2. A named human runs `approve-gold --config <field-check config>`, which
   writes a schema-3 approval binding the intent design, the field-check design
   and the package and gold hashes of the selected scenarios.
3. `preflight` and every `run` verify that approval. A field-check run accepts
   only a schema-3 approval, and a schema-3 approval authorizes no other mode.

## Schedule order

Cells run grouped by coding model, then scenario, with the harness arm before
the native arm. There is no shuffle: cells share no state, no claim compares
the arms, and grouping by model keeps LM Studio loads together. Because the
native cell always runs second, it may find a warm helper model and a warm
provider prompt cache.

## Build

The design identity does not include the build. One approval covers
configurations that differ only in their frozen build; each preflight and run
manifest records the build it used.

## Retired drivers

The field check replaces stage-root wrapper scripts that swapped the pinned
model list at runtime and skipped the gold-approval check. No check is skipped
here.
