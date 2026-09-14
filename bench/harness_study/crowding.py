"""Crowded-graph probe: an offline crowding gate and delivery report. Never calls a model.

The gate seeds a probe package into a disposable workspace with the frozen
daemon and indexer, asks the harness context route exactly what a run would
ask, and reports whether the deciding record's claim, title or IRI reaches the
coding model, plus its search rank. The report measures the same question from
a finished field-check run's evidence.
"""
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import threading

from .artifacts import canonical_json
from .scenario import SCENARIOS, load_scenario, starts_empty, tree_manifest
from .seed import episode_prompt, prepare_workspace, seed_graph, seed_iri
from .validation import test_results

HEADER = re.compile(r"^\[(?P<kind>[A-Za-z]+)\] (?P<label>.*) \((?P<iri>https?://[^\s()]+)\)$")
BARE_IRI = re.compile(r"^https?://\S+$")
INVENTORY_START = "Current knowledge inventory:"
EVIDENCE_START = "Topic evidence ("
PLAN_EVIDENCE_START = "Plan evidence ("
MIN_RANK = 16
PLAN_TOPIC_BYTES = 4000
BUDGET_BYTES = 84_992
EDIT_TOOLS = {"edit", "write", "patch", "multiedit"}
LEAKS = ("claim_anywhere", "topic_evidence", "walk", "dossier_title", "policy_reason")


def split_context(context):
    """Inventory names and evidence records (with walk flags) of a harness context text."""
    inventory, evidence, current = [], [], None
    section = "preamble" if INVENTORY_START in context else "evidence"
    for line in context.splitlines():
        if line.startswith(INVENTORY_START):
            section = "inventory"
            continue
        if line.startswith(EVIDENCE_START) or line.startswith(PLAN_EVIDENCE_START):
            section, current = "evidence", None
            continue
        match = HEADER.match(line)
        if section == "inventory":
            if match:
                inventory.append(match.groupdict())
        elif section == "evidence":
            if match:
                current = dict(match.groupdict(), lines=[], walked=False)
                evidence.append(current)
            elif current is not None and line:
                current["lines"].append(line)
                current["walked"] = current["walked"] or line.startswith("linkedVia: ")
    return {"inventory": inventory, "evidence": evidence}


def membership(response, *, iri, title, claim):
    context = response.get("context", "")
    parsed = split_context(context)
    files = response.get("files") or []
    dossiers = "\n".join(item.get("dossier", "") for item in files)
    policies = "\n".join(json.dumps(item.get("policy"), ensure_ascii=False) for item in files)
    return {"inventory": any(item["iri"] == iri for item in parsed["inventory"]),
            "topic_evidence": any(item["iri"] == iri and not item["walked"] for item in parsed["evidence"]),
            "walk": any(item["iri"] == iri and item["walked"] for item in parsed["evidence"]),
            "dossier_title": title in dossiers or iri in dossiers,
            "policy_reason": title in policies or iri in policies,
            "claim_anywhere": claim in "\n".join((context, dossiers, policies))}


def parse_ranking(text):
    """Items of a get_relevant_context reply in order; each item's own IRI is its bare-IRI line."""
    items, current = [], None
    for line in text.splitlines():
        if line.startswith("• "):
            current = {"iri": None, "walked": False}
            items.append(current)
            continue
        if current is None:
            continue
        stripped = line.strip()
        if stripped.startswith("linkedVia:"):
            current["walked"] = True
        elif BARE_IRI.match(stripped):
            current["iri"] = stripped
    return [item for item in items if item["iri"]]


def rank_of(items, iri):
    """1-based search rank among records the search itself returned (walked records are not ranked)."""
    ranked = [item["iri"] for item in items if not item["walked"]]
    return ranked.index(iri) + 1 if iri in ranked else None


def cross_check(items, evidence):
    top = [record["iri"] for record in evidence if not record["walked"]]
    ranked = [item["iri"] for item in items if not item["walked"]][:len(top)]
    return bool(top) and set(top) == set(ranked)


