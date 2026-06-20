"""Benchmark configuration — paths, arms, models, endpoints.

Defaults mirror the repo-root .env (LM Studio at endor:1234); override via env vars.
The harness code lives in this open `bench/` dir; large/private artifacts (corpus chunks,
run outputs, throwaway working dirs) live under BENCH_HOME, outside the repo.
"""
import os
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent          # the moosedev repo
BENCH = Path(__file__).resolve().parent                 # bench/
BENCH_HOME = Path(os.environ.get("BENCH_HOME", Path.home() / "code" / "moosedev_benches"))

# Binaries / endpoints (same OpenAI-compatible LM Studio surface for both LLM roles).
MOOSEDEV_BIN = os.environ.get("MOOSEDEV_BIN", str(REPO / "target" / "release" / "moosedev"))
ONTOLOGY_DIR = os.environ.get("MOOSEDEV_ONTOLOGY_DIR", str(REPO / "ontologies"))  # shared shapes
LLM_BASE_URL = os.environ.get("MOOSEDEV_LLM_BASE_URL", "http://endor:1234/v1")
LLM_API_KEY = os.environ.get("MOOSEDEV_LLM_API_KEY", "lmstudio")
NLQ_MODEL = os.environ.get("MOOSEDEV_LLM_MODEL", "gemma-4-26b-a4b-it-mlx")   # MOOSEDev internal NLQ
AGENT_MODEL = os.environ.get("AGENT_MODEL", "lmstudio/qwen3.6-35b-a3b-mlx")   # opencode provider/model

VENV_PY = BENCH / ".venv" / "bin" / "python"

ARMS = ["B0", "B1-md", "B1-rag", "B1-notes", "B2"]  # B1-notes = the team's REAL accumulated docs (ecological)
CELL_TIMEOUT = int(os.environ.get("BENCH_CELL_TIMEOUT", "600"))  # per-cell opencode wall-clock cap (s)
KEEP_WORK = bool(os.environ.get("BENCH_KEEP_WORK"))  # keep throwaway per-run workdirs (debug); default: delete

# Per-corpus config. Each corpus has its own isolated store (data_dir) + pinned clone (repo).
# Skeleton = MOOSEDev-on-itself (the live ~30-record graph, null/plumbing case).
# Thesis corpora live under BENCH_HOME (pinned fresh clones); rust-rfcs is public, trivyn private.
CORPORA = {
    "moosedev": {
        "data_dir": str(REPO / ".moosedev"),
        "repo": str(REPO),
    },
    "rust-rfcs": {
        "data_dir": str(BENCH_HOME / "rust-rfcs" / ".moosedev"),
        "repo": str(BENCH_HOME / "rust-rfcs"),
        "sha": "7160a96b584ddd8b80128d90f0cf41b0eaa26a3e",
    },
    "trivyn": {
        "data_dir": str(BENCH_HOME / "trivyn" / ".moosedev"),
        "repo": str(BENCH_HOME / "trivyn"),
        "sha": "1e6e44dff78038d7e3cca6c6439e8f8cacda0d63",
        "private": True,  # clone, graph, B1 export, tasks, runs stay in BENCH_HOME — never the open repo
        # Comprehension-debt premise: these are HUMAN-facing docs (a handbook Opus wrote from the code),
        # never meant as agent context. Stripped from the agent's working tree so captured memory is the
        # ONLY source of rationale — otherwise trivyn's own handbook makes every arm converge. They remain
        # in the full clone and serve as grading ground truth.
        "agent_exclude": ["docs-dev", "spec", "doc", "doc-dist", "tasks", "CONVENTIONS.md",
                          "README.md", "CLAUDE.md", "agents.md", ".grok", ".claude"],
    },
    "burrow": {
        "data_dir": str(Path.home() / ".moosedev-stores" / "burrow"),
        "repo": str(Path.home() / "code" / "burrow"),
        "sha": "d641966",
        # Public repo (rhinoman/burrow), bootstrapped into its own store via the linked-graph skill.
        # Strip the human-facing WHY docs so captured memory is the only rationale source
        # (comprehension-debt premise). The Go code stays.
        "agent_exclude": ["README.md", "MANIFESTO.md", "CLAUDE.md", "spec", "LICENSE"],
    },
    "moosedev-temporal": {
        # The temporally-bootstrapped moosedev graph (real supersession chains + a real timeline).
        # Currency A/B corpus: B2 reads this graph (get_relevant_context serves CURRENT only); B1 is
        # a currency-blind free-text export (all records incl. superseded, no status line) — the
        # faithful append-only baseline. Q&A-only (materialize_tree:false on its tasks).
        "data_dir": str(Path.home() / ".moosedev-stores" / "moosedev-temporal"),
        "repo": str(REPO),
    },
    "burrow-temporal": {
        # The temporally-bootstrapped burrow graph (gpt-5.4-mini/codex capture; real 2026-02 timeline
        # + 4 supersede chains). Second-corpus currency A/B, same shape as moosedev-temporal: B2 serves
        # CURRENT only; B1 is the currency-blind free-text export (--no-status). Q&A-only.
        "data_dir": str(Path.home() / ".moosedev-stores" / "burrow-temporal"),
        "repo": str(Path.home() / "code" / "burrow"),
    },
    "trivyn-temporal": {
        # The temporally-bootstrapped trivyn graph (gpt-5.4-mini/codex; real 2025-07..2026-06 timeline,
        # 416 records, 42 supersede chains incl. ~20 rank-inverted). At-scale currency A/B. PRIVATE:
        # graph/export/tasks/runs stay under BENCH_HOME, never the open repo (records ref trivyn code).
        "data_dir": str(Path.home() / ".moosedev-stores" / "trivyn-temporal"),
        "repo": str(BENCH_HOME / "trivyn"),
        "private": True,
        # B1-notes ecological free-text memory = the team's REAL accumulated docs (NOT the graph export).
        # tasks/*.md = lessons.md (2263 lines) + topical guides; excludes the benchmark *.json specs.
        "notes_paths": ["tasks/*.md", "CONVENTIONS.md"],
    },
}


