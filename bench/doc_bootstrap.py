"""Doc-driven bootstrap: decompose a project's DESIGN DOCS into a MOOSEDev graph (typed records +
edges), for repos whose decisions live in docs rather than recoverable from commit diffs (Lesson
2b1513e6 — a code repo's git history is the wrong source; a decision-bearing artifact is right).

Hybrid (invariant #1): this deterministic driver enumerates + section-chunks the docs and runs ONE
headless capture agent per chunk (codex), strictly sequential against a shared store, each following
skills/bootstrap-existing-codebase.md — recall -> align -> record (typed) -> relate -> supersede.
Reuses temporal_bootstrap's serve/mcp/count/snapshot machinery; only the SOURCE differs (docs, not
commits). Resumable via a docs-applied.log.

  bench/.venv/bin/python bench/doc_bootstrap.py --repo ~/code/codegraph \
      --data-dir ~/.moosedev-stores/codegraph [--limit N] [--resume] [--dry-run] [--model gpt-5.4]
"""
import argparse
import re
import subprocess
import time
from pathlib import Path

import config
from temporal_bootstrap import (start_serve, mcp, count_records, snapshot, _codex_moosedev_overrides,
                                 CAPTURE_TS_FILE, CAPTURE_AUTHOR_FILE, REPO_ROOT)

SKILL = REPO_ROOT / "skills" / "bootstrap-existing-codebase.md"
MAX_CHARS = 14000  # per capture chunk — keep one coherent slice of decisions in the agent's focus


def enumerate_docs(repo: str) -> list[tuple[str, str, str]]:
    """Return [(doc_id, date, content), …] — design docs + plans (decision-dense), then the CHANGELOG
    oldest→newest (so a later 'Removed X' can supersede an earlier 'Added X'). Large docs are split at
    markdown section boundaries into ≤MAX_CHARS chunks."""
    r = Path(repo)
    design = sorted((r / "docs" / "design").glob("*.md")) + sorted((r / "docs" / "plans").glob("*.md"))
    out: list[tuple[str, str, str]] = []
    for f in design:
        date = _git_date(repo, f)
        for j, ch in enumerate(_chunk(f.read_text())):
            out.append((f"{f.relative_to(r)}#{j}", date, ch))
    changelog = r / "CHANGELOG.md"
    if changelog.exists():
        # split at version headers, REVERSE to oldest-first so supersession chains form forward
        parts = re.split(r"(?m)^(?=#{1,2} )", changelog.read_text())
        parts = [p for p in parts if p.strip()][::-1]
        for j, ch in enumerate(_pack(parts)):
            out.append((f"CHANGELOG.md#{j}", _git_date(repo, changelog), ch))
    return out


def _chunk(text: str) -> list[str]:
    return _pack([p for p in re.split(r"(?m)^(?=#{1,2} )", text) if p.strip()]) or [text]


def _hardsplit(p: str) -> list[str]:
    """Guarantee ≤MAX_CHARS: cascade ### sub-headers → blank-line paragraphs → hard char windows."""
    out: list[str] = []
    for s in re.split(r"(?m)^(?=#{3,} )", p):
        if len(s) <= MAX_CHARS:
            out.append(s)
            continue
        cur = ""
        for para in s.split("\n\n"):
            seg = para + "\n\n"
            while len(seg) > MAX_CHARS:  # a single monster paragraph: hard-window it
                out.append(seg[:MAX_CHARS])
                seg = seg[MAX_CHARS:]
            if cur and len(cur) + len(seg) > MAX_CHARS:
                out.append(cur)
                cur = seg
            else:
                cur += seg
        if cur.strip():
            out.append(cur)
    return out


def _pack(parts: list[str]) -> list[str]:
    """Accumulate section parts into ≤MAX_CHARS chunks; oversized sections are hard-split first."""
    norm = [q for p in parts for q in ([p] if len(p) <= MAX_CHARS else _hardsplit(p))]
    chunks, cur = [], ""
    for p in norm:
        if cur and len(cur) + len(p) > MAX_CHARS:
            chunks.append(cur)
            cur = p
        else:
            cur += p
    if cur.strip():
        chunks.append(cur)
    return chunks


def _git_date(repo: str, f: Path) -> str:
    try:
        d = subprocess.run(["git", "-C", repo, "log", "-1", "--format=%cI", "--", str(f)],
                           capture_output=True, text=True, timeout=20).stdout.strip()
        return d or "2026-01-01T00:00:00Z"
    except Exception:
        return "2026-01-01T00:00:00Z"


