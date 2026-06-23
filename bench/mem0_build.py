"""Build the B1-mem0 store: mem0 ingests a corpus's RAW docs and extracts memories ITS OWN way.

Competitor-fair (Lesson 440abc78): mem0 receives the SAME raw doc slices B2's doc_bootstrap saw
(reused via doc_bootstrap.enumerate_docs), NOT the flattened B2 graph — capture is the competitor's
own, never handed to it. Extraction LLM = gpt-5.4-mini via OpenRouter (matches B2's capture model);
embedder = a local sentence-transformers model; vector store = local on-disk qdrant.

The build REPORTS how many memories mem0 actually stored (the lossy-capture check) + a spot-check,
so a low downstream recall can be attributed to retrieval architecture, not silent capture loss.

  # add OPENROUTER_API_KEY=... to the gitignored repo .env, then:
  bench/.venv/bin/python bench/mem0_build.py --corpus codegraph [--limit N] [--reset]
"""
import argparse
import os
import shutil
import sys

import config
from doc_bootstrap import enumerate_docs


def build_memory(corpus: str):
    """Instantiate mem0 with the matched-extraction-LLM + local-embedder + on-disk-qdrant config."""
    from mem0 import Memory
    store = config.mem0_store_path(corpus)
    cfg = {
        "llm": {"provider": "openai", "config": {"model": config.MEM0_LLM_MODEL, "temperature": 0.0}},
        "embedder": {"provider": "huggingface", "config": {"model": config.MEM0_EMBED_MODEL}},
        "vector_store": {"provider": "qdrant", "config": {
            "path": str(store / "qdrant"), "on_disk": True,
            "collection_name": corpus, "embedding_model_dims": config.MEM0_EMBED_DIMS}},
    }
    return Memory.from_config(cfg), store


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="codegraph")
    ap.add_argument("--limit", type=int, default=None, help="ingest only the first N doc slices (smoke test)")
    ap.add_argument("--reset", action="store_true", help="delete any existing store before ingesting")
    a = ap.parse_args()

    if not os.environ.get("OPENROUTER_API_KEY"):
        sys.exit("OPENROUTER_API_KEY not set (add it to the repo .env) — needed for mem0's extraction LLM.")

    repo = config.CORPORA[a.corpus]["repo"]
    docs = enumerate_docs(repo)
    if a.limit:
        docs = docs[:a.limit]

    if a.reset:
        shutil.rmtree(config.mem0_store_path(a.corpus), ignore_errors=True)

    mem, store = build_memory(a.corpus)
    print(f"ingesting {len(docs)} doc slices from {repo}", flush=True)
    print(f"  extraction LLM = {config.MEM0_LLM_MODEL} (OpenRouter) | embedder = {config.MEM0_EMBED_MODEL}",
          flush=True)
    total = 0
    for i, (doc_id, _date, content) in enumerate(docs):
        res = mem.add(content, user_id=a.corpus, metadata={"doc_id": doc_id})
        n = len(res.get("results", [])) if isinstance(res, dict) else 0
        total += n
        print(f"  [{i + 1}/{len(docs)}] {doc_id}: +{n} memories", flush=True)

    allm = mem.get_all(filters={"user_id": a.corpus}, top_k=100000)  # default top_k=20 would under-report
    items = allm.get("results", []) if isinstance(allm, dict) else (allm or [])
    print(f"\nSTORED {len(items)} memories total ({total} add-events) in {store}")
    print("spot-check (first 8):")
    for it in items[:8]:
        text = it.get("memory", "") if isinstance(it, dict) else str(it)
        print("  -", text[:160])


if __name__ == "__main__":
    main()
