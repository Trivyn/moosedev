#!/usr/bin/env python3
"""Run a contiguous range of floor-study cells, managing LM Studio between them.

Kept outside bench/harness_study on purpose: every file under the driver tree is
fingerprinted into the preflight, so a runner living there would invalidate the
campaign's own preflight (Lesson d650a3a1).

Two things this must get right that a naive loop gets wrong:

1. `run` exits 1 whenever the run status is not success, so an `agent_failure`
   (a real scientific result) is indistinguishable from a crash by exit code.
   Every cell is therefore classified from its sealed outcome.json, never from
   the exit status, and only infrastructure failures stop the campaign.
2. LM Studio's /api/v0/models returns a valid but EMPTY list on this version,
   so an unload loop driven by it silently unloads nothing and stacks every
   tier's weights until the machine dies. Only /api/v1/models is used here.
"""
import argparse
import json
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
STAGE = Path(__file__).resolve().parent
ENDPOINT = "http://127.0.0.1:1234/api/v1/models"
PYTHON = REPO / "bench/.venv/bin/python"
LMS = Path.home() / ".lmstudio/bin/lms"
# The driver's own load hangs for this key: it also matches the -qat variant, so
# LM Studio asks which to load and never returns. Preload it explicitly with -y.
PRELOAD = {"google/gemma-4-26b-a4b"}


def stamp():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def log(message, path):
    line = f"{stamp()} {message}"
    print(line, flush=True)
    with path.open("a") as stream:
        stream.write(line + "\n")


def inventory():
    with urllib.request.urlopen(ENDPOINT, timeout=30) as response:
        return json.load(response).get("models") or []


def loaded_contexts():
    """key -> context_length of each loaded instance whose identifier is the key."""
    found = {}
    for model in inventory():
        for instance in model.get("loaded_instances") or []:
            if (instance.get("identifier") or instance.get("id")) == model["key"]:
                found[model["key"]] = (instance.get("config") or {}).get("context_length")
    return found


def unload(key, journal):
    log(f"  unload {key}", journal)
    subprocess.run([str(LMS), "unload", key], capture_output=True, timeout=300,
                   stdin=subprocess.DEVNULL)


def preload(key, context, journal):
    log(f"  preload {key} at context {context}", journal)
    result = subprocess.run(
        [str(LMS), "load", key, "-y", "--identifier", key,
         "--context-length", str(context), "--parallel", "1"],
        capture_output=True, timeout=1800, stdin=subprocess.DEVNULL)
    if result.returncode:
        raise RuntimeError(f"preload failed for {key}: {result.stderr.decode(errors='replace')[:300]}")


def prepare(cell, expected, journal):
    """Leave exactly the models this cell needs loaded, at their pinned contexts."""
    required = [cell["model"]]
    if cell["condition"] != "without":
        required.append(cell["helper"])
    keep = set(required) | {cell["helper"]}          # the 7 GB helper stays warm between arms
    for key, context in loaded_contexts().items():
        if key not in keep:
            unload(key, journal)
        elif context != expected[key]:
            # The driver refuses a preloaded model at the wrong context; unloading
            # lets it load the model itself at the pinned one.
            log(f"  {key} loaded at {context}, expected {expected[key]}", journal)
            unload(key, journal)
    for key in required:
        if key in PRELOAD and key not in loaded_contexts():
            preload(key, expected[key], journal)
    current = loaded_contexts()
    for key in required:
        if key in current and current[key] != expected[key]:
            raise RuntimeError(f"{key} is loaded at {current[key]}, expected {expected[key]}")


def classify(store):
    """The sealed outcome decides, never the exit code."""
    runs = sorted((store / "runs").glob("*")) if (store / "runs").is_dir() else []
    if not runs:
        return "no-run", None
    outcome = json.loads((runs[-1] / "outcome.json").read_text())
    episodes = [e for e in outcome.get("episodes", []) if e.get("status") != "unattempted"]
    horizon = 0
    for episode in outcome.get("episodes", []):
        if episode.get("status") != "success":
            break
        horizon += 1
    return outcome.get("status"), {"run_id": runs[-1].name, "attempted": len(episodes),
                                   "horizon": horizon,
                                   "seconds": round(sum((e.get("metrics") or {}).get("elapsed_seconds") or 0
                                                        for e in episodes))}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--from", dest="start", type=int, required=True)
    parser.add_argument("--to", dest="end", type=int, required=True)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    frozen = json.loads((STAGE / "preflight.json").read_text())
    if not frozen.get("ready"):
        raise SystemExit("preflight is not ready")
    config = frozen["config"]
    helper = config["helper_model"]
    expected = {m["id"]: m["runtime_context_tokens"] for m in config["local_models"]}
    schedule = frozen["schedule"]
    journal = STAGE / "campaign.log"

    cells = [c for c in schedule if args.start <= c["schedule_index"] <= args.end]
    log(f"campaign cells {args.start}-{args.end} ({len(cells)}) "
        f"tiers {sorted({c['tier'] for c in cells})} dry_run={args.dry_run}", journal)

    consecutive_infrastructure = 0
    for cell in cells:
        index = cell["schedule_index"]
        store = STAGE / f"store-cell-{index:02d}"
        label = (f"cell {index:2d} {cell['tier']} rep{cell['repetition']} "
                 f"{cell['scenario_id']:26} {cell['backend']}")
        if store.exists():
            status, detail = classify(store)
            if status and status != "no-run":
                log(f"{label} -> SKIP, already {status} {detail}", journal)
                continue
        if args.dry_run:
            need = [cell["model"]] + ([helper] if cell["condition"] != "without" else [])
            log(f"{label} -> would need {need}", journal)
            continue
        log(label, journal)
        prepare(dict(cell, helper=helper), expected, journal)
        started = time.monotonic()
        with (STAGE / f"cell-{index:02d}.stdout").open("wb") as out, \
             (STAGE / f"cell-{index:02d}.stderr").open("wb") as err:
            code = subprocess.run(
                [str(PYTHON), "-m", "bench.harness_study", "run", str(STAGE / "preflight.json"),
                 "--cell", str(index), "--store", str(store)],
                cwd=REPO, stdout=out, stderr=err, stdin=subprocess.DEVNULL).returncode
        status, detail = classify(store)
        log(f"  -> {status} {detail} exit={code} wall={round(time.monotonic()-started)}s", journal)
        with (STAGE / "campaign.jsonl").open("a") as stream:
            stream.write(json.dumps({"cell": index, "tier": cell["tier"],
                                     "repetition": cell["repetition"],
                                     "scenario": cell["scenario_id"], "backend": cell["backend"],
                                     "status": status, "detail": detail, "exit": code,
                                     "at": stamp()}) + "\n")
        if status in ("infrastructure_failure", "preflight_failure", "no-run"):
            consecutive_infrastructure += 1
            if consecutive_infrastructure >= 2:
                log("stopping: two consecutive infrastructure failures", journal)
                return 1
        else:
            consecutive_infrastructure = 0
    log("range complete", journal)
    return 0


if __name__ == "__main__":
    sys.exit(main())