def plan_topic(plan):
    topic = (plan["summary"].strip() + "\n" + " ".join(plan["files"])).strip()
    return topic.encode()[:PLAN_TOPIC_BYTES].decode(errors="ignore")


def v1_verdict(memberships, ranks, min_rank=MIN_RANK):
    failures, notes = [], []
    for entry in memberships:
        failures.extend(f"{entry['topic']} / {entry['files']}: {key}" for key in LEAKS if entry["membership"].get(key))
    for topic, value in sorted(ranks.items()):
        if not value.get("cross_check"):
            notes.append(f"{topic}: rank unavailable (cross-check failed); verdict rests on membership")
        elif value.get("rank") is not None and value["rank"] < min_rank:
            failures.append(f"{topic}: search rank {value['rank']} is below {min_rank}")
    return {"passed": not failures, "failures": failures, "notes": notes, "min_rank": min_rank}


def v2_verdict(plan_results, required=2):
    reached = sum(1 for result in plan_results if result.get("reaches"))
    return {"passed": bool(plan_results) and reached >= required and reached * 2 > len(plan_results),
            "reached": reached, "plans": len(plan_results), "required": required}


def first_sentence(text):
    return re.split(r"(?<=\.)\s", text.strip(), maxsplit=1)[0]


def _segments(prompt):
    markers = (("instructions", "You are the coding sensor"), ("conversation", "Recent conversation"),
               ("navigation", "Repository paths"), ("knowledge", "Current accepted knowledge:"),
               ("dossiers", "Entity dossiers:"), ("state", "Current harness state"),
               ("observations", "Recent observations"), ("observations", "Last result:"))
    found = sorted((position, name) for name, marker in markers
                   for position in [prompt.find(marker)] if position >= 0)
    segments = []
    if not found or found[0][0] > 0:
        segments.append(("preamble", prompt[:found[0][0]] if found else prompt))
    for index, (position, name) in enumerate(found):
        end = found[index + 1][0] if index + 1 < len(found) else len(prompt)
        segments.append((name, prompt[position:end]))
    refined = []
    for name, text in segments:
        if name != "knowledge":
            refined.append((name, text))
            continue
        inventory, evidence, plan = text.find(INVENTORY_START), text.find(EVIDENCE_START), text.find(PLAN_EVIDENCE_START)
        cuts = sorted((position, label) for position, label in ((inventory, "inventory"), (evidence, "topic_evidence"),
                                                                  (plan, "plan_evidence")) if position >= 0)
        refined.append(("knowledge", text[:cuts[0][0]] if cuts else text))
        for index, (position, label) in enumerate(cuts):
            end = cuts[index + 1][0] if index + 1 < len(cuts) else len(text)
            part = text[position:end]
            if label != "inventory":
                walked = "\n".join("\n".join(record["lines"]) for record in split_context(part)["evidence"]
                                   if record["walked"])
                refined.append(("walk", walked))
            refined.append((label, part))
    return refined


def locate(prompt, *, iri, title, claim):
    segments = _segments(prompt)

    def where(needle):
        walked = any(name == "walk" and needle in text for name, text in segments)
        names = []
        for name, text in segments:
            if needle in text and name not in names and not (walked and name in ("topic_evidence", "plan_evidence")):
                names.append(name)
        return names
    return {"claim_sections": where(claim), "title_sections": where(title), "iri_sections": where(iri)}


def _events(run):
    with (Path(run) / "events.jsonl").open("rb") as stream:
        for line in stream:
            try:
                yield json.loads(line)
            except ValueError:
                continue


def _stdout_values(run):
    for event in _events(run):
        if event.get("channel") != "stdout":
            continue
        try:
            yield event.get("sequence"), json.loads(event["payload"]["text"])
        except (KeyError, TypeError, ValueError):
            continue