def doc_prompt(doc_id: str, content: str, repo_name: str) -> str:
    return f"""You are the doc-bootstrap capture agent for `{doc_id}` of `{repo_name}`.

Read {SKILL} and follow it EXACTLY for THIS document slice. A `moosedev` MCP server is attached to the
shared in-progress store holding records from EARLIER slices — recall it FIRST (get_relevant_context)
so you align to existing concepts and don't duplicate.

Capture the DURABLE, TYPED knowledge this slice records — one fact per record:
- ArchitecturalDecision (a design choice that was made / shipped), Requirement (a goal/need it serves),
  Constraint (a rule/guard: "silent beats wrong", "build against ≥2 real repos"), Lesson (a "lesson
  already paid for"), Consequence (a trade-off/result), AntiPattern, SystemComponent (a crate/module).
- Link edges where the text states them: isMotivatedBy (decision→requirement/constraint), constrains,
  hasRationale, concerns (→component), and SUPERSEDES when this slice shelves/replaces/deprecates a
  decision you find in recall (supersede that existing IRI — do NOT invent a new one).
Status cues map to lifecycle: shipped/done→accepted, shelved/"deliberately not built"/removed→deprecated
or superseded, "hole identified"/backlog→proposed.

The record timestamp + author are set automatically by the driver — record normally, do NOT pass them.
If this slice is not decision-bearing (pure prose/reference), record nothing and say so. End with the
skill's report (what you wrote: kind/title/IRI per record)."""


def run_capture(doc_id: str, content: str, repo_name: str, data_dir: str, model: str) -> str:
    final_file = Path(data_dir) / "doc-bootstrap-final.txt"
    cmd = ["codex", "exec", "-m", model, "--dangerously-bypass-approvals-and-sandbox",
           "--skip-git-repo-check", "--json", "-o", str(final_file)]
    cmd += _codex_moosedev_overrides(data_dir)
    cmd += [doc_prompt(doc_id, content, repo_name) + f"\n\nDOCUMENT SLICE `{doc_id}`:\n---\n{content}"]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=900, cwd=str(REPO_ROOT))
    except subprocess.TimeoutExpired:
        return "[capture agent TIMED OUT after 900s]"
    tail = final_file.read_text()[-500:] if final_file.exists() else (r.stdout or "")[-500:]
    return tail + (f"\n[stderr] {r.stderr[-200:]}" if r.returncode != 0 else "")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--limit", type=int, default=None, help="only the first N slices")
    ap.add_argument("--resume", action="store_true", help="skip slices in docs-applied.log")
    ap.add_argument("--dry-run", action="store_true", help="list slices only, no capture")
    ap.add_argument("--model", default="gpt-5.4")
    a = ap.parse_args()

    repo_name = Path(a.repo).name
    slices = enumerate_docs(a.repo)
    if a.limit:
        slices = slices[:a.limit]
    print(f"repo={repo_name}  slices={len(slices)}  store={a.data_dir}", flush=True)
    if a.dry_run:
        for i, (doc_id, date, content) in enumerate(slices, 1):
            print(f"[{i}/{len(slices)}] {doc_id:<48} {len(content):>6} chars  ({date[:10]})", flush=True)
        return

    applied_log = Path(a.data_dir) / "docs-applied.log"
    Path(a.data_dir).mkdir(parents=True, exist_ok=True)
    done = set(applied_log.read_text().split("\n")) if (a.resume and applied_log.exists()) else set()
    serve = start_serve(a.data_dir)
    applied = 0
    try:
        for i, (doc_id, date, content) in enumerate(slices, 1):
            if doc_id in done:
                continue
            Path(CAPTURE_TS_FILE).write_text(date)
            Path(CAPTURE_AUTHOR_FILE).write_text("codegraph docs <bootstrap>")
            before = count_records(a.data_dir)
            run_capture(doc_id, content, repo_name, a.data_dir, a.model)
            new = count_records(a.data_dir) - before
            applied += 1
            applied_log.open("a").write(doc_id + "\n")
            print(f"[{i}/{len(slices)}] DONE {doc_id:<46} +{new} records", flush=True)
        snapshot(a.data_dir, "doc-final")
        print(f"validate: {mcp(a.data_dir, 'validate_against_architecture', {}).splitlines()[0]}", flush=True)
        print(f"total records: {count_records(a.data_dir)}", flush=True)
    finally:
        serve.terminate()
        try:
            serve.wait(timeout=10)
        except subprocess.TimeoutExpired:
            serve.kill()


if __name__ == "__main__":
    main()
