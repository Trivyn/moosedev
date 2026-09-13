# Dependency map: entity outbox

Status: draft for maintainer gold review, revision 2 after blind reader audit
round 1. Written before the hidden tests.

Each probe records its round-1 verdict. Condition A gave a reader only the
episode prompt and the previous reference code; condition B added only the
deciding records. INFERABLE means condition A answered correctly and with
confidence.

Every retention probe is labelled with what it measures. **Correctness**: the
previous code cannot show the rule, so an arm without the knowledge is expected
to get it wrong. **Cost**: the rule is visible in the previous code, so a
graph-first agent's value is not having to read and infer it from source; it is
scored on reads, searches, requests and tokens before the first correct edit.

Revision 2 keeps every round-1 probe and the natural reference code. It adds two
e1 reasons that govern future code paths, each with a correctness probe on the
first path it reaches:

- **Full document** (`#/episodes/0/prompt`): "The indexer replaces its stored document with the payload of each \"created\" or \"updated\" event instead of merging, so every such event must carry the entity's complete current data; this applies to every code path that changes entity data."
- **Deleted event** (`#/episodes/0/prompt`): "The indexer removes a document only when it receives a \"deleted\" event for that ID, so every code path that removes an entity must emit exactly one \"deleted\" event for it."

Harness delivery routes: **dossier** (record linked to `outbox.py`
`Outbox.emit`, the resolution target, delivered when `outbox.py` is attached),
**topic** (top 12 records matching the objective; this project stays under a
dozen records), **search** (`search(query)` returns accepted knowledge first).
The full-document and deleted-event records govern `registry.py`, which is
not the resolution target, so topic recall and search are their routes.
Native OpenCode reads `PROJECT_NOTES.md` in full.

## e1: task probes only

| Probe | Test | Decided by |
| --- | --- | --- |
| e1-per-entity | `OutboxTests.test_sequence_per_entity` | e1 prompt |
| e1-reopen | `OutboxTests.test_sequence_continues_after_reopen` | e1 prompt |
| e1-pending | `OutboxTests.test_pending_lists_events_in_order` | e1 prompt |

## e2: delete, and IDs may be created again

### e2-delete-event: `RegistryTests.test_delete_emits_one_deleted_event` (retention, measures correctness, new)
- Round 1: not audited (new in revision 2). The e2 prompt no longer says that delete emits an event.
- Deciding sentence: the e1 deleted-event reason quoted above.
- Previous reference code: `reference/e1/registry.py` has no removal path; nothing shows how the indexer learns of a removal.
- Code-plus-prompt default: delete the entity row, emit nothing.
- Well-posed: the e2 prompt defines what delete removes and never mentions events.
- Discrimination: **high**.
- Gold record: `outbox-deleted-event`. Harness delivery: topic, search.
- Negative: `delete_without_event`.

### e2-recreate: `RegistryTests.test_recreated_entity_continues_sequence` (retention, measures cost)
- Round 1: INFERABLE (the separate persistent `sequences` table in `reference/e1/outbox.py`).
- Deciding sentence: `#/episodes/0/prompt`, "The indexer stores the last seq it has seen for every entity ID forever, discards any event whose seq is not greater, and treats any gap as lost events. So an entity ID's seq must stay strictly increasing and contiguous for as long as that ID has ever existed in the outbox, including across restarts."
- Cost measured: taking the lifetime rule from `outbox-seq-lifetime` instead of reading `outbox.py` to see that the counter outlives the entity.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `outbox-seq-lifetime`. Harness delivery: dossier, topic.
- Negative: `recreate_restarts`.

## e3: acknowledgement, compaction and bulk removal

