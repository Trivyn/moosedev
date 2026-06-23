"""Stage 0b ($0, offline): do FACET-KEYED queries favor structure even on clean rust-rfcs?

Stage 0 used "decision → facet" queries (describe decision X, ask its drawback) — blob-favorable, since
the blob co-locates X's identity AND its drawback. This tests the INVERSE: "facet → decision" — the query
IS a specific drawback, and the task is to find the decision that has it. The search key (drawback) is the
WHOLE of a focused Consequence node but only ~1/4 of the blob (diluted) — and at scale the blob collides
with every RFC mentioning that vocabulary in any section. If the focused Consequence SEED out-ranks the
blob and the gap WIDENS with N, then *some* query types benefit from decomposition even on clean data
(with expansion bringing the answer decision from the Consequence seed in Stage 1).
"""
import json
import random

import httpx

import config
from scale_targets import load_corpus, by_title, section, TOK, leak
from scale_decomp_stage0 import section_chunks, rank_of, hit, NS

SYS = ("You write the search query a developer types to FIND a design decision GIVEN one of its "
       "consequences. Describe the DRAWBACK / limitation / downside itself (the symptom) in general, "
       "shared vocabulary; do NOT name the feature, the decision, or any RFC number. Output ONLY the "
       "query, 1-2 sentences.")


def author_facet(drawbacks: str, base_url: str, model: str) -> str:
    resp = httpx.post(f"{base_url.rstrip('/')}/chat/completions", timeout=120.0, json={
        "model": model, "temperature": 0.0,
        "messages": [{"role": "system", "content": SYS},
                     {"role": "user", "content": f"This is the DRAWBACK of some design decision:\n\n"
                      f"{drawbacks[:1400]}\n\nWrite the search query to find the decision that has this drawback."}],
    })
    resp.raise_for_status()
    return " ".join(resp.json()["choices"][0]["message"]["content"].strip().strip('"').split())


def main() -> None:
    recs = load_corpus()
    idx = by_title(recs)
    targets = json.loads((config.BENCH / "scale" / "targets_rust-rfcs_sections.json").read_text())
    draw = [t for t in targets if t["section"] == "Consequence"]
    base_url, model = config.LLM_BASE_URL, config.NLQ_MODEL

    print(f"authoring {len(draw)} facet-keyed (drawback→decision) queries via {model} …", flush=True)
    fq = []
    for t in draw:
        body = section(idx[t["title"]]["text"], "Drawbacks")
        q = author_facet(body, base_url, model)
        ot, fs, leaky, _ = leak(q, t["title"])
        fq.append({"title": t["title"], "query": q, "leaky": leaky})
        print(f"  {t['title'][:40]:<40} feat={fs:.2f}{' LEAKY' if leaky else ''}  Q: {q[:80]}", flush=True)

    tset = {t["title"] for t in targets}
    pool = [r["title"] for r in recs if r["title"] not in tset]
    random.Random(0).shuffle(pool)
    print(f"\n{'N':>5}  {'blob@5':>7}{'blob@10':>8} | {'consSEED@5':>11}{'consSEED@10':>12} | {'Δ@5':>7}")
    deltas = {}
    for n in NS:
        titles = list(tset) + pool[: max(0, n - len(tset))]
        subset = [idx[t] for t in titles]
        from rank_bm25 import BM25Okapi
        bm_blob = BM25Okapi([TOK(r["text"]) for r in subset])
        blob_pos = {r["title"]: i for i, r in enumerate(subset)}
        dchunks = [c for r in subset for c in section_chunks(r)]
        bm_dec = BM25Okapi([TOK(c["text"]) for c in dchunks])
        cons_pos = {c["rfc"]: i for i, c in enumerate(dchunks) if c["kind"] == "Consequence"}
        b5 = b10 = c5 = c10 = 0
        for f in fq:
            rb = rank_of(bm_blob, f["query"], blob_pos[f["title"]])
            rc = rank_of(bm_dec, f["query"], cons_pos[f["title"]]) if f["title"] in cons_pos else None
            b5 += hit(rb, 5); b10 += hit(rb, 10); c5 += hit(rc, 5); c10 += hit(rc, 10)
        m = len(fq)
        deltas[n] = (c5 - b5) / m
        print(f"{n:>5}  {b5/m:>7.2f}{b10/m:>8.2f} | {c5/m:>11.2f}{c10/m:>12.2f} | {(c5-b5)/m:>+7.2f}")

    widen = deltas[NS[-1]] - deltas[NS[0]]
    print(f"\nΔ@5 (Consequence SEED − blob), facet-keyed: N={NS[0]} {deltas[NS[0]]:+.2f} → N={NS[-1]} "
          f"{deltas[NS[-1]]:+.2f}  widen={widen:+.2f}")
    if deltas[NS[-1]] > 0 and widen >= 0:
        print("→ YES: facet-keyed queries favor the focused node, and it holds/widens with N. Decomposition")
        print("  CAN help on rust-rfcs for this query TYPE (seed precision; Stage 1 expansion brings the answer).")
    elif deltas[NS[-1]] > 0:
        print("→ focused node wins but the gap narrows with N — partial; Stage 1 dense+expansion decides.")
    else:
        print("→ even facet-keyed, the blob wins on rust-rfcs — decomposition's retrieval edge needs a messier corpus.")


if __name__ == "__main__":
    main()
