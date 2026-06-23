"""Build the FROZEN target/query spec for the scale-degradation benchmark (one-time).

A "target" is a record present in every N-store; a "query" is the developer question used to
retrieve it. For rust-rfcs we pick ~25 RFCs: the 3 anchor tasks reuse their already-vetted prompts;
the rest get a query authored ONCE by the local NLQ_MODEL ("ask the question in the problem's SHARED
vocabulary; never quote the title/number"), then a deterministic, no-LLM leak filter records the
query<->title / query<->body token overlap so a reviewer can audit that queries aren't keyword-spiked
(a leaky query would hide the very decay the benchmark tests). Output is keyed by TITLE — the per-store
IRI is resolved later from each store's parity export (record_important_decision mints a fresh UUID per
call, so IRIs are not stable across stores).

  MOOSEDEV_LLM_BASE_URL=http://localhost:1234/v1 .venv/bin/python scale_targets.py [--n 25] [--seed 0]

Re-run only to regenerate the spec; the sweep then reads the frozen JSON and is 100% deterministic.
"""
import argparse
import json
import random
import re
from pathlib import Path

import httpx

import config

CORPUS = "rust-rfcs"
OUT = config.BENCH / "scale"
TOK = lambda s: re.findall(r"[a-z0-9]+", s.lower())  # identical to the B1-rag / BM25 tokenizer
STOP = {"the", "a", "an", "of", "to", "for", "and", "or", "in", "on", "with", "is", "are", "be",
        "this", "that", "it", "as", "by", "at", "from", "rfc", "rust", "based", "using", "via"}

SYS = ("You write the natural question a software developer would ask to recover the RATIONALE of a "
       "design decision. Phrase it in the problem's GENERAL, SHARED vocabulary — describe the problem, "
       "do NOT name the specific feature, and NEVER quote the RFC title or its number. Output ONLY the "
       "question, 1-2 sentences, no preamble.")


def load_corpus() -> list[dict]:
    return json.loads(config.corpus_chunks_path(CORPUS).read_text())


def by_title(records: list[dict]) -> dict[str, dict]:
    return {r["title"]: r for r in records}


def anchor_tasks() -> list[tuple[str, str]]:
    """(decision_title, prompt) for the 3 vetted rust-rfcs anchor tasks — real, non-leaky queries."""
    out = []
    for f in sorted((config.BENCH / "tasks_public" / CORPUS).glob("*.json")):
        t = json.loads(f.read_text())
        title = (t.get("ground_truth") or {}).get("decision_title", "").strip()
        if title and t.get("prompt"):
            out.append((title, t["prompt"].strip()))
    return out


def section(text: str, name: str) -> str:
    """Pull one '## Section' body out of a chunk's text (Summary/Motivation live there verbatim)."""
    m = re.search(rf"^##\s+{name}\s*$(.*?)(?=^##\s|\Z)", text, re.M | re.S | re.I)
    return m.group(1).strip() if m else ""


def title_feature_tokens(title: str) -> set[str]:
    """Distinctive tokens of an 'RFC N: feature' title (drop 'RFC', the number, stopwords)."""
    feat = title.split(":", 1)[1] if ":" in title else title
    return {w for w in TOK(feat) if w not in STOP and not w.isdigit() and len(w) > 2}


def jaccard(a: set[str], b: set[str]) -> float:
    return len(a & b) / len(a | b) if (a or b) else 0.0


def rfc_num(title: str) -> str:
    m = re.search(r"RFC\s+(\d+)", title)
    return m.group(1) if m else ""


def author_query(rec: dict, base_url: str, model: str, reinforce: bool = False) -> str:
    sm = (f"Summary:\n{section(rec['text'], 'Summary')}\n\n"
          f"Motivation:\n{section(rec['text'], 'Motivation')}")[:1600]
    extra = (" The earlier attempt named the feature too directly; describe ONLY the underlying problem."
             if reinforce else "")
    resp = httpx.post(f"{base_url.rstrip('/')}/chat/completions", timeout=120.0, json={
        "model": model, "temperature": 0.0,
        "messages": [{"role": "system", "content": SYS + extra},
                     {"role": "user", "content": f"Decision context (a Rust RFC):\n\n{sm}\n\nWrite the developer's question now."}],
    })
    resp.raise_for_status()
    q = resp.json()["choices"][0]["message"]["content"].strip().strip('"').strip()
    return " ".join(q.split())  # collapse whitespace/newlines


