"""Decompose archived harness prompts by section, per episode, offline."""
import base64, json, sys, glob, os
from collections import defaultdict
sys.path.insert(0, os.getcwd())
from bench.harness_study.crowding import _segments

def prompts(run):
    """Every agent request the proxy archived, in order, with its episode."""
    for line in open(os.path.join(run, "events.jsonl")):
        try: e = json.loads(line)
        except ValueError: continue
        p = e.get("payload") or {}
        if e.get("channel") != "model" or p.get("event") != "request_bytes": continue
        if p.get("role") != "agent": continue
        try: body = json.loads(base64.b64decode(p["raw_base64"]))
        except Exception: continue
        msgs = body.get("messages") or []
        text = "\n".join(m.get("content") or "" for m in msgs if isinstance(m.get("content"), str))
        if len(text) < 500: continue          # skip the tiny compatibility probe
        yield p.get("episode"), text

def main(store):
    run = sorted(glob.glob(os.path.join(store, "runs", "*")))[-1]
    per_ep = defaultdict(lambda: defaultdict(list))
    totals = defaultdict(list)
    prev_by_ep = {}
    repeat = defaultdict(list)
    for ep, text in prompts(run):
        totals[ep].append(len(text))
        for name, seg in _segments(text):
            per_ep[ep][name].append(len(seg))
        prev = prev_by_ep.get(ep)
        if prev is not None:
            # crude but honest: longest common prefix as a proxy for re-sent content
            n = min(len(prev), len(text)); i = 0
            while i < n and prev[i] == text[i]: i += 1
            repeat[ep].append(i / len(text))
        prev_by_ep[ep] = text

    print(f"store: {os.path.basename(store)}   run: {os.path.basename(run)}")
    eps = sorted(totals)
    print(f"\n{'episode':8} {'reqs':>5} {'mean bytes':>11} {'max bytes':>10} {'mean prefix reused':>19}")
    for ep in eps:
        t = totals[ep]
        r = repeat.get(ep) or [0]
        print(f"{ep:8} {len(t):5} {sum(t)//len(t):11,} {max(t):10,} {sum(r)/len(r):18.1%}")

    names = sorted({n for ep in eps for n in per_ep[ep]})
    print(f"\nMEAN BYTES PER SECTION, BY EPISODE")
    print(f"{'section':16} " + " ".join(f"{ep:>10}" for ep in eps) + f" {'e1->last':>12}")
    for name in names:
        row = []
        for ep in eps:
            v = per_ep[ep].get(name) or [0]
            row.append(sum(v) // len(v))
        growth = "-" if not row[0] else f"{row[-1]/row[0]:.1f}x"
        print(f"{name:16} " + " ".join(f"{v:10,}" for v in row) + f" {growth:>12}")

if __name__ == "__main__":
    main(sys.argv[1])
