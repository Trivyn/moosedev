# Harness pilot

This package implements the [approved protocol](../../spec/harness_evaluation_protocol.md).
It compares complete coding setups, including their native context management.
The 16-run pilot is development data, excluded from any later confirmatory study.

**Before inference:** review [GOLD_REVIEW.md](scenarios/GOLD_REVIEW.md). The two
fixtures are synthetic adaptations of public source behavior; their invented
requirements are not historical project-graph facts. Approval of the protocol
does not approve these reference answers. No scored run has been executed yet.

## Prepare and verify

Run from the repository root with Python 3.11+ and Rust. Initial execution support
is macOS only; other platforms fail closed. No Python packages are required.

```sh
python3 -m unittest discover -s bench/harness_study/tests
MOOSEDEV_RUN_STUDY_SANDBOX_TESTS=1 python3 -m unittest discover -s bench/harness_study/tests -p test_study_isolation.py
cargo test --features harness --example harness_study_session
python3 -m bench.harness_study validate --output target/harness-study/validation.json
python3 -m bench.harness_study build
python3 -m bench.harness_study init-config target/harness-study/config.json
```

`build` owns `cargo build --release --locked --features harness --bins --example
harness_study_session`. It freezes only binaries from this checkout's `target`,
including the matching daemon/proxy. Fill `binary_manifest` in the configuration
with the returned absolute manifest path. There is no Homebrew/PATH fallback for
MOOSEDev. Private source archives, source identities, patches and build receipts
are retained, including failed builds. Agents can read the executable files;
they cannot read the build directory's source archives.

After actual maintainer review, record its identity and the exact package hashes:

```sh
python3 -m bench.harness_study approve-gold --reviewer MAINTAINER --output target/harness-study/gold-approval.json
```

Set `gold_approval` to that absolute path. Then freeze the machine configuration:

```sh
python3 -m bench.harness_study preflight target/harness-study/config.json --output target/harness-study/preflight.json
MOOSEDEV_RUN_STUDY_CONFORMANCE=1 python3 -m unittest discover -s bench/harness_study/tests -p test_study_conformance.py
```

Preflight inventories models without loading or generating, hashes weights and
client runtimes, verifies model-key-to-path associations in LM Studio's local
index, and records a deterministic randomized schedule. It copies daemon assets
under `target/harness-study/assets`. The local index and response identity are
runtime evidence, not a cryptographic attestation of GPU execution. A changed
model preset, driver, binary, asset, weight or approval requires new preflight.
Conformance probes save their own evidence and use a rejecting helper stub; they
do not contact a real model. Native coding interactions are assessed in the pilot.

Configuration defaults: Qwen3.8-27B 5-bit, Gemma 4 26B-A4B 5-bit, Gemma E4B 4-bit;
E4B is the fixed daemon helper. The 4-bit A4B QAT variant is a different model.
Client context is 32,768 tokens; local temperature is zero, Codex reasoning is medium.
The local recording proxy requires an explicit temperature matching the frozen
generation policy before forwarding any request. OpenCode's custom model declares
temperature support, and its coding and auxiliary agents specify zero. Requests
are preserved unchanged; missing or mismatched settings fail as infrastructure
errors rather than silently using provider defaults.
Other native settings/presets and observed requests are retained. Loading is
outside episode timing. By default the reported runtime context must equal the
requested client/load context. A local-model entry may explicitly pin a different
`runtime_context_tokens` at least as large as the client context. This supports
MLX runtimes that override the requested setting through automatic fitting; the
observed capacity must still match the frozen value exactly. It does not enforce
a shared hard token limit. Changing this policy requires a new configuration and
preflight; retain any failed attempt and link its replacement. The maintainer has authorized
unloading the 4-bit QAT model to make room for Qwen and E4B if needed.

## Run and retain

```sh
python3 -m bench.harness_study run target/harness-study/preflight.json --cell 0
```

Run cells 0–15 serially in the frozen order. Every invocation creates a new run ID;
failed attempts remain in the denominator. A deliberate replacement supplies
`--replacement-for OLD_RUN_ID`; it does not erase the old attempt. Do not selectively
repeat poor outcomes. Each cell has three fresh conversations, with actual source,
notes and graph retained. Gold is consulted for grading only after the agent and
daemon stop. A terminal failure marks dependent episodes unattempted.

