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
import time
import uuid

import config
from grade import grade
from grade_code import grade_patch

# Code tasks materialize the corpus working tree and are graded on the resulting patch (diff);
# Q&A tasks run in an empty workdir and are graded on the final assistant message.
CODE_TASK_TYPES = {"constraint_code"}
GIT_ID = ["-c", "user.email=bench@local", "-c", "user.name=bench"]


def load_task(corpus: str, task_id: str) -> dict:
    return json.loads((config.BENCH / "tasks_public" / corpus / f"{task_id}.json").read_text())


def arm_opencode_config(arm: str, corpus: str) -> dict:
    """Project-local opencode.json: disable the global omni MCP, add the arm's memory MCP (if any)."""
    c = config.CORPORA[corpus]
    mcp = {"omni": {"type": "local", "command": ["/opt/homebrew/bin/omni", "--mcp"], "enabled": False}}
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
    return {"$schema": "https://opencode.ai/config.json", "mcp": mcp}


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


def materialize_tree(corpus: str, wd):
    """Lay down the corpus's pinned tracked tree (no .git, no gitignored target/.moosedev) into wd
    via `git archive`. Excluding .git enforces the comprehension-debt premise (no history for the
    agent); .env is dropped too."""
    c = config.CORPORA[corpus]
    ref = c.get("sha", "HEAD")
    cmd = f"git -C {shlex.quote(c['repo'])} archive {shlex.quote(ref)} | tar -x -C {shlex.quote(str(wd))}"
    subprocess.run(cmd, shell=True, check=True)
    (wd / ".env").unlink(missing_ok=True)


def git_baseline(wd):
    """Commit the materialized tree + overlays as a baseline, so the agent's edits are diffable."""
    subprocess.run(["git", "init", "-q"], cwd=wd, check=True)
    subprocess.run(["git", *GIT_ID, "add", "-A"], cwd=wd, check=True)
    subprocess.run(["git", *GIT_ID, "commit", "-q", "-m", "baseline"], cwd=wd, check=True)


def prepare_workdir(run_id: str, arm: str, corpus: str, task: dict):
    wd = config.WORK_ROOT / run_id
    wd.mkdir(parents=True, exist_ok=True)
    if task["type"] in CODE_TASK_TYPES:
        materialize_tree(corpus, wd)
    shutil.copy(config.BENCH / "arms" / arm / "AGENTS.md", wd / "AGENTS.md")
    (wd / "opencode.json").write_text(json.dumps(arm_opencode_config(arm, corpus), indent=2))
    if arm == "B1-md":
        write_markdown_corpus(corpus, wd)
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


def run_cell(corpus: str, task_id: str, arm: str, model: str) -> dict:
    task = load_task(corpus, task_id)
    is_code = task["type"] in CODE_TASK_TYPES
    run_id = f"{corpus}_{task_id}_{arm}_{uuid.uuid4().hex[:8]}"
    wd = prepare_workdir(run_id, arm, corpus, task)
    # --pure: no external opencode plugins, so runs are insulated from the global setup.
    cmd = ["opencode", "run", "--pure", "--model", model, "--format", "json",
           "--dir", str(wd), task["prompt"]]
    t0 = time.time()
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    wall_ms = int((time.time() - t0) * 1000)
    ev = parse_events(proc.stdout)
    config.RUNS_DIR.mkdir(parents=True, exist_ok=True)

    if is_code:  # grade the patch (diff), save it as a referenced artifact
        subprocess.run(["git", *GIT_ID, "add", "-A"], cwd=wd, check=True)
        patch = subprocess.run(["git", *GIT_ID, "diff", "--cached"], cwd=wd,
                               capture_output=True, text=True).stdout
        (config.RUNS_DIR / f"{run_id}.patch").write_text(patch)
        g = grade_patch(patch, task["ground_truth"])
        metrics = {k: g[k] for k in ("implemented", "violated", "complied", "files")}
        metrics["patch_len"] = len(patch)
    else:
        g = grade(ev["final_text"], task["ground_truth"])
        metrics = {k: g[k] for k in ("coverage", "cited")}

    row = {
        "run_id": run_id, "corpus": corpus, "task_id": task_id, "task_type": task["type"],
        "hop_count": task.get("hop_count"), "arm": arm, "mode": "tooluse", "agent_model": model,
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
        "final_text": ev["final_text"], "opencode_exit": proc.returncode,
    }
    with open(config.RUNS_DIR / "skeleton.jsonl", "a") as f:
        f.write(json.dumps(row) + "\n")
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="moosedev")
    ap.add_argument("--task", default="shared_backend")
    ap.add_argument("--arm", choices=config.ARMS, help="single arm; default runs all")
    ap.add_argument("--model", default=config.AGENT_MODEL)
    args = ap.parse_args()

    arms = [args.arm] if args.arm else config.ARMS
    rows = []
    for arm in arms:
        print(f"\n=== running {arm} ===", flush=True)
        row = run_cell(args.corpus, args.task, arm, args.model)
        rows.append(row)
        tk = row["tokens"]
        metrics = " ".join(f"{k}={v}" for k, v in row["metrics"].items() if k != "files")
        print(f"  score={row['score']} passed={row['passed']} {metrics} "
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