def leak(query: str, title: str) -> tuple[float, float, bool, str]:
    """Return (overlap_title, distinctive_token_hits as a 0..1 share, leaky?, why)."""
    qtok = set(TOK(query))
    feat = title_feature_tokens(title)
    hits = qtok & feat
    num = rfc_num(title)
    leaky = (num and num in qtok) or len(hits) >= 2
    why = f"num={num in qtok} feat_hits={sorted(hits)}"
    return jaccard(qtok, set(TOK(title))), (len(hits) / len(feat) if feat else 0.0), leaky, why


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=25, help="total targets (incl. anchors)")
    ap.add_argument("--seed", type=int, default=0)
    a = ap.parse_args()
    base_url, model = config.LLM_BASE_URL, config.NLQ_MODEL
    records = load_corpus()
    idx = by_title(records)

    targets: list[dict] = []
    used: set[str] = set()

    # 1) Anchors: vetted task prompts, no LLM, no leak filtering (they are the gold standard).
    for title, prompt in anchor_tasks():
        if title in idx:
            ot, _, _, _ = leak(prompt, title)
            targets.append({"title": title, "query": prompt, "source": "anchor",
                            "overlap_title": round(ot, 3), "feat_share": 0.0, "sentinel": False})
            used.add(title)
        else:
            print(f"  WARN anchor title not in corpus, skipping: {title!r}")

    # 2) Sentinel: a deliberately distinctive query that MUST rank #1 at small N (harness canary).
    pool = [r["title"] for r in records if r["title"] not in used]
    rng = random.Random(a.seed)
    rng.shuffle(pool)
    sent_title = pool.pop(0)
    targets.append({"title": sent_title, "query": sent_title.split(":", 1)[-1].strip(),
                    "source": "sentinel", "overlap_title": 1.0, "feat_share": 1.0, "sentinel": True})
    used.add(sent_title)

    # 3) LLM-authored, leak-filtered targets for the remainder.
    need = max(0, a.n - len(targets))
    print(f"authoring {need} queries via {model} @ {base_url} …", flush=True)
    for title in pool:
        if len(targets) >= a.n:
            break
        rec = idx[title]
        try:
            q = author_query(rec, base_url, model)
            ot, fs, leaky, why = leak(q, title)
            if leaky:  # one reinforced re-author; keep the lower-overlap result
                q2 = author_query(rec, base_url, model, reinforce=True)
                ot2, fs2, leaky2, _ = leak(q2, title)
                if fs2 < fs:
                    q, ot, fs, leaky = q2, ot2, fs2, leaky2
            overlap_text = jaccard(set(TOK(q)), set(TOK(rec["text"])))
            targets.append({"title": title, "query": q, "source": "llm",
                            "overlap_title": round(ot, 3), "feat_share": round(fs, 3),
                            "overlap_text": round(overlap_text, 3), "leaky": leaky, "sentinel": False})
            tag = " LEAKY" if leaky else ""
            print(f"  [{len(targets)}/{a.n}] {title[:48]:<48} ot={ot:.2f} feat={fs:.2f}{tag}", flush=True)
        except Exception as e:
            print(f"  SKIP {title[:48]}: {type(e).__name__}: {e}", flush=True)

    OUT.mkdir(parents=True, exist_ok=True)
    out_path = OUT / f"targets_{CORPUS}.json"
    out_path.write_text(json.dumps(targets, indent=2))
    leaky_n = sum(1 for t in targets if t.get("leaky"))
    ots = sorted(t["overlap_title"] for t in targets if t["source"] == "llm")
    print(f"\nwrote {len(targets)} targets -> {out_path}")
    print(f"  sources: {sum(t['source']=='anchor' for t in targets)} anchor, 1 sentinel, "
          f"{sum(t['source']=='llm' for t in targets)} llm; {leaky_n} still-leaky")
    if ots:
        print(f"  llm overlap_title: min={ots[0]:.2f} median={ots[len(ots)//2]:.2f} max={ots[-1]:.2f} "
              f"(pre-registered audit of query keyword-spiking)")


if __name__ == "__main__":
    main()
