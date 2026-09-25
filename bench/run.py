"""Run benchmark cells: one (corpus, task, arm) via opencode headless -> a JSONL row.

Skeleton scope: tooluse mode, single model, MOOSEDev-on-itself. Arms differ ONLY in the memory MCP
injected via a per-arm project-local opencode.json + symmetric AGENTS.md overlay.

Usage:
  python run.py                      # run all arms for the skeleton task, print a summary
  python run.py --arm B2             # run a single arm
"""
import argparse
import hashlib
import json
import os
import re
import shlex
from collections import Counter
import shutil
import signal
import subprocess
import threading
from pathlib import Path
import time
import urllib.request
import uuid

import config
from grade import grade
from grade_set import grade_set
from grade_code import grade_patch

# Code tasks materialize the corpus working tree and are graded on the resulting patch (diff).
# Q&A (context_qa) tasks ALSO materialize the (docs-stripped) tree so the agent has code to explore
# — cold flails hunting for the stripped rationale, memory short-circuits it — but are graded on the
# final assistant message, not a diff (and need no git baseline since there is no patch).
CODE_TASK_TYPES = {"constraint_code"}
TREE_TASK_TYPES = {"constraint_code", "context_qa"}  # task types that get a materialized code tree
GIT_ID = ["-c", "user.email=bench@local", "-c", "user.name=bench"]


def trial_identity(data_dir: str, expected_pid: int | None = None,
                   expected_binary_sha256: str | None = None) -> dict:
    """Fingerprint and verify the harness-owned trial backend."""
    addr_path = Path(data_dir) / "http.addr"
    addr = addr_path.read_text().strip()
    if not addr:
        raise RuntimeError(f"empty MOOSEDev HTTP address file: {addr_path}")
    with urllib.request.urlopen(f"http://{addr}/api/v1/health", timeout=5) as response:
        health = json.load(response)
    version = health.get("version")
    if health.get("status") != "ok" or not version:
        raise RuntimeError(f"invalid MOOSEDev health response: {health!r}")

    if expected_pid is not None:
        pid_path = Path(data_dir) / "moosedev-serve.pid"
        try:
            running_pid = int(pid_path.read_text().strip())
        except (OSError, ValueError) as exc:
            raise RuntimeError(f"invalid MOOSEDev daemon pid file: {pid_path}") from exc
        if running_pid != expected_pid:
            raise RuntimeError(
                f"health endpoint belongs to daemon pid {running_pid}, expected {expected_pid}"
            )

    binary = Path(config.MOOSEDEV_BIN).resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    if expected_binary_sha256 is not None and digest != expected_binary_sha256:
        raise RuntimeError(
            "configured MOOSEDev binary changed after the trial daemon was started"
        )
    return {
        "trial_epoch": f"{version}+{digest[:12]}",
        "moosedev_version": version,
        "moosedev_binary_sha256": digest,
    }


def load_task(corpus: str, task_id: str) -> dict:
    return json.loads((config.corpus_tasks_path(corpus) / f"{task_id}.json").read_text())


def serving_regime(model: str) -> dict:
    """What actually served this cell: quantisation, format, and the reasoning the build defaults to.

    None of this was recorded until 2026-09-20, and its absence cost real work. The ladder turned
    out to mix 4-bit, 5-bit and Q4_K_M and four different reasoning defaults, and recovering that
    required querying a live LM Studio -- an avenue that closes the moment a model is deleted. It
    got worse when a second quantisation of qwen/qwen3.8-27b was downloaded: both variants answer
    to the SAME model key, so without this field a 5-bit and an 8-bit campaign are indistinguishable
    in runs.jsonl. Best effort: a probe failure must never fail a cell.
    """
    provider = model.split("/", 1)[0]
    out = {"provider": provider, "quantization": None, "format": None,
           "reasoning_default": None, "selected_variant": None}
    if provider != "lmstudio":
        return out                      # hosted: the serving stack is the provider's, not ours
    key = model.split("/", 1)[1]
    try:
        import urllib.request
        base = config.LLM_BASE_URL.rsplit("/v1", 1)[0]
        served = urllib.request.urlopen(f"{base}/api/v1/models", timeout=10).read()
        for m in json.loads(served)["models"]:
            if m.get("key") != key:
                continue
            out["quantization"] = (m.get("quantization") or {}).get("name")
            out["format"] = m.get("format")
            out["selected_variant"] = m.get("selected_variant")
            out["reasoning_default"] = ((m.get("capabilities") or {}).get("reasoning") or {}).get("default")
            break
    except Exception as e:
        out["probe_error"] = str(e)[:120]
    return out