### e3-delete-many-events: `BulkTests.test_delete_many_emits_deleted_for_each` (retention, measures correctness, new)
- Round 1: not audited (new in revision 2).
- Deciding sentence: the e1 deleted-event reason.
- Previous reference code: `reference/e2/registry.py` emits a deleted event inside `delete`. A bulk, all-or-nothing removal has no precedent; validating every ID and then removing the rows with one `DELETE ... WHERE id IN (...)` bypasses `delete`.
- Code-plus-prompt default: check all IDs, then remove the rows in one statement, emitting nothing.
- Well-posed: the e3 prompt defines what `delete_many` removes and its failure rule, and never mentions events.
- Discrimination: **medium** (calling `delete` in a loop after validation also passes).
- Gold record: `outbox-deleted-event`. Harness delivery: topic, search.
- Negative: `delete_many_without_events`.

### e3-compaction: `CompactionTests.test_sequence_survives_compaction_and_reopen` (retention, measures cost)
- Round 1: INFERABLE (the `sequences` table survives compaction).
- Deciding sentence: `#/episodes/0/prompt`, the indexer sentence quoted under e2-recreate.
- Cost measured: taking contiguity from `outbox-seq-contiguous` instead of checking how numbering is stored before deleting events.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `outbox-seq-contiguous` (with `outbox-compaction`). Harness delivery: dossier, topic.
- Negative: `max_seq_after_compact`.

## e4: indexer v2 epochs supersede the lifetime rule; patch

### e4-patch-full: `PatchTests.test_patch_event_carries_complete_data` (retention, measures correctness, new)
- Round 1: not audited (new in revision 2).
- Deciding sentence: the e1 full-document reason quoted above.
- Previous reference code: `reference/e3/registry.py` `update` replaces the whole data and emits it; a partial change has no precedent, and the event's payload for it is not shown.
- Code-plus-prompt default: emit the changes that were passed in.
- Well-posed: the e4 prompt says patch merges changes and emits an "updated" event, and says nothing about the payload.
- Discrimination: **medium to high**.
- Gold record: `outbox-full-document`. Harness delivery: topic, search.
- Negative: `patch_partial_payload`.

### e4-epoch-compaction: `EpochTests.test_compaction_never_resets_sequence_within_epoch` (retention, measures cost)
- Round 1: INFERABLE (`last_seq` in the `sequences` table).
- Deciding sentence: `#/episodes/0/prompt`, the indexer sentence; the e4 prompt says "the existing sequence rules still apply within an epoch".
- Cost measured: taking contiguity from `outbox-seq-contiguous` while redesigning numbering for epochs.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `outbox-seq-contiguous`. Harness delivery: dossier, topic.
- Negative: `epoch_reset_after_compact`.

## e5: rename

### e5-rename-events: `RenameTests.test_rename_emits_deleted_and_full_created` (retention, measures correctness, new)
- Round 1: not audited (new in revision 2). The e5 prompt no longer says which events rename emits.
- Deciding sentences: the e1 deleted-event and full-document reasons.
- Previous reference code: `reference/e4/registry.py` has `delete` and `create`; renaming a row in place (`UPDATE entities SET id = ?`) is the shortest implementation and emits nothing.
- Code-plus-prompt default: rename the row in place.
- Well-posed: after a rename the old ID no longer exists and the new ID holds the data; the prompt never mentions events.
- Discrimination: **medium** (delete-then-create also passes).
- Gold records: `outbox-deleted-event`, `outbox-full-document`. Harness delivery: topic, search.
- Negative: `rename_in_place` (also fails the currency probe, declared).

### e5-rename-epoch: `RenameTests.test_rename_onto_deleted_id_starts_new_epoch` (currency)
- Round 1: CURRENCY. A no-memory reader is right by design; condition A does not apply to currency probes.
- Deciding sentence: `#/episodes/3/prompt`, "When a deleted entity ID starts existing again, it starts a new epoch: its epoch number is the previous epoch + 1 (an ID's first life is epoch 1) and its seq starts again at 1."
- Stale lifetime knowledge continues the old numbering.
- Gold record: `outbox-epoch` (current), with `outbox-seq-lifetime` stale. Harness delivery: dossier, topic; the superseded record is excluded from the working set.
- Negatives: `rename_continues_lifetime`, `rename_in_place`.
