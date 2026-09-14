"""Diagnostics only: the model's own actions read as knowledge queries. Never calls a model.

Three offline measurements, none of which changes the harness, the gate or a probe package:

1. Action census over retained study evidence: the searches, greps, reads and edits models actually
   issue, and whether a search-like action came before the first edit.
2. Pattern lookup on the crowded late fees probe: what a precise-first lookup of those patterns would
   push from the frozen daemon (indexed definitions matched by name, then the dossiers and linked
   evidence of their files), next to evidence-only topic recall of the raw pattern.
3. Edit-time and read-time grounding: the indexed definitions a harness edit's attribute accesses, or a
   delivered claim's words, would surface from files the model had not read.
"""
import ast
from collections import Counter, defaultdict
import json
from pathlib import Path
import re
import shutil
import subprocess
import textwrap

from .artifacts import canonical_json
from .crowding import HEADER, LINKED_EVIDENCE_START, _deciding, _segments, delivery, fact_iris
from .scenario import SCENARIOS, load_scenario, starts_empty, tree_manifest
from .seed import episode_prompt, prepare_workspace

REPO = Path(__file__).resolve().parents[2]
DEFAULT_CROWDED_ROOTS = ("target/harness-crowded-probe-v1/before", "target/harness-crowded-probe-v1/after")
DEFAULT_ROOTS = DEFAULT_CROWDED_ROOTS + tuple(
    f"target/{name}/evidence" for name in (
        "harness-symbolic-baseline-v1", "harness-symbolic-baseline-v2", "harness-symbolic-baseline-v3",
        "harness-evolution-stage2-baseline-v1", "harness-evolution-stage2-baseline-v2",
        "harness-evolution-stage2-recovery-v1", "harness-evolution-stage2-recovery-v2",
        "harness-evolution-stage1", "harness-intent-pilot-v1")) + ("target/harness-study/evidence",)
PROBE_QUERIES = ("late_fee", "FeePolicy", "segment", "charity")
# The package README places account segments in accounts.py; the crowded probe's charity check depends on it.
EXPECTED_EDIT_GROUNDING = {"segment": ("accounts.py", "SEGMENTS")}
SEARCH_COMMAND = re.compile(r"(?:^|[\s;|&(`])(?:git\s+grep|grep|egrep|fgrep|rg|ag|ack|find)(?=\s|$)")
SEARCH_LIKE = ("search", "grep_command", "grep", "glob", "list", "graph_tool")
HARNESS_EDITS = ("edit", "replace", "write")
NATIVE_EDITS = ("edit", "write", "patch", "multiedit")
SKIP_ROLES = ("parameter", "type_parameter", "local")
MAX_TASK_JOURNAL_BYTES = 400 * 1024 * 1024
PATTERN_BYTES = 160
TOPIC_BYTES = 4000
STOP_TERMS = frozenset((
    "grep egrep fgrep rg ag ack find git name iname type head tail echo include exclude path and not the "
    "py python txt md json toml yaml ls cat sed awk xargs wc sort uniq file files dev null print exec "
    "maxdepth mindepth recursive pycache").split())
STOP_WORDS = frozenset((
    "a an and any are as at be been being but by can could each every for from had has have if in into is it "
    "its itself kind may more most must never no nor not of on once one only or other own per same should so "
    "some such than that the their them then there these they this those through to under until very was "
    "were what when where which while who why will with without would because allows allowed proposed "
    "rejected fee fees").split())


# ---------------------------------------------------------------------------
# 1. Action census
# ---------------------------------------------------------------------------

def run_dirs(root):
    """Run directories under a study root: `runs/*`, or field-check `store-cell-*/runs/*`."""
    root = Path(root)
    if (root / "runs").is_dir():
        return sorted(path for path in (root / "runs").iterdir() if path.is_dir())
    return sorted(path for path in root.glob("store-cell-*/runs/*") if path.is_dir())