def corpus_chunks_path(corpus: str) -> Path:
    """Where a corpus's B1 chunk export lives. Private corpora (trivyn) stay under BENCH_HOME,
    never in the open repo; public corpora use the gitignored bench/corpus/ dir."""
    c = CORPORA[corpus]
    base = (BENCH_HOME / corpus) if c.get("private") else CORPUS_DIR
    base.mkdir(parents=True, exist_ok=True)
    return base / f"{corpus}.json"


def corpus_tasks_path(corpus: str) -> Path:
    """Where a corpus's task specs + ground truth live. Private corpora (trivyn) keep their
    tasks under BENCH_HOME — the prompts/ground-truth reference private code and must never land
    in the open repo; public corpora (moosedev, rust-rfcs) use bench/tasks_public/."""
    c = CORPORA[corpus]
    return (BENCH_HOME / corpus / "tasks") if c.get("private") else (BENCH / "tasks_public" / corpus)


def corpus_runs_path(corpus: str) -> Path:
    """Where a corpus's run artifacts (JSONL rows, .patch diffs, transcripts) land. Private corpora
    (trivyn) keep them under BENCH_HOME — patches and final_text carry private code, so being merely
    gitignored in the open repo isn't enough; they must live outside it. Public corpora use
    bench/runs/ (gitignored)."""
    c = CORPORA[corpus]
    base = (BENCH_HOME / corpus / "runs") if c.get("private") else RUNS_DIR
    base.mkdir(parents=True, exist_ok=True)
    return base

# Output / scratch (kept out of the open repo).
CORPUS_DIR = BENCH / "corpus"          # B1 chunk exports (gitignored)
RUNS_DIR = BENCH / "runs"              # JSONL rows (gitignored)
WORK_ROOT = BENCH_HOME / "_work"        # throwaway per-run working dirs

# Temporal (git-walk) bootstrap. The per-episode CAPTURE is a real headless agent (claude -p)
# following skills/temporal-episode-capture.md — it calls read/align/record/relate/supersede/
# validate itself (the normal workflow), stamping records with the commit's date+author.
TB_CAPTURE_MODEL = os.environ.get("TB_CAPTURE_MODEL", "sonnet")          # claude -p model for the capture agent
TB_MAX_TURNS = int(os.environ.get("TB_MAX_TURNS", "60"))                 # max agent turns per episode
TB_MAX_DIFF_BYTES = int(os.environ.get("TB_MAX_DIFF_BYTES", "24000"))    # per-episode diff cap in the agent prompt
TB_VALIDATE_EVERY = int(os.environ.get("TB_VALIDATE_EVERY", "10"))       # validate_against_architecture every N applied episodes
TB_SNAPSHOT_ROOT = os.environ.get("TB_SNAPSHOT_ROOT", str(BENCH_HOME / "_temporal_snapshots"))
TB_TRIVIAL_LINES = int(os.environ.get("TB_TRIVIAL_LINES", "8"))          # mechanical-subject episodes below this many changed lines are skipped
TB_MIN_LINES = int(os.environ.get("TB_MIN_LINES", "2"))                  # tiny diffs without a why-cue are skipped
