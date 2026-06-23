"""Scale-degradation probe — does target-retrieval degrade as corpus size N grows?

Modes:
- Stage 0 ($0, no stores): `--stage0` — subset the full export in-memory, measure B1-rag (BM25) target
  rank vs N + the early-kill slope. The NECESSARY-condition test.
- Stage 1 per-N (the wrapper drives these against scale_build's stores/exports):
  `--arm b1rag --export <per-N export.json>`            BM25 over that N's parity chunks
  `--arm b2 --store <N-store> --target-map <map.json>`  get_relevant_context on the running serve
  Both append a row per target to `--out`. The serve's MOOSEDEV_DENSE_FLOOR (0.99 BM25F / 0.50 hybrid)
  + MOOSEDEV_EXPAND_HOPS=0 are set by the wrapper at serve startup; this probe just connects + queries.
"""
import argparse
import asyncio
import json
import random
import re
from pathlib import Path

import numpy as np
from rank_bm25 import BM25Okapi

import config
from mcp_client import call_tool

TOK = lambda s: re.findall(r"[a-z0-9]+", s.lower())  # identical to B1-rag / BM25 / scale_targets
LIMIT = 10  # mirror B2 get_relevant_context top-k so hit@k / MRR are symmetric across arms
SCALE = config.BENCH / "scale"
KG_IRI = re.compile(r"https?://[^\s]+/kg/[A-Za-z]+/[0-9a-fA-F-]{36}$")  # a ranked record IRI line


def load_targets(corpus: str) -> list[dict]:
    return json.loads((SCALE / f"targets_{corpus}.json").read_text())


def connect_env(store: str) -> dict:
    return {"MOOSEDEV_DATA_DIR": store, "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
            "MOOSEDEV_NO_AUTOSPAWN": "1", "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
            "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY, "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL}


def parse_ranked_iris(text: str) -> list[str]:
    """Ranked record IRIs from get_relevant_context output (each record prints its IRI on a bare line)."""
    return [m.group(0) for ln in text.splitlines() if (m := KG_IRI.match(ln.strip()))]


def _bm25_ranks(chunks: list[dict], targets: list[dict]) -> list[tuple[dict, int | None, int]]:
    bm = BM25Okapi([TOK(c["text"]) for c in chunks])
    t2i = {c["title"]: i for i, c in enumerate(chunks)}
    res = []
    for t in targets:
        if t["title"] not in t2i:
            res.append((t, None, 0)); continue
        scores = bm.get_scores(TOK(t["query"]))
        tidx = t2i[t["title"]]
        order = sorted(range(len(chunks)), key=lambda i: (-scores[i], i))
        rank = order.index(tidx) + 1 if scores[tidx] > 0 else None
        res.append((t, rank, int((scores > 0).sum())))
    return res


def append_rows(out: Path, rows: list[dict]) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "a") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")


# ---- Stage 1 per-N probes --------------------------------------------------------------------

def probe_b1rag_export(corpus, export, n, seed, out):
    chunks = json.loads(Path(export).read_text())
    targets = load_targets(corpus)
    rows = [{"corpus": corpus, "N": n, "seed": seed, "arm": "b1rag", "floor": None,
             "query": t["query"], "target": t["title"], "rank": rank, "n_returned": nret,
             "source": t["source"]}
            for t, rank, nret in _bm25_ranks(chunks, targets)]
    append_rows(out, rows)
    print(f"  b1rag  N={n} s={seed}: hit@5={_hit([r['rank'] for r in rows],5):.2f} "
          f"MRR={_mrr([r['rank'] for r in rows]):.3f}")


def probe_b2_serve(corpus, store, floor, target_map, n, seed, out):
    tmap = json.loads(Path(target_map).read_text())  # title -> per-store iri
    targets = load_targets(corpus)
    env = connect_env(store)
    rows = []
    for t in targets:
        iri = tmap.get(t["title"])
        txt = asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"], env,
                                    "get_relevant_context", {"topic": t["query"], "limit": LIMIT}))
        ranked = parse_ranked_iris(txt)
        rank = ranked.index(iri) + 1 if (iri and iri in ranked) else None
        rows.append({"corpus": corpus, "N": n, "seed": seed, "arm": "b2", "floor": float(floor),
                     "query": t["query"], "target": t["title"], "rank": rank,
                     "n_returned": len(ranked), "source": t["source"]})
    append_rows(out, rows)
    print(f"  b2@{floor} N={n} s={seed}: hit@5={_hit([r['rank'] for r in rows],5):.2f} "
          f"MRR={_mrr([r['rank'] for r in rows]):.3f}")


# ---- Stage 0 ($0 necessary-condition test) ----------------------------------------------------