def harness_actions(task):
    """(event index, kind, action) for each model action and harness read result in one task journal."""
    actions = []
    for index, event in enumerate(task.get("events", [])):
        message = event.get("message", "") if isinstance(event, dict) else str(event)
        if message.startswith("Model action: "):
            try:
                action = json.loads(message[len("Model action: "):])
            except ValueError:
                continue
            if isinstance(action, dict):
                actions.append((index, action.get("action"), action))
        elif message.startswith("Read ") and ":" in message:
            actions.append((index, "read_result", {"file": message[len("Read "):].split(":", 1)[0]}))
    return actions


def _command_text(value):
    return " ".join(value) if isinstance(value, list) else str(value or "")


def classify_harness(kind, action):
    """A harness action's census category and its query-like text."""
    if kind == "search":
        return "search", str(action.get("query", ""))
    if kind == "command":
        command = _command_text(action.get("command"))
        return ("grep_command" if SEARCH_COMMAND.search(command) else "command"), command
    if kind in HARNESS_EDITS:
        return "edit", str(action.get("file", ""))
    if kind in ("read", "read_result"):
        return "read", str(action.get("file", ""))
    if kind == "inspect":
        return "inspect", f"{action.get('event')}:{action.get('offset')}"
    if kind in ("plan", "replan"):
        return "plan", ""
    return "other", str(kind)


def classify_native(tool, arguments):
    """A native tool call's census category and its query-like text (OpenCode tool names)."""
    arguments = arguments if isinstance(arguments, dict) else {}
    if tool == "grep":
        include = arguments.get("include")
        return "grep", str(arguments.get("pattern", "")) + (f" include={include}" if include else "")
    if tool == "glob":
        return "glob", str(arguments.get("pattern", ""))
    if tool == "list":
        return "list", str(arguments.get("path", ""))
    if tool == "bash":
        command = _command_text(arguments.get("command"))
        return ("grep_command" if SEARCH_COMMAND.search(command) else "command"), command
    if tool == "read":
        return "read", str(arguments.get("filePath", ""))
    if tool in NATIVE_EDITS:
        return "edit", str(arguments.get("filePath", ""))
    return "other", str(tool)


def classify_codex(item):
    """A Codex exec item's census category: shell commands, MOOSEDev MCP calls and file changes."""
    kind = item.get("type")
    if kind == "command_execution":
        command = _command_text(item.get("command"))
        return ("grep_command" if SEARCH_COMMAND.search(command) else "command"), command
    if kind == "mcp_tool_call":
        return "graph_tool", f"{item.get('tool')} {json.dumps(item.get('arguments'), sort_keys=True)}"
    if kind == "file_change":
        return "edit", ""
    return "other", str(kind)


def _stdout_value(line):
    try:
        event = json.loads(line)
        if event.get("channel") != "stdout":
            return None
        value = json.loads(event["payload"]["text"])
        return value if isinstance(value, dict) else None
    except (KeyError, TypeError, ValueError):
        return None


def native_actions(run, backend):
    """(order, category, text) from a native run's events, streamed with a byte prefilter."""
    marker = b"tool_use" if backend == "opencode" else b"item.started"
    actions = []
    with (Path(run) / "events.jsonl").open("rb") as stream:
        for order, line in enumerate(stream):
            if marker not in line:
                continue
            value = _stdout_value(line)
            if value is None:
                continue
            if backend == "opencode" and value.get("type") == "tool_use":
                part = value.get("part") or {}
                category, text = classify_native(part.get("tool"), (part.get("state") or {}).get("input"))
            elif backend != "opencode" and value.get("type") == "item.started":
                category, text = classify_codex(value.get("item") or {})
            else:
                continue
            actions.append((order, category, text))
    return actions


def summarize_actions(actions):
    """Counts by category, search-like patterns, and whether a search-like action preceded the first edit.

    `actions` is one ordered sequence (a task journal or a native run) of (order, category, text).
    """
    counts = Counter(category for _, category, _ in actions)
    first_edit = min((order for order, category, _ in actions if category == "edit"), default=None)
    searches = [(order, category, text) for order, category, text in actions if category in SEARCH_LIKE]
    return {"counts": dict(counts), "edited": first_edit is not None,
            "patterns": [[category, text[:PATTERN_BYTES]] for _, category, text in searches],
            "search_before_first_edit": (any(order < first_edit for order, _, _ in searches)
                                         if first_edit is not None else bool(searches))}


