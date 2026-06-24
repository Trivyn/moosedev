"""Run benchmark cells: one (corpus, task, arm) via opencode headless -> a JSONL row.

Skeleton scope: tooluse mode, single model, MOOSEDev-on-itself. Arms differ ONLY in the memory MCP
injected via a per-arm project-local opencode.json + symmetric AGENTS.md overlay.

Usage:
  python run.py                      # run all arms for the skeleton task, print a summary
  python run.py --arm B2             # run a single arm
"""
import argparse
import json
import re
import shlex
from collections import Counter
import shutil
import subprocess
from pathlib import Path
import time
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


def load_task(corpus: str, task_id: str) -> dict:
    return json.loads((config.corpus_tasks_path(corpus) / f"{task_id}.json").read_text())


def arm_opencode_config(arm: str, corpus: str, mode: str = "tooluse") -> dict:
    """Project-local opencode.json: disable the global omni MCP, add the arm's memory MCP (if any).
    In ORACLE mode the harness prepends retrieved knowledge to the prompt instead, so no live memory
    tool is given — isolating knowledge-value from the agent's willingness to call the tool (H8)."""
    c = config.CORPORA[corpus]
    mcp = {"omni": {"type": "local", "command": ["/opt/homebrew/bin/omni", "--mcp"], "enabled": False}}
    if mode != "oracle":
        if arm == "B1-rag":
            mcp["freetext-recall"] = {
                "type": "local",
                "command": [str(config.VENV_PY), str(config.BENCH / "freetext_mcp" / "server.py")],
                "environment": {"FREETEXT_CORPUS": str(config.corpus_chunks_path(corpus))},
                "enabled": True,
            }
        elif arm == "B2":
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
    }


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
    ref = c.get("sha", "HEAD")
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


def prepare_workdir(run_id: str, arm: str, corpus: str, task: dict, mode: str = "tooluse"):
    wd = config.WORK_ROOT / run_id
    wd.mkdir(parents=True, exist_ok=True)
    if task["type"] in TREE_TASK_TYPES and task.get("materialize_tree", True):
        materialize_tree(corpus, wd)  # a task may opt out (e.g. a pure memory-currency Q&A)
    shutil.copy(config.BENCH / "arms" / arm / "AGENTS.md", wd / "AGENTS.md")
    (wd / "opencode.json").write_text(json.dumps(arm_opencode_config(arm, corpus, mode), indent=2))
    if arm == "B1-md":
        write_markdown_corpus(corpus, wd)
    if arm == "B1-notes":
        write_notes_corpus(corpus, wd)
    if task["type"] in CODE_TASK_TYPES:
        git_baseline(wd)  # overlays (AGENTS.md, docs/) are baseline too -> excluded from the diff
    return wd


