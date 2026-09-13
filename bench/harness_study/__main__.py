"""python3 -m bench.harness_study --help"""
import argparse
import json
from pathlib import Path
import tarfile
import sys

from .artifacts import ArtifactStore, canonical_json
from .binaries import REPO, build_and_freeze
from .config import approval_payload, development_config, evolution_config, preflight, template
from . import evolution
from .grading import record_review, report
from .validation import validate_fixtures

DEFAULT_STORE = REPO / "target/harness-study/evidence"


def write_new(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as stream:
        stream.write(canonical_json(value))


def main(argv=None):
    parser = argparse.ArgumentParser(description="Reproducible MOOSEDev harness pilot; gold approval precedes all model runs.")
    sub = parser.add_subparsers(dest="command", required=True)
    command = sub.add_parser("init-config", help="write a machine-specific configuration template")
    command.add_argument("output", type=Path)
    command = sub.add_parser("init-development", help="derive six local harness diagnostic cells from an approved pilot")
    command.add_argument("parent_preflight", type=Path)
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--study-id", required=True)
    command.add_argument("--output", type=Path, required=True)
    command = sub.add_parser("init-evolution", help="derive a frozen six-, twelve- or eighteen-cell harness evolution or symbolic-baseline config")
    command.add_argument("parent_preflight", type=Path)
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--stage", choices=evolution.MODES, required=True)
    command.add_argument("--study-id", required=True)
    command.add_argument("--output", type=Path, required=True)
    command = sub.add_parser("build", help="build/freeze only this checkout's release binaries")
    command.add_argument("--indexer-manifest", type=Path)
    command = sub.add_parser("validate", help="execute reference and negative fixtures; no model calls")
    command.add_argument("--output", type=Path, required=True)
    command.add_argument("--scenarios", nargs="+")
    command = sub.add_parser("approve-gold", help="record an actual human's approval of current scenario hashes")
    command.add_argument("--reviewer", required=True)
    command.add_argument("--config", type=Path, help="bind the approved intent design and selected scenario set")
    command.add_argument("--output", type=Path, required=True)
    command = sub.add_parser("preflight", help="inventory/fingerprint; Stage 2 also runs neutral native response probes")
    command.add_argument("config", type=Path)
    command.add_argument("--output", type=Path, required=True)
    for name in ("report", "regrade", "export", "usage-report"):
        command = sub.add_parser(name, help="offline evidence operation; never calls models")
        command.add_argument("--store", type=Path, default=DEFAULT_STORE)
        command.add_argument("--output", type=Path, required=True)
    command = sub.add_parser("review", help="append an evidence-bound semantic judgment")
    command.add_argument("run_id")
    command.add_argument("judgment", type=Path)
    command.add_argument("--store", type=Path, default=DEFAULT_STORE)
    command = sub.add_parser("run", help="run one frozen schedule cell; new ID on every attempt")
    command.add_argument("preflight", type=Path)
    command.add_argument("--cell", type=int, required=True)
    command.add_argument("--replacement-for")
    command.add_argument("--store", type=Path, default=DEFAULT_STORE)
    args = parser.parse_args(argv)
    if args.command == "init-config":
        write_new(args.output, template())
        result = {"config": str(args.output)}
    elif args.command == "init-development":
        result = development_config(json.loads(args.parent_preflight.read_text()), args.binary_manifest, args.study_id)
        write_new(args.output, result)
    elif args.command == "init-evolution":
        result = evolution_config(json.loads(args.parent_preflight.read_text()), args.binary_manifest,
                                  args.study_id, args.stage)
        write_new(args.output, result)
    elif args.command == "build":
        build = build_and_freeze(indexer_manifest=args.indexer_manifest)
        result = {"build_id": build["build_id"], "manifest": str(Path(build["directory"]) / "manifest.json")}
    elif args.command == "validate":
        result = validate_fixtures(args.scenarios)
        write_new(args.output, result)
        print(json.dumps({"passed": result["passed"], "cases": result["case_count"], "evidence": str(args.output)}))
        return 0 if result["passed"] else 1
    elif args.command == "approve-gold":
        result = approval_payload(args.reviewer, config=json.loads(args.config.read_text()) if args.config else None)
        write_new(args.output, result)
    elif args.command == "preflight":
        result = preflight(json.loads(args.config.read_text()))
        write_new(args.output, result)
        print(json.dumps({"ready": result["ready"], "checks": result["checks"], "evidence": str(args.output)}))
        return 0 if result["ready"] else 1
    elif args.command in {"report", "regrade"}:
        result = report(args.store.resolve())
        write_new(args.output, result)
    elif args.command == "usage-report":
        from .usage import report_store
        result = report_store(args.store)
        write_new(args.output, result)
    elif args.command == "review":
        result = {"review": str(record_review(args.store.resolve(), args.run_id, json.loads(args.judgment.read_text())))}
    elif args.command == "export":
        store = ArtifactStore(args.store.resolve())
        report(store.root)  # Retains invalid/unsealed attempts in the export as well.
        if args.output.resolve().is_relative_to(store.root):
            raise ValueError("export destination must be outside the source store")
        with tarfile.open(args.output, "x:gz") as archive:
            archive.add(store.root, arcname="evidence", recursive=True)
        result = {"archive": str(args.output), "release_status": "private; separate publication review required"}
    else:
        from .run import run_cell
        frozen = json.loads(args.preflight.read_text())
        if args.cell < 0 or args.cell >= len(frozen["schedule"]):
            raise ValueError("cell index is outside the frozen schedule")
        result = run_cell(args.store, frozen, frozen["schedule"][args.cell], replacement_for=args.replacement_for)
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 1 if args.command == "run" and result["status"] != "success" else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, RuntimeError) as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        sys.exit(1)
