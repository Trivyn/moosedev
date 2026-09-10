# Source mapping: retry ledger

Status: proposed synthetic adaptation; maintainer review pending. Source revision:
`2d6428f5d9b4837110ece588fd38f1750bb2626a`. No private source has been copied.

## durable-retries

Public anchor: `tests/harness_daemon.rs`,
`lost_capture_response_and_restart_reuse_record_identity`, lines 186–236. The test
simulates interruption after graph commit but before a durable completion flag,
then asserts a retry reuses record identity and produces only one proposal. It
also rejects changed payloads under the same operation ID. This is observable
source/test evidence for durable retry identity and payload consistency, not
proof that the synthetic ledger's schema or business rules existed in MOOSEDev.

Graph candidate: Requirement
`https://moosedev.dev/kg/Requirement/b31c7834-065b-461b-b1cd-a39f58cf84d7`
(Harness resumes without losing obligations or repeating writes). The graph was
largely captured using frontier models and served as a discovery aid; it is not
the independent reference answer. The public regression is the source anchor.

## Synthetic choices requiring approval

Episode 1 maps graph record creation to a SQLite balance update and receipt.
The prompt explicitly introduces lost responses, original-receipt replay,
atomic effect/receipt persistence, and conflicting-payload rejection. The
starting schema and README supply the original storage contract.

Episode 2 deliberately changes the policy from global identity to account-scoped
identity. This is a **new fictional requirement**, not an assertion about
MOOSEDev's historical design. Its rationale (independent clients reuse IDs),
compatibility rule, and migration obligation are all explicit in the prompt.

Episode 3 introduces a restartable importer with independently committed items.
The prompt explicitly specifies stopping at conflicts and retaining the prefix;
all-or-nothing transactions are not a valid alternative for this task.

These prompt statements become the normative evidence only after maintainer
approval. Gold facts require equivalent supported understanding, not reference
wording or table layouts. At episode 1 `retry-global` is current; after episode 2
it is creditable only as superseded history. No model should be expected to
capture the episode-2 reversal before it is introduced.