def parse_events(stdout: str) -> dict:
    events = [json.loads(l) for l in stdout.splitlines() if l.strip().startswith("{")]
    agent_in = agent_out = steps = 0
    tools, texts = [], []
    nlq_p = nlq_c = 0
    for e in events:
        t = e.get("type")
        p = e.get("part", {}) or {}
        if t == "step_finish" and "tokens" in p:
            steps += 1
            agent_in += p["tokens"].get("input", 0)
            agent_out += p["tokens"].get("output", 0)
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
        "agent_in": agent_in, "agent_out": agent_out, "steps": steps, "tools": tools,
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
             backend: str = "opencode", month: str = None) -> dict:
    task = load_task(corpus, task_id)
    is_code = task["type"] in CODE_TASK_TYPES
    run_id = f"{corpus}_{task_id}_{arm}_{mode}_{(agent or 'build')}_{uuid.uuid4().hex[:8]}"
    prompt = task["prompt"]
    if arm == "B1-notes" and mode == "oracle":
        # B1-notes is the agent-grep-the-real-docs baseline -> tooluse only. Oracle-over-notes (a
        # retriever pushing chunks of the real docs) is the future B1-rag-notes variant (AD b3205dcb).
        raise SystemExit("B1-notes is tooluse-only; oracle-over-notes is not implemented (use B1-rag).")
    if mode == "oracle" and arm != "B0":  # inject what the agent's memory tool would have returned
        # Best-case retrieval: a focused topic (the task subject, NOT the answer), so a null is
        # unambiguous — the relevant record is front-and-center, isolating knowledge-value from both
        # fetch-willingness (H8) and query-quality (a verbose prompt dilutes BM25 and buries it).
        topic = task.get("memory_topic") or task["prompt"]
        # B1 arms get the FREE-TEXT push (BM25 over the export, currency-blind); B2 gets the
        # structured get_relevant_context (current-only). This is the only thing that makes push
        # differentiate B1 from B2 — see the currency test (oracle is otherwise arm-independent).
        if arm in ("B1-md", "B1-rag"):
            ctx = freetext_oracle_context(corpus, topic, k=6)
        else:
            ctx = oracle_context(corpus, topic, k=6)
        prompt = ("Relevant recorded project knowledge (architectural decisions, lessons, constraints) "
                  "retrieved from project memory — consult it where it applies:\n\n"
                  f"{ctx}\n\n---\n\nTask:\n\n{task['prompt']}")
    if prompt_prefix:  # diagnostic: forceful in-prompt guidance (e.g. "call get_relevant_context first")
        prompt = f"{prompt_prefix}\n\n{prompt}"
    wd = prepare_workdir(run_id, arm, corpus, task, mode)
    final_file = wd / "_codex_final.txt"  # codex -o canonical final message
    if backend == "codex":
        # codex CLI harness (codex subscription; more reliable GPT tool-calling). MCP per arm via
        # -c overrides; reads the -o final message. Q&A-focused (no patch extraction yet).
        cmd = ["codex", "exec", "-m", model, "--dangerously-bypass-approvals-and-sandbox",
               "--skip-git-repo-check", "--json", "-o", str(final_file)]
        cmd += codex_mcp_overrides(arm, corpus, mode)
        if variant:  # codex reasoning effort, e.g. minimal|low|medium|high
            cmd += ["-c", f'model_reasoning_effort="{variant}"']
        cmd += [prompt]
    else:
        # --pure: no external opencode plugins, so runs are insulated from the global setup.
        cmd = ["opencode", "run", "--pure", "--model", model, "--format", "json", "--dir", str(wd)]
        if agent:    # opencode agent/mode (build|plan|general|explore|custom); build is the default
            cmd += ["--agent", agent]
        if variant:  # provider reasoning effort (e.g. high|max|minimal)
            cmd += ["--variant", variant]
        cmd += [prompt]
    t0 = time.time()
    timed_out = False
    try:
        # stdin=DEVNULL: `codex exec` reads extra instructions from stdin when stdin is piped (not a
        # TTY) and BLOCKS on EOF — so an unattended/agent launch (no TTY) hangs the full CELL_TIMEOUT
        # with zero output. The prompt is always passed as an arg, so the agent never needs stdin.
        # (Masked when launched via the user's interactive `!` TTY; surfaces under automation.)
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=config.CELL_TIMEOUT,
                              stdin=subprocess.DEVNULL,
                              cwd=str(wd) if backend == "codex" else None)
        stdout, returncode = proc.stdout, proc.returncode
    except subprocess.TimeoutExpired as e:
        # A hung/slow cell is a RESULT, not a crash: record it and grade whatever the agent wrote
        # on disk so far (a partial patch is still signal — e.g. a violating import already added).
        timed_out = True
        out = e.stdout
        stdout = out.decode() if isinstance(out, (bytes, bytearray)) else (out or "")
        returncode = 124
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
        "capability_class": task.get("capability_class"),  # grouping key for capability_qa rows
        "hop_count": task.get("hop_count"), "arm": arm, "mode": mode, "agent_model": model,
        "backend": backend,
        "internal_nlq_model": config.NLQ_MODEL if arm == "B2" else None,
        "score": g["score"], "passed": g["passed"], "metrics": metrics,
        "tokens": {
            "agent_prompt": ev["agent_in"], "agent_completion": ev["agent_out"],
            "internal_prompt": ev["nlq_prompt"], "internal_completion": ev["nlq_completion"],
        },
        # thrashing signals (collected for BOTH Q&A and code tasks): step count, the raw tool-call
        # sequence, per-tool counts, and total tool calls. agent flailing shows up here.
        "wall_clock_ms": wall_ms, "agent_steps": ev["steps"], "tool_calls": ev["tools"],
        "tool_counts": dict(Counter(ev["tools"])), "n_tool_calls": len(ev["tools"]),
        "final_text": ev["final_text"], "opencode_exit": returncode, "timed_out": timed_out,
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
    ap.add_argument("--mode", default="tooluse", choices=["tooluse", "oracle"])
    ap.add_argument("--agent", default=None, help="opencode agent/mode: build|plan|general|explore")
    ap.add_argument("--variant", default=None, help="reasoning effort, e.g. high|max (opencode) | low|medium (codex)")
    ap.add_argument("--prompt-prefix", default="", help="diagnostic: text prepended to the task prompt")
    ap.add_argument("--month", default=None, help="trial month label YYYY-MM (default: the run's month)")
    args = ap.parse_args()

    model = args.model or ("gpt-5.5" if args.backend == "codex" else config.AGENT_MODEL)
    arms = [args.arm] if args.arm else config.ARMS
    rows = []
    for arm in arms:
        print(f"\n=== running {arm} ({args.mode}, backend={args.backend}, model={model}) ===", flush=True)
        try:
            row = run_cell(args.corpus, args.task, arm, model, args.mode, args.agent,
                           args.variant, args.prompt_prefix, args.backend, args.month)
        except Exception as e:  # one arm's failure must not abort the rest of the matrix
            print(f"  ARM FAILED: {type(e).__name__}: {e}", flush=True)
            continue
        rows.append(row)
        tk = row["tokens"]
        metrics = " ".join(f"{k}={v}" for k, v in row["metrics"].items() if k != "files")
        to = " TIMEOUT" if row.get("timed_out") else ""
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