def task_journals(run):
    return sorted(path for path in Path(run).glob("episodes/*/workspace/.moosedev/harness/tasks/*.json")
                  if not path.name.endswith(".usage.json"))


def run_census(run):
    """One sealed run's census row, or None with a skip reason."""
    run = Path(run)
    if not (run / "outcome.json").is_file() or not (run / "manifest.json").is_file():
        return None, "unsealed (no outcome.json or manifest.json)"
    try:
        manifest = json.loads((run / "manifest.json").read_text())
    except ValueError:
        return None, "unreadable manifest"
    row = {"run": str(run), "backend": manifest.get("backend"), "model": manifest.get("model"),
           "scenario": manifest.get("scenario_id"), "condition": manifest.get("condition"),
           "intent_policy": manifest.get("intent_policy")}
    if manifest.get("backend") == "harness":
        journals = task_journals(run)
        if journals:
            summaries, notes = [], []
            for journal in journals:
                if journal.stat().st_size > MAX_TASK_JOURNAL_BYTES:
                    notes.append(f"{journal.name}: task journal over {MAX_TASK_JOURNAL_BYTES} bytes, not read")
                    continue
                try:
                    task = json.loads(journal.read_bytes())
                except ValueError:
                    notes.append(f"{journal.name}: unreadable task journal")
                    continue
                # A read result is the harness answering a model read, not a second action.
                summaries.append(summarize_actions([(index, *classify_harness(kind, action))
                                                    for index, kind, action in harness_actions(task)
                                                    if kind != "read_result"]))
            if summaries:
                counts = Counter()
                for summary in summaries:
                    counts.update(summary["counts"])
                return dict(row, source="task_journal", tasks=len(summaries), notes=notes, counts=dict(counts),
                            edited=any(summary["edited"] for summary in summaries),
                            patterns=[pattern for summary in summaries for pattern in summary["patterns"]],
                            search_before_first_edit=any(summary["search_before_first_edit"] for summary in summaries)), None
        try:
            outcome = json.loads((run / "outcome.json").read_text())
        except ValueError:
            return None, "unreadable outcome"
        counts = Counter()
        for episode in outcome.get("episodes", []):
            counts.update((episode.get("intent_activity") or {}).get("actions_by_kind") or {})
        mapped = Counter()
        for kind, number in counts.items():
            mapped[classify_harness(kind, {})[0]] += number
        return dict(row, source="outcome_counts", tasks=0, notes=["no task journal; counts from outcome intent_activity"],
                    counts=dict(mapped), edited=mapped.get("edit", 0) > 0, patterns=[],
                    search_before_first_edit=None), None
    if not (run / "events.jsonl").is_file():
        return None, "no events.jsonl"
    summary = summarize_actions(native_actions(run, manifest.get("backend")))
    return dict(row, source="events", tasks=None, notes=[], **summary), None


def census(roots):
    """Census rows for every sealed run under the roots, plus the skipped runs and why."""
    rows, skipped = [], []
    for root in roots:
        for run in run_dirs(root):
            row, reason = run_census(run)
            if row is None:
                skipped.append({"run": str(run), "reason": reason})
            else:
                rows.append(dict(row, root=str(root)))
    return rows, skipped


def aggregate(rows, keys=("backend", "model")):
    """Per-group runs, runs with a search-like action, before-first-edit counts, category totals and top patterns."""
    groups = defaultdict(list)
    for row in rows:
        groups[tuple(row.get(key) for key in keys)].append(row)
    table = []
    for group, members in sorted(groups.items(), key=lambda item: tuple(str(value) for value in item[0])):
        counts, patterns = Counter(), Counter()
        for row in members:
            counts.update(row["counts"])
            patterns.update(f"{category}: {text[:80]}" for category, text in row["patterns"])
        searched = [row for row in members if any(row["counts"].get(category) for category in SEARCH_LIKE)]
        before = [row for row in members if row["search_before_first_edit"]]
        table.append(dict(zip(keys, group), runs=len(members), runs_with_search=len(searched),
                          runs_search_before_first_edit=len(before),
                          before_first_edit_fraction=round(len(before) / len(members), 3),
                          outcome_count_only=sum(1 for row in members if row["source"] == "outcome_counts"),
                          counts=dict(sorted(counts.items())), top_patterns=patterns.most_common(8)))
    return table


