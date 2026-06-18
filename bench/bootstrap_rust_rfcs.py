"""Deterministic per-RFC bootstrap of the rust-lang/rfcs corpus into a MOOSEDev graph.

One merged RFC (text/NNNN-name.md) -> one ArchitecturalDecision record. The record's rationale is
the RFC's OWN sections (Summary + Motivation + Rationale/Alternatives + Drawbacks) — the why &
why-not — extracted verbatim. This is content-parity source for B1 and B2 alike. Grading ground
truth is the RFC text itself in the working tree, never this record (capture policy: primary-source
ground truth, no circularity).

Writes go through ONE persistent MCP session to a `moosedev --serve` backend already pointed at the
target data dir (start it explicitly; NO_AUTOSPAWN — backend-lifecycle Lesson a7dec1f3). Run the
backend with the matching MOOSEDEV_DATA_DIR before invoking this.

  python bootstrap_rust_rfcs.py --limit 15 --data-dir <smoke dir>   # walking-skeleton smoke
  python bootstrap_rust_rfcs.py                                     # full corpus into config dir
"""
import argparse
import asyncio
import os
import re
import time
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

import config

RFC_DIR = Path(config.CORPORA["rust-rfcs"]["repo"]) / "text"
SECTION_RE = re.compile(r"^##\s+(.+?)\s*$", re.M)
CAP = 1800  # per-section char cap: substantive but bounded so records don't bloat

# Sections kept as the record's rationale, in capture order. Lowercased exact-match against the
# RFC's own ## headings; the why (summary/motivation) and the why-not (alternatives/drawbacks).
KEEP = ["summary", "motivation", "problems", "rationale and alternatives", "alternatives", "drawbacks"]


def parse_rfc(path: Path) -> dict:
    text = path.read_text(errors="replace")
    parts = SECTION_RE.split(text)  # [preamble, name1, body1, name2, body2, ...]
    sections: dict[str, str] = {}
    for i in range(1, len(parts) - 1, 2):
        name = parts[i].strip().lower()
        sections.setdefault(name, parts[i + 1].strip())

    m = re.match(r"(\d+)-(.+)", path.stem)
    num = int(m.group(1)) if m else 0
    feat = (m.group(2) if m else path.stem).replace("-", " ")
    title = f"RFC {num}: {feat}"

    chunks = []
    for key in KEEP:
        body = sections.get(key)
        if body:
            chunks.append(f"## {key.title()}\n{body[:CAP]}")
    desc = f"Rust {title} (rust-lang/rfcs text/{path.name}).\n\n" + "\n\n".join(chunks)
    return {"num": num, "title": title, "description": desc, "file": path.name,
            "sections": sorted(sections)}


def corpus_env(data_dir: str) -> dict:
    return {
        "MOOSEDEV_DATA_DIR": data_dir,
        "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
        "MOOSEDEV_NO_AUTOSPAWN": "1",  # the backend is started explicitly; never auto-spawn
        "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
        "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY,
        "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL,
    }


async def bootstrap(data_dir: str, limit: int | None) -> None:
    files = sorted(RFC_DIR.glob("*.md"))
    if limit:
        files = files[:limit]
    print(f"bootstrapping {len(files)} RFCs -> {data_dir}", flush=True)
    params = StdioServerParameters(command=config.MOOSEDEV_BIN, args=["--connect"],
                                   env={**os.environ, **corpus_env(data_dir)})
    t0 = time.time()
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            for n, f in enumerate(files, 1):
                rec = parse_rfc(f)
                await session.call_tool("record_important_decision", {
                    "title": rec["title"], "kind": "ArchitecturalDecision",
                    "description": rec["description"], "status": "accepted",
                })
                if n % 25 == 0 or n == len(files):
                    print(f"  {n}/{len(files)}  ({(time.time() - t0):.1f}s)  last: {rec['title'][:60]}",
                          flush=True)
    print(f"done: {len(files)} records in {(time.time() - t0):.1f}s", flush=True)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=None, help="bootstrap only the first N RFCs (smoke)")
    ap.add_argument("--data-dir", default=config.CORPORA["rust-rfcs"]["data_dir"])
    args = ap.parse_args()
    asyncio.run(bootstrap(args.data_dir, args.limit))


if __name__ == "__main__":
    main()
