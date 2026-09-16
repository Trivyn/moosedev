#!/usr/bin/env python3
"""Pool per-cell stores into one store so grading.report can score the study.

Each cell runs into its own store, but `grading.report` pools within a single
store root. Merging is legitimate rather than tampering: every run verifies
independently against its own seal, and `run_index.jsonl` is an append-only
ledger whose entries are keyed by run_id, so carrying each run's entries along
with its directory preserves exactly the evidence the seal covers. Run IDs are
UUIDs, so no cell can collide with another.

Runs are hardlinked, not copied: the merged store costs no extra disk.
"""
import argparse
import json
import shutil
from pathlib import Path

STAGE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--stage", type=Path, default=STAGE,
                        help="directory holding store-cell-* (default: beside this script)")
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--fresh", action="store_true", help="rebuild from scratch")
    args = parser.parse_args()
    stage = args.stage.resolve()
    args.out = args.out or stage / "evidence-pooled"
    if args.fresh and args.out.exists():
        shutil.rmtree(args.out)
    (args.out / "runs").mkdir(parents=True, exist_ok=True)

    wanted, index, linked, skipped = set(), [], 0, 0
    # Shared client archives live at the STORE root and are verified with
    # st_nlink == 1, an explicit anti-aliasing rule, so these are real copies
    # while everything else is hardlinked. There are only a couple of distinct
    # digests across the whole campaign, so one copy each is cheap.
    assets = args.out / "assets"
    assets.mkdir(exist_ok=True)
    for source in sorted(STAGE.glob("store-cell-*/assets/*.tar")):
        target = assets / source.name
        if not target.exists():
            shutil.copy2(source, target)
            print(f"  copied asset {source.name[:12]}... ({target.stat().st_size / 1e6:.0f} MB)")
    for store in sorted(stage.glob("store-cell-*")):
        for run in sorted((store / "runs").glob("*")):
            if not (run / "seal.json").is_file():
                skipped += 1           # still running, or never sealed
                continue
            target = args.out / "runs" / run.name
            if not target.exists():
                # Hardlink the tree; identical bytes, no extra disk.
                shutil.copytree(run, target, copy_function=lambda s, d: Path(d).hardlink_to(s))
                linked += 1
            wanted.add(run.name)
        ledger = store / "run_index.jsonl"
        if ledger.is_file():
            for line in ledger.read_text().splitlines():
                if line.strip():
                    index.append(json.loads(line))
    kept = [entry for entry in index if entry.get("run_id") in wanted]
    (args.out / "run_index.jsonl").write_text(
        "".join(json.dumps(entry, sort_keys=True, separators=(",", ":")) + "\n" for entry in kept))
    print(f"pooled {len(wanted)} sealed runs ({linked} newly linked, {skipped} unsealed skipped)")
    print(f"index entries: {len(kept)}")
    print(f"store: {args.out}")


if __name__ == "__main__":
    main()