# ---------------------------------------------------------------------------
# Names, definitions and source
# ---------------------------------------------------------------------------

def pattern_terms(text):
    """Identifier-like terms of a search pattern or command, without shell words and flags."""
    terms = []
    for token in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text or ""):
        if len(token.strip("_")) < 3 or token.lower() in STOP_TERMS or token in terms:
            continue
        terms.append(token)
    return terms


def symbol_role(symbol):
    """The SCIP descriptor role of a symbol's last descriptor."""
    symbol = symbol or ""
    if symbol.startswith("local "):
        return "local"
    if symbol.endswith(")") and re.search(r"\([^()]*\)$", symbol) and not symbol.endswith("()"):
        return "parameter"
    if symbol.endswith("]"):
        return "type_parameter"
    if symbol.endswith(")."):
        return "method"
    if symbol.endswith("#"):
        return "type"
    if symbol.endswith("/"):
        return "namespace"
    if symbol.endswith("."):
        return "term"
    return "other"


def name_tokens(name):
    bare = name.rsplit(".", 1)[-1]
    return [part.lower() for part in re.findall(r"[A-Z]+(?![a-z])|[A-Z]?[a-z]+|\d+", bare.replace("_", " "))]


def term_forms(term):
    lowered = term.lower()
    forms = {lowered, lowered + "s", lowered + "es"}
    if lowered.endswith("es"):
        forms.add(lowered[:-2])
    if lowered.endswith("s"):
        forms.add(lowered[:-1])
    return forms


def name_matches(term, name):
    """A definition name matches a term exactly (plural or case aside), or, for a one-word term, by any name token."""
    forms = term_forms(term)
    bare = name.rsplit(".", 1)[-1]
    if bare.lower() in forms:
        return True
    compound = "_" in term.strip("_") or any(char.isupper() for char in term[1:])
    return not compound and any(token in forms for token in name_tokens(bare))


def normalize_definitions(entities):
    return [{"name": entity.get("name") or "", "file": entity.get("file") or "", "symbol": entity.get("symbol") or "",
             "role": symbol_role(entity.get("symbol")), "dossier_records": len(entity.get("dossier_records") or [])}
            for entity in entities]


def definition_source(text, name):
    """(line, source segment) of a module, class or function definition or an assignment to `name`, else None."""
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return None
    bare = name.rsplit(".", 1)[-1]
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.name == bare:
            return node.lineno, ast.get_source_segment(text, node) or ""
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if ((isinstance(target, ast.Name) and target.id == bare)
                        or (isinstance(target, ast.Attribute) and target.attr == bare)):
                    return node.lineno, ast.get_source_segment(text, node) or ""
    return None


def string_values(segment):
    """Every string constant in a source segment (dictionary keys and values included)."""
    try:
        tree = ast.parse(textwrap.dedent(segment or ""))
    except SyntaxError:
        return set()
    return {node.value for node in ast.walk(tree) if isinstance(node, ast.Constant) and isinstance(node.value, str)}


def pushed_definition(definition, sources, *, expected=None):
    located = definition_source(sources.get(definition["file"], ""), definition["name"])
    line, segment = located if located else (None, "")
    return {"name": definition["name"], "file": definition["file"], "role": definition["role"], "line": line,
            "preview_bytes": len(segment.encode()), "string_values": sorted(string_values(segment))[:24],
            "expected": expected == (definition["file"], definition["name"])}


# ---------------------------------------------------------------------------
# 3a. Edit-time grounding
# ---------------------------------------------------------------------------

def _literal_values(node, constants):
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return [node.value]
    if isinstance(node, ast.Name) and node.id in constants:
        return list(constants[node.id])
    if isinstance(node, ast.Attribute) and node.attr in constants:
        return list(constants[node.attr])
    if isinstance(node, (ast.Set, ast.Tuple, ast.List)):
        return [value for element in node.elts for value in _literal_values(element, constants)]
    return []


