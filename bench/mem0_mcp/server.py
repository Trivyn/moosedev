"""B1-mem0 recall MCP server: recall(query) -> mem0.search over the prebuilt mem0 store.

The competitor arm's retrieval surface (mirrors freetext_mcp/server.py so the agent sees the SAME
recall() contract as B1-rag — only the backing memory system differs). Search needs only the local
embedder + on-disk qdrant: no extraction LLM and no OpenRouter key. mem0 requires an LLM in its
config to construct, so a dummy OPENAI_API_KEY satisfies the client ctor; the LLM is never called
on a search.
"""
import os

from mcp.server.fastmcp import FastMCP
from mem0 import Memory

CORPUS = os.environ.get("MEM0_CORPUS", "codegraph")
STORE = os.environ["MEM0_STORE"]                                    # qdrant path (…/<corpus>-mem0/qdrant)
EMBED_MODEL = os.environ.get("MEM0_EMBED_MODEL", "BAAI/bge-base-en-v1.5")
EMBED_DIMS = int(os.environ.get("MEM0_EMBED_DIMS", "768"))
TOPK = int(os.environ.get("MEM0_TOPK", "10"))

os.environ.setdefault("OPENAI_API_KEY", "sk-noop")  # satisfy mem0's LLM client ctor; never called on a search

_mem = Memory.from_config({
    "llm": {"provider": "openai", "config": {"model": "openai/gpt-5.4-mini"}},
    "embedder": {"provider": "huggingface", "config": {"model": EMBED_MODEL}},
    "vector_store": {"provider": "qdrant", "config": {
        "path": STORE, "on_disk": True, "collection_name": CORPUS, "embedding_model_dims": EMBED_DIMS}},
})

mcp = FastMCP("mem0-recall")


@mcp.tool()
def recall(query: str) -> str:
    """Search recorded project knowledge (architectural decisions, lessons, constraints,
    requirements, patterns) and return the most relevant recorded entries."""
    res = _mem.search(query, filters={"user_id": CORPUS}, top_k=TOPK)
    items = res.get("results", []) if isinstance(res, dict) else (res or [])
    hits = [it.get("memory", "") for it in items if isinstance(it, dict) and it.get("memory")]
    if not hits:
        return "No recorded project knowledge matched the query."
    return f"Top {len(hits)} recorded entries:\n\n" + "\n\n".join(f"- {h}" for h in hits)


if __name__ == "__main__":
    mcp.run()
