# Harness simplification

Behavior-preserving cleanup after checkpoint commit `114ccfe`.
The accepted harness decisions and graph remain authoritative.

- [x] Commit the verified execution/recovery fixes before refactoring.
- [x] Separate executor workspace access, scratch lifecycle, OS sandbox policy,
  output handling, and filesystem primitives into focused private modules.
- [x] Separate runner capture and model-prompt responsibilities; consolidate
  identical governing-proposal checks without changing durable task state.
- [x] Remove concrete duplication in controller dispatch and TUI input/view helpers.
- [x] Check suspected dead code against callers and platform/test configurations.
- [x] Verify exact moves mechanically; run scoped regressions at coherent change
  boundaries, then build, formatting, Clippy, and no-default-features checks.
- [x] Capture and link the durable organization rule, validate the graph, and
  report the resulting structure and any deliberately retained complexity.

No harness behavior, prompt text, journal schema, security policy, or approval
semantics should change. Tests keep their existing assertions. OS process probes
run serially. This pass stays separate from the checkpoint commit.

## Result

- `executor.rs`: 2,612 → 246 lines. Private modules own workspace/snapshots,
  scratch/cache lifecycle, fd-relative filesystem operations, sandbox policy,
  output, and unchanged executor tests. Production child modules are 68–447 lines.
- `runner.rs`: 2,477 → 1,380 lines. Capture/evidence review and model transport/
  prompt construction live in private modules. Durable state and public API stay
  at the runner boundary.
- One governing-proposal predicate, one directory cleanup owner, shared controller
  dispatch, composer sanitization, tab reset, and controller shutdown handling.
- Kept distinct startup validation paths and platform-specific filesystem behavior;
  no confidently dead production code found after checking callers/configurations.
- Moved production bodies were mechanically compared against the checkpoint;
  behavior-preserving consolidations were reviewed separately. Existing test
  assertions and integration test files are unchanged.

## Verification

- Harness library: 49 passed, 6 OS-gated tests ignored in the ordinary run.
- Executor including actual macOS sandbox probes: 25 passed, serial execution.
- macOS executor recovery: 4 passed, serial execution.
- Runner integration: 30 passed, 2 ignored (OS-specific finish and live model).
- Session integration: 7 passed.
- Graph: 20 shapes, zero violations; 177 pre-existing advisories.
- `cargo fmt --check` and `git diff --check`: clean.
- `cargo clippy --features harness --all-targets -- -D warnings`: clean.
- `cargo build --features harness --bins`: passed.
- `cargo check --no-default-features --lib`: passed.
- Linux runtime confinement was not exercised on this macOS host.

## Durable record

Lesson: **Keep harness mechanisms behind focused module boundaries**
`https://moosedev.dev/kg/Lesson/9ceca86a-c77f-4106-8e5b-a904ec87a04a`.
Linked to the active-agency policy engine, executor entry point, and Runner;
existing sandbox/capture Lessons and the build isolation decision also link to
moved implementations. The substrate reports stale until the next full index;
position-based links resolved the expected names from current source.
