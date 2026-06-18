"""Write the curated trivyn knowledge records into the trivyn corpus graph (strong-model capture).

The records were synthesized by survey subagents reading trivyn's spec/, docs-dev handbook,
CONVENTIONS, and git history (the bootstrap-existing-codebase skill). This writer is mechanical:
it reads records.json (a list of {kind,title,description,...}), dedups near-identical titles
(keeping the longest description), validates the kind against the canonical set, and writes each
via ONE persistent MCP session to the trivyn --serve backend (NO_AUTOSPAWN; backend started
explicitly — Lesson a7dec1f3). trivyn is PRIVATE: records.json + the graph live under BENCH_HOME.

Grading ground truth for trivyn tasks is the trivyn docs/code itself, never these records.
"""
import asyncio
import json
import os
import re
import time
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

import config

CANONICAL = {"ArchitecturalDecision", "Constraint", "Requirement", "Pattern", "AntiPattern", "Lesson"}
RECORDS = config.BENCH_HOME / "trivyn" / "records.json"
DUP_THRESHOLD = 0.65  # title token-set Jaccard above this = near-duplicate


def _norm(title: str) -> set:
    return set(re.findall(r"[a-z0-9]+", title.lower()))


def dedup(records: list[dict]) -> tuple[list[dict], int]:
    kept: list[dict] = []
    dropped = 0
    for r in records:
        toks = _norm(r["title"])
        hit = None
        for k in kept:
            inter = len(toks & k["_toks"])
            union = len(toks | k["_toks"]) or 1
            if inter / union >= DUP_THRESHOLD:
                hit = k
                break
        if hit is None:
            kept.append({**r, "_toks": toks})
        else:
            dropped += 1
            if len(r.get("description", "")) > len(hit.get("description", "")):
                hit.update({**r, "_toks": hit["_toks"]})
    for k in kept:
        k.pop("_toks", None)
    return kept, dropped


def corpus_env(data_dir: str) -> dict:
    return {
        "MOOSEDEV_DATA_DIR": data_dir,
        "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
        "MOOSEDEV_NO_AUTOSPAWN": "1",
        "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
        "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY,
        "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL,
    }


async def main() -> None:
    raw = json.loads(RECORDS.read_text())
    records, dropped = dedup(raw)
    bad = [r["kind"] for r in records if r["kind"] not in CANONICAL]
    if bad:
        raise SystemExit(f"non-canonical kinds present: {sorted(set(bad))}")
    data_dir = config.CORPORA["trivyn"]["data_dir"]
    print(f"{len(raw)} candidates -> {len(records)} after dedup ({dropped} dropped) -> {data_dir}",
          flush=True)
    by_kind: dict[str, int] = {}
    for r in records:
        by_kind[r["kind"]] = by_kind.get(r["kind"], 0) + 1
    print("by kind:", by_kind, flush=True)

    params = StdioServerParameters(command=config.MOOSEDEV_BIN, args=["--connect"],
                                   env={**os.environ, **corpus_env(data_dir)})
    t0 = time.time()
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            for n, r in enumerate(records, 1):
                await session.call_tool("record_important_decision", {
                    "title": r["title"], "kind": r["kind"],
                    "description": r["description"], "status": "accepted",
                })
                if n % 20 == 0 or n == len(records):
                    print(f"  {n}/{len(records)}  ({time.time() - t0:.1f}s)", flush=True)
    print(f"done: {len(records)} records in {time.time() - t0:.1f}s", flush=True)


if __name__ == "__main__":
    asyncio.run(main())
