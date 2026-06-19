"""Temporal (git-walk) bootstrap: replay a repo's trunk history oldest->newest into a MOOSEDev
graph with real supersession chains, a real timeline, and real provenance.

Hybrid (invariant #1): this DETERMINISTIC driver does the git-walk, triage, strict ordering +
look-back, and snapshotting. The per-episode CAPTURE is a real headless agent (`claude -p`)
following skills/temporal-episode-capture.md against the shared store — it recalls -> aligns ->
records -> relates -> supersedes -> validates exactly as in normal work, stamping records with the
commit's date + author (the optional MCP timestamp/author args). One agent per commit, strictly
sequential, so each episode sees only the graph as it stood before it (look-back, never forward).

  bench/.venv/bin/python bench/temporal_bootstrap.py --repo <path> --data-dir <fresh store> \
      [--trunk main] [--limit N] [--dry-run] [--resume] [--model sonnet] [--milestone-every K]
"""
import argparse
import asyncio
import json
import os
import re
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

import config
from mcp_client import call_tool

REPO_ROOT = config.REPO                                    # the moosedev repo (for the skill + --add-dir)
SKILL = REPO_ROOT / "skills" / "temporal-episode-capture.md"
PROJECT_GRAPH = "https://moosedev.dev/kg/project"
PROV_GRAPH = "https://moosedev.dev/kg/provenance"
RDFS_LABEL = "http://www.w3.org/2000/01/rdf-schema#label"

# --- git layer (deterministic, read-only; bench/run.py:120 style) ------------------------------


def _git(repo: str, *args: str) -> str:
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout


def detect_trunk(repo: str, override: str | None) -> str:
    if override:
        return override
    try:
        ref = _git(repo, "symbolic-ref", "refs/remotes/origin/HEAD").strip()
        if ref:
            return ref.rsplit("/", 1)[-1]
    except subprocess.CalledProcessError:
        pass
    try:
        m = re.search(r"HEAD branch:\s*(\S+)", _git(repo, "remote", "show", "origin"))
        if m and m.group(1) != "(unknown)":
            return m.group(1)
    except subprocess.CalledProcessError:
        pass
    for name in ("main", "master", "develop", "dev", "trunk"):
        try:
            _git(repo, "rev-parse", "--verify", name)
            return name
        except subprocess.CalledProcessError:
            continue
    raise SystemExit(f"could not detect trunk for {repo}; pass --trunk")


US, RS = "\x1f", "\x1e"  # field / record separators (survive multi-line commit bodies)
_FMT = US.join(["%H", "%an", "%ae", "%aI", "%P", "%s", "%b"]) + RS


@dataclass
class Episode:
    sha: str
    author: str
    email: str
    date: str  # %aI, RFC3339 — passed verbatim as the record timestamp
    subject: str
    body: str
    is_merge: bool


def enumerate_episodes(repo: str, trunk: str) -> list[Episode]:
    """First-parent trunk history, oldest->newest. --first-parent excludes commits from branches
    never merged into trunk (abandoned work is unreachable); merged work enters via its merge."""
    raw = _git(repo, "log", "--first-parent", "--reverse", f"--format={_FMT}", trunk)
    eps: list[Episode] = []
    for rec in raw.split(RS):
        rec = rec.strip("\n")
        if not rec:
            continue
        sha, an, ae, date, parents, subject, body = rec.split(US, 6)
        eps.append(Episode(sha, an, ae, date, subject, body, len(parents.split()) > 1))
    return eps


def episode_diff(repo: str, ep: Episode, max_bytes: int) -> tuple[str, list[str], bool]:
    if ep.is_merge:
        diff = _git(repo, "diff", f"{ep.sha}^1", ep.sha)  # show --first-parent is wrong for merges
    else:
        diff = _git(repo, "show", "--first-parent", "--format=", ep.sha)
    files = re.findall(r"^\+\+\+ b/(.+)$", diff, re.M)
    truncated = False
    if len(diff.encode()) > max_bytes:
        diff = diff.encode()[:max_bytes].decode("utf-8", "ignore")
        diff += f"\n\n[TRUNCATED to {max_bytes} bytes; {len(files)} files changed]"
        truncated = True
    return diff, files, truncated


# --- triage (deterministic skip of non-decision episodes; conservative — favor SEND) -----------

_SKIP_SUBJECT = re.compile(
    r"^(merge branch|merge remote|merge pull|bump |chore\(deps\)|cargo fmt|rustfmt|gofmt|"
    r"prettier|fmt:|fmt$|typo|fix typo|wip\b|version bump|release v?\d|update changelog|"
    r"update lockfile|clippy)", re.I)
_NOISE = re.compile(r"(Cargo\.lock|package-lock\.json|go\.sum|bun\.lock|\.gitignore$|^\.github/|"
                    r"^vendor/|^node_modules/|\.snap$|__pycache__)")
_WHY_CUE = re.compile(r"\b(because|so that|in order to|instead of|we tried|trade-?off|never|"
                      r"must not|revert|superse|replace|no longer|deprecat|drop\b|remove)\b", re.I)