The default private evidence store is `target/harness-study/evidence`, separate
from the disposable agent workspaces. It contains scenario/driver/protocol copies,
build provenance, exact prompts and scripted approvals, JSONL events, local
agent/helper inference requests and response bytes, daemon logs, boundary source,
notes and graph/task snapshots, and checks/results. Credentials are excluded and
known provisioned tokens are redacted if echoed. Hosted provider-internal prompts
are unavailable; Codex runs are ephemeral. OpenCode JSON events are currently the
native session evidence; a separate native session database export is not claimed.
Token fields absent from native evidence remain null. Helper wire usage can be
reviewed separately; it is not added to agent totals.

Each run's manifest, files and seal are checked against an append-only global
index. Preserve **both run directories and indexes** when moving evidence. These
hashes detect changes relative to the retained index; they are not an external
digital signature. Incomplete snapshotting leaves the execution directory intact
and records its location. Do not delete it before recovering missing evidence.

macOS refuses nested Seatbelt initialization. The harness therefore retains its
native fd-relative read/edit and per-command sandbox boundaries, with no second
sandbox around its controller. Codex selects `danger-full-access` **only inside
the mandatory study outer sandbox**, preventing its incompatible inner sandbox;
OpenCode uses that outer sandbox too. Daemons are separately confined. These
setup-specific settings are part of the frozen comparison, not equivalent native
sandbox configurations.

The scripted reviewer sees scope and structural validity only, never gold. Its
acceptance is labeled simulation, and receives no semantic-quality credit. All
clarifications are supplied upfront; repeated requests receive a fixed response.
Local inference goes through an exact-model recording proxy. Agent file access
uses the boundaries above. Local network endpoints are exact. Hosted clients
reach only a loopback CONNECT gateway, which admits configured HTTPS hostnames
and public destination IPs on port 443. TLS stays opaque, so shared-CDN virtual
hosts remain indistinguishable inside an admitted tunnel. Clients that ignore
proxy settings fail closed; no broader fallback is enabled.

Confined Codex can require explicit public TLS trust because its native trust
discovery is unavailable inside the sandbox. Set `codex_ca_bundle` to a public
PEM CA file (on this machine, `/private/etc/ssl/cert.pem`). Preflight validates and
freezes it by SHA-256; runs verify and archive that copy, and Codex receives a
runtime-local copy through `CODEX_CA_CERTIFICATE`. Private keys are rejected.
Certificate verification stays enabled, with no added keychain or network grant.
Changing hosted trust requires a new configuration/preflight; retain earlier
local outcomes with their original version labels and disclose the hosted-only
change instead of selectively repeating valid agent results.

## Independent grading and offline reproduction

```sh
python3 -m bench.harness_study review RUN_ID judgment.json
python3 -m bench.harness_study regrade --output target/harness-study/regraded.json
python3 -m bench.harness_study report --output target/harness-study/report.json
python3 -m bench.harness_study export --output target/harness-study/private-evidence.tar.gz
```

These commands do not execute candidate code or call models/network. Behavioral
results come from frozen executed checks. `regrade` recomputes aggregates from
those results and current retained judgments; changing hidden tests requires a
separately versioned grading execution, not silent replacement of prior checks.

A judgment contains `reviewer_id`, the manifest's `scenario_gold_sha256`, and
`claims`: each has a unique `claim_id`, `verdict` (`supported`, `missing`,
`unsupported`, `duplicate`, `stale`), and evidence spans `{path, start_line,
end_line}` in sealed artifacts. Missing claims require a rationale describing the
inspected evidence. Supply `episode_id` and `fact_id` to map a judgment to expected
facts; incomplete mappings do not imply missing facts. Equivalent baseline prose
can receive the same credit as a typed graph record. Corrections supply the prior
review UUID in `supersedes`; disagreement remains separate reviewer evidence.

Have semantic reviewers inspect anonymized evidence where practical, without
model labels or expected rankings. Keep the blinding procedure with the reviews;
the tool does not claim automatic blinding. Export bundles contain private source
archives and require separate review before publication.