def edit_accesses(text):
    """Attribute accesses on function parameters in edited Python, each with the string literals compared to it.

    Returns (accesses, parsed): accesses maps attribute name to sorted literals; parsed is False when the
    edited text is not parseable Python (a partial replacement), in which case nothing is reported.
    """
    try:
        tree = ast.parse(textwrap.dedent(text or ""))
    except SyntaxError:
        return {}, False
    params, constants = set(), {}
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            params.update(arg.arg for arg in node.args.posonlyargs + node.args.args + node.args.kwonlyargs
                          if arg.arg not in ("self", "cls"))
        if isinstance(node, ast.Assign):
            values = _literal_values(node.value, {})
            if values:
                for target in node.targets:
                    if isinstance(target, ast.Name):
                        constants[target.id] = values
                    elif isinstance(target, ast.Attribute):
                        constants[target.attr] = values

    def parameter_attribute(node):
        return isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id in params

    accesses = {}
    for node in ast.walk(tree):
        if parameter_attribute(node):
            accesses.setdefault(node.attr, set())
    for node in ast.walk(tree):
        if not isinstance(node, ast.Compare):
            continue
        sides = [node.left, *node.comparators]
        attributes = [side.attr for side in sides if parameter_attribute(side)]
        for side in sides:
            if parameter_attribute(side):
                continue
            for literal in _literal_values(side, constants):
                for attribute in attributes:
                    accesses[attribute].add(literal)
    return {name: sorted(values) for name, values in sorted(accesses.items())}, True


def edit_text(kind, action):
    if kind == "edit":
        return action.get("after") or ""
    if kind == "replace":
        return action.get("new_text") or ""
    return action.get("content") or ""


def ground_edit(file, text, reads_before, definitions, sources, expected=EXPECTED_EDIT_GROUNDING):
    """What an edit-time lookup of each accessed attribute would push from unread files, with mismatch signals."""
    accesses, parsed = edit_accesses(text)
    rows = []
    for attribute, literals in accesses.items():
        target = expected.get(attribute)
        pushed = [pushed_definition(definition, sources, expected=target) for definition in definitions
                  if definition["role"] not in SKIP_ROLES and definition["file"] != file
                  and definition["file"] not in reads_before and name_matches(attribute, definition["name"])]
        valued = [item for item in pushed if item["string_values"]]
        rows.append({"attribute": attribute, "literals": literals, "pushed": pushed,
                     "segments_surfaced": any(item["name"] == "SEGMENTS" and item["file"] == "accounts.py"
                                              for item in pushed),
                     "literal_mismatch": {literal: bool(valued) and all(literal not in item["string_values"]
                                                                        for item in valued)
                                          for literal in literals},
                     "expected_target": list(target) if target else None,
                     "pushed_bytes": sum(item["preview_bytes"] for item in pushed),
                     "noise_bytes": sum(item["preview_bytes"] for item in pushed if not item["expected"])})
    return {"file": file, "parsed": parsed, "reads_before": sorted(reads_before), "attributes": rows}


def edit_grounding(task, definitions, sources):
    """Edit-time grounding for every harness edit, replace and write in one task journal."""
    reads, results = set(), []
    for index, kind, action in harness_actions(task):
        if kind in ("read", "read_result"):
            if kind == "read_result":
                reads.add(action.get("file", ""))
            continue
        if kind not in HARNESS_EDITS:
            continue
        file = action.get("file", "")
        result = ground_edit(file, edit_text(kind, action), set(reads), definitions, sources)
        results.append(dict(result, event=index, kind=kind))
        reads.add(file)
    return results


# ---------------------------------------------------------------------------
# 3b. Read-time grounding
# ---------------------------------------------------------------------------

def prompt_read_files(prompt):
    """Files whose current source a harness prompt carries."""
    marker = "Current source, refreshed before this action:\n"
    position = prompt.find(marker)
    if position < 0:
        return []
    line = prompt[position + len(marker):].split("\n", 1)[0]
    try:
        value = json.loads(line)
    except ValueError:
        return []
    return sorted(value) if isinstance(value, dict) else []