def triage(ep: Episode, diff: str, files: list[str]) -> str | None:
    """Return a skip-reason, or None to SEND. Conservative — favor SEND; every skip is reported."""
    if not files:
        return "empty-diff"
    if ep.is_merge:
        return None  # always SEND non-empty merges — feature decisions land at the merge commit
    why = _WHY_CUE.search(f"{ep.subject}\n{ep.body}")
    if _SKIP_SUBJECT.search(ep.subject) and len(ep.body.strip()) < 40 and not why:
        return f"mechanical:{ep.subject[:40]}"  # e.g. cargo fmt / bump / typo, at any diff size
    if all(_NOISE.search(f) for f in files):
        return "all-noise-paths"
    changed = len(re.findall(r"(?m)^\+(?!\+\+)", diff)) + len(re.findall(r"(?m)^-(?!--)", diff))
    if changed <= config.TB_MIN_LINES and not why:
        return "tiny-no-why-cue"
    return None


# --- backend lifecycle (one serve for the whole walk; single-writer, sequential) ---------------


def start_serve(data_dir: str) -> subprocess.Popen:
    Path(data_dir).mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "MOOSEDEV_DATA_DIR": data_dir, "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
           "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL, "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY,
           "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL}
    proc = subprocess.Popen([config.MOOSEDEV_BIN, "--serve"], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sock = Path(data_dir) / "moosedev.sock"
    for _ in range(240):  # up to ~120s for a fresh store's first vector-index build
        if sock.exists():
            return proc
        if proc.poll() is not None:
            raise RuntimeError(f"moosedev --serve exited early (code {proc.returncode})")
        time.sleep(0.5)
    proc.terminate()
    raise RuntimeError("moosedev --serve did not become ready (socket never appeared)")


def mcp(data_dir: str, tool: str, args: dict) -> str:
    """Direct tool call against the running serve (NO_AUTOSPAWN — connect to our backend)."""
    env = {"MOOSEDEV_DATA_DIR": data_dir, "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
           "MOOSEDEV_NO_AUTOSPAWN": "1"}
    return asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"], env, tool, args))


def _count(out: str) -> int:
    m = re.search(r'"value"\s*:\s*"(\d+)"', out)
    return int(m.group(1)) if m else 0


def count_records(data_dir: str) -> int:
    return _count(mcp(data_dir, "sparql", {"query":
        f"SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ GRAPH <{PROJECT_GRAPH}> "
        f"{{ ?s <{RDFS_LABEL}> ?l }} }}"}))


def records_on_day(data_dir: str, date: str) -> int:
    """Records whose hasTimestamp falls on the commit's calendar day. A guard that the agent
    stamped the COMMIT date, not 'now' (day-level is robust to UTC offset normalization)."""
    return _count(mcp(data_dir, "sparql", {"query":
        f"SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ GRAPH <{PROJECT_GRAPH}> {{ ?s ?p ?t . "
        f'FILTER(STRENDS(STR(?p),"hasTimestamp") && STRSTARTS(STR(?t),"{date[:10]}")) }} }}'}))


def snapshot(data_dir: str, label: str) -> None:
    root = Path(config.TB_SNAPSHOT_ROOT)
    root.mkdir(parents=True, exist_ok=True)
    for graph, suffix in ((PROJECT_GRAPH, "project"), (PROV_GRAPH, "prov")):
        nt = mcp(data_dir, "sparql", {"query":
            f"CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{graph}> {{ ?s ?p ?o }} }}"})
        (root / f"{label}.{suffix}.nt").write_text(nt)


# --- per-episode capture agent (claude -p, the normal workflow) --------------------------------


def write_mcp_config(data_dir: str) -> str:
    cfg = {"mcpServers": {"moosedev": {"command": config.MOOSEDEV_BIN, "args": ["--connect"],
            "env": {"MOOSEDEV_DATA_DIR": data_dir, "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
                    "MOOSEDEV_NO_AUTOSPAWN": "1"}}}}
    path = Path(config.TB_SNAPSHOT_ROOT).parent / "temporal-mcp.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(cfg, indent=2))
    return str(path)


def episode_prompt(ep: Episode, diff: str, repo_name: str) -> str:
    return f"""You are the temporal-bootstrap capture agent for commit {ep.sha[:10]} of `{repo_name}`.

Read {SKILL} and follow it EXACTLY for THIS single commit. A `moosedev` MCP server is attached to
the shared in-progress store; it holds only decisions from EARLIER commits, so recall it first.

COMMIT {ep.sha}
AUTHOR: {ep.author} <{ep.email}>
DATE: {ep.date}
SUBJECT: {ep.subject}
BODY:
{ep.body.strip() or "(none)"}

DIFF:
{diff}

CRITICAL: pass timestamp="{ep.date}" and author="{ep.author} <{ep.email}>" on EVERY
record_important_decision and supersede_decision call (the commit's values, never now / yourself).
If this commit reverses a decision you find in recall, supersede that existing IRI (do not invent).
If it is not decision-bearing, record nothing and say so. End with the skill's report."""


def run_capture_agent(ep: Episode, diff: str, repo_name: str, mcp_config: str, model: str) -> str:
    cmd = ["claude", "-p", episode_prompt(ep, diff, repo_name),
           "--mcp-config", mcp_config, "--add-dir", str(REPO_ROOT),
           "--model", model, "--dangerously-skip-permissions",
           "--max-turns", str(config.TB_MAX_TURNS)]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=900)
    except subprocess.TimeoutExpired:
        return "[capture agent TIMED OUT after 900s]"
    return (r.stdout or "")[-600:] + (f"\n[stderr] {r.stderr[-300:]}" if r.returncode != 0 else "")


