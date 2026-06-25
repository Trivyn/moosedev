"""Benchmark configuration — paths, arms, models, endpoints.

Defaults mirror the repo-root .env (LM Studio at localhost:1234); override via env vars.
The harness code lives in this open `bench/` dir; large/private artifacts (corpus chunks,
run outputs, throwaway working dirs) live under BENCH_HOME, outside the repo.
"""
import os
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent          # the moosedev repo
BENCH = Path(__file__).resolve().parent                 # bench/
BENCH_HOME = Path(os.environ.get("BENCH_HOME", Path.home() / "code" / "moosedev_benches"))


def _load_dotenv(path: Path, keys=("OPENROUTER_API_KEY",)) -> None:
    """Pull ONLY the named secret keys from the repo .env into os.environ (without overriding existing).
    The bench computes every other setting from its own defaults below; loading the moosedev binary's
    WHOLE .env clobbers path vars — notably MOOSEDEV_BIN=$ROOT/... (unexpanded $ROOT) — which silently
    breaks the MCP client (the server never launches, so the agent has no get_relevant_context tool and
    falls back to grepping). So this is a narrow secret-loader, NOT a general .env importer."""
    try:
        for line in path.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                if k.strip() in keys:
                    os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))
    except FileNotFoundError:
        pass


_load_dotenv(REPO / ".env")

# Binaries / endpoints (same OpenAI-compatible LM Studio surface for both LLM roles).
_bin_env = os.environ.get("MOOSEDEV_BIN")  # defensive: a bad/unexpanded env path must not break the MCP
MOOSEDEV_BIN = _bin_env if (_bin_env and os.path.exists(_bin_env)) else str(REPO / "target" / "release" / "moosedev")
ONTOLOGY_DIR = os.environ.get("MOOSEDEV_ONTOLOGY_DIR", str(REPO / "ontologies"))  # shared shapes
LLM_BASE_URL = os.environ.get("MOOSEDEV_LLM_BASE_URL", "http://localhost:1234/v1")
LLM_API_KEY = os.environ.get("MOOSEDEV_LLM_API_KEY", "lmstudio")
NLQ_MODEL = os.environ.get("MOOSEDEV_LLM_MODEL", "google/gemma-4-26b-a4b-qat")   # MOOSEDev internal NLQ
AGENT_MODEL = os.environ.get("AGENT_MODEL", "lmstudio/qwen3.6-35b-a3b-mlx")   # opencode provider/model

VENV_PY = BENCH / ".venv" / "bin" / "python"

# ── B1-mem0 competitor arm (mem0ai) ──────────────────────────────────────────
# A real vector-memory tool that CAPTURES the raw docs ITS OWN way (LLM extraction), never the
# flattened B2 graph (Lesson 440abc78 — a competitor ingests raw source itself). Extraction LLM =
# gpt-5.4-mini via OpenRouter to MATCH B2's doc-bootstrap capture model; embedder = a local
# sentence-transformers model (OpenRouter serves no embeddings); vector store = local on-disk qdrant.
# Only the build (mem0_build.py) needs OPENROUTER_API_KEY; the codex search-time MCP does not.
MEM0_LLM_MODEL = os.environ.get("MEM0_LLM_MODEL", "openai/gpt-5.4-mini")        # OpenRouter slug
MEM0_EMBED_MODEL = os.environ.get("MEM0_EMBED_MODEL", "BAAI/bge-base-en-v1.5")  # local HF embedder
MEM0_EMBED_DIMS = int(os.environ.get("MEM0_EMBED_DIMS", "768"))                 # must match MEM0_EMBED_MODEL


def mem0_store_path(corpus: str) -> Path:
    """Local on-disk mem0 (qdrant) store for the B1-mem0 arm — gitignored, never the open repo."""
    p = Path.home() / ".moosedev-stores" / f"{corpus}-mem0"
    p.mkdir(parents=True, exist_ok=True)
    return p