def local_provider(model: str) -> dict:
    """Run-local provider definition for a model served by the local LM Studio.

    opencode resolves a bare `lmstudio/...` model against the USER's global config and the
    models.dev catalog. Both lag the machine's real inventory — this LM Studio build serves an
    EMPTY /v1/models so opencode can discover nothing, and the global entry points at a different
    host — so a cell either dies with ProviderModelNotFoundError or, worse, quietly runs somewhere
    else. Pinning endpoint and model id per run is the same insulation `--pure` buys for plugins.
    """
    provider, _, model_id = model.partition("/")
    if not model_id:
        return {}
    # OpenRouter is pinned for the same reason LM Studio is: the published
    # capability numbers were produced through it, and a comparison is only a
    # comparison if the endpoint is the one we think it is. Routing the frontier
    # control through opencode (rather than codex, as the paper did) holds the
    # BACKEND constant with the local-model cells, so the model is the only thing
    # that differs between them.
    endpoints = {
        "lmstudio": ("Local LM Studio", config.LLM_BASE_URL, config.LLM_API_KEY,
                     config.LOCAL_CONTEXT, config.LOCAL_OUTPUT),
        "openrouter": ("OpenRouter", "https://openrouter.ai/api/v1",
                       os.environ.get("OPENROUTER_API_KEY", ""), 200_000, 32_000),
        # Not every local model can be served by LM Studio: the OptiQ quants run under
        # mlx-optiq, started by hand on its own port. `local/<alias>` points a cell at any
        # OpenAI-compatible server without pretending it is LM Studio — which matters, because
        # the rung's model management (load, unload, presence check) is LM Studio's alone and
        # would silently do nothing here.
        "local": ("Local OpenAI-compatible server",
                  os.environ.get("BENCH_LOCAL_BASE_URL", ""),
                  # mlx-optiq rejects any bearer token not prefixed `sk-optiq-`; servers that
                  # ignore the header entirely are unaffected by the default, and anything else
                  # sets BENCH_LOCAL_API_KEY.
                  os.environ.get("BENCH_LOCAL_API_KEY", "sk-optiq-local"),
                  config.LOCAL_CONTEXT, config.LOCAL_OUTPUT),
    }
    if provider not in endpoints:
        return {}
    name, base_url, api_key, context, output = endpoints[provider]
    if not base_url:
        raise SystemExit(f"{provider} needs BENCH_LOCAL_BASE_URL in the environment; refusing "
                         f"to run a cell with no endpoint to run it against")
    if not api_key:
        raise SystemExit(f"{provider} needs its API key in the environment; refusing to run "
                         f"a cell whose endpoint is not the one it claims")
    # The id on the wire may differ from the id on the command line: mlx-optiq serves a model
    # under its filesystem path, and a path cannot survive opencode's `provider/model` split.
    # So `local/qwen122b` is the handle, BENCH_LOCAL_MODEL_ID is what the server is actually
    # asked for, and the row records the handle either way.
    wire_id = os.environ.get("BENCH_LOCAL_MODEL_ID") if provider == "local" else None
    return {provider: {
        "npm": "@ai-sdk/openai-compatible", "name": name,
        "options": {"baseURL": base_url, "apiKey": api_key},
        "models": {model_id: {"id": wire_id or model_id, "name": model_id,
                              "limit": {"context": context, "output": output}}},
    }}


# A pure-memory Q&A carries no source tree, so ANY filesystem or shell route is an escape hatch,
# not a capability under test: a 27B smoke cell found `moosedev` on PATH and tried to export and
# grep the graph, and missed only because MOOSEDEV_DATA_DIR was unset so it read the empty workdir.
# Scoring that as memory would be scoring luck, so every WRITE and every route off the workdir is
# denied. read/glob/grep stay allowed deliberately: the workdir is empty, so they cannot substitute
# for memory, and a model that greps an empty tree instead of calling its memory tool is exhibiting
# the exact failure the harness exists to prevent — a distractor to measure, not to remove.
# Deny by NAME, never `"*": "deny"` with an allow-list. A wildcard deny also covers the arm's MCP
# tools, which would make every tooluse cell record zero memory calls — reading in the table as
# "the model would not call its memory tool" when the config forbade it. That is the finding this
# matrix exists to establish, so the rig must not be able to manufacture it.
SEALED_PERMISSION = {"bash": "deny", "edit": "deny", "write": "deny", "patch": "deny",
                     "webfetch": "deny", "websearch": "deny", "external_directory": "deny"}


def memory_server_arm(arm: str, mode: str) -> bool:
    """True when the cell is actually handed the moosedev MCP. Defined once and used both to
    install the server and to arm the abort rule that watches for its abandonment, so the two
    cannot drift apart into a rule that fires in a cell which never had the tool."""
    return arm == "B2" and mode not in ("oracle", "walk")


def arm_opencode_config(arm: str, corpus: str, mode: str = "tooluse",
                        model: str = None, sealed: bool = False) -> dict:
    """Project-local opencode.json: disable the global omni MCP, add the arm's memory MCP (if any).
    In ORACLE mode the harness prepends retrieved knowledge to the prompt instead, so no live memory
    tool is given — isolating knowledge-value from the agent's willingness to call the tool (H8)."""
    c = config.CORPORA[corpus]
    mcp = {"omni": {"type": "local", "command": ["/opt/homebrew/bin/omni", "--mcp"], "enabled": False}}
    if mode not in ("oracle", "walk") and arm == "B1-rag":
        mcp["freetext-recall"] = {
            "type": "local",
            "command": [str(config.VENV_PY), str(config.BENCH / "freetext_mcp" / "server.py")],
            "environment": {"FREETEXT_CORPUS": str(config.corpus_chunks_path(corpus))},
            "enabled": True,
        }
    elif memory_server_arm(arm, mode):
        mcp["moosedev"] = {
            "type": "local",
            "command": [config.MOOSEDEV_BIN, "--connect"],
            "environment": {
                "MOOSEDEV_DATA_DIR": c["data_dir"],
                "MOOSEDEV_NO_AUTOSPAWN": "1",  # the harness owns the backend; never auto-spawn
                "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
                "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY,
                "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL,
            },
            "enabled": True,
        }
    return {
        "$schema": "https://opencode.ai/config.json",
        # Frozen, IDENTICAL context management across all arms so MEMORY is the only variable.
        # opencode's default auto-compaction needs a `reserved` buffer to fire BEFORE a turn
        # overflows the window — otherwise a reading agent's context is silently middle-truncated
        # by the server (LM Studio TruncateMiddle), corrupting the run. This makes the cold/free-text
        # arms cope via opencode's own (visible) compaction rather than invisible server truncation.
        # prune drops OLD tool outputs (accumulated file reads) — without it a read-heavy agent
        # balloons past the window and the server silently middle-truncates (TruncateMiddle). auto
        # summarizes when full; reserved keeps headroom so compaction fires before overflow.
        "compaction": {"auto": True, "prune": True, "reserved": 16384},
        "mcp": mcp,
        **({"permission": SEALED_PERMISSION} if sealed else {}),
        **({"provider": provider} if (provider := local_provider(model or "")) else {}),
    }


