"""Retrieval-slice seed-recall A/B for the hybrid dense channel (Instrument A).

Measures, deterministically and with ZERO agent spend, whether the new hybrid BM25F⊕dense seed in
`get_relevant_context` recovers a CURRENT record that pure BM25F buries when the query uses the
*stale* vocabulary of the record it superseded — the comprehension-debt case the feature targets
(a concept filed under different words as the project ages). Extends Lesson 9e7ebeb6, which mined
the BM25 side of this by hand; here we add the dense channel and compare.

Method (per supersede chain  cur ──supersedes──▶ old,  cur current, old historical):
  - target  = cur  (the current record; get_relevant_context serves current-only, so old never shows)
  - queries = OLD-FRAMING (old's title verbatim — a user who remembers the old framing) and
              NEUTRAL (terms common to both titles — a subject query naming neither framing)
  - call get_relevant_context(topic=query, limit=K) against a live serve whose MOOSEDEV_DENSE_FLOOR
    is set by the orchestrator (0.99 ⇒ dense gated off ⇒ pure BM25F; 0.50 ⇒ hybrid), parse the
    ranked record IRIs, find the rank of `cur`.
Outputs per-floor JSON; `--compare` prints recall@1/3/5/10 + MRR for each floor and lists the chains
HYBRID recovers that BM25F misses (these seed the Instrument-B vocabulary-drift tasks).

Run via run_hybrid_seed_recall.sh (it owns the floor-toggled serve lifecycle).
"""
import argparse
import asyncio
import json
import os
import re
from pathlib import Path

import config
from export_corpus import corpus_env
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

PROJECT_GRAPH = "https://moosedev.dev/kg/project"
RDFS_LABEL = "http://www.w3.org/2000/01/rdf-schema#label"
# A record's own IRI is printed by get_relevant_context as a standalone line; inline cross-refs in
# descriptions use short ids, not full IRIs, so a full-line match yields exactly the ranked records.
KG_IRI = re.compile(r"https?://[^\s]+/kg/[A-Za-z]+/[0-9a-fA-F-]{36}$")
LIMIT = 10
HISTORICAL = {"superseded", "deprecated", "retracted", "historical", "rejected"}
CURRENT = {"accepted", "proposed"}
OUT_DIR = config.BENCH / "seed_recall"

# Supersede chains with both titles + lifecycle status. Predicate matched by local-name (STRENDS)
# so the ontology namespace stays out of the code (decouple-code-from-ontology-ttl).
CHAINS_SPARQL = f"""
SELECT ?cur ?curTitle ?curStatus ?old ?oldTitle ?oldStatus WHERE {{
  GRAPH <{PROJECT_GRAPH}> {{
    ?cur ?p ?old .
    FILTER(STRENDS(STR(?p), "supersedes"))
    ?cur <{RDFS_LABEL}> ?curTitle .
    ?old <{RDFS_LABEL}> ?oldTitle .
    OPTIONAL {{ ?cur ?cs ?curStatus . FILTER(STRENDS(STR(?cs), "hasLifecycleStatus")) }}
    OPTIONAL {{ ?old ?os ?oldStatus . FILTER(STRENDS(STR(?os), "hasLifecycleStatus")) }}
  }}
}}"""

# Fallback discovery if the predicate guess is wrong (no chains found).
PRED_SPARQL = f"""
SELECT DISTINCT ?p WHERE {{ GRAPH <{PROJECT_GRAPH}> {{ ?s ?p ?o }}
  FILTER(CONTAINS(LCASE(STR(?p)), "supersede")) }}"""

STOP = set("""a an the of to in on for and or is are be was were been being with without by as at
from into over under this that these those it its their your our his her not no than then so such
what which who whom whose how when where why current currently state still only just both each any
all more most via using use used uses do does did done has have had we you they i project""".split())


def terms(title: str) -> list[str]:
    toks = re.findall(r"[a-z0-9][a-z0-9\-]+", title.lower())
    return [t for t in toks if t not in STOP and len(t) > 2]


def neutral_query(old_title: str, cur_title: str) -> str:
    """Subject terms common to both framings — names the topic without either side's distinctive words."""
    shared = [t for t in dict.fromkeys(terms(cur_title)) if t in set(terms(old_title))]
    return " ".join(shared)


def parse_ranked_iris(text: str) -> list[str]:
    return [s for line in text.splitlines() if KG_IRI.match(s := line.strip())]


async def _call(session: ClientSession, tool: str, args: dict) -> str:
    res = await session.call_tool(tool, args)
    return "\n".join(c.text for c in res.content if getattr(c, "type", None) == "text")


def _bindings(sparql_json: str) -> list[dict]:
    return json.loads(sparql_json)["results"]["bindings"]


def mine_chains(rows: list[dict]) -> list[dict]:
    """Currency pairs: cur is current, old is historical. Dedup on (cur, old)."""
    chains, seen = [], set()
    for b in rows:
        cur, old = b["cur"]["value"], b["old"]["value"]
        cs = b.get("curStatus", {}).get("value", "").lower()
        os_ = b.get("oldStatus", {}).get("value", "").lower()
        if (cur, old) in seen:
            continue
        seen.add((cur, old))
        if cs and cs not in CURRENT:        # cur must be a live record (else it never shows in output)
            continue
        if os_ and os_ not in HISTORICAL:   # old must be the superseded side
            continue
        chains.append({
            "cur": cur, "curTitle": b["curTitle"]["value"], "curStatus": cs,
            "old": old, "oldTitle": b["oldTitle"]["value"], "oldStatus": os_,
        })
    return chains


