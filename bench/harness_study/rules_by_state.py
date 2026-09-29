"""Offline estimate of the project-rules section under rules-by-state (context
plan, Group B item 4), from a conversation's recorded task journals. Never
calls a model.

Today every governing rule renders with its `via:` line and full claim
(`project_rules`, src/harness/runner/model.rs):

    \\n[{kind}] {label} ({iri})\\n{via}\\n{claim}

Under rules-by-state a rule already settled for the step renders as one line,
`[kind] label (iri) — <how it was settled>; <via>`, and one closing line counts
the settled rules. Only Requirements are shortened: a Constraint keeps its full
claim whatever its state (the lead's design correction; Constraint 979354f7
Rule 1 keeps every Constraint delivered). The all-kinds figure is reported
beside it only for comparison ("if Constraints were shortened too").

A rule is settled for a request when
- its IRI is in the `decided` set (a `--decided` file: rules an accepted
  decision isMotivatedBy, as the daemon's `decided_by` would report);
- proxy (on unless disabled): an earlier journal of the conversation approved a
  plan that addresses it, standing in for the capture edges that approval
  minted; or
- a plan this journal approved before the request addresses it (the prompt
  shows that plan as approved: Auto mode, or "Approved plan (amend it").
Auto-mode exception: in an Auto request the rules the approved plan addresses
stay open, so the builder keeps the claims it implements.

Plans' `deferred` and `satisfied` lists are not counted as settling. The
harness counts neither from an earlier plan either, but it does count the
current plan's `satisfied` claims, so figures here are conservative there.
The harness also settles an earlier plan's addresses only once every file of
it was edited under it; this estimate does not check edits, so it can
overstate settlement for a plan replaced part-way.
"""
import json
import re
from pathlib import Path
from statistics import median

from .prefix_reuse import PURPOSE, sections

RULE_HEADER = re.compile(r"^\[(\w+)\] (.*) \((\S+)\)$", re.M)
# The closing line for rules named without their claim, and the output
# instruction that follows the rules in the same prompt section.
UNCLAIMED = re.compile(r"^\d+ project rule\(s\) named without their claim", re.M)
OUTPUT_LINE = re.compile(r"^(Return one JSON object|Return exactly one JSON action\.|Call exactly one tool)", re.M)
AMEND_MARKER = "\nApproved plan (amend it; keep what still holds): "
PLAN_MARKER = "\nPlan: "
APPROVED_MARKER = "\nThe displayed plan is approved."
MODE_LINE = re.compile(r"\nMode: (\w+)\n")
# A representative accepted-decision IRI, as long as the harness mints them.
DECISION_IRI = "https://moosedev.dev/kg/ArchitecturalDecision/00000000-0000-0000-0000-000000000000"
SHORTENED_KINDS = frozenset({"Requirement"})
DECIDED, EARLIER_PLAN, THIS_TASK = "decided", "earlier task's plan", "this task's plan"


def parse_rules(section):
    """The project-rules section as (head, rules, tail): head is the header
    text, each rule a dict with kind, label, iri, via and its exact block
    text, and tail the closing and output lines. Joined they are the section."""
    headers = list(RULE_HEADER.finditer(section))
    if not headers:
        return section, [], ""
    last = headers[-1].end()
    ends = [match.start() for match in (UNCLAIMED.search(section, last), OUTPUT_LINE.search(section, last))
            if match]
    tail_at = len(section)
    if ends:
        tail_at = min(ends)
        # The unclaimed line brings its own leading newline.
        if UNCLAIMED.match(section, tail_at):
            tail_at -= 1
    rules = []
    for index, match in enumerate(headers):
        start = match.start() - 1
        end = headers[index + 1].start() - 1 if index + 1 < len(headers) else tail_at
        via = section[match.end() + 1:].split("\n", 1)[0]
        rules.append({"kind": match.group(1), "label": match.group(2), "iri": match.group(3),
                      "via": via, "text": section[start:end]})
    return section[:headers[0].start() - 1], rules, section[tail_at:]


def one_line(rule, note):
    return f"\n[{rule['kind']}] {rule['label']} ({rule['iri']}) — {note}; {rule['via']}"