def linked_claims(prompt):
    """(iri, kind, title, description) of each record in a prompt's linked evidence section."""
    records, current = [], None
    for name, text in _segments(prompt):
        if name != "linked_evidence":
            continue
        for line in text.splitlines():
            match = HEADER.match(line)
            if match:
                current = {"iri": match["iri"], "kind": match["kind"], "title": match["label"], "description": ""}
                records.append(current)
            elif current is not None and line.startswith("hasDescription: "):
                current["description"] = line[len("hasDescription: "):]
    return records


def content_words(text):
    words = []
    for token in re.findall(r"[A-Za-z]+", text or ""):
        lowered = token.lower()
        if len(lowered) < 4 or lowered in STOP_WORDS or lowered in words:
            continue
        words.append(lowered)
    return words


def ground_claim(description, unread, definitions, *, expected_locations=()):
    """Definitions in unread files whose names match a delivered claim's content words."""
    matches = []
    for word in content_words(description):
        for definition in definitions:
            if definition["role"] in SKIP_ROLES or definition["file"] not in unread:
                continue
            if name_matches(word, definition["name"]):
                matches.append({"word": word, "name": definition["name"], "file": definition["file"],
                                "role": definition["role"],
                                "label": "expected" if (definition["file"], definition["name"]) in expected_locations
                                else "unlabeled"})
    return matches


def read_grounding(task, definitions, project_files, facts_by_iri, associations):
    """Read-time grounding for each record first delivered in a task's linked evidence."""
    seen, rows = set(), []
    locations = defaultdict(set)
    for association in associations:
        locations[association["fact"]].add((association["file"], association["name"].rsplit(".", 1)[-1]))
    for index, request in enumerate(task.get("model_requests", [])):
        prompt = request.get("prompt") or ""
        if LINKED_EVIDENCE_START not in prompt:
            continue
        unread = set(project_files) - set(prompt_read_files(prompt))
        for record in linked_claims(prompt):
            if record["iri"] in seen:
                continue
            seen.add(record["iri"])
            fact_id = facts_by_iri.get(record["iri"])
            matches = ground_claim(record["description"], unread, definitions,
                                   expected_locations=locations.get(fact_id, set()))
            rows.append({"request": index, "fact_id": fact_id, "iri": record["iri"], "title": record["title"],
                         "unread_files": sorted(unread), "matches": matches,
                         "expected": sum(1 for match in matches if match["label"] == "expected"),
                         "unlabeled": sum(1 for match in matches if match["label"] == "unlabeled"),
                         "segments_match": any(match["name"] == "SEGMENTS" and match["file"] == "accounts.py"
                                               for match in matches)})
    return rows


# ---------------------------------------------------------------------------
# 2. Pattern lookup (live, frozen daemon) and the whole diagnostic
# ---------------------------------------------------------------------------

def match_definitions(terms, definitions):
    return [definition for definition in definitions if definition["role"] not in SKIP_ROLES
            and any(name_matches(term, definition["name"]) for term in terms)]


def linked_section_bytes(context):
    position = context.find(LINKED_EVIDENCE_START)
    return len(context[position:].encode()) if position >= 0 else 0


def lookup_row(request, label, text, definitions, facts, *, deciding_fact, expected):
    """One pattern's precise-first lookup (definitions by name, then their files' push) and its topic fallback."""
    terms = pattern_terms(text)
    matched = match_definitions(terms, definitions)
    files = sorted({definition["file"] for definition in matched})[:32]
    topic = (text or label).encode()[:TOPIC_BYTES].decode(errors="ignore") or label
    code = None
    if files:
        response = request("/api/v1/harness/context", {"topic": topic, "files": files})
        context = response.get("context", "")
        dossiers = "".join("\n" + item.get("dossier", "") for item in response.get("files", []))
        code = dict(delivery(context + dossiers, facts, deciding_fact=deciding_fact, expected=expected),
                    files=files, context_bytes=len(context.encode()), linked_evidence_bytes=linked_section_bytes(context),
                    dossier_bytes=len(json.dumps(response.get("files", [])).encode()))
    fallback_response = request("/api/v1/harness/context", {"topic": topic, "files": [], "evidence_only": True})
    fallback_text = fallback_response.get("context", "")
    fallback = dict(delivery(fallback_text, facts, deciding_fact=deciding_fact, expected=expected),
                    bytes=len(fallback_text.encode()))
    return {"label": label, "pattern": text[:PATTERN_BYTES], "terms": terms,
            "matched": [{"name": item["name"], "file": item["file"], "role": item["role"]} for item in matched],
            "segments_surfaced": any(item["name"] == "SEGMENTS" and item["file"] == "accounts.py" for item in matched),
            "code_lookup": code, "topic_fallback": fallback}