async def run_floor(corpus: str, label: str, floor: float) -> None:
    params = StdioServerParameters(
        command=config.MOOSEDEV_BIN, args=["--connect"],
        env={**os.environ, **corpus_env(corpus)},
    )
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            rows = _bindings(await _call(session, "sparql", {"query": CHAINS_SPARQL}))
            chains = mine_chains(rows)
            if not chains:
                preds = _bindings(await _call(session, "sparql", {"query": PRED_SPARQL}))
                print("NO CHAINS. supersede-like predicates seen:",
                      [p["p"]["value"] for p in preds])
                return
            records = []
            for ch in chains:
                variants = {"old": ch["oldTitle"]}
                nq = neutral_query(ch["oldTitle"], ch["curTitle"])
                if nq:
                    variants["neutral"] = nq
                for variant, q in variants.items():
                    text = await _call(session, "get_relevant_context", {"topic": q, "limit": LIMIT})
                    ranked = parse_ranked_iris(text)
                    rank = ranked.index(ch["cur"]) + 1 if ch["cur"] in ranked else None
                    records.append({
                        "cur": ch["cur"], "curTitle": ch["curTitle"], "old": ch["old"],
                        "oldTitle": ch["oldTitle"], "variant": variant, "query": q,
                        "rank": rank, "n_returned": len(ranked),
                    })
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    out = OUT_DIR / f"{corpus}_{label}.json"
    out.write_text(json.dumps({"floor": floor, "label": label, "limit": LIMIT,
                               "n_chains": len(chains), "records": records}, indent=2))
    print(f"[{label} @ floor={floor}] {len(chains)} chains, {len(records)} (chain,variant) probes -> {out}")


def _recall(records: list[dict], variant: str, k: int) -> float:
    rs = [r for r in records if r["variant"] == variant]
    return sum(1 for r in rs if r["rank"] and r["rank"] <= k) / len(rs) if rs else 0.0


def _mrr(records: list[dict], variant: str) -> float:
    rs = [r for r in records if r["variant"] == variant]
    return sum((1.0 / r["rank"]) if r["rank"] else 0.0 for r in rs) / len(rs) if rs else 0.0


def compare(corpus: str, bm: str, hy: str) -> None:
    a = json.loads((OUT_DIR / f"{corpus}_{bm}.json").read_text())
    b = json.loads((OUT_DIR / f"{corpus}_{hy}.json").read_text())
    ar, br = a["records"], b["records"]
    print(f"\n=== Instrument A: seed-recall A/B on {corpus} "
          f"({a['n_chains']} currency chains, limit={a['limit']}) ===")
    print(f"{bm} floor={a['floor']}  vs  {hy} floor={b['floor']}\n")
    for variant in ("old", "neutral"):
        if not [r for r in ar if r["variant"] == variant]:
            continue
        print(f"-- {variant}-framing query --")
        print(f"{'metric':<12}{bm:>10}{hy:>10}{'Δ':>9}")
        for k in (1, 3, 5, 10):
            x, y = _recall(ar, variant, k), _recall(br, variant, k)
            print(f"recall@{k:<6}{x:>10.3f}{y:>10.3f}{y - x:>+9.3f}")
        x, y = _mrr(ar, variant), _mrr(br, variant)
        print(f"{'MRR':<12}{x:>10.3f}{y:>10.3f}{y - x:>+9.3f}\n")

    # Chains HYBRID recovers (cur in hybrid top-LIMIT) that BM25F misses — seeds the drift tasks.
    bm_rank = {(r["cur"], r["variant"]): r["rank"] for r in ar}
    recovered = [r for r in br
                 if r["rank"] and not bm_rank.get((r["cur"], r["variant"]))]
    improved = [r for r in br if r["rank"] and (br0 := bm_rank.get((r["cur"], r["variant"])))
                and r["rank"] < br0]
    print(f"=== HYBRID-recovered (cur surfaced by hybrid, MISSED by BM25F): {len(recovered)} ===")
    for r in sorted(recovered, key=lambda r: (r["variant"], r["rank"])):
        print(f"  [{r['variant']}] hybrid#{r['rank']}  cur={r['cur'].rsplit('/', 1)[-1]}")
        print(f"      curTitle: {r['curTitle'][:96]}")
        print(f"      query   : {r['query'][:96]}")
    print(f"\n=== HYBRID-improved rank (both hit, hybrid ranks cur higher): {len(improved)} ===")
    for r in sorted(improved, key=lambda r: r["variant"]):
        b0 = bm_rank[(r["cur"], r["variant"])]
        print(f"  [{r['variant']}] {b0} -> {r['rank']}  {r['curTitle'][:80]}")
    # Machine-readable recovered set for the drift-task authoring step.
    (OUT_DIR / f"{corpus}_recovered.json").write_text(json.dumps(recovered, indent=2))
    print(f"\nrecovered set -> {OUT_DIR / f'{corpus}_recovered.json'}")


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="trivyn-temporal")
    ap.add_argument("--floor-label")
    ap.add_argument("--floor", type=float)
    ap.add_argument("--compare", nargs=2, metavar=("BM25F_LABEL", "HYBRID_LABEL"))
    a = ap.parse_args()
    if a.compare:
        compare(a.corpus, *a.compare)
    else:
        asyncio.run(run_floor(a.corpus, a.floor_label, a.floor))