ARMS = ["B0", "B1-md", "B1-rag", "B1-notes", "B1-mem0", "B2"]  # B1-notes = REAL docs; B1-mem0 = mem0 competitor
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
    "moosedev-temporal": {
        # The temporally-bootstrapped moosedev graph (real supersession chains + a real timeline).
        # Currency A/B corpus: B2 reads this graph (get_relevant_context serves CURRENT only); B1 is
        # a currency-blind free-text export (all records incl. superseded, no status line) — the
        # faithful append-only baseline. Q&A-only (materialize_tree:false on its tasks).
        "data_dir": str(Path.home() / ".moosedev-stores" / "moosedev-temporal"),
        "repo": str(REPO),
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
    "codegraph": {
        # Public TS code-knowledge-graph tool (colbymchenry/codegraph). DOC-bootstrapped
        # (doc_bootstrap.py) from docs/design + docs/plans + CHANGELOG into a typed graph — the NEUTRAL,
        # not-our-doc-style capability corpus (Lesson 2b1513e6: decision-bearing docs, not git churn).
        # Richer edge set than trivyn (resultsIn/violates/learnedFrom/concerns all instantiated). PUBLIC
        # → tasks/export/runs use the open bench dirs.
        "data_dir": str(Path.home() / ".moosedev-stores" / "codegraph"),
        "repo": str(Path.home() / "code" / "codegraph"),
        # B1-notes (ecological free-text): grep the SAME raw docs doc_bootstrap consumed to build B2
        # (design + plans + CHANGELOG) — the honest "team just kept design docs" baseline, and the
        # identical source mem0 ingests (Lesson 440abc78: a competitor captures raw source its own way,
        # never the flattened B2 graph). Same source as B2 → only capture+representation differ.
        "notes_paths": ["docs/design/*.md", "docs/plans/*.md", "CHANGELOG.md"],
    },
    "trivyn-trial": {
        # LIVE in-anger trial store (AD 07415633): the caught-up trivyn-temporal graph, kept current by
        # real-work capture. repo = the LIVE trivyn checkout (NO pinned sha) so B0 materializes CURRENT
        # HEAD and the cold-arm tree EVOLVES month over month (the longitudinal signal). Rationale-bearing
        # docs excluded so B0 cannot recover the 'why' from the tree (comprehension-debt premise).
        "data_dir": str(Path.home() / ".moosedev-stores" / "trivyn-trial"),
        "repo": str(Path.home() / "code" / "trivyn"),
        "private": True,
        "agent_exclude": ["docs-dev", "spec", "doc", "doc-dist", "tasks", "CONVENTIONS.md",
                          "README.md", "CLAUDE.md", "AGENTS.md", "agents.md", ".grok", ".claude"],
    },
    "moose-trial": {
        # LIVE in-anger trial store (AD 07415633): moose bootstrapped from scratch, kept current by
        # real-work capture. repo = the LIVE moose checkout (NO pinned sha) -> B0 sees current HEAD.
        # agent_exclude refined at probe-authoring time (strip moose's rationale docs so B0 can't cheat).
        "data_dir": str(Path.home() / ".moosedev-stores" / "moose-trial"),
        "repo": str(Path.home() / "code" / "moose"),
        "private": True,
        "agent_exclude": ["README.md", "CLAUDE.md", "AGENTS.md", "agents.md", "CONVENTIONS.md",
                          "docs", "spec", "notes"],
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
# Throwaway per-run working dirs. Override with BENCH_WORK_ROOT to ISOLATE a run's workdirs from the
# shared default — a materialize_tree:false (pure-memory) agent runs `find ..` and will read any
# sibling workdir's source tree left by a CONCURRENT run, confounding the memory test + ballooning
# tokens. An isolated /tmp root keeps `..` barren.
WORK_ROOT = Path(os.environ.get("BENCH_WORK_ROOT", str(BENCH_HOME / "_work")))

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