def walk_context(corpus: str, task: dict) -> str:
    """WALK/push: exactly what the harness pushes, from POST /harness/context.

    Not `get_entity_dossier`. That was the first attempt and it is a lookalike,
    not the product: the MCP dossier LISTS a superseded record beside its
    replacement, while the harness's walk resolves supersession to its head
    (`Hop::SupersessionHead`, rendered "via: supersedes ...") and leads with the
    linked evidence of the files' code. Measuring the MCP dossier and calling it
    the harness would have tested the wrong mechanism — verified by pushing all
    eight currency anchors through it and finding the stale record present in
    8/8.
    """
    import urllib.request
    addr = os.environ.get("MOOSEDEV_HARNESS_HTTP", "127.0.0.1:7475")
    body = json.dumps({
        "topic": task["prompt"][:200],
        "files": [task["anchor_file"]] if task.get("anchor_file") else [],
        "evidence_only": not task.get("anchor_file"),
    }).encode()
    req = urllib.request.Request(f"http://{addr}/api/v1/harness/context", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as response:
        payload = json.loads(response.read())
    # The runner builds its prompt from THREE fields, not one: `context` carries the
    # walk, `governing_constraints` the Project rules, and `files[].dossier` the direct
    # records of each file's code. Reading only `context` drops every direct record —
    # which made the push look as though it withheld the current knowledge as well as
    # the stale, and nearly produced a report of a product defect that was a bug here.
    parts = [payload.get("context", "")]
    rules = payload.get("governing_constraints") or []
    if rules:
        parts.append("\n\nProject rules:\n" + "\n".join(
            f"- [{r.get('label','')}] {r.get('claim','')} ({r.get('via','')})" for r in rules))
    for entry in payload.get("files") or []:
        parts.append(f"\n\nEntity dossier for {entry.get('file','')}:\n{entry.get('dossier','')}")
    return "".join(parts)


def oracle_context(corpus: str, topic: str, k: int = 4) -> str:
    """ORACLE mode: harness-side retrieval of the records the agent WOULD get from its memory tool,
    to prepend to the prompt. Uses get_relevant_context (symbolic BM25 retrieval; content parity means
    this is the same captured knowledge B1 holds as text). Isolates knowledge-value from fetch-willingness."""
    import asyncio
    from mcp_client import call_tool
    from export_corpus import corpus_env
    return asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"], corpus_env(corpus),
                                 "get_relevant_context", {"topic": topic, "limit": k}))


def freetext_oracle_context(corpus: str, topic: str, k: int = 6) -> str:
    """ORACLE/push for the B1 FREE-TEXT arm: BM25 over the exported chunks — the SAME retrieval the
    B1-rag MCP does — so push can deliver the free-text representation. Unlike B2's
    get_relevant_context (current-only), this is currency-BLIND: superseded chunks are retrievable,
    which is the whole point of the currency comparison."""
    import re
    from rank_bm25 import BM25Okapi
    records = json.loads(config.corpus_chunks_path(corpus).read_text())
    tok = lambda s: re.findall(r"[a-z0-9]+", s.lower())
    bm25 = BM25Okapi([tok(r["text"]) for r in records])
    scores = bm25.get_scores(tok(topic))
    ranked = sorted(range(len(records)), key=lambda i: scores[i], reverse=True)[:k]
    hits = [records[i]["text"] for i in ranked if scores[i] > 0]
    return ("Top recorded entries:\n\n" + "\n\n".join(hits)) if hits else "No recorded knowledge matched."