def probe_results(run, probes):
    outcome = json.loads((Path(run) / "outcome.json").read_text())
    attempted = [episode for episode in outcome.get("episodes", []) if episode.get("status") != "unattempted"]
    checks = attempted[0].get("checks") if attempted else []
    observed = test_results(checks[0].get("stderr", "") if checks else "")
    return {probe["id"]: observed.get(probe["test"], "not run") for probe in probes}


def harness_report(run, *, iri, title, claim, probes):
    states = [(sequence, value["task"]) for sequence, value in _stdout_values(run)
              if value.get("type") == "state" and value.get("task")]
    final = states[-1][1] if states else {}
    first = next(((sequence, task) for sequence, task in states if task.get("edits")), None)
    requests = final.get("model_requests", [])
    before = len(first[1].get("model_requests", [])) if first else len(requests)
    locations = [dict(locate(item.get("prompt", ""), iri=iri, title=title, claim=claim),
                      index=index, purpose=item.get("purpose")) for index, item in enumerate(requests)]
    messages = [event.get("message", "") for event in final.get("events", [])]
    plans = []
    for message in messages:
        if message.startswith("Proposed plan: "):
            try:
                plan = json.loads(message[len("Proposed plan: "):])
            except ValueError:
                continue
            entry = {"summary": plan.get("summary", ""), "files": plan.get("files", [])}
            if entry not in plans:
                plans.append(entry)
    intents = final.get("intent_events", [])
    return {"arm": "harness", "first_edit_sequence": first[0] if first else None,
            "requests": len(requests), "requests_before_first_edit": before,
            "claim_before_first_edit": any(item["claim_sections"] for item in locations[:before]),
            "claim_after_first_edit": any(item["claim_sections"] for item in locations[before:]),
            "title_before_first_edit": any(item["title_sections"] for item in locations[:before]),
            "request_locations": locations,
            "searches": [event.get("detail") or event.get("message") for event in intents
                         if event.get("kind") == "knowledge_search"],
            "search_returned_record": any(message.startswith("Accepted project knowledge for") and iri in message
                                          for message in messages),
            "plan_recall": [event.get("detail") for event in intents if event.get("kind") == "plan_recall"],
            "reads": [message.split(":", 1)[0][len("Read "):] for message in messages if message.startswith("Read ")],
            "plans": plans, "probes": probe_results(run, probes)}


def native_report(run, *, claim, probes):
    tools = []
    for sequence, value in _stdout_values(run):
        if value.get("type") == "tool_use":
            part = value.get("part") or {}
            state = part.get("state") or {}
            tools.append({"sequence": sequence, "tool": part.get("tool"), "input": state.get("input") or {},
                          "output": state.get("output") if isinstance(state.get("output"), str)
                          else json.dumps(state.get("output"))})
    first = next((tool["sequence"] for tool in tools if tool["tool"] in EDIT_TOOLS), None)
    earlier = [tool for tool in tools if first is None or tool["sequence"] < first]
    notes = [tool for tool in earlier if "PROJECT_NOTES.md" in json.dumps(tool["input"])]
    bodies = [event for event in _events(run) if event.get("channel") == "model"
              and isinstance(event.get("payload", {}).get("body"), dict)
              and (first is None or event.get("sequence", 0) < first)]
    return {"arm": "native", "first_edit_sequence": first,
            "tool_counts": {name: sum(1 for tool in tools if tool["tool"] == name) for name in sorted({t["tool"] for t in tools})},
            "notes_read_before_first_edit": bool(notes),
            "claim_before_first_edit": any(claim in (tool["output"] or "") for tool in earlier)
                                       or any(claim in json.dumps(event["payload"]["body"], ensure_ascii=False) for event in bodies),
            "probes": probe_results(run, probes)}


def _deciding(scenario, fact_id):
    fact = next(item for item in scenario["initial_facts"] if item["id"] == fact_id)
    return seed_iri(scenario["id"] + "/fact/" + fact_id), fact["title"], first_sentence(fact["description"])