def sweep_b1rag_full(corpus, ns, seeds):
    records = json.loads(config.corpus_chunks_path(corpus).read_text())
    by_title = {r["title"]: r for r in records}
    targets = [t for t in load_targets(corpus) if t["title"] in by_title]
    tset = [by_title[t["title"]] for t in targets]
    ttitles = {r["title"] for r in tset}
    nontargets = [r for r in records if r["title"] not in ttitles]
    rows = []
    for seed in seeds:
        pool = list(nontargets)
        random.Random(seed).shuffle(pool)  # nested ladder, SAME as scale_build
        for n in ns:
            subset = tset + pool[: max(0, n - len(tset))]
            for t, rank, nret in _bm25_ranks(subset, targets):
                rows.append({"corpus": corpus, "N": len(subset), "seed": seed, "arm": "b1rag",
                             "floor": None, "query": t["query"], "target": t["title"],
                             "rank": rank, "n_returned": nret, "source": t["source"]})
    return rows


# ---- metrics --------------------------------------------------------------------------------

def _capped(rank):
    return rank if (rank is not None and rank <= LIMIT) else None


def _mrr(ranks):
    return float(np.mean([1.0 / r if (r := _capped(x)) else 0.0 for x in ranks]))


def _hit(ranks, k):
    return float(np.mean([1 if (c := _capped(x)) and c <= k else 0 for x in ranks]))


def _slope_logN(ns, ys):
    return float(np.polyfit(np.log10(np.asarray(ns, float)), np.asarray(ys, float), 1)[0])


def report_stage0(rows):
    ns = sorted({r["N"] for r in rows})
    print("\n=== Stage 0: B1-rag (BM25) target-retrieval vs N — does free-text precision DECAY? ===")
    print(f"corpus={rows[0]['corpus']}  targets={len({r['target'] for r in rows})}  "
          f"seeds={sorted({r['seed'] for r in rows})}  (top-{LIMIT})")
    print(f"{'N':>6}{'hit@1':>8}{'hit@5':>8}{'hit@10':>8}{'MRR':>8}{'medRank*':>10}")
    mrr_by_n = []
    for n in ns:
        rk = [r["rank"] for r in rows if r["N"] == n]
        capped = [(_capped(x) or LIMIT + 1) for x in rk]
        mrr = _mrr(rk); mrr_by_n.append(mrr)
        print(f"{n:>6}{_hit(rk,1):>8.2f}{_hit(rk,5):>8.2f}{_hit(rk,10):>8.2f}{mrr:>8.3f}"
              f"{float(np.median(capped)):>10.1f}")
    targets = sorted({r["target"] for r in rows})
    rr = {(t, n): float(np.mean([1.0 / c if (c := _capped(r["rank"])) else 0.0
                                 for r in rows if r["target"] == t and r["N"] == n]))
          for t in targets for n in ns}
    rng = np.random.default_rng(0)
    betas = [_slope_logN(ns, [float(np.mean([rr[(targets[i], n)] for i in samp])) for n in ns])
             for samp in (rng.choice(len(targets), len(targets), replace=True) for _ in range(2000))]
    b, lo, hi = _slope_logN(ns, mrr_by_n), float(np.percentile(betas, 2.5)), float(np.percentile(betas, 97.5))
    print(f"\nMRR slope vs log10(N): β = {b:+.3f}  [95% CI {lo:+.3f}, {hi:+.3f}]")
    if hi < 0:
        print("→ B1-rag DECAYS with N (premise holds). PROCEED to Stage 1 (does B2 resist?).")
    elif lo >= 0:
        print("→ EARLY KILL: B1-rag does NOT decay; thesis A4's premise is dead — route to other instruments.")
    else:
        print("→ AMBIGUOUS: slope CI spans 0.")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="rust-rfcs")
    ap.add_argument("--arm", choices=["b1rag", "b2"])
    ap.add_argument("--stage0", action="store_true")
    ap.add_argument("--export"); ap.add_argument("--store"); ap.add_argument("--target-map")
    ap.add_argument("--floor"); ap.add_argument("--n", type=int); ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--ns", default="50,100,200,400,634")
    ap.add_argument("--out", default=None)
    a = ap.parse_args()

    if a.stage0:
        rows = sweep_b1rag_full(a.corpus, [int(x) for x in a.ns.split(",")], [a.seed])
        append_rows(SCALE / f"probe_{a.corpus}_stage0.jsonl", rows)
        report_stage0(rows)
        return
    out = Path(a.out) if a.out else SCALE / f"probe_{a.corpus}.jsonl"
    if a.arm == "b1rag":
        probe_b1rag_export(a.corpus, a.export, a.n, a.seed, out)
    elif a.arm == "b2":
        probe_b2_serve(a.corpus, a.store, a.floor, a.target_map, a.n, a.seed, out)
    else:
        raise SystemExit("specify --stage0, or --arm b1rag/--arm b2")


if __name__ == "__main__":
    main()
