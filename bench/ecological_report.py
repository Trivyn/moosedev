"""Instrument C focused report: the ecological headline (B0 / B1-notes / B2) on trivyn-temporal,
restricted to THIS refresh's rows (runs_ecological.jsonl) so the pre-hybrid history in runs.jsonl
doesn't dilute it. B2 here uses the new hybrid seed (default floor). Confirms the standing result —
B2 beats stale real-notes on currency + is cheaper (Lessons 046bc2d8, 6227d977) — survives hybrid.

STRICT currency = coverage==1 AND not stale (Lesson 9fc21dc0).
"""
import collections
import json
from pathlib import Path

OUT = Path.home() / "code" / "moosedev_benches" / "trivyn-temporal" / "runs"


def strict_current(r: dict) -> bool:
    m = r.get("metrics") or {}
    return m.get("coverage") == 1.0 and not m.get("stale")


def main() -> None:
    p = OUT / "runs_ecological.jsonl"
    if not p.exists():
        print("no runs_ecological.jsonl")
        return
    rows = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
    rows = [r for r in rows if r["task_id"].endswith("currency")]
    by = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in rows:
        by[r["task_id"]][r["arm"]].append(r)

    print("\n=== Instrument C: ecological headline (hybrid B2), trivyn-temporal, codex/gpt-5.4-mini ===")
    print(f"{'task':<26}{'arm':<10}{'n':>4}{'current%':>10}{'mean cov':>10}{'mean tok':>10}")
    for task in sorted(by):
        for arm in sorted(by[task]):
            rs = by[task][arm]
            n = len(rs)
            cur = sum(strict_current(r) for r in rs)
            cov = sum((r.get("metrics") or {}).get("coverage", 0.0) for r in rs) / n
            tok = sum(r["tokens"]["agent_prompt"] + r["tokens"]["agent_completion"] for r in rs) / n
            print(f"{task:<26}{arm:<10}{n:>4}{f'{round(100*cur/n)}% ({cur}/{n})':>10}{cov:>10.2f}{tok:>10.0f}")
        print()


if __name__ == "__main__":
    main()
