"""Stage 0 ($0, offline): the capability completeness+cost gap, before any agent spend.

B2 ceiling = each question's ground-truth SPARQL set (exact, 1 query, ~|set| rows). B1 ceiling = BM25
over the content-parity text export: set-recall@k = |expected ∩ top-k| / |expected| for k∈{5,10,25,all}
+ the chunk-tokens B1 must ingest to reach each k. Shows, for $0: B1 top-k can't cover large/negation
sets, and reaching completeness means ingesting the whole corpus; B2 returns the exact set in one query.
"""
import argparse
import json
from collections import defaultdict

import numpy as np
from rank_bm25 import BM25Okapi

import config
from scale_probe import TOK


def load_tasks(corpus):
    d = config.corpus_tasks_path(corpus)
    return [t for f in sorted(d.glob("*.json")) for t in [json.loads(f.read_text())]
            if t.get("type") == "capability_qa"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="trivyn-temporal")
    corpus = ap.parse_args().corpus
    chunks = json.loads(config.corpus_chunks_path(corpus).read_text())
    by_iri_len = {c["iri"]: len(c["text"]) for c in chunks}
    bm = BM25Okapi([TOK(c["text"]) for c in chunks])
    iris = [c["iri"] for c in chunks]
    KS = [5, 10, 25, len(chunks)]
    corpus_kb = sum(len(c["text"]) for c in chunks) // 1024
    tasks = load_tasks(corpus)
    print(f"=== Capability Stage-0 ($0): B1 BM25 set-recall vs B2 exact-SPARQL, {corpus} ===")
    print(f"{len(tasks)} questions over {len(chunks)} chunks ({corpus_kb}KB). B2 = 1.00 recall, 1 query, ~|set| rows.\n")
    print(f"{'id':<31}{'class':<15}{'|set|':>5}   B1 recall@5  @10   @25   @all   tok@5")
    rows = []
    for t in tasks:
        exp = {e["iri"] for e in t["ground_truth"]["expected_set"]}
        if not exp:
            continue
        scores = bm.get_scores(TOK(t["prompt"]))
        order = sorted(range(len(chunks)), key=lambda i: (-scores[i], i))
        rec, tok5 = [], 0
        for k in KS:
            topk = {iris[i] for i in order[:k]}
            rec.append(len(exp & topk) / len(exp))
        tok5 = sum(by_iri_len[iris[i]] for i in order[:5]) // 4  # ~chars/4 ≈ tokens
        print(f"{t['id']:<31}{t['capability_class']:<15}{len(exp):>5}      {rec[0]:.2f}  {rec[1]:.2f}  "
              f"{rec[2]:.2f}  {rec[3]:.2f}   {tok5:>5}")
        rows.append((t["capability_class"], len(exp), rec))

    print()
    byc = defaultdict(list)
    for klass, sz, rec in rows:
        byc[klass].append(rec[0])
    for klass in sorted(byc):
        print(f"  {klass:<18} mean B1 recall@5 = {np.mean(byc[klass]):.2f}   (B2 = 1.00, exact, 1 query)")
    full = sum(len(c["text"]) for c in chunks) // 4
    print(f"\nB1 reaches recall 1.0 only at k=all — ingesting ~{full:,} tokens (the whole {corpus_kb}KB corpus);")
    print(f"B2 returns each exact set in ONE query (~hundreds of tokens). The completeness+cost gap is the capability.")


if __name__ == "__main__":
    main()
