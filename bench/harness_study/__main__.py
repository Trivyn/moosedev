"""python3 -m bench.harness_study --help"""
import argparse
import json
from pathlib import Path
import tarfile
import sys

from .artifacts import ArtifactStore, canonical_json
from .binaries import REPO, build_and_freeze
from .config import approval_payload, development_config, evolution_config, field_check_config, preflight, template
from . import evolution, field_check, intent, model_table
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
    command = sub.add_parser("init-field-check", help="derive an exploratory, never-scored field check of model-table models on reviewed packages")
    command.add_argument("parent_preflight", type=Path)
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--study-id", required=True)
    command.add_argument("--models", nargs="+", choices=list(model_table.MODELS), required=True)
    command.add_argument("--scenarios", nargs="+", choices=list(field_check.SCENARIOS), required=True)
    command.add_argument("--approval", type=Path, required=True,
                         help="where the human field-check approval will be written; it need not exist yet")
    command.add_argument("--output", type=Path, required=True)
    command = sub.add_parser("crowding-gate", help="seed a probe package with the frozen daemon and measure today's push; no model calls")
    command.add_argument("--scenario", default="late_fees_crowded")
    command.add_argument("--deciding-fact", default="fees-np7")
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--parent-preflight", type=Path, required=True, help="ready preflight supplying the indexer and assets")
    command.add_argument("--plans", type=Path, help="JSON list of {summary, files} plans whose topics are measured")
    command.add_argument("--output", type=Path, required=True, help="new directory for the gate evidence")
    command = sub.add_parser("crowding-levers", help="diagnostic only: what dossier claims and read-source recall would deliver; no model calls")
    command.add_argument("--scenario", default="late_fees_crowded")
    command.add_argument("--deciding-fact", default="fees-np7")
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--parent-preflight", type=Path, required=True, help="ready preflight supplying the indexer and assets")
    command.add_argument("--plans", type=Path, help="JSON list of {summary, files} plans whose files form extra read sets")
    command.add_argument("--extra-seeds", type=Path, help="diagnostic seeds appended in memory only; the package is never changed")
    command.add_argument("--output", type=Path, required=True, help="new directory for the diagnostic evidence")
    command = sub.add_parser("crowding-tiers", help="diagnostic only: what structural, NLQ and small-k tiers would deliver; "
                                                   "no model calls unless LM Studio already has the helper loaded")
    command.add_argument("--scenario", default="late_fees_crowded")
    command.add_argument("--deciding-fact", default="fees-np7")
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--parent-preflight", type=Path, required=True, help="ready preflight supplying the indexer and assets")
    command.add_argument("--plans", type=Path, required=True, help="JSON list of {name, summary, files} plans")
    command.add_argument("--templates", nargs="+", default=["dotted"], choices=["dotted", "label", "component"],
                         help="tier-2 question templates, one NLQ call per plan per template")
    command.add_argument("--lmstudio", default="http://127.0.0.1:1234",
                         help="LM Studio server; tier 2 runs only if the helper is already loaded there (never loads models)")
    command.add_argument("--output", type=Path, required=True, help="new directory for the diagnostic evidence")
    command = sub.add_parser("intercept-diagnostics", help="diagnostic only: model actions as knowledge queries "
                                                           "(action census, pattern lookup, edit- and read-time grounding); no model calls")
    command.add_argument("--scenario", default="late_fees_crowded")
    command.add_argument("--deciding-fact", default="fees-np7")
    command.add_argument("--binary-manifest", type=Path, required=True)
    command.add_argument("--parent-preflight", type=Path, required=True, help="ready preflight supplying the indexer and assets")
    command.add_argument("--roots", nargs="+", default=None, help="study evidence roots for the action census")
    command.add_argument("--crowded-roots", nargs="+", default=None, help="crowded probe field-check roots")
    command.add_argument("--output", type=Path, required=True, help="new directory for the diagnostic evidence")
    command = sub.add_parser("crowding-report", help="offline delivery report over field-check run directories; no model calls")
    command.add_argument("runs", type=Path, nargs="+")
    command.add_argument("--scenario", default="late_fees_crowded")
    command.add_argument("--deciding-fact", default="fees-np7")
    command.add_argument("--output", type=Path, required=True)
    command = sub.add_parser("build", help="build/freeze only this checkout's release binaries")
    command.add_argument("--indexer-manifest", type=Path)
    command = sub.add_parser("validate", help="execute reference and negative fixtures; no model calls")
    command.add_argument("--output", type=Path, required=True)
    command.add_argument("--scenarios", nargs="+")
    command = sub.add_parser("approve-gold", help="record an actual human's approval of current scenario hashes")
    command.add_argument("--reviewer", required=True)
    command.add_argument("--config", type=Path, help="bind the intent or field-check design and its selected scenario set")
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
    elif args.command == "init-field-check":
        result = field_check_config(json.loads(args.parent_preflight.read_text()), args.binary_manifest,
                                    args.study_id, args.models, args.scenarios, args.approval)
        write_new(args.output, result)
    elif args.command == "crowding-gate":
        from .crowding import gate
        plans = json.loads(args.plans.read_text()) if args.plans else []
        result = gate(scenario_id=args.scenario, binary_manifest=args.binary_manifest,
                      parent_preflight=args.parent_preflight, output=args.output, plans=plans,
                      deciding_fact=args.deciding_fact)
        print(json.dumps({"v1": result["v1"], "v2": result["v2"], "ranks": result["ranks"],
                          "evidence": str(args.output / "gate.json")}))
        return 0
    elif args.command == "crowding-levers":
        from .crowding import lever_diagnostics
        plans = json.loads(args.plans.read_text()) if args.plans else []
        extra = json.loads(args.extra_seeds.read_text()) if args.extra_seeds else []
        result = lever_diagnostics(scenario_id=args.scenario, binary_manifest=args.binary_manifest,
                                   parent_preflight=args.parent_preflight, output=args.output, plans=plans,
                                   deciding_fact=args.deciding_fact, extra_facts=extra)
        print(json.dumps({"evidence": str(args.output / "levers.json")}))
        return 0
    elif args.command == "crowding-tiers":
        from .crowding import tier_diagnostics, tier_summary
        result = tier_diagnostics(scenario_id=args.scenario, binary_manifest=args.binary_manifest,
                                  parent_preflight=args.parent_preflight, output=args.output,
                                  plans=json.loads(args.plans.read_text()), deciding_fact=args.deciding_fact,
                                  lmstudio=args.lmstudio, templates=args.templates)
        print(json.dumps({"helper": result["helper"], "summary": tier_summary(result),
                          "evidence": str(args.output / "tiers.json")}))
        return 0
    elif args.command == "intercept-diagnostics":
        from .intercept import DEFAULT_CROWDED_ROOTS, DEFAULT_ROOTS, intercept_diagnostics, summary
        result = intercept_diagnostics(roots=args.roots or list(DEFAULT_ROOTS),
                                       crowded_roots=args.crowded_roots or list(DEFAULT_CROWDED_ROOTS),
                                       scenario_id=args.scenario, binary_manifest=args.binary_manifest,
                                       parent_preflight=args.parent_preflight, output=args.output,
                                       deciding_fact=args.deciding_fact)
        print(json.dumps({"summary": summary(result), "evidence": str(args.output / "intercept.json")}))
        return 0
    elif args.command == "crowding-report":
        from .crowding import report as crowding_report
        result = crowding_report(args.runs, scenario_id=args.scenario, deciding_fact=args.deciding_fact)
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
