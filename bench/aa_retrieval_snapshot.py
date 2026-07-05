"""A/A retrieval snapshot — neutrality evidence for store maintenance.

Captures `get_relevant_context` output for every frozen probe of a corpus,
deriving the topic exactly as run.py's oracle mode does (`memory_topic` or
`prompt`, limit 6). Run against the LIVE daemon before and after a store
maintenance operation (e.g. a namespace migration), then diff:

    python3 aa_retrieval_snapshot.py --corpus trivyn-trial --out .../pre
    # ... maintenance ...
    python3 aa_retrieval_snapshot.py --corpus trivyn-trial --out .../post
    diff -r .../pre .../post   # empty diff = retrieval-neutral maintenance

An empty diff is the pre-registered evidence that the maintenance did not
change what B2 serves (in-anger trial protocol, AD 07415633).
"""

import argparse
import asyncio
import json
from pathlib import Path

import config
from export_corpus import corpus_env
from mcp_client import call_tool


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int, default=6)
    args = ap.parse_args()

    out = Path(args.out).expanduser()
    out.mkdir(parents=True, exist_ok=True)
    tasks_dir = Path(config.corpus_tasks_path(args.corpus))
    task_files = sorted(tasks_dir.glob("*.json"))
    if not task_files:
        raise SystemExit(f"no task JSONs under {tasks_dir}")

    for tf in task_files:
        task = json.loads(tf.read_text())
        topic = task.get("memory_topic") or task["prompt"]
        text = asyncio.run(
            call_tool(
                config.MOOSEDEV_BIN,
                ["--connect"],
                corpus_env(args.corpus),
                "get_relevant_context",
                {"topic": topic, "limit": args.limit},
            )
        )
        (out / f"{tf.stem}.txt").write_text(text)
        print(f"{tf.stem}: {len(text)} bytes")
    print(f"\n{len(task_files)} snapshots -> {out}")


if __name__ == "__main__":
    main()
