# Dependency map: entity outbox

Status: draft for maintainer gold review. Written before the hidden tests.

Columns per probe: the deciding earlier sentence, what the previous reference
code reveals, the default a reader with only that code and the current prompt
would pick, why the probe is well-posed, channels, expected discrimination, the
gold record that decides it, and how the harness delivers that record.

Harness delivery routes: **dossier** (record linked to `outbox.py`
`Outbox.emit`, the resolution target, delivered when `outbox.py` is attached),
**topic** (top 12 records matching the objective; this project stays under a
dozen records), **search** (`search(query)` returns accepted knowledge first).
Native OpenCode reads `PROJECT_NOTES.md` in full. Code comments, visible tests
and README text are legitimate channels in both arms; agent-written visible
tests that encode the lifetime rule are recorded as retention through tests.

## e1: task probes only

| Probe | Test | Decided by |
| --- | --- | --- |
| e1-per-entity | `OutboxTests.test_sequence_per_entity` | e1 prompt |
| e1-reopen | `OutboxTests.test_sequence_continues_after_reopen` | e1 prompt |
| e1-pending | `OutboxTests.test_pending_lists_events_in_order` | e1 prompt |

## e2: delete, and IDs may be created again

### e2-recreate: `RegistryTests.test_recreated_entity_continues_sequence` (retention)
- Deciding sentence: `#/episodes/0/prompt`, "The indexer stores the last seq it has seen for every entity ID forever, discards any event whose seq is not greater, and treats any gap as lost events. So an entity ID's seq must stay strictly increasing and contiguous for as long as that ID has ever existed in the outbox, including across restarts."
- Previous reference code: `reference/e1/outbox.py` keeps a per-entity counter row. Nothing says the counter must outlive the entity.
- Code-plus-prompt default: "removes the entity" reads as removing every per-entity row, including the counter, so a re-created ID restarts at 1 and the indexer silently discards its events.
- Well-posed: e2 says IDs may be created again and does not mention numbering; the e1 lifetime rule decides it.
- Discrimination: **high**.
- Gold record: `outbox-seq-lifetime`. Harness delivery: dossier (governs `Outbox.emit`), topic.
- Negative: `recreate_restarts`.

## e3: acknowledgement and compaction

### e3-compaction: `CompactionTests.test_sequence_survives_compaction_and_reopen` (retention)
- Deciding sentence: `#/episodes/0/prompt`, the indexer sentence quoted under e2-recreate.
- Previous reference code: `reference/e2/outbox.py` keeps a counter table separate from events, so compaction naturally leaves numbering alone.
- Code-plus-prompt default: an implementation that numbers from the stored events (`MAX(seq)`) restarts after compaction empties them. With the reference's counter table the natural choice is safe.
- Well-posed: e3 says only which events are deleted; numbering is decided by the e1 contract.
- Discrimination: **medium** (low against the reference code, higher when an agent's own code derives seq from stored events).
- Gold record: `outbox-seq-contiguous` (with `outbox-compaction`). Harness delivery: dossier, topic.
- Negative: `max_seq_after_compact`.

## e4: indexer v2 epochs supersede the lifetime rule

### e4-epoch-compaction: `EpochTests.test_compaction_never_resets_sequence_within_epoch` (retention)
- Deciding sentence: `#/episodes/0/prompt`, the indexer sentence. The e4 prompt says only "the existing sequence rules still apply within an epoch".
- Previous reference code: `reference/e3/outbox.py` keeps one counter per entity ID. The e4 rewrite must add epochs, which invites numbering per epoch from stored events.
- Code-plus-prompt default: compute the next seq as `MAX(seq)` over the entity's events in the current epoch, which restarts after compaction.
- Well-posed: within an epoch the indexer still discards any seq that is not greater and treats gaps as loss.
- Discrimination: **medium**.
- Gold record: `outbox-seq-contiguous`. Harness delivery: dossier, topic.
- Negative: `epoch_reset_after_compact`.

## e5: rename

### e5-rename-epoch: `RenameTests.test_rename_onto_deleted_id_starts_new_epoch` (currency)
- Deciding sentence: `#/episodes/3/prompt`, "When a deleted entity ID is created again, it starts a new epoch: its epoch number is the previous epoch + 1 and its seq starts again at 1."
- Previous reference code: `reference/e4/registry.py` and `outbox.py` implement epochs on re-creation.
- Code-plus-prompt default: implementing rename as delete-then-create reuses the epoch path and passes. A reader holding **stale** lifetime knowledge (seq continues for as long as the ID ever existed) continues the old numbering.
- Well-posed: renaming onto a previously deleted ID creates that ID again.
- Discrimination: **low** against a code reader; aimed at stale notes or records.
- Gold record: `outbox-epoch` (current), with `outbox-seq-lifetime` stale. Harness delivery: dossier, topic; the superseded record is excluded from the working set.
- Negative: `rename_continues_lifetime`.
