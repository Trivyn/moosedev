"""Section-aware targets for the DECOMPOSITION benchmark (one-time, frozen).

Each target is a (RFC, section) pair whose answer lives in ONE section: Drawbacks (→ the decomposed
graph's `Consequence` node) or Motivation (→ its `Requirement` node). The query describes the decision
in the problem's SHARED vocabulary (never the feature/number) and asks specifically for that section,
so the answer-bearing record differs by representation (blob/AD vs the focused section node) but the
question is identical across arms. Deterministic leak filter + frozen output, exactly like scale_targets.

  MOOSEDEV_LLM_BASE_URL=http://localhost:1234/v1 .venv/bin/python scale_targets_sections.py [--n 25]
"""
import argparse
import json
import random
import re

import httpx

import config
from scale_targets import CORPUS, OUT, TOK, load_corpus, by_title, section, leak, jaccard

# section -> (answer-bearing decomposed node KIND, what the query asks for)
SECTIONS = {
    "Drawbacks":  ("Consequence", "the main DRAWBACK / limitation / cost"),
    "Motivation": ("Requirement", "the PROBLEM that motivated it"),
}
MIN_BODY = 200  # a section needs a substantive body to be a distinctive answer

SYS = ("You write the natural question a software developer would ask. Describe the DECISION in the "
       "problem's GENERAL, SHARED vocabulary (never name the specific feature or RFC number), then ask "
       "specifically for {ask}. Output ONLY the question, 1-2 sentences, no preamble.")


def author(summary: str, body: str, ask: str, base_url: str, model: str, reinforce: bool = False) -> str:
    extra = " The earlier attempt named the feature; describe ONLY the underlying problem." if reinforce else ""
    ctx = f"Decision (summary):\n{summary}\n\nThe relevant section:\n{body}"[:1600]
    resp = httpx.post(f"{base_url.rstrip('/')}/chat/completions", timeout=120.0, json={
        "model": model, "temperature": 0.0,
        "messages": [{"role": "system", "content": SYS.format(ask=ask) + extra},
                     {"role": "user", "content": f"{ctx}\n\nWrite the developer's question now."}],
    })
    resp.raise_for_status()
    q = resp.json()["choices"][0]["message"]["content"].strip().strip('"').strip()
    return " ".join(q.split())


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=25)
    ap.add_argument("--seed", type=int, default=0)
    a = ap.parse_args()
    base_url, model = config.LLM_BASE_URL, config.NLQ_MODEL
    idx = by_title(load_corpus())

    # candidate (title, section) pairs with a substantive body
    cands = []
    for title, rec in idx.items():
        summ = section(rec["text"], "Summary")
        for sec in SECTIONS:
            body = section(rec["text"], sec)
            if len(body) >= MIN_BODY and len(summ) >= 80:
                cands.append((title, sec, summ, body))
    rng = random.Random(a.seed)
    rng.shuffle(cands)

    targets, used = [], set()
    # sentinel: a Drawbacks target with a deliberately distinctive query (must rank #1 small-N canary)
    for title, sec, summ, body in cands:
        if sec == "Drawbacks":
            kind = SECTIONS[sec][0]
            targets.append({"title": title, "section": kind, "rfc_section": sec,
                            "query": f"{title.split(':',1)[-1].strip()} drawbacks",
                            "source": "sentinel", "sentinel": True, "overlap_title": 1.0})
            used.add((title, sec))
            break

    print(f"authoring section queries via {model} @ {base_url} …", flush=True)
    for title, sec, summ, body in cands:
        if len(targets) >= a.n or (title, sec) in used or title in {t["title"] for t in targets}:
            continue  # one section per RFC, keep targets RFC-distinct
        kind, ask = SECTIONS[sec]
        try:
            q = author(summ, body, ask, base_url, model)
            ot, fs, leaky, _ = leak(q, title)
            if leaky:
                q2 = author(summ, body, ask, base_url, model, reinforce=True)
                ot2, fs2, leaky2, _ = leak(q2, title)
                if fs2 < fs:
                    q, ot, fs, leaky = q2, ot2, fs2, leaky2
            targets.append({"title": title, "section": kind, "rfc_section": sec, "query": q,
                            "source": "llm", "sentinel": False,
                            "overlap_title": round(ot, 3), "feat_share": round(fs, 3), "leaky": leaky})
            print(f"  [{len(targets)}/{a.n}] {sec:<10} {title[:42]:<42} ot={ot:.2f} feat={fs:.2f}"
                  f"{' LEAKY' if leaky else ''}", flush=True)
        except Exception as e:
            print(f"  SKIP {title[:40]}: {type(e).__name__}: {e}", flush=True)

    OUT.mkdir(parents=True, exist_ok=True)
    out_path = OUT / f"targets_{CORPUS}_sections.json"
    out_path.write_text(json.dumps(targets, indent=2))
    by_sec = {}
    for t in targets:
        by_sec[t["section"]] = by_sec.get(t["section"], 0) + 1
    print(f"\nwrote {len(targets)} targets -> {out_path}")
    print(f"  by answer-node kind: {by_sec}; leaky: {sum(1 for t in targets if t.get('leaky'))}")


if __name__ == "__main__":
    main()
