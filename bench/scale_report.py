"""Stage 1 report: does B2's dense channel RESIST the decay B1-rag suffers as N grows?

Reads scale/probe_{corpus}.jsonl (rows from scale_probe b1rag + b2@{0.99,0.50}). Prints per-N MRR for
each arm, the headline Δ = MRR[B2 hybrid 0.50] − MRR[B1-rag], the within-engine control
Δ_dense = MRR[B2 0.50] − MRR[B2 0.99], the slope of Δ vs log N with a target-bootstrap 95% CI, and the
PRE-REGISTERED STRICT verdict (VALIDATED / KILL-retrieval-parity / AMBIGUOUS).
"""
import argparse
import json
from pathlib import Path

import numpy as np

import config
from scale_probe import _capped, _hit, _slope_logN, load_targets, LIMIT
from hybrid_ab_report import wilson


def load(corpus, path=None):
    p = Path(path) if path else config.BENCH / "scale" / f"probe_{corpus}.jsonl"
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="rust-rfcs")
    ap.add_argument("--probe", default=None)
    a = ap.parse_args()
    rows = load(a.corpus, a.probe)
    ns = sorted({r["N"] for r in rows})
    targets = sorted({r["target"] for r in rows})

    def match(arm, floor, n, t=None):
        return [r["rank"] for r in rows if r["arm"] == arm and r["N"] == n
                and (floor is None or r.get("floor") == floor) and (t is None or r["target"] == t)]

    def rr(arm, floor):  # per-(target,N) mean capped reciprocal rank (averaged over seeds)
        return {(t, n): float(np.mean([1.0 / c if (c := _capped(x)) else 0.0 for x in match(arm, floor, n, t)])
                              ) if match(arm, floor, n, t) else 0.0
                for t in targets for n in ns}

    rr_b1, rr_b2_05, rr_b2_99 = rr("b1rag", None), rr("b2", 0.5), rr("b2", 0.99)
    mrr = lambda d, n: float(np.mean([d[(t, n)] for t in targets]))

    print(f"\n=== Scale-degradation: Δ = MRR[B2 hybrid 0.50] − MRR[B1-rag] vs N  ({a.corpus}) ===")
    print("Δ_dense = MRR[B2 0.50] − MRR[B2 0.99] (within-engine control, isolates the dense channel)")
    print(f"{'N':>6}{'B1rag':>8}{'B2@.99':>8}{'B2@.50':>8}{'Δ':>9}{'Δ_dense':>9}{'h5_b1':>7}{'h5_b2':>7}")
    deltas = []
    for n in ns:
        m1, m99, m05 = mrr(rr_b1, n), mrr(rr_b2_99, n), mrr(rr_b2_05, n)
        deltas.append(m05 - m1)
        print(f"{n:>6}{m1:>8.3f}{m99:>8.3f}{m05:>8.3f}{m05-m1:>+9.3f}{m05-m99:>+9.3f}"
              f"{_hit(match('b1rag',None,n),5):>7.2f}{_hit(match('b2',0.5,n),5):>7.2f}")

    # slope of Δ vs log N + target-bootstrap 95% CI
    rng = np.random.default_rng(0)
    betas = []
    for _ in range(3000):
        s = rng.choice(len(targets), len(targets), replace=True)
        ds = [float(np.mean([rr_b2_05[(targets[i], n)] - rr_b1[(targets[i], n)] for i in s])) for n in ns]
        betas.append(_slope_logN(ns, ds))
    b = _slope_logN(ns, deltas)
    lo, hi = float(np.percentile(betas, 2.5)), float(np.percentile(betas, 97.5))
    widen = deltas[-1] - deltas[0]

    # endpoint hit@5 Wilson bands (non-overlap at N_max?)
    def wil(arm, floor, n, k=5):
        rk = match(arm, floor, n)
        hits = sum(1 for x in rk if (c := _capped(x)) and c <= k)
        return hits, len(rk), wilson(hits, len(rk))
    hb1, hb2 = wil("b1rag", None, ns[-1]), wil("b2", 0.5, ns[-1])
    nonoverlap = hb2[2][0] > hb1[2][1]

    print(f"\nΔ slope vs log10(N): β = {b:+.3f}  [95% CI {lo:+.3f}, {hi:+.3f}]")
    print(f"endpoint Δ widening: {widen:+.3f}  (Δ@N={ns[0]} {deltas[0]:+.3f} → Δ@N={ns[-1]} {deltas[-1]:+.3f})")
    print(f"hit@5 @N={ns[-1]}: B1-rag {hb1[0]}/{hb1[1]} [{hb1[2][0]:.2f},{hb1[2][1]:.2f}]  "
          f"B2 {hb2[0]}/{hb2[1]} [{hb2[2][0]:.2f},{hb2[2][1]:.2f}]  non-overlap={nonoverlap}")

    # PRE-REGISTERED STRICT verdict
    if b > 0 and lo > 0 and widen >= 0.10 and nonoverlap:
        v = "VALIDATED — the retrieval edge COMPOUNDS with scale (supports the months-long trial)"
    elif (lo <= 0 <= hi) or b <= 0 or widen < 0.05:
        v = ("KILL (retrieval-parity, NOT a project kill) — structured ≈ a vector index on "
             "retrieval-at-scale; route to the capability/structural/governance/longitudinal instruments")
    else:
        v = "AMBIGUOUS — inconclusive; needs more targets / larger N before a green light"
    print(f"\nVERDICT [{a.corpus}]: {v}")


if __name__ == "__main__":
    main()