def _slug(s: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-")[:60] or "record"


def write_markdown_corpus(corpus: str, wd):
    """B1-md: render the content-parity export as one markdown file per record under docs/decisions/."""
    chunks = json.loads(config.corpus_chunks_path(corpus).read_text())
    d = wd / "docs" / "decisions"
    d.mkdir(parents=True, exist_ok=True)
    for i, r in enumerate(chunks):
        (d / f"{i:03d}-{_slug(r['title'])}.md").write_text(r["text"] + "\n")  # text already = "# title\n\n…"
    return len(chunks)


def write_notes_corpus(corpus: str, wd):
    """B1-notes (ecological, AD b3205dcb): copy the team's REAL accumulated docs (e.g. lessons.md +
    topical guides, per corpus `notes_paths`) into the workdir so the agent greps them — how knowledge
    is ACTUALLY kept, NOT the graph export. Source is the corpus repo, not corpus_chunks_path."""
    repo = Path(config.CORPORA[corpus]["repo"])
    dest = wd / "docs" / "notes"
    dest.mkdir(parents=True, exist_ok=True)
    n = 0
    for pat in config.CORPORA[corpus].get("notes_paths", []):
        for src in sorted(repo.glob(pat)):
            if src.is_file():
                shutil.copy(src, dest / src.name)
                n += 1
    return n


def materialize_tree(corpus: str, wd):
    """Lay down the corpus's pinned tracked tree (no .git, no gitignored target/.moosedev) into wd
    via `git archive`. Excluding .git enforces the comprehension-debt premise (no history for the
    agent); .env is dropped too."""
    c = config.CORPORA[corpus]
    # BENCH_TREE_REF re-materializes a corpus as it stood at an earlier commit (a cold-arm
    # re-measurement against a past batch's tree) without touching the corpus config.
    ref = os.environ.get("BENCH_TREE_REF") or c.get("sha", "HEAD")
    cmd = f"git -C {shlex.quote(c['repo'])} archive {shlex.quote(ref)} | tar -x -C {shlex.quote(str(wd))}"
    subprocess.run(cmd, shell=True, check=True)
    (wd / ".env").unlink(missing_ok=True)
    # Comprehension-debt premise: strip HUMAN-facing docs / agent working-notes (handbook, spec,
    # tasks/*.md) so captured memory is the only source of rationale. Otherwise the repo's own
    # handbook makes every arm converge. These remain in the full clone as grading ground truth.
    for rel in c.get("agent_exclude", []):
        t = wd / rel
        if t.is_dir():
            shutil.rmtree(t, ignore_errors=True)
        else:
            t.unlink(missing_ok=True)


def git_baseline(wd):
    """Commit the materialized tree + overlays as a baseline, so the agent's edits are diffable."""
    subprocess.run(["git", "init", "-q"], cwd=wd, check=True)
    subprocess.run(["git", *GIT_ID, "add", "-A"], cwd=wd, check=True)
    subprocess.run(["git", *GIT_ID, "commit", "-q", "-m", "baseline"], cwd=wd, check=True)


def prepare_workdir(run_id: str, arm: str, corpus: str, task: dict, mode: str = "tooluse",
                    model: str = None):
    sealed = task["type"] == "capability_qa"
    wd = config.WORK_ROOT / run_id
    wd.mkdir(parents=True, exist_ok=True)
    if task["type"] in TREE_TASK_TYPES and task.get("materialize_tree", True):
        materialize_tree(corpus, wd)  # a task may opt out (e.g. a pure memory-currency Q&A)
    shutil.copy(config.BENCH / "arms" / arm / "AGENTS.md", wd / "AGENTS.md")
    (wd / "opencode.json").write_text(
        json.dumps(arm_opencode_config(arm, corpus, mode, model, sealed), indent=2))
    if arm == "B1-md":
        write_markdown_corpus(corpus, wd)
    if arm == "B1-notes":
        write_notes_corpus(corpus, wd)
    if task["type"] in CODE_TASK_TYPES:
        git_baseline(wd)  # overlays (AGENTS.md, docs/) are baseline too -> excluded from the diff
    return wd


# --- no-progress abort -------------------------------------------------------
# The tool-call probe qualifies a model that CAN emit a call; it cannot see one that
# stops making progress. Hermes-4-70B made a single memory call, then repeated two grep
# signatures 94 and 93 times against the empty workdir — 193 calls carrying just 8
# distinct signatures, 4.97M prompt tokens, 28 minutes — and concluded the graph was
# empty while 74 Lessons sat one sparql call away (Lesson 65d330ac). Cutting at the 8th
# repeat costs about one of those 28 minutes; over a 13-cell rung it is the difference
# between a fast disqualification and a lost day.
ABORT_REPEAT = int(os.environ.get("BENCH_ABORT_REPEAT", "8"))
ABORT_SILENT = int(os.environ.get("BENCH_ABORT_SILENT", "40"))


class NoProgress:
    """Names the first pathology in the agent's tool stream, or stays quiet.

    Armed ONLY for sealed capability_qa cells, and the scope is load-bearing rather than
    incidental: both rules assume the environment cannot change under the agent, which is true
    only where every write is denied and the workdir is empty. In a code cell neither rule is
    sound — see the caller.

    Within that scope both describe an agent that has stopped acquiring information, which is
    the only thing a cell's remaining minutes can buy, and both are written to be unable to
    fire on a healthy cell: a false abort would silently turn a scoring cell into a zero and
    corrupt the very comparison the matrix exists to make.

    `repeat` — an IDENTICAL (tool, arguments) signature returns an identical result, so the
    Nth is information-free by construction, whatever the model intended.

    `no_memory` — only after the agent has ALREADY used the memory server successfully, so it
    fires on abandoning a tool known to work, never on a model that simply never calls it.
    That latter case is the matrix's own headline finding and must be allowed to run and score.
    """

    def __init__(self, memory_arm: bool):
        self.memory_arm = memory_arm
        self.sigs = Counter()
        self.since_memory = 0
        self.saw_memory = False
        self.reason = None
        self.detail = None

    def observe(self, tool, args):
        if self.reason or not tool:
            return self.reason
        if args is not None:
            sig = (tool, json.dumps(args, sort_keys=True, default=str))
            self.sigs[sig] += 1
            if self.sigs[sig] >= ABORT_REPEAT:
                self.reason = "repeat"
                self.detail = f"{tool} called {self.sigs[sig]}x with identical arguments"
                return self.reason
        if tool.startswith("moosedev_"):
            self.saw_memory, self.since_memory = True, 0
        else:
            self.since_memory += 1
            if self.memory_arm and self.saw_memory and self.since_memory >= ABORT_SILENT:
                self.reason = "no_memory"
                self.detail = (f"{self.since_memory} consecutive non-memory calls after the "
                               f"agent had already used the memory server")
        return self.reason

    def feed(self, line: str):
        """One opencode JSONL line in; an abort reason out, or None. opencode emits exactly
        one `tool_use` event per call, always at status `completed`, with the arguments under
        part.state.input — verified against the Hermes traces this exists to catch."""
        line = line.strip()
        if not line.startswith("{"):
            return None
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            return None
        if e.get("type") != "tool_use":
            return None
        part = e.get("part") or {}
        return self.observe(part.get("tool"), (part.get("state") or {}).get("input"))


def _terminate(proc):
    """Kill the agent's whole process group: opencode spawns the arm's MCP servers as children,
    and signalling only the parent leaves them holding the store's RocksDB LOCK, so the next
    cell cannot open it."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if proc.poll() is not None:
            return
        try:
            os.killpg(os.getpgid(proc.pid), sig)
        except (ProcessLookupError, PermissionError):
            (proc.terminate if sig == signal.SIGTERM else proc.kill)()
        try:
            proc.wait(timeout=10)
            return
        except subprocess.TimeoutExpired:
            continue


def agent_errors(stdout: str) -> list:
    """opencode reports a provider failure as a JSON `error` event on STDOUT, not on stderr:
    a bad endpoint, a rejected key or an unknown model all arrive this way, seconds in, with
    zero steps. Surfacing them is the difference between "this model scored 0.0" and "this
    cell never reached the model" — the pilot that motivated this was a 401 from a key prefix,
    and it presented as a clean zero."""
    out = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{") or '"error"' not in line:
            continue
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        if e.get("type") != "error":
            continue
        err = e.get("error") or {}
        msg = ((err.get("data") or {}).get("message")) or err.get("message") or str(err)[:200]
        out.append(f"{err.get('name', 'error')}: {msg}")
    return out


def run_agent(cmd, cwd, timeout, watch=None):
    """Run the agent and stream its JSONL, so a cell can be cut short while it still has
    minutes left to waste. subprocess.run hands back stdout only at exit, which is far too
    late to notice a loop. Returns (stdout, returncode, timed_out, aborted_reason).

    The deadline runs on a timer rather than inside the read loop: a model that stalls
    silently produces no lines, so a loop-body check would never fire.
    """
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            stdin=subprocess.DEVNULL, text=True, bufsize=1,
                            cwd=cwd, start_new_session=True)
    state = {"timed_out": False}

    def on_deadline():
        state["timed_out"] = True
        _terminate(proc)

    timer = threading.Timer(timeout, on_deadline)
    timer.start()
    # stderr is drained by a thread: leaving it unread deadlocks the child once the pipe fills.
    err = []
    drain = threading.Thread(target=lambda: err.append(proc.stderr.read() or ""), daemon=True)
    drain.start()
    lines, aborted = [], None
    try:
        for line in proc.stdout:
            lines.append(line)
            if watch is not None and watch.feed(line):
                aborted = watch.reason
                _terminate(proc)
                break
    finally:
        timer.cancel()
        try:
            proc.stdout.close()
        except OSError:
            pass
        rc = proc.wait()
        drain.join(timeout=5)
    return ("".join(lines), (124 if state["timed_out"] else rc), state["timed_out"], aborted,
            "".join(err))


def parse_events(stdout: str) -> dict:
    events = [json.loads(l) for l in stdout.splitlines() if l.strip().startswith("{")]
    agent_in = agent_out = agent_reasoning = steps = 0
    tools, texts = [], []
    nlq_p = nlq_c = 0
    for e in events:
        t = e.get("type")
        p = e.get("part", {}) or {}
        if t == "step_finish" and "tokens" in p:
            steps += 1
            agent_in += p["tokens"].get("input", 0)
            agent_out += p["tokens"].get("output", 0)
            # opencode reports reasoning SEPARATELY from output, and dropping it made the one
            # lever we cared about invisible: Qwen3.8-27B at 5-bit/xhigh vs 8-bit/thinking-off
            # showed 2094 vs 2120 median completion tokens, which looked like proof reasoning
            # had not changed. It could not have shown a change -- neither run counted a single
            # reasoning token. Kept separate from agent_out so the two eras stay comparable.
            agent_reasoning += p["tokens"].get("reasoning", 0)
        elif t == "tool_use":
            tools.append(p.get("tool"))
            out = (p.get("state") or {}).get("output", "") or ""
            for m in re.finditer(r"tokens: prompt=(\d+) completion=(\d+)", out):
                nlq_p += int(m.group(1))
                nlq_c += int(m.group(2))
        elif t == "text":
            tx = p.get("text") or ""
            if tx.strip():
                texts.append(tx)
    return {
        "agent_in": agent_in, "agent_out": agent_out, "agent_reasoning": agent_reasoning,
        "steps": steps, "tools": tools,
        "nlq_prompt": nlq_p, "nlq_completion": nlq_c, "final_text": "\n".join(texts),
    }


def parse_codex_events(stdout: str) -> dict:
    """Parse `codex exec --json` JSONL into the same shape as parse_events (opencode).
    Events: turn.completed{usage:{input_tokens,output_tokens,...}}; item.completed{item:{
    type:mcp_tool_call,tool,result}} and {item:{type:agent_message,text}}. output_tokens already
    includes reasoning tokens (OpenAI convention), so it is not added separately."""
    agent_in = agent_out = steps = 0
    tools, texts = [], []
    nlq_p = nlq_c = 0
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        if e.get("type") == "turn.completed":
            u = e.get("usage") or {}
            agent_in += u.get("input_tokens", 0)
            agent_out += u.get("output_tokens", 0)
            steps += 1
        elif e.get("type") == "item.completed":
            it = e.get("item") or {}
            if it.get("type") == "mcp_tool_call":
                tools.append(it.get("tool"))
                res = it.get("result") or {}
                for part in (res.get("content") or []):
                    for m in re.finditer(r"tokens: prompt=(\d+) completion=(\d+)", part.get("text", "") or ""):
                        nlq_p += int(m.group(1))
                        nlq_c += int(m.group(2))
            elif it.get("type") == "agent_message":
                tx = it.get("text") or ""
                if tx.strip():
                    texts.append(tx)
    return {
        "agent_in": agent_in, "agent_out": agent_out, "steps": steps, "tools": tools,
        "nlq_prompt": nlq_p, "nlq_completion": nlq_c, "final_text": "\n".join(texts),
    }


def codex_mcp_overrides(arm: str, corpus: str, mode: str) -> list:
    """codex `-c` MCP-server overrides for the arm (mirrors arm_opencode_config's MCP logic).
    Oracle mode pushes context in the prompt, so no live MCP; B0/B1-md get none."""
    if mode == "oracle":
        return []
    c = config.CORPORA[corpus]

    def srv(name: str, command: str, cmd_args: list, env: dict) -> list:
        toml_args = "[" + ", ".join(f'"{a}"' for a in cmd_args) + "]"
        toml_env = "{ " + ", ".join(f'{k} = "{v}"' for k, v in env.items()) + " }"
        return ["-c", f'mcp_servers.{name}.command="{command}"',
                "-c", f"mcp_servers.{name}.args={toml_args}",
                "-c", f"mcp_servers.{name}.env={toml_env}"]

    if arm == "B1-rag":
        return srv("freetext", str(config.VENV_PY),
                   [str(config.BENCH / "freetext_mcp" / "server.py")],
                   {"FREETEXT_CORPUS": str(config.corpus_chunks_path(corpus))})
    if arm == "B2":
        return srv("moosedev", config.MOOSEDEV_BIN, ["--connect"], {
            "MOOSEDEV_DATA_DIR": c["data_dir"], "MOOSEDEV_NO_AUTOSPAWN": "1",
            "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL, "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY,
            "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL})
    if arm == "B1-mem0":  # competitor: mem0 over its OWN capture of the raw docs (Lesson 440abc78)
        return srv("mem0", str(config.VENV_PY),
                   [str(config.BENCH / "mem0_mcp" / "server.py")],
                   {"MEM0_STORE": str(config.mem0_store_path(corpus) / "qdrant"),
                    "MEM0_CORPUS": corpus,
                    "MEM0_EMBED_MODEL": config.MEM0_EMBED_MODEL,
                    "MEM0_EMBED_DIMS": str(config.MEM0_EMBED_DIMS),
                    "OPENAI_API_KEY": "sk-noop"})
    return []


def run_cell(corpus: str, task_id: str, arm: str, model: str, mode: str = "tooluse",
             agent: str = None, variant: str = None, prompt_prefix: str = "",
             backend: str = "opencode", month: str = None, trial_epoch: str = None,
             moosedev_version: str = None, moosedev_binary_sha256: str = None) -> dict:
    task = load_task(corpus, task_id)
    is_code = task["type"] in CODE_TASK_TYPES
    run_id = f"{corpus}_{task_id}_{arm}_{mode}_{(agent or 'build')}_{uuid.uuid4().hex[:8]}"
    prompt = task["prompt"]
    if arm == "B1-notes" and mode == "oracle":
        # B1-notes is the agent-grep-the-real-docs baseline -> tooluse only. Oracle-over-notes (a
        # retriever pushing chunks of the real docs) is the future B1-rag-notes variant (AD b3205dcb).
        raise SystemExit("B1-notes is tooluse-only; oracle-over-notes is not implemented (use B1-rag).")
    if mode == "walk":
        # push what the harness pushes: this entity's dossier, nothing else
        if not task.get("anchor_file"):
            raise SystemExit("walk mode needs an anchor_file on the task "
                             "(currency_build.py writes one)")
        ctx = walk_context(corpus, task)
        prompt = ("Recorded project knowledge for the code in question, pushed from the "
                  "project graph (current knowledge only):\n\n"
                  f"{ctx}\n\n---\n\nTask:\n\n{task['prompt']}")
    elif mode == "oracle" and arm != "B0":  # inject what the agent's memory tool would have returned
        # Best-case retrieval: a focused topic (the task subject, NOT the answer), so a null is
        # unambiguous — the relevant record is front-and-center, isolating knowledge-value from both
        # fetch-willingness (H8) and query-quality (a verbose prompt dilutes BM25 and buries it).
        topic = task.get("memory_topic") or task["prompt"]
        # B1 arms get the FREE-TEXT push (BM25 over the export, currency-blind); B2 gets the
        # structured get_relevant_context (current-only). This is the only thing that makes push
        # differentiate B1 from B2 — see the currency test (oracle is otherwise arm-independent).
        # k=6 is right for a code task (a handful of records bear on the edit). A capability
        # question asks for a SET of 7-203 records, so 6 would cripple push by construction rather
        # than measure it. Capability cells push at the retrieval tool's own ceiling — push's
        # genuine best case. It still cannot deliver an exhaustive set, and that is the FINDING
        # (push is retrieval; completeness is a symbolic query), not a defect of the setup.
        k = 100 if task["type"] == "capability_qa" else 6
        if arm in ("B1-md", "B1-rag"):
            ctx = freetext_oracle_context(corpus, topic, k=k)
        else:
            ctx = oracle_context(corpus, topic, k=k)
        prompt = ("Relevant recorded project knowledge (architectural decisions, lessons, constraints) "
                  "retrieved from project memory — consult it where it applies:\n\n"
                  f"{ctx}\n\n---\n\nTask:\n\n{task['prompt']}")
    if prompt_prefix:  # diagnostic: forceful in-prompt guidance (e.g. "call get_relevant_context first")
        prompt = f"{prompt_prefix}\n\n{prompt}"
    wd = prepare_workdir(run_id, arm, corpus, task, mode, model)
    final_file = wd / "_codex_final.txt"  # codex -o canonical final message
    if backend == "codex":
        # codex CLI harness (codex subscription; more reliable GPT tool-calling). MCP per arm via
        # -c overrides; reads the -o final message. Q&A-focused (no patch extraction yet).
        cmd = ["codex", "exec", "-m", model, "--dangerously-bypass-approvals-and-sandbox",
               "--skip-git-repo-check", "--json", "-o", str(final_file)]
        cmd += codex_mcp_overrides(arm, corpus, mode)
        # Route the SAME model through another provider when Codex's hosted list drops it
        # (Sep 2026: gpt-5.4-mini vanished from the ChatGPT-account model list mid-trial).
        # OpenRouter's id carries the vendor prefix, so pass e.g. `-m openai/gpt-5.4-mini`.
        if provider := os.environ.get("CODEX_MODEL_PROVIDER"):
            cmd += ["-c", f'model_provider="{provider}"']
            if provider == "openrouter":
                cmd += ["-c", 'model_providers.openrouter.name="OpenRouter"',
                        "-c", 'model_providers.openrouter.base_url="https://openrouter.ai/api/v1"',
                        "-c", 'model_providers.openrouter.env_key="OPENROUTER_API_KEY"']
        if variant:  # codex reasoning effort, e.g. minimal|low|medium|high
            cmd += ["-c", f'model_reasoning_effort="{variant}"']
        cmd += [prompt]
        # A cold arm is only cold if the process cannot READ outside its workdir: codex's
        # bypass flag lifts its own seatbelt, and an unconfined B0 walked `..` into the frozen
        # task JSON (the gold) and sibling checkouts' rationale docs (Lesson 0ef057d5). The
        # profile is a macOS seatbelt (SBPL) file; sandbox-exec wraps the whole codex tree.
        if profile := os.environ.get("BENCH_SANDBOX_PROFILE"):
            cmd = ["sandbox-exec", "-f", profile] + cmd
        # Scope the egress allow-list proxy to the agent process only: the harness's own
        # calls (judge, MCP identity probes) must not be routed through it.
        if proxy := os.environ.get("BENCH_CODEX_PROXY"):
            cmd = ["env", f"HTTPS_PROXY={proxy}", f"HTTP_PROXY={proxy}"] + cmd
    else:
        # --pure: no external opencode plugins, so runs are insulated from the global setup.
        cmd = ["opencode", "run", "--pure", "--model", model, "--format", "json", "--dir", str(wd)]
        if agent:    # opencode agent/mode (build|plan|general|explore|custom); build is the default
            cmd += ["--agent", agent]
        if variant:  # provider reasoning effort (e.g. high|max|minimal)
            cmd += ["--variant", variant]
        cmd += [prompt]
    # The no-progress watcher reads opencode's event shape, which is verified against real
    # traces; codex runs unwatched rather than on a guessed schema, and the frontier control
    # it carries has never shown this pathology. stdin=DEVNULL throughout: `codex exec` reads
    # extra instructions from stdin when stdin is piped (not a TTY) and BLOCKS on EOF, so an
    # unattended launch hangs the full CELL_TIMEOUT with zero output. The prompt is always an
    # arg, so the agent never needs stdin. (Masked under an interactive `!` TTY.)
    # Both rules assume an IMMUTABLE environment: that the Nth identical call cannot return
    # anything new, and that a long run of filesystem calls cannot be the real work. Neither
    # holds in a code cell, where the workdir is writable — re-running one test command after
    # an edit is correct behaviour, and so is a burst of file calls after a single memory
    # lookup. Replaying all 1,314 traces on disk showed exactly that, and only that: the sole
    # false positives were code cells, one of them a score=1.0 frontier run. A sealed
    # capability_qa cell denies every write and carries an empty workdir, so there the premise
    # holds by construction and an identical call really is information-free.
    watch = (NoProgress(memory_server_arm(arm, mode))
             if backend == "opencode" and task["type"] == "capability_qa" else None)
    t0 = time.time()
    # A hung, slow or looping cell is a RESULT, not a crash: record it and grade whatever the
    # agent wrote on disk so far (a partial patch is still signal).
    stdout, returncode, timed_out, aborted, stderr = run_agent(
        cmd, str(wd) if backend == "codex" else None, config.CELL_TIMEOUT, watch)
    # A cell that dies before its first step produced no events to explain itself, so its
    # score is a rig failure wearing a measurement's clothes. Keep the agent's own words:
    # swallowing them is what turns a config typo into an afternoon of model-blaming.
    cell_errors = agent_errors(stdout)
    if returncode != 0:
        for msg in cell_errors[:3] or [(stderr or "(no stderr)").strip()[-1200:]]:
            print(f"  CELL FAILED exit={returncode}: {msg}", flush=True)
    wall_ms = int((time.time() - t0) * 1000)
    if backend == "codex":
        ev = parse_codex_events(stdout)
        if final_file.exists():
            ft = final_file.read_text().strip()
            if ft:
                ev["final_text"] = ft
    else:
        ev = parse_events(stdout)
    runs_dir = config.corpus_runs_path(corpus)  # private corpora -> BENCH_HOME, never the open repo
    (runs_dir / f"{run_id}.events.json").write_text(stdout or "")  # raw transcript: tool args + outputs

    if is_code:  # grade the patch (diff), save it as a referenced artifact
        subprocess.run(["git", *GIT_ID, "add", "-A"], cwd=wd, check=True)
        patch = subprocess.run(["git", *GIT_ID, "diff", "--cached"], cwd=wd,
                               capture_output=True, text=True).stdout
        (runs_dir / f"{run_id}.patch").write_text(patch)
        g = grade_patch(patch, task["ground_truth"])
        metrics = {k: g[k] for k in ("implemented", "violated", "complied", "files")}
        metrics["patch_len"] = len(patch)
    elif task["type"] == "capability_qa":  # set recall/precision/F1 vs the graph-derived expected set
        g = grade_set(ev["final_text"], task["ground_truth"])
        metrics = {k: g[k] for k in ("recall", "precision", "f1", "n_expected", "n_predicted", "n_matched")}
    else:
        g = grade(ev["final_text"], task["ground_truth"])
        metrics = {k: g[k] for k in ("coverage", "cited", "stale")}

    row = {
        "run_id": run_id, "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t0)),  # UTC cell start
        "corpus": corpus, "task_id": task_id, "task_type": task["type"],
        # longitudinal trial dimension: the checkpoint month (override) or the run's own month.
        # `corpus` already identifies the project (trivyn-trial / moose-trial), so no separate field.
        "month": month or time.strftime("%Y-%m", time.gmtime(t0)),
        # A trial epoch fingerprints the implementation under test while the probes, gold answers,
        # judge, and kill thresholds remain frozen. Non-trial runs leave these fields null.
        "trial_epoch": trial_epoch,
        "moosedev_version": moosedev_version,
        "moosedev_binary_sha256": moosedev_binary_sha256,
        "capability_class": task.get("capability_class"),  # grouping key for capability_qa rows
        "hop_count": task.get("hop_count"), "arm": arm, "mode": mode, "agent_model": model,
        "backend": backend,
        "internal_nlq_model": config.NLQ_MODEL if arm == "B2" else None,
        # The configuration that produced this number, so it never has to be reconstructed.
        "serving": serving_regime(model),
        "score": g["score"], "passed": g["passed"], "metrics": metrics,
        "tokens": {
            "agent_prompt": ev["agent_in"], "agent_completion": ev["agent_out"],
            "agent_reasoning": ev.get("agent_reasoning", 0),
            "internal_prompt": ev["nlq_prompt"], "internal_completion": ev["nlq_completion"],
        },
        # thrashing signals (collected for BOTH Q&A and code tasks): step count, the raw tool-call
        # sequence, per-tool counts, and total tool calls. agent flailing shows up here.
        "wall_clock_ms": wall_ms, "agent_steps": ev["steps"], "tool_calls": ev["tools"],
        "tool_counts": dict(Counter(ev["tools"])), "n_tool_calls": len(ev["tools"]),
        "final_text": ev["final_text"], "opencode_exit": returncode, "timed_out": timed_out,
        # An aborted cell was CUT, not answered: its score is a floor, not a measurement, and
        # reports must be able to tell the two apart. A scored zero says the model tried and
        # failed; this says the rig stopped it, and why.
        "aborted": bool(aborted), "abort_reason": aborted,
        "abort_detail": watch.detail if (watch and aborted) else None,
        # A cell that RAN TO COMPLETION and still produced no final text is a distinct
        # outcome from one that answered wrongly, and both score F1 0.000 — so the table
        # cannot tell them apart. Qwen3.5-122B does this (Lesson 4a26f1ce): it abstains
        # rather than confabulate.
        #
        # The completion condition is load-bearing, and the first version of this flag
        # omitted it and was wrong. A timed-out cell has no final text either, but because
        # it was KILLED mid-generation, not because the model declined — 10 of 17 flagged
        # rows turned out to be timeouts and one a signal kill (exit -5), which briefly made
        # a 27B infrastructure fault look like the model going quiet on pushed context.
        # Silence only means something when the model had the chance to speak.
        "empty_answer": (
            not (ev["final_text"] or "").strip() and not timed_out and returncode == 0
        ),
        "cell_errors": cell_errors or None,
        "stderr_tail": (stderr or "").strip()[-2000:] if returncode != 0 else None,
        "agent_provider": os.environ.get("CODEX_MODEL_PROVIDER") if backend == "codex" else None,
    }
    with open(runs_dir / "runs.jsonl", "a") as f:
        f.write(json.dumps(row) + "\n")
    if not config.KEEP_WORK:  # the workdir is throwaway (agents leave multi-GB target/node_modules);
        shutil.rmtree(wd, ignore_errors=True)  # the patch + events.json + row are already saved
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="moosedev")
    ap.add_argument("--task", default="shared_backend")
    ap.add_argument("--arm", choices=config.ARMS, help="single arm; default runs all")
    ap.add_argument("--model", default=None, help="agent model; defaults per backend")
    ap.add_argument("--backend", default="opencode", choices=["opencode", "codex"],
                    help="agent harness: opencode (default) or the codex CLI")
    ap.add_argument("--mode", default="tooluse", choices=["tooluse", "oracle", "walk"])
    ap.add_argument("--agent", default=None, help="opencode agent/mode: build|plan|general|explore")
    ap.add_argument("--variant", default=None, help="reasoning effort, e.g. high|max (opencode) | low|medium (codex)")
    ap.add_argument("--prompt-prefix", default="", help="diagnostic: text prepended to the task prompt")
    ap.add_argument("--month", default=None, help="trial month label YYYY-MM (default: the run's month)")
    ap.add_argument("--trial-epoch", default=None, help="versioned trial implementation epoch")
    ap.add_argument("--moosedev-version", default=None, help="running backend health version")
    ap.add_argument("--moosedev-binary-sha256", default=None, help="SHA-256 of the trial binary")
    ap.add_argument("--print-trial-identity", metavar="DATA_DIR",
                    help="print epoch, health version, and binary SHA-256, then exit")
    ap.add_argument("--trial-daemon-pid", type=int,
                    help="expected harness-owned daemon PID for identity verification")
    ap.add_argument("--expected-binary-sha256",
                    help="binary digest captured immediately before daemon startup")
    args = ap.parse_args()

    if args.print_trial_identity:
        identity = trial_identity(args.print_trial_identity, args.trial_daemon_pid,
                                  args.expected_binary_sha256)
        print("\t".join(identity[k] for k in (
            "trial_epoch", "moosedev_version", "moosedev_binary_sha256")))
        return

    supplied_identity = (args.trial_epoch, args.moosedev_version, args.moosedev_binary_sha256)
    if any(supplied_identity) and not all(supplied_identity):
        ap.error("--trial-epoch, --moosedev-version, and --moosedev-binary-sha256 must be supplied together")

    model = args.model or ("gpt-5.5" if args.backend == "codex" else config.AGENT_MODEL)
    arms = [args.arm] if args.arm else config.ARMS
    rows = []
    for arm in arms:
        print(f"\n=== running {arm} ({args.mode}, backend={args.backend}, model={model}) ===", flush=True)
        try:
            row = run_cell(args.corpus, args.task, arm, model, args.mode, args.agent,
                           args.variant, args.prompt_prefix, args.backend, args.month,
                           args.trial_epoch, args.moosedev_version, args.moosedev_binary_sha256)
        except Exception as e:  # one arm's failure must not abort the rest of the matrix
            print(f"  ARM FAILED: {type(e).__name__}: {e}", flush=True)
            continue
        rows.append(row)
        tk = row["tokens"]
        metrics = " ".join(f"{k}={v}" for k, v in row["metrics"].items() if k != "files")
        to = " TIMEOUT" if row.get("timed_out") else ""
        if row.get("aborted"):  # visible in the rung's line, so a cut cell is never read as a score
            to += f" ABORTED[{row['abort_reason']}: {row['abort_detail']}]"
        if row.get("empty_answer"):
            to += " EMPTY-ANSWER"
        print(f"  score={row['score']} passed={row['passed']} {metrics}{to} "
              f"steps={row['agent_steps']} tools={row['tool_calls']}")
        print(f"  agent_tokens={tk['agent_prompt']}+{tk['agent_completion']} "
              f"internal_nlq={tk['internal_prompt']}+{tk['internal_completion']} "
              f"wall={row['wall_clock_ms']}ms exit={row['opencode_exit']}")

    print("\n==== SUMMARY ====")
    print(f"{'arm':<6} {'score':>5} {'pass':>5} {'agent_tok':>10} {'nlq_tok':>8} {'wall_ms':>8}  metrics")
    for r in rows:
        tk = r["tokens"]
        metrics = " ".join(f"{k}={v}" for k, v in r["metrics"].items() if k != "files")
        print(f"{r['arm']:<6} {r['score']:>5} {str(r['passed']):>5} "
              f"{tk['agent_prompt'] + tk['agent_completion']:>10} "
              f"{tk['internal_prompt'] + tk['internal_completion']:>8} {r['wall_clock_ms']:>8}  {metrics}")


if __name__ == "__main__":
    main()