def crowded_patterns(rows):
    """Distinct search-like patterns from crowded probe runs, then the fixed probe queries."""
    patterns = []
    for row in rows:
        for category, text in row["patterns"]:
            entry = (f"{row['backend']}:{category}", text)
            if entry not in patterns:
                patterns.append(entry)
    return patterns + [("probe_query", query) for query in PROBE_QUERIES]


def _under(path, roots):
    path = Path(path).resolve()
    return any(path.is_relative_to(Path(root).resolve()) for root in roots)


def intercept_diagnostics(*, roots, crowded_roots, scenario_id, binary_manifest, parent_preflight, output, deciding_fact):
    """Census over the roots, then the crowded probe's live pattern lookup and offline edit- and read-time grounding."""
    from .binaries import verify_binaries
    from .daemon import OwnedDaemon
    from .indexing import (apply_overlay, index_workspace, ready_dossiers, resolution_tables, short_probe_runtime,
                           verify_indexer)
    roots = [REPO / root if not Path(root).is_absolute() else Path(root) for root in roots]
    crowded_roots = [REPO / root if not Path(root).is_absolute() else Path(root) for root in crowded_roots]
    directory = Path(output).absolute()
    directory.mkdir(parents=True)
    rows, skipped = census(dict.fromkeys(roots + crowded_roots))
    census_result = {"schema_version": 1, "diagnostic": "action census; not part of any verdict",
                     "roots": [str(root) for root in roots], "runs": rows, "skipped": skipped,
                     "by_backend_model": aggregate(rows), "by_backend_model_scenario":
                         aggregate(rows, ("backend", "model", "scenario"))}
    (directory / "census.json").write_bytes(canonical_json(census_result))

    scenario = load_scenario(scenario_id)
    iri, title, claim = _deciding(scenario, deciding_fact)
    facts = scenario["initial_facts"]
    expected = list(scenario["episodes"][0].get("expected_fact_ids", []))
    associations = resolution_tables(scenario_id)[1]
    project = SCENARIOS / scenario_id / "project"
    project_files = sorted(name for name in tree_manifest(project) if name.endswith(".py"))
    sources = {name: (project / name).read_text() for name in project_files}
    crowded = [row for row in rows if _under(row["run"], crowded_roots)]

    binaries = verify_binaries(Path(binary_manifest))
    parent = json.loads(Path(parent_preflight).read_text())
    indexer = verify_indexer(parent["indexer"])
    workspace = directory / "workspace"
    shutil.copytree(project, workspace)
    subprocess.run(["/usr/bin/git", "init", "-q", str(workspace)], check=True, capture_output=True)
    prepare_workspace(workspace, scenario, "harness")
    apply_overlay(workspace)
    index = index_workspace(indexer, binaries["binaries"]["daemon"], workspace, directory / "index-runtime")
    with short_probe_runtime(directory / "daemon-runtime") as runtime:
        with OwnedDaemon(executable=Path(binaries["binaries"]["daemon"]), expected_sha256=binaries["binary_hashes"]["daemon"],
                         workspace=workspace, runtime=runtime, assets=Path(parent["assets"]["directory"]),
                         helper_model="unused-no-inference", helper_endpoint="http://127.0.0.1:9/v1",
                         log_path=directory / "daemon.log", indexer=indexer) as daemon:
            ready_dossiers(daemon, scenario, seed=True, require_empty=starts_empty(scenario))
            resolved = daemon._request("/api/v1/harness/intent/resolve", {"files": project_files, "refresh_index": False})
            definitions = normalize_definitions(resolved.get("entities", []))
            (directory / "definitions.json").write_bytes(canonical_json(
                {"files": project_files, "definitions": definitions, "unresolved": resolved.get("unresolved", [])}))
            lookups = [lookup_row(daemon._request, label, text, definitions, facts,
                                  deciding_fact=deciding_fact, expected=expected)
                       for label, text in crowded_patterns(crowded)]
            checkpoint = daemon.checkpoint()

    facts_by_iri = fact_iris(scenario)
    edits, reads = [], []
    for row in crowded:
        if row["backend"] != "harness":
            continue
        for journal in task_journals(row["run"]):
            task = json.loads(journal.read_bytes())
            label = {"run": row["run"], "model": row["model"], "task": journal.stem}
            edits.extend(dict(entry, **label) for entry in edit_grounding(task, definitions, sources))
            reads.extend(dict(entry, **label) for entry in read_grounding(task, definitions, project_files,
                                                                           facts_by_iri, associations))
    result = {"schema_version": 1, "diagnostic": "model actions as knowledge queries; not part of any verdict",
              "scenario_id": scenario_id, "package_sha256": scenario["package_sha256"], "build_id": binaries["build_id"],
              "deciding_fact": deciding_fact, "iri": iri, "title": title, "claim": claim, "index": index,
              "checkpoint": checkpoint,
              "routes": {"name_to_definitions": "none: no daemon, HTTP or MCP route resolves a bare identifier to indexed "
                                                "definitions project-wide; this diagnostic enumerates definitions with "
                                                "POST /api/v1/harness/intent/resolve over every project file (at most 32 "
                                                "files, 256 entities) and matches names in Python",
                         "needed": "a daemon route (or runner-side Substrate call) that looks an identifier up across the "
                                   "whole index by display name, returning definitions with file, symbol, role and "
                                   "source range",
                         "push": "POST /api/v1/harness/context with the matched files (dossiers plus linked evidence)",
                         "fallback": "POST /api/v1/harness/context evidence_only with the raw pattern as topic"},
              "skip_roles": list(SKIP_ROLES), "census": {"runs": len(rows), "skipped": len(skipped),
                                                         "by_backend_model": census_result["by_backend_model"]},
              "definitions": definitions, "lookups": lookups, "edit_grounding": edits, "read_grounding": reads}
    (directory / "intercept.json").write_bytes(canonical_json(result))
    return result


