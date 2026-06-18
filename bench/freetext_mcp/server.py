"""B1 free-text recall MCP server.

Exposes a single `recall(query)` tool over the exported-graph-as-text chunks (BM25). This is the
free-text-RAG arm: the SAME knowledge as the B2 graph (content parity), accessed by lexical
retrieval instead of symbolic query. (Skeleton uses BM25; upgrade to an embedding retriever for the
full matrix — see the benchmark plan, "B1 must be a competent RAG".)
"""
import json
import os
import re

from mcp.server.fastmcp import FastMCP
from rank_bm25 import BM25Okapi

CORPUS = os.environ["FREETEXT_CORPUS"]
TOPK = int(os.environ.get("FREETEXT_TOPK", "5"))

_records = json.loads(open(CORPUS).read())


def _tok(s: str) -> list[str]:
    return re.findall(r"[a-z0-9]+", s.lower())


_bm25 = BM25Okapi([_tok(r["text"]) for r in _records])

mcp = FastMCP("freetext-recall")


@mcp.tool()
def recall(query: str) -> str:
    """Search recorded project knowledge (architectural decisions, lessons, constraints,
    requirements, patterns) and return the most relevant recorded entries."""
    scores = _bm25.get_scores(_tok(query))
    ranked = sorted(range(len(_records)), key=lambda i: scores[i], reverse=True)[:TOPK]
    hits = [_records[i]["text"] for i in ranked if scores[i] > 0]
    if not hits:
        return "No recorded project knowledge matched the query."
    return f"Top {len(hits)} recorded entries:\n\n" + "\n\n".join(hits)


if __name__ == "__main__":
    mcp.run()