def report(runs, *, scenario_id, deciding_fact):
    scenario = load_scenario(scenario_id)
    iri, title, claim = _deciding(scenario, deciding_fact)
    probes = scenario["episodes"][0]["probes"]
    results = []
    for run in runs:
        manifest = json.loads((Path(run) / "manifest.json").read_text())
        if manifest.get("backend") == "harness":
            entry = harness_report(run, iri=iri, title=title, claim=claim, probes=probes)
        else:
            entry = native_report(run, claim=claim, probes=probes)
        results.append(dict(entry, run=str(run), model=manifest.get("model"), backend=manifest.get("backend")))
    return {"schema_version": 1, "scenario_id": scenario_id, "package_sha256": scenario["package_sha256"],
            "deciding_fact": deciding_fact, "iri": iri, "title": title, "claim": claim, "runs": results}


def mcp_relevant_context(executable, socket, data_dir, topic, limit=100, timeout=120):
    """One get_relevant_context call through `moosedev --connect`, never auto-spawning a backend."""
    environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "MOOSEDEV_NO_AUTOSPAWN": "1",
                   "MOOSEDEV_SOCKET": str(socket), "MOOSEDEV_DATA_DIR": str(data_dir), "HOME": str(data_dir)}
    process = subprocess.Popen([str(executable), "--connect", str(socket)], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
    timer = threading.Timer(timeout, process.kill)
    timer.start()
    try:
        def send(message):
            process.stdin.write((json.dumps(message) + "\n").encode())
            process.stdin.flush()

        def receive(identifier):
            while True:
                line = process.stdout.readline()
                if not line:
                    raise RuntimeError("MCP connection closed: " + process.stderr.read().decode(errors="replace")[-2000:])
                message = json.loads(line)
                if message.get("id") == identifier:
                    if "error" in message:
                        raise RuntimeError(f"MCP error: {message['error']}")
                    return message["result"]
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
              "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                         "clientInfo": {"name": "crowding-gate", "version": "1"}}})
        receive(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": {"name": "get_relevant_context", "arguments": {"topic": topic, "limit": limit}}})
        result = receive(2)
        return "\n".join(item.get("text", "") for item in result.get("content", []))
    finally:
        timer.cancel()
        process.kill()
        process.wait()


def gate(*, scenario_id, binary_manifest, parent_preflight, output, plans=(), deciding_fact):
    """Seed the package with the frozen daemon and measure today's push, ranks and plan-topic reach."""
    from .binaries import verify_binaries
    from .daemon import OwnedDaemon
    from .indexing import apply_overlay, index_workspace, ready_dossiers, short_probe_runtime, verify_indexer
    scenario = load_scenario(scenario_id)
    iri, title, claim = _deciding(scenario, deciding_fact)
    binaries = verify_binaries(Path(binary_manifest))
    parent = json.loads(Path(parent_preflight).read_text())
    indexer = verify_indexer(parent["indexer"])
    assets = parent["assets"]
    directory = Path(output).absolute()
    directory.mkdir(parents=True)
    (directory / "responses").mkdir()
    workspace = directory / "workspace"
    shutil.copytree(SCENARIOS / scenario_id / "project", workspace)
    subprocess.run(["/usr/bin/git", "init", "-q", str(workspace)], check=True, capture_output=True)
    prepare_workspace(workspace, scenario, "harness")
    apply_overlay(workspace)
    project_files = sorted(tree_manifest(SCENARIOS / scenario_id / "project"))
    episode = scenario["episodes"][0]
    topics = {"objective": episode_prompt(episode, "harness").strip(), "bare": episode["prompt"].strip()}
    file_sets = {"none": [], "fees.py": ["fees.py"], "all": project_files}
    plans = [{"summary": plan["summary"], "files": list(plan["files"])} for plan in plans]
    result = {"schema_version": 1, "scenario_id": scenario_id, "package_sha256": scenario["package_sha256"],
              "gold_sha256": scenario["gold_sha256"],
              "seed_graph_sha256": hashlib.sha256(seed_graph(scenario).encode()).hexdigest(),
              "build_id": binaries["build_id"], "deciding_fact": deciding_fact, "iri": iri, "title": title,
              "claim": claim, "topics": topics, "file_sets": file_sets, "min_rank": MIN_RANK,
              "budget_bytes": BUDGET_BYTES}

    def save(name, value):
        (directory / "responses" / f"{name}.json").write_bytes(canonical_json(value))
        return value

    result["index"] = index_workspace(indexer, binaries["binaries"]["daemon"], workspace, directory / "index-runtime")
    with short_probe_runtime(directory / "daemon-runtime") as runtime:
        with OwnedDaemon(executable=Path(binaries["binaries"]["daemon"]), expected_sha256=binaries["binary_hashes"]["daemon"],
                         workspace=workspace, runtime=runtime, assets=Path(assets["directory"]),
                         helper_model="unused-no-inference", helper_endpoint="http://127.0.0.1:9/v1",
                         log_path=directory / "daemon.log", indexer=indexer) as daemon:
            readiness = ready_dossiers(daemon, scenario, seed=True, require_empty=starts_empty(scenario))
            result["readiness"] = {"conforms": [op["reviewed"].get("conforms") for op in readiness["seed_operations"]],
                                   "bindings": sum(len(op["request"]["bindings"]) for op in readiness["seed_operations"])}
            memberships = []
            for topic_name, topic in topics.items():
                for set_name, files in file_sets.items():
                    response = save(f"{topic_name}-{set_name}",
                                    daemon._request("/api/v1/harness/context", {"topic": topic, "files": files}))
                    memberships.append({"topic": topic_name, "files": set_name,
                                        "membership": membership(response, iri=iri, title=title, claim=claim),
                                        "bytes": {"context": len(response.get("context", "").encode()),
                                                  "files": len(json.dumps(response.get("files", [])).encode())}})
            evidence = {name: save(f"{name}-evidence", daemon._request(
                "/api/v1/harness/context", {"topic": topic, "files": [], "evidence_only": True}))
                for name, topic in topics.items()}
            plan_results = []
            for number, plan in enumerate(plans):
                topic = plan_topic(plan)
                response = save(f"plan-{number}-evidence", daemon._request(
                    "/api/v1/harness/context", {"topic": topic, "files": [], "evidence_only": True}))
                evidence[f"plan-{number}"] = response
                member = membership(response, iri=iri, title=title, claim=claim)
                plan_results.append({"plan": plan, "topic": topic, "membership": member,
                                     "reaches": member["topic_evidence"] or member["walk"]})
            ranks = {}
            ranked_topics = dict(topics, **{f"plan-{number}": plan_topic(plan) for number, plan in enumerate(plans)})
            for name, topic in ranked_topics.items():
                text = mcp_relevant_context(Path(binaries["binaries"]["daemon"]), daemon.socket,
                                            getattr(daemon, "data", workspace / ".moosedev"), topic)
                (directory / "responses" / f"{name}-rank.txt").write_text(text)
                items = parse_ranking(text)
                ranks[name] = {"rank": rank_of(items, iri), "items": len(items),
                               "cross_check": cross_check(items, split_context(evidence[name].get("context", ""))["evidence"])}
            repeat = save("objective-none-repeat", daemon._request(
                "/api/v1/harness/context", {"topic": topics["objective"], "files": []}))
            first = json.loads((directory / "responses" / "objective-none.json").read_text())
            result["repeat_unchanged"] = (split_context(repeat.get("context", "")) == split_context(first.get("context", "")))
            result["checkpoint"] = daemon.checkpoint()
    result["memberships"] = memberships
    result["ranks"] = ranks
    result["plan_results"] = plan_results
    result["largest_push_bytes"] = max(entry["bytes"]["context"] + entry["bytes"]["files"] for entry in memberships)
    result["v1"] = v1_verdict(memberships, {name: ranks[name] for name in topics})
    if not result["repeat_unchanged"]:
        result["v1"]["notes"].append("the repeated objective query differed after rank queries")
    result["v2"] = v2_verdict(plan_results) if plans else None
    (directory / "gate.json").write_bytes(canonical_json(result))
    return result