def summary(result):
    """Compact rows for the report: census groups, lookups, edits with attribute accesses, and read-time claims."""
    return {
        "census": [{key: group[key] for key in ("backend", "model", "runs", "runs_with_search",
                                                  "runs_search_before_first_edit", "before_first_edit_fraction",
                                                  "outcome_count_only")}
                   for group in result["census"]["by_backend_model"]],
        "lookups": [{"label": row["label"], "pattern": row["pattern"][:60], "matched": len(row["matched"]),
                     "segments": row["segments_surfaced"],
                     "code": None if row["code_lookup"] is None else {
                         key: row["code_lookup"][key] for key in ("deciding_claim", "other_claims", "context_bytes",
                                                                  "linked_evidence_bytes", "dossier_bytes")},
                     "fallback": {key: row["topic_fallback"][key] for key in ("deciding_claim", "other_claims", "bytes")}}
                    for row in result["lookups"]],
        "edits": [{"model": entry["model"], "event": entry["event"], "file": entry["file"],
                   "attributes": [{"attribute": item["attribute"], "literals": item["literals"],
                                   "pushed": [f"{pushed['file']}:{pushed['name']}" for pushed in item["pushed"]],
                                   "segments": item["segments_surfaced"], "mismatch": item["literal_mismatch"],
                                   "noise_bytes": item["noise_bytes"]} for item in entry["attributes"]]}
                  for entry in result["edit_grounding"] if entry["attributes"]],
        "reads": [{"model": entry["model"], "fact": entry["fact_id"], "matches": len(entry["matches"]),
                   "expected": entry["expected"], "unlabeled": entry["unlabeled"], "segments": entry["segments_match"]}
                  for entry in result["read_grounding"]],
    }