# --- run loop ----------------------------------------------------------------------------------


def run(repo, data_dir, trunk_override, limit, dry_run, resume, model, milestone_every):
    trunk = detect_trunk(repo, trunk_override)
    repo_name = Path(repo).name
    episodes = enumerate_episodes(repo, trunk)
    if limit:
        episodes = episodes[:limit]
    print(f"trunk={trunk}  episodes={len(episodes)}  repo={repo_name}  store={data_dir}", flush=True)

    applied_log = Path(data_dir) / "temporal-applied.log"
    done = set(applied_log.read_text().split()) if (resume and applied_log.exists()) else set()

    if dry_run:
        sent = skipped = 0
        for i, ep in enumerate(episodes, 1):
            diff, files, _ = episode_diff(repo, ep, config.TB_MAX_DIFF_BYTES)
            reason = triage(ep, diff, files)
            if reason:
                skipped += 1
                print(f"[{i}/{len(episodes)}] SKIP {ep.sha[:8]} ({reason})", flush=True)
            else:
                sent += 1
                tag = "merge" if ep.is_merge else "commit"
                print(f"[{i}/{len(episodes)}] SEND {ep.sha[:8]} [{tag}] {ep.subject[:66]}", flush=True)
        print(f"\nDRY RUN: would send {sent}, skip {skipped} (of {len(episodes)})", flush=True)
        return

    mcp_config = write_mcp_config(data_dir)
    serve = start_serve(data_dir)
    applied = sent = skipped = 0
    try:
        for i, ep in enumerate(episodes, 1):
            if ep.sha in done:
                continue
            diff, files, _ = episode_diff(repo, ep, config.TB_MAX_DIFF_BYTES)
            reason = triage(ep, diff, files)
            if reason:
                skipped += 1
                print(f"[{i}/{len(episodes)}] SKIP {ep.sha[:8]} ({reason})", flush=True)
                continue
            sent += 1
            before = count_records(data_dir)
            run_capture_agent(ep, diff, repo_name, mcp_config, model)
            new = count_records(data_dir) - before
            on_day = records_on_day(data_dir, ep.date)
            applied += 1
            applied_log.open("a").write(ep.sha + "\n")
            flag = "" if (new == 0 or on_day >= new) else "  !!STAMP-MISMATCH (records not on commit day)"
            print(f"[{i}/{len(episodes)}] DONE {ep.sha[:8]} +{new} records (on-day~{on_day}){flag}", flush=True)
            if applied % config.TB_VALIDATE_EVERY == 0:
                print(f"    {mcp(data_dir, 'validate_against_architecture', {}).splitlines()[0]}", flush=True)
            if milestone_every and applied % milestone_every == 0:
                snapshot(data_dir, f"{i:04d}_{ep.sha[:8]}")
        snapshot(data_dir, "final")
        print(f"validate(final): {mcp(data_dir, 'validate_against_architecture', {}).splitlines()[0]}", flush=True)
    finally:
        serve.terminate()
        try:
            serve.wait(timeout=10)
        except subprocess.TimeoutExpired:
            serve.kill()
    print(f"\nsummary: sent={sent} applied={applied} skipped={skipped} (of {len(episodes)})", flush=True)


def main():
    ap = argparse.ArgumentParser(description="Temporal git-walk bootstrap into a MOOSEDev store.")
    ap.add_argument("--repo", required=True, help="path to the git repo to replay")
    ap.add_argument("--data-dir", required=True, help="fresh moosedev store to build into")
    ap.add_argument("--trunk", default=None, help="trunk branch (default: auto-detect)")
    ap.add_argument("--limit", type=int, default=None, help="only the first N episodes")
    ap.add_argument("--dry-run", action="store_true", help="enumerate + triage only, no capture")
    ap.add_argument("--resume", action="store_true", help="skip episodes in temporal-applied.log")
    ap.add_argument("--model", default=config.TB_CAPTURE_MODEL, help="claude -p model")
    ap.add_argument("--milestone-every", type=int, default=0, help="snapshot every K applied episodes")
    a = ap.parse_args()
    run(a.repo, a.data_dir, a.trunk, a.limit, a.dry_run, a.resume, a.model, a.milestone_every)


if __name__ == "__main__":
    main()