def counted_line(counts):
    """The closing line naming the settled rules by kind and the search route."""
    kinds = "; ".join(f"{kind}: {n}" for kind, n in sorted(counts.items()))
    return (f"\n{sum(counts.values())} settled project rule(s) shown as one line ({kinds}): each is decided "
            "by an accepted decision or addressed by an approved plan and needs no answer in your plan; "
            "search project knowledge for its claim\n")


def estimate(section, settled, kinds=SHORTENED_KINDS):
    """The rules section with each settled rule of `kinds` on one line.
    `settled` maps an IRI to its note. Returns (text, full, one_line)."""
    head, rules, tail = parse_rules(section)
    out, counts = [head], {}
    for rule in rules:
        note = settled.get(rule["iri"])
        if note is not None and rule["kind"] in kinds:
            out.append(one_line(rule, note))
            counts[rule["kind"]] = counts.get(rule["kind"], 0) + 1
        else:
            out.append(rule["text"])
    if counts:
        out.append(counted_line(counts))
    out.append(tail)
    shortened = sum(counts.values())
    return "".join(out), len(rules) - shortened, shortened


def mode(prompt):
    """The harness state's Mode line (Plan or Auto), or None."""
    state = prompt.find("\nCurrent harness state")
    match = MODE_LINE.search(prompt, max(state, 0))
    return match.group(1) if match else None


def shown_plan(prompt):
    """(plan, approved) for the plan the prompt shows: the one JSON line after
    the accepted knowledge, just before the source. approved is True when the
    prompt shows it as approved (Auto mode, or amending an approved plan)."""
    start = max(prompt.find("\nConfigured model ID:"), 0)
    end = prompt.find("\nCurrent source, refreshed", start)
    end = len(prompt) if end < 0 else end
    at, marker = max((prompt.rfind(m, start, end), m) for m in (AMEND_MARKER, PLAN_MARKER))
    if at < 0:
        return None, False
    line = prompt[at + len(marker):].split("\n", 1)[0]
    try:
        plan = json.loads(line)
    except ValueError:
        return None, False
    if not isinstance(plan, dict):
        return None, False
    approved = marker == AMEND_MARKER or mode(prompt) == "Auto" or APPROVED_MARKER in prompt[start:]
    return plan, approved


def addressed(plan, rules):
    """The IRIs a plan's addresses name, by IRI or by rule label."""
    by_label = {rule["label"]: rule["iri"] for rule in rules}
    return {by_label.get(entry, entry) for entry in (plan or {}).get("addresses") or []}


def journal_rows(task, name, decided, earlier):
    """One row per action request of one journal. `earlier` holds the IRIs
    earlier journals' approved plans addressed (empty without the proxy)."""
    rows, approved_here, approved_plans = [], {}, []
    for index, request in enumerate(task.get("model_requests", [])):
        prompt = request.get("prompt")
        if request.get("purpose") != PURPOSE or not isinstance(prompt, str):
            continue
        section = dict(sections(prompt)).get("project rules", "")
        _, rules, _ = parse_rules(section)
        plan, approved = shown_plan(prompt)
        current_plan = addressed(plan, rules) if approved else set()
        if approved:
            # The summary is a per-step view; files, checks and addresses are whole.
            key = json.dumps([plan.get(field) for field in ("files", "checks", "addresses")])
            if key not in approved_plans:
                approved_plans.append(key)
            ordinal = approved_plans.index(key) + 1
            for iri in current_plan:
                approved_here.setdefault(iri, ordinal)
        request_mode = mode(prompt)
        settled, sources = {}, {}
        for rule in rules:
            iri = rule["iri"]
            if request_mode == "Auto" and iri in current_plan:
                continue
            if iri in decided:
                settled[iri], source = f"decided by {DECISION_IRI}", DECIDED
            elif iri in earlier:
                settled[iri], source = f"decided by {DECISION_IRI}", EARLIER_PLAN
            elif iri in approved_here:
                settled[iri], source = f"addressed by approved plan {approved_here[iri]}", THIS_TASK
            else:
                continue
            sources[source] = sources.get(source, 0) + 1
        text, full, shortened = estimate(section, settled)
        text_all, _, shortened_all = estimate(section, settled, kinds=frozenset(r["kind"] for r in rules))
        rows.append({
            "journal": name, "request": index, "mode": request_mode, "rules": len(rules),
            "current_bytes": len(section.encode()), "estimated_bytes": len(text.encode()),
            "estimated_all_kinds_bytes": len(text_all.encode()),
            "n_full": full, "n_one_line": shortened, "n_one_line_all_kinds": shortened_all,
            "settled_by": sources,
        })
    return rows


