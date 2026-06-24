"""Blind LLM-judge for context_recovery probes — the in-anger trial's headline scorer (AD 07415633).

Scores how well a probe ANSWER recovers the SPECIFIC rationale in a frozen GOLD answer, on a 0/0.5/1
rubric. The judge is BLIND to which arm (B0 cold vs B2 graph) produced the answer — it sees only the
question, the gold 'why', and the candidate text, never the arm. Non-destructive: reads runs.jsonl,
appends to runs_judged.jsonl (one row per judged answer), never mutates the originals (regrade-safe).

GROUND TRUTH is the task's frozen ground_truth.gold — authored from PRIMARY sources (git/PRs/maintainer
knowledge), NEVER the in-graph record (no circularity — AD 47f3f038, Lesson 440abc78).

JUDGE MODEL defaults to the LOCAL qat model at endor (free, available, keeps the trial fully local —
invariant #9). Override JUDGE_BASE_URL / JUDGE_API_KEY / JUDGE_MODEL for a frontier judge.

VALIDATE THE JUDGE (Lesson a6529240): before trusting a B2-B0 delta, eyeball that a known-good answer
scores ~1.0 and a known-miss ~0.0; a judge that can't tell recovery from a plausible miss is untrustworthy.

DEAD-MCP GUARD (Lesson 0a890ea9): a B2 answer whose run never called get_relevant_context is flagged —
the memory tool may have silently failed, so the datum is suspect, not a real "graph didn't help".

  bench/.venv/bin/python bench/judge_recovery.py --corpus trivyn-trial --task <probe_id> --arms B0 B2 [--month YYYY-MM]
"""
import argparse
import json
import os
import re
import time

import config

JUDGE_MODEL = os.environ.get("JUDGE_MODEL", "google/gemma-4-26b-a4b-qat")
JUDGE_BASE_URL = os.environ.get("JUDGE_BASE_URL", "http://endor:1234/v1")
JUDGE_API_KEY = os.environ.get("JUDGE_API_KEY", "lmstudio")

RUBRIC = (
    "You are grading how well a CANDIDATE ANSWER recovers the SPECIFIC reason/rationale stated in the "
    "REFERENCE. Return ONLY a JSON object {\"score\": <0, 0.5, or 1>, \"why\": \"<one short sentence>\"}:\n"
    "  1   = states the SAME specific why as the reference (different wording is fine).\n"
    "  0.5 = partially correct: right direction but vague, incomplete, or mixed with an error.\n"
    "  0   = misses it, is generic, or asserts a wrong reason.\n"
    "Be STRICT: a plausible-sounding but non-specific answer scores 0, not 0.5. Judge ONLY against the "
    "reference; do not reward extra unverifiable detail. Output ONLY the JSON object, nothing else."
)


def _client():
    from openai import OpenAI
    return OpenAI(api_key=JUDGE_API_KEY, base_url=JUDGE_BASE_URL)


def _parse_score(txt: str):
    """Tolerant parse of the judge reply: prefer an embedded JSON object, else regex a 0/0.5/1 score."""
    m = re.search(r"\{.*\}", txt, re.S)
    if m:
        try:
            obj = json.loads(m.group(0))
            return float(obj.get("score")), (obj.get("why") or "")
        except Exception:
            pass
    m = re.search(r'score["\s:]*([01](?:\.\d+)?|0?\.5)', txt, re.I)
    if m:
        return float(m.group(1)), txt
    m = re.search(r"\b(0\.5|0|1)(?:\.0)?\b", txt)
    return (float(m.group(1)) if m else 0.0), txt


def judge_one(client, question: str, gold: str, answer: str) -> dict:
    """One blind 0/0.5/1 recovery score. The prompt never names the arm."""
    prompt = (RUBRIC + "\n\nQUESTION:\n" + question + "\n\nREFERENCE (the correct why):\n" + gold +
              "\n\nCANDIDATE ANSWER:\n" + (answer.strip() or "(empty)"))
    r = client.chat.completions.create(
        model=JUDGE_MODEL, temperature=0,
        messages=[{"role": "user", "content": prompt}],
    )
    s, why = _parse_score((r.choices[0].message.content or "").strip())
    return {"judge_score": max(0.0, min(1.0, s)), "judge_why": why[:300]}


def _fired_recall(row: dict) -> bool:
    """Did this run actually call the structured recall tool? (dead-MCP guard, Lesson 0a890ea9)"""
    tc = row.get("tool_counts") or {}
    return any(k.endswith("get_relevant_context") for k in tc)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--task", required=True)
    ap.add_argument("--arms", nargs="+", default=["B0", "B2"])
    ap.add_argument("--month", default=None, help="only judge rows stamped with this YYYY-MM")
    a = ap.parse_args()

    task = json.loads((config.corpus_tasks_path(a.corpus) / f"{a.task}.json").read_text())
    gold = (task.get("ground_truth") or {}).get("gold")
    if not gold:
        raise SystemExit(f"task {a.task}: no ground_truth.gold (the frozen primary-source 'why') — cannot judge.")
    question = task["prompt"]

    runs_path = config.corpus_runs_path(a.corpus) / "runs.jsonl"
    rows = [json.loads(l) for l in open(runs_path) if l.strip()]
    client = _client()
    out_path = config.corpus_runs_path(a.corpus) / "runs_judged.jsonl"

    print(f"task={a.task}  judge={JUDGE_MODEL} @ {JUDGE_BASE_URL} (BLIND to arm)  month={a.month or 'any'}  gold_len={len(gold)}")
    judged = []
    for arm in a.arms:
        r = [x for x in rows if x["task_id"] == a.task and x["arm"] == arm
             and (a.month is None or x.get("month") == a.month)]
        if not r:
            print(f"  {arm}: no rows")
            continue
        x = r[-1]  # most recent run for this (task, arm, month)
        v = judge_one(client, question, gold, x.get("final_text") or "")
        fired = _fired_recall(x)
        judged.append({
            "run_id": x["run_id"], "corpus": a.corpus, "task_id": a.task, "arm": arm,
            "month": x.get("month"), "judge_model": JUDGE_MODEL,
            "judge_score": v["judge_score"], "judge_why": v["judge_why"],
            "recall_fired": fired if arm == "B2" else None,
            "judged_ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        })
        warn = "  !! B2 never called get_relevant_context (dead-MCP? datum suspect)" if (arm == "B2" and not fired) else ""
        print(f"  {arm}: judge_score={v['judge_score']:.2f}  ({v['judge_why']}){warn}")

    with open(out_path, "a") as f:
        for rec in judged:
            f.write(json.dumps(rec) + "\n")
    print(f"wrote {len(judged)} judged rows -> {out_path}")


if __name__ == "__main__":
    main()
