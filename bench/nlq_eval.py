"""NLQ eval harness: run a query set against a MOOSEDev serve and score resolution + answer quality.

Each queries.jsonl row: {id, intent, query, expect_any:[markers], avoid:[stale markers], expect_iri, notes}.
Every query is graded into one of four buckets so we can see WHERE NLQ fails:
  - correct    : walked the graph AND hit an expect marker AND leaked no stale marker
  - wrong      : walked/answered but missed every marker (or leaked a stale one)
  - unresolved : no walk, not clarified — a resolution miss (altLabel/hiddenLabel candidate)
  - clarified  : the single-shot "needs clarification" gate (MOOSE Core; altLabels won't move it)

The clarified-vs-rest split is the point: it says whether altLabels are worth adding (they help
the 'unresolved'/'wrong' buckets) or whether the Core gate dominates.

Usage: bench/nlq_eval.py [--data-dir DIR] [--queries FILE]   (needs a `--serve` already running on DIR)
"""
import argparse
import asyncio
import json
import os
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import config  # noqa: E402
from mcp import ClientSession, StdioServerParameters  # noqa: E402
from mcp.client.stdio import stdio_client  # noqa: E402

BUCKETS = ["correct", "wrong", "unresolved", "clarified"]
SYM = {"correct": "OK ", "wrong": "WRONG", "unresolved": "MISS", "clarified": "CLAR"}


def env(data_dir: str) -> dict:
    return {"MOOSEDEV_DATA_DIR": data_dir, "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
            "MOOSEDEV_NO_AUTOSPAWN": "1", "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
            "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY, "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL}


def grade(q: dict, ans: str) -> dict:
    low = ans.lower()
    clarified = "needs clarification" in low
    m = re.search(r"triples walked:\s*(\d+)", ans)
    walked = int(m.group(1)) if m else 0
    hit = [k for k in q.get("expect_any", []) if k.lower() in low]
    leak = [k for k in q.get("avoid", []) if k.lower() in low]
    iri = q.get("expect_iri") or ""
    iri_cited = bool(iri) and iri in ans
    if clarified:
        bucket = "clarified"
    elif walked > 0 and hit and not leak:
        bucket = "correct"
    elif walked > 0:
        bucket = "wrong"
    else:
        bucket = "unresolved"
    return {"bucket": bucket, "walked": walked, "hit": hit, "leak": leak, "iri_cited": iri_cited}


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data-dir", default="/Users/jcadam/code/moosedev/.moosedev")
    ap.add_argument("--queries", default=str(Path(__file__).resolve().parent / "nlq_eval" / "queries.jsonl"))
    args = ap.parse_args()

    qs = [json.loads(l) for l in Path(args.queries).read_text().splitlines() if l.strip()]
    params = StdioServerParameters(command=config.MOOSEDEV_BIN, args=["--connect"],
                                   env={**os.environ, **env(args.data_dir)})
    rows = []
    async with stdio_client(params) as (r, w):
        async with ClientSession(r, w) as s:
            await s.initialize()
            txt = lambda res: "\n".join(c.text for c in res.content if getattr(c, "type", None) == "text")
            for q in qs:
                try:
                    a = txt(await s.call_tool("query", {"question": q["query"]}))
                except Exception as e:
                    a = f"ERROR: {e}"
                rows.append((q, grade(q, a)))

    n = len(rows)
    print(f"\n=== NLQ eval: {n} queries on {args.data_dir} ===")
    print(f"{'id':<24}{'intent':<9}{'bucket':<6}{'walk':>5}  markers / iri")
    cnt, by_intent = Counter(), {}
    for q, g in rows:
        cnt[g["bucket"]] += 1
        by_intent.setdefault(q["intent"], Counter())[g["bucket"]] += 1
        mk = "+".join(g["hit"]) or "-"
        if g["leak"]:
            mk += f"  STALE:{'+'.join(g['leak'])}"
        if g["iri_cited"]:
            mk += "  [iri✓]"
        print(f"{q['id']:<24}{q['intent']:<9}{SYM[g['bucket']]:<6}{g['walked']:>5}  {mk}")

    print("\n--- buckets ---")
    for b in BUCKETS:
        print(f"  {b:<11}{cnt[b]:>3}/{n}  ({100 * cnt[b] // n if n else 0}%)")
    print("\n--- by intent ---")
    for it, c in sorted(by_intent.items()):
        print(f"  {it:<10} " + "  ".join(f"{b}={c[b]}" for b in BUCKETS if c[b]))
    past = cnt["correct"] + cnt["wrong"] + cnt["unresolved"]
    print(f"\nDIAGNOSTIC: clarified(Core gate)={cnt['clarified']}/{n}  |  past-the-gate={past}/{n} "
          f"(correct={cnt['correct']}, wrong={cnt['wrong']}, unresolved={cnt['unresolved']})")
    print("altLabel/hiddenLabel targets 'unresolved'+'wrong'; 'clarified' is the Core single-shot gate.")


if __name__ == "__main__":
    asyncio.run(main())
