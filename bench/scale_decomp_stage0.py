"""Stage 0 ($0, offline): does decomposition help retrieval BEFORE building any graph store?

For each Drawbacks query, compare three pure-BM25 retrievals over the same N RFCs:
- BLOB index (N full-RFC chunks): is the target's blob in top-k?  (today's RAG)
- DECOMPOSED index (~4N section chunks): is the target's CONSEQUENCE chunk (the answer) in top-k? (precision)
- ... and is the target's AD chunk in top-k?  (the seed expansion would bridge from in Stage 1)
Swept at N=50/100/200/400. If the focused Consequence chunk out-ranks the blob MORE as N grows, the
PRECISION mechanism is alive. If only the AD surfaces (not the Consequence), the value is EXPANSION —
which Stage 1's hops=2 tests. Either way this is $0 and decides what to expect before building stores.
"""
import json
import random
import re

import numpy as np
from rank_bm25 import BM25Okapi

import config
from scale_targets import load_corpus, by_title, section, TOK

# RFC section -> (decomposed node kind, label suffix). Rationale+Alternatives fold into one Alternative.
SECMAP = [("Summary", "ArchitecturalDecision", ""), ("Motivation", "Requirement", "— motivation"),
          ("Drawbacks", "Consequence", "— drawbacks"), ("Rationale And Alternatives", "Alternative", "— alternatives")]
NS = [50, 100, 200, 400]


def section_chunks(rec: dict) -> list[dict]:
    """Decompose one RFC chunk into its section chunks (label + body), mirroring the planned graph nodes."""
    out = []
    for name, kind, suffix in SECMAP:
        body = section(rec["text"], name)
        if name == "Rationale And Alternatives":
            body = (body + "\n" + section(rec["text"], "Alternatives")).strip()
        if body:
            out.append({"rfc": rec["title"], "kind": kind, "text": f"{rec['title']} {suffix}\n\n{body}"})
    return out


def rank_of(bm: BM25Okapi, query: str, target_idx: int) -> int | None:
    scores = bm.get_scores(TOK(query))
    if scores[target_idx] <= 0:
        return None
    return sorted(range(len(scores)), key=lambda i: (-scores[i], i)).index(target_idx) + 1


def hit(r, k):
    return 1 if (r is not None and r <= k) else 0


def main() -> None:
    recs = load_corpus()
    idx = by_title(recs)
    targets = json.loads((config.BENCH / "scale" / "targets_rust-rfcs_sections.json").read_text())
    draw = [t for t in targets if t["section"] == "Consequence"]
    print(f"Stage 0 precision gate: {len(draw)} Drawbacks→Consequence targets")
    for t in draw[:3]:
        print(f"  Q: {t['query'][:96]}")
    tset = {t["title"] for t in targets}
    pool = [r["title"] for r in recs if r["title"] not in tset]
    random.Random(0).shuffle(pool)

    print(f"\n{'N':>5}  {'blob@5':>7}{'blob@10':>8} | {'cons@5':>7}{'cons@10':>8} | {'ad@5':>6}{'ad@10':>7} | {'Δcons@5':>8}")
    deltas = {}
    for n in NS:
        titles = list(tset) + pool[: max(0, n - len(tset))]
        subset = [idx[t] for t in titles]
        bm_blob = BM25Okapi([TOK(r["text"]) for r in subset])
        blob_pos = {r["title"]: i for i, r in enumerate(subset)}
        dchunks = [c for r in subset for c in section_chunks(r)]
        bm_dec = BM25Okapi([TOK(c["text"]) for c in dchunks])
        cons_pos = {c["rfc"]: i for i, c in enumerate(dchunks) if c["kind"] == "Consequence"}
        ad_pos = {c["rfc"]: i for i, c in enumerate(dchunks) if c["kind"] == "ArchitecturalDecision"}
        b5 = b10 = c5 = c10 = a5 = a10 = 0
        for t in draw:
            rb = rank_of(bm_blob, t["query"], blob_pos[t["title"]])
            rc = rank_of(bm_dec, t["query"], cons_pos[t["title"]]) if t["title"] in cons_pos else None
            ra = rank_of(bm_dec, t["query"], ad_pos[t["title"]]) if t["title"] in ad_pos else None
            b5 += hit(rb, 5); b10 += hit(rb, 10); c5 += hit(rc, 5); c10 += hit(rc, 10); a5 += hit(ra, 5); a10 += hit(ra, 10)
        m = len(draw)
        deltas[n] = (c5 - b5) / m
        print(f"{n:>5}  {b5/m:>7.2f}{b10/m:>8.2f} | {c5/m:>7.2f}{c10/m:>8.2f} | {a5/m:>6.2f}{a10/m:>7.2f} | {(c5-b5)/m:>+8.2f}")

    widen = deltas[NS[-1]] - deltas[NS[0]]
    print(f"\nΔcons@5 (focused Consequence − blob): N={NS[0]} {deltas[NS[0]]:+.2f} → N={NS[-1]} {deltas[NS[-1]]:+.2f}  widen={widen:+.2f}")
    if deltas[NS[-1]] > 0 and widen > 0:
        print("→ focused-chunk PRECISION advantage WIDENS with N — proceed to Stage 1 (graph + hops).")
    elif deltas[NS[-1]] <= 0:
        print("→ focused Consequence does NOT beat the blob at scale on its own; the decomposed win (if any)")
        print("  must come from EXPANSION (AD seed → Consequence). Stage 1's hops=2 is the real test — proceed,")
        print("  watching whether the AD chunk surfaces (ad@k above) so expansion has a seed to bridge from.")
    else:
        print("→ precision present but not compounding; Stage 1's hops factorial attributes precision vs expansion.")


if __name__ == "__main__":
    main()