def summarize(rows):
    """Max and median of current and estimated rules bytes over prompts that
    carry rules: Plan-mode prompts and all of them."""
    def stats(chosen):
        if not chosen:
            return {"prompts": 0}
        out = {"prompts": len(chosen)}
        for key in ("current_bytes", "estimated_bytes", "estimated_all_kinds_bytes"):
            values = [row[key] for row in chosen]
            out[key.removesuffix("_bytes")] = {"max": max(values), "median": round(median(values))}
        return out
    with_rules = [row for row in rows if row["rules"]]
    return {"plan_mode": stats([row for row in with_rules if row["mode"] == "Plan"]),
            "overall": stats(with_rules)}


def conversation(tasks, decided=frozenset(), proxy=True):
    """Rows and summaries over journals given in conversation order."""
    rows, per_journal, earlier = [], {}, set()
    for name, task in tasks:
        found = journal_rows(task, name, decided, earlier if proxy else set())
        rows.extend(found)
        per_journal[name] = summarize(found)
        for plan in task.get("approved_plans") or []:
            earlier.update(plan.get("addresses") or [])
    return {"settings": {"proxy": proxy, "decided": len(decided), "shortened_kinds": sorted(SHORTENED_KINDS)},
            "rows": rows, "journals": per_journal, "summary": summarize(rows)}


def read_decided(path):
    """IRIs, one per line; blank lines and # comments ignored."""
    return frozenset(line.strip() for line in Path(path).read_text().splitlines()
                     if line.strip() and not line.strip().startswith("#"))


def report_files(paths, decided_file=None, proxy=True):
    tasks = [(Path(path).stem[:8], json.loads(Path(path).read_text())) for path in paths]
    decided = read_decided(decided_file) if decided_file else frozenset()
    return conversation(tasks, decided, proxy)


def table(result):
    """The report as plain text: one line per request, then the summaries."""
    lines = [f"{'journal':8} {'req':>3} {'mode':4} {'rules':>5} {'current':>8} {'estimated':>9} "
             f"{'all-kinds':>9} {'full':>4} {'1-line':>6} {'1-line*':>7}  settled by"]
    for row in result["rows"]:
        settled = ", ".join(f"{source} {n}" for source, n in sorted(row["settled_by"].items()))
        lines.append(f"{row['journal']:8} {row['request']:>3} {row['mode'] or '?':4} {row['rules']:>5} "
                     f"{row['current_bytes']:>8} {row['estimated_bytes']:>9} "
                     f"{row['estimated_all_kinds_bytes']:>9} {row['n_full']:>4} {row['n_one_line']:>6} "
                     f"{row['n_one_line_all_kinds']:>7}  {settled}")
    lines.append("estimated: settled Requirements on one line (Constraints full); "
                 "all-kinds / 1-line*: if Constraints were shortened too")
    scopes = [(name, summary) for name, summary in result["journals"].items()] + [("all", result["summary"])]
    for name, summary in scopes:
        for scope in ("plan_mode", "overall"):
            s = summary[scope]
            if not s["prompts"]:
                lines.append(f"{name} {scope}: no prompts with rules")
                continue
            lines.append(
                f"{name} {scope} ({s['prompts']} prompts): current max {s['current']['max']} / median "
                f"{s['current']['median']}; estimated max {s['estimated']['max']} / median "
                f"{s['estimated']['median']}; all-kinds max {s['estimated_all_kinds']['max']} / median "
                f"{s['estimated_all_kinds']['median']}")
    settings = result["settings"]
    lines.append(f"settings: proxy={'on' if settings['proxy'] else 'off'}, decided IRIs={settings['decided']}")
    return "\n".join(lines)
