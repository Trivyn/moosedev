# Harness recovery development evaluation v1

This is a diagnostic follow-up to the completed sixteen-cell pilot, authorized
with the harness recovery implementation plan. It evaluates three installed local
models on both unchanged approved scenarios, using only the MOOSEDev harness:
six cells, each with up to three sequential episodes. It is not a new controlled
comparison with the old OpenCode or frontier-model results. Do not pool the two
evaluations or replace any original outcome.

The original scenario packages, gold approval, prompts, initial graphs, scope-only
review rules, local temperature zero, context settings, helper model, time limits,
serial inference lock, isolation, hidden checks, and independent semantic grading
remain in force. `init-development` derives these settings from the successful
pilot preflight, requires a different study ID and newly frozen repository build,
and produces only the six local harness cells. No frontier client or credentials
are required. The normal `pilot` schedule remains sixteen cells.

## Recovery and response policy

The requested harness response policy is explicitly `auto`. The native harness
performs bounded neutral streaming/nonstreaming compatibility probes and records
its resolved provider policy. All probe requests and responses travel through the
same audited model proxy as task calls; probe schema is `harness_response_probe`.
Task journals and `harness_response_compatibility` events retain the native receipt.
Startup probes belong to compatibility cost, not action/capture repair counts.

The runner owns its two automatic correction attempts (three candidates total).
The reviewer waits while typed recovery status is `generating` or `retrying`, even
if a previous diagnostic is still visible. `awaiting_guidance` ends the episode as
an agent failure; the simulated reviewer never replenishes an exhausted repair
budget with generic guidance. Other approvals retain their scope-only rules.
Transport or paused failures retain the existing failure policy. No retry,
approval, evidence substitution, or source modification is injected by the driver.

`harness_recovery` events preserve distinct recovery transitions with task ID and
model-request count. Episode outcomes retain the final recovery state, request
counts by purpose, decision IDs/attempts, repair-generation count, and response
receipt. Missing attempt metadata remains unknown rather than reported as zero.
Report recovery behavior separately from hidden-test success and semantic scores.
Failing cells and unattempted later episodes remain in the evidence.

## Run and retain

Complete deterministic regression tests and neutral native compatibility probes
before model task execution. Stop source edits before freezing the build; the
build command refuses changing source inputs. Use these commands from the repo,
substituting the manifest returned by the first command for `NEW_MANIFEST`:

```sh
python3 -m bench.harness_study build
python3 -m bench.harness_study init-development target/harness-study/preflight-v4.json --binary-manifest NEW_MANIFEST --study-id harness-recovery-development-v1 --output target/harness-recovery-development-v1/config.json
python3 -m bench.harness_study preflight target/harness-recovery-development-v1/config.json --output target/harness-recovery-development-v1/preflight.json
python3 -m bench.harness_study run target/harness-recovery-development-v1/preflight.json --cell 0 --store target/harness-recovery-development-v1/evidence
```

Run cell indices 1 through 5 sequentially with the same preflight and store. A
failed cell returns nonzero: retain it and continue the remaining scheduled cells.
Use `--replacement-for` only for a documented infrastructure replacement, never to
erase model failure. Any code or effective configuration revision requires a new
build/configuration/preflight identity before further execution. Keep the old
attempt in its original store and report the revision explicitly.

Generate an offline report, independently judge retained semantic evidence using
the unchanged rubric, register judgments with `review`, and regenerate the report
to a new filename. The full original pilot store is deliberately rejected for
development runs. Archive the new store outside `target/` with `export`; it contains
sealed per-run source/build receipts, configurations, both protocols, scenario
packages, traces, workspaces, outcomes, and append-only review history.

```sh
python3 -m bench.harness_study report --store target/harness-recovery-development-v1/evidence --output target/harness-recovery-development-v1/report.json
python3 -m bench.harness_study export --store target/harness-recovery-development-v1/evidence --output bench/private-evidence/harness-recovery-development-v1/evidence.tar.gz
```

Create the private archive directory first. Also retain the pre-run neutral-probe
artifacts, top-level configuration/preflight, selected schedule/run mapping,
reports, test logs, and analysis sources alongside that archive. Verify every
sealed run before export, verify extracted archive members against their stored
hashes, retain an archive SHA-256 and manifest, and prove offline report replay from
the archive before treating retention as complete. Do not modify the original
pilot archive, and do not publish private source or runtime evidence.

Request accounting (version 1) is observation only. Every proxy request has a
physical ID. The instrumented harness adds bounded correlation headers for its
request ID, purpose, decision, and candidate; the proxy records these headers and
does not forward them upstream. Request JSON is unchanged by the study proxy. An explicit bounded HTTP 400/422
rejection of `stream_options` passes through so the native client can negotiate
its fallback; both physical attempts and their observed usage are retained.
Other provider errors and redirects retain the existing refusal behavior.
Compatibility probes, coding, capture, helpers, and repairs appear separately.
Multiple provider attempts for one candidate remain distinct requests. Repeated
harness snapshots enrich the existing request, rather than increasing its count.

Episode and run outcomes include `request_usage`. `sources.proxy` is authoritative
for local agent and helper requests; `sources.native` retains Codex turn and
OpenCode step observations separately. Codex native turns are the hosted-agent
measurement boundary; unexported auxiliary requests remain excluded and unknown.
Native and proxy usage are never summed. The final reported usage snapshot of a
request replaces earlier cumulative snapshots. Earlier snapshots remain retained.
Incomplete framing, cancellation, missing usage, and missing individual fields do
not become zeros. Each field reports observed sum, observed request count, final
request count, and a total only when the entire request group is covered. A
harness receipt without a corresponding proxy request prevents claiming complete
local-agent totals. Native journal legacy gaps and persistence errors are
disclosed separately; they do not invalidate complete independently observed
proxy usage. Existing resource metrics use only complete authoritative
fields. Cache writes and reasoning remain separate provider-native fields; their
inclusion in input/output is not inferred. `total_tokens` is provider-reported,
never computed by adding possibly overlapping categories. Tokenizer units and
subscription or local hardware costs are not interchangeable dollar prices.

An offline accounting report can be replayed into a **new** file:

```sh
python3 -m bench.harness_study usage-report --store PATH_TO_EVIDENCE --output NEW_REPORT.json
```

Replay verifies run seals and identifies the exact event-file hashes. It does not
modify sealed outcomes, prior reports, or earlier study versions. Older runs with
missing streaming usage continue to report incomplete coverage; instrumentation
cannot reconstruct unobserved tokens. All new comparisons require a newly frozen
build, driver, configuration, and separate run identity.
