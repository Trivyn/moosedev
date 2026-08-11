"""LLM-judge fair RECALL re-grade for capability_qa set tasks — regrade-safe (stored transcripts).

WHY: grade_set matches predicted items to expected titles by difflib >= 0.88, scoring PARAPHRASES as
misses -> a false-0 recall for competitor arms that answer in their OWN words (Lesson a6529240). This
tool re-grades RECALL by asking a strict gpt-5.4-mini judge which of the expected items a full answer
actually covers (meaning, not string).

Reading the WHOLE answer (not pre-extracted bullets) is format-robust: it handles B2's fenced
"title | IRI" blocks, markdown tables, and "old -> new" lists that a bullet-extractor silently misses
(the bug that made an earlier per-bullet judge score B2 0.00 on a task it strict-scored 1.00).

VALIDATION (always eyeball the B2 row): the judge MUST roughly reproduce the structured arm (B2) strict
recall. If JUDGE_rec << strict for B2, the judge is buggy/biased and the competitor numbers are untrustworthy.

PERSISTENCE: every judged row is appended to a `judge_verdicts.jsonl` sibling of the corpus runs
file, recording WHICH expected items the judge credited (indices AND titles), not just the recall
it produced. This is the ONE number in the benchmark that cannot be regraded deterministically: the
judge is an LLM, so re-running it is a fresh sample, not a replay (temperature=0 narrows drift, it
does not remove it, and the model behind a slug moves). Re-execution was therefore never the way to
check this column. The verdict log makes it auditable the way it actually can be -- by INSPECTION:
a reader sees exactly which items were credited for each arm and can judge the judge, instead of
taking a transcribed aggregate on faith. Each record pins `judge_model` so a later divergence is
attributable rather than mysterious. Append-only, mirroring runs.jsonl; on re-runs the LAST record
for a (task_id, arm) wins, matching how this script already picks the last matching run row.

  OPENROUTER_API_KEY in .env:
  bench/.venv/bin/python bench/regrade_judge.py --corpus codegraph --task set_all_constraints \
      --arms B2 B1-notes B1-mem0
"""
import argparse
import datetime
import json
import os
from pathlib import Path

import config

JUDGE_MODEL = os.environ.get("JUDGE_MODEL", "openai/gpt-5.4-mini")
CHUNK = 120  # expected items per judge call — keeps the prompt bounded for large sets (e.g. 297)


def _client():
    from openai import OpenAI
    key = os.environ.get("OPENROUTER_API_KEY")
    if not key:
        raise SystemExit("OPENROUTER_API_KEY not set (.env) — needed for the LLM judge.")
    return OpenAI(api_key=key, base_url="https://openrouter.ai/api/v1")


def _covered_chunk(client, answer: str, expected: list, base: int) -> set:
    """Indices (base+local, 1-based) of the expected items the full answer covers."""
    known = "\n".join(f"{i + 1}. {t}" for i, t in enumerate(expected))
    prompt = (
        "You are grading RECALL: which KNOWN ITEMS does the system ANSWER actually cover?\n\n"
        "KNOWN ITEMS:\n" + known + "\n\nSYSTEM ANSWER:\n" + answer +
        "\n\nReturn ONLY a JSON object {\"covered\": [numbers]} listing the KNOWN ITEM numbers the ANSWER "
        "genuinely asserts or lists (it may use different wording, IRIs, a table, or an 'old -> new' form). "
        "Be STRICT: include a number only if the answer states THAT specific item, not merely a related topic."
    )
    r = client.chat.completions.create(
        model=JUDGE_MODEL, temperature=0,
        messages=[{"role": "user", "content": prompt}],
        response_format={"type": "json_object"},
    )
    obj = json.loads(r.choices[0].message.content)
    out = set()
    for n in (obj.get("covered") or []):
        try:
            k = int(n)
        except (TypeError, ValueError):
            continue
        if 1 <= k <= len(expected):
            out.add(base + k)
    return out


def judge_recall(client, answer: str, expected: list) -> set:
    """Strict full-answer coverage of `expected`, chunked so big sets stay in a bounded prompt."""
    covered = set()
    for s in range(0, len(expected), CHUNK):
        covered |= _covered_chunk(client, answer, expected[s:s + CHUNK], s)
    return covered


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="codegraph")
    ap.add_argument("--task", default="set_all_constraints")
    ap.add_argument("--arms", nargs="+", default=["B2", "B1-notes", "B1-mem0"])
    ap.add_argument("--out", help="verdict log path (default: judge_verdicts.jsonl beside the runs file)")
    ap.add_argument("--no-log", action="store_true", help="print only; do not append verdicts")
    ap.add_argument("--repeat", type=int, default=1,
                    help="judge each (task, arm) N times; each draw is logged separately so "
                         "judge_report.py can show the instrument's spread")
    a = ap.parse_args()

    task = json.loads((config.corpus_tasks_path(a.corpus) / f"{a.task}.json").read_text())
    expected = [e["title"] for e in task["ground_truth"]["expected_set"]]
    n_exp = len(expected)
    runs_dir = config.corpus_runs_path(a.corpus)
    rows = [json.loads(l) for l in open(runs_dir / "runs.jsonl")]
    client = _client()

    print(f"task={a.task}  |expected|={n_exp}  judge={JUDGE_MODEL}  (validate: JUDGE_rec must ~= strict for B2)\n")
    print(f"{'arm':9} {'strict_rec':>10} | {'JUDGE_rec':>9} {'covered':>8}")
    verdicts = []
    for arm in a.arms:
        r = [x for x in rows if x["task_id"] == a.task and x["arm"] == arm]
        if not r:
            continue
        if len(r) > 1:
            print(f"note: {len(r)} runs for {a.task}/{arm}; judging most recent ({r[-1]['run_id']})")
        x = r[-1]
        ans = x["final_text"] or ""
        strict = x["metrics"].get("recall", 0)
        for rep in range(a.repeat):
            cov = judge_recall(client, ans, expected) if ans.strip() else set()
            rec = len(cov) / n_exp if n_exp else 0.0
            draw = f"  draw {rep + 1}/{a.repeat}" if a.repeat > 1 else ""
            print(f"{arm:9} {strict:>10.2f} | {rec:>9.2f} {len(cov):>8}{draw}")
            verdicts.append({
                "ts": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
                "corpus": a.corpus, "task_id": a.task, "arm": arm, "run_id": x.get("run_id"),
                "judge_model": JUDGE_MODEL, "n_expected": n_exp,
                "strict_recall": strict, "judge_recall": rec,
                # Indices are 1-based into the task's expected_set; titles are carried so a reader
                # can audit the judgement without re-deriving the ordering from the task JSON.
                "covered_idx": sorted(cov),
                "covered_titles": [expected[i - 1] for i in sorted(cov)],
            })

    if verdicts and not a.no_log:
        out = Path(a.out) if a.out else runs_dir / "judge_verdicts.jsonl"
        with open(out, "a", encoding="utf-8") as fh:
            for v in verdicts:
                fh.write(json.dumps(v) + "\n")
        print(f"\nappended {len(verdicts)} verdict(s) -> {out}")


if __name__ == "__main__":
    main()
