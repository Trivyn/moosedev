"""What reached the graph during an episode, and whether it is usable.

Deterministic and arm-neutral: every number here is computed from the sealed
canonical graph and the sealed event stream, never from a model. Semantic truth
about a captured record stays where the protocol puts it — in retained reviewer
judgments (see `grading._knowledge_metrics`). This module answers the questions
that do not need a reader: was anything written, is it well formed, is it
reachable.

Scope note. `harness_study_workflow` declares `capture_attempt_count` and
`capture_failure_rate` unavailable, and that remains true of the evidence family
it reads (native protocol errors and proxy receipts). This module reads a
different family — the per-episode `kg.nq` snapshots plus the normalized agent
event stream — so the two do not disagree.

Two asymmetries the caller must carry into any report:

- The harness writes proposals and a reviewer ratifies them; an MCP arm's
  `record_important_decision` writes directly. Both are measured at the END of
  the episode, after review, which is the only arm-neutral moment.
- A harness capture scored `Restates` deliberately produces NO record, only a
  receipt and links onto the record it restates. A zero-record episode is
  therefore not necessarily a capture failure, which is why attempts are
  reported beside records and never folded into them.
"""
import json
from pathlib import Path
import re

from .reviewer import KINDS as RECORD_KINDS

# A record is reachable if something points at it or it points at something
# other than its own type and provenance. An orphan is findable by lexical luck
# alone, which is exactly the failure a typed graph is supposed to prevent.
_PROVENANCE = re.compile(r"/ns/prov#")
_QUAD = re.compile(r'^\s*<([^>]+)>\s+<([^>]+)>\s+(<[^>]+>|"(?:[^"\\]|\\.)*"(?:\^\^<[^>]+>|@[\w-]+)?)\s')
# Titles this short cannot distinguish one decision from another; the pilot saw a
# Consequence titled "Test query" written by a 9B (Lesson 5242c359).
MIN_TITLE = 12


def local_name(iri):
    """The term after the last `#` or `/`. Never match a hardcoded ontology IRI
    (Constraint 19bb4d8a): the graph's namespace is not this module's business."""
    return re.split(r"[#/]", iri)[-1]


def parse_quads(text):
    """(subject, predicate, object, object_is_iri) for each readable N-Quad line."""
    for line in text.splitlines():
        match = _QUAD.match(line)
        if match is None:
            continue
        subject, predicate, value = match.group(1), match.group(2), match.group(3)
        if value.startswith("<"):
            yield subject, predicate, value[1:-1], True
        else:
            body = value.split('"^^<')[0].rsplit('"@', 1)[0]
            yield subject, predicate, json.loads(body if body.endswith('"') else body + '"'), False


def records(text):
    """Typed knowledge records in a canonical graph, keyed by IRI."""
    found = {}
    for subject, predicate, value, is_iri in parse_quads(text):
        name = local_name(predicate)
        if name == "type" and is_iri and local_name(value) in RECORD_KINDS:
            found.setdefault(subject, _blank())["kind"] = local_name(value)
    for subject, predicate, value, is_iri in parse_quads(text):
        record = found.get(subject)
        if record is None:
            continue
        name = local_name(predicate)
        if name in ("hasTitle", "label") and not is_iri:
            record["title"] = record["title"] or value
        elif name == "hasDescription" and not is_iri:
            record["description"] = record["description"] or value
        elif name == "hasLifecycleStatus" and not is_iri:
            record["status"] = value
        elif is_iri and name != "type" and not _PROVENANCE.search(predicate):
            record["links"].append(name)
    return found


def _blank():
    return {"kind": None, "title": "", "description": "", "status": "", "links": []}


def validity(record, *, prompt=None):
    """Whether one record is usable later: named distinctly, explained, reachable.

    Structural only. A record can satisfy every clause here and still be false;
    that judgment belongs to a reviewer, never to this function.
    """
    title = (record.get("title") or "").strip()
    degenerate = (not title or len(title) < MIN_TITLE or title == record.get("kind")
                  or (prompt is not None and title == prompt.strip().splitlines()[0].strip()))
    return {"titled": not degenerate,
            "described": bool((record.get("description") or "").strip()),
            "linked": bool(record.get("links")),
            "valid": bool(not degenerate and (record.get("description") or "").strip()
                          and record.get("links"))}


def graph_at(run, relative):
    """A sealed canonical graph, or None when that snapshot holds no graph."""
    path = Path(run) / relative / ".moosedev" / "kg.nq"
    return path.read_text() if path.is_file() else None


def _events(run):
    path = Path(run) / "events.jsonl"
    if not path.is_file():
        return
    with path.open("rb") as stream:
        for line in stream:
            try:
                yield json.loads(line)
            except ValueError:
                continue


def attempts(run, backend, episode_id):
    """Capture and retrieval tool calls an episode made, from the sealed stream.

    Read from the `native` channel, where the runner already persisted each
    normalized observation: re-deriving them here could drift from what the run
    actually saw.

    Only meaningful where the agent itself calls the tools. For the harness the
    model never does — the runner drives capture — so this returns None there
    and the caller reports the runner's journal counts instead. Returning zero
    would read as "it did not try" for an arm that is never asked to.
    """
    if backend == "harness":
        return None
    if not (Path(run) / "events.jsonl").is_file():
        # Unobserved is not zero. Reporting 0 for a run with no event log would
        # read as "it never called its memory tool" on missing evidence.
        return None
    counted = {"capture": 0, "retrieval": 0, "capture_errors": 0}
    for event in _events(run):
        if event.get("channel") != "native":
            continue
        payload = event.get("payload") or {}
        if payload.get("episode") != episode_id:
            continue
        if payload.get("capture") is not None:
            counted["capture"] += 1
            if payload.get("error") is not None:
                counted["capture_errors"] += 1
        if payload.get("retrieval") is not None:
            counted["retrieval"] += 1
    return counted


def checkpoints(run):
    """Per-episode daemon checkpoints: SHACL conformance and unratified proposals."""
    found = {}
    for event in _events(run):
        if event.get("channel") != "checkpoint":
            continue
        payload = event.get("payload") or {}
        episode = payload.get("episode")
        if isinstance(episode, str):
            found[episode] = payload
    return found


def episode_capture(run, *, backend, episodes):
    """Per-episode capture outcomes, each measured against the episode before it.

    `episodes` is the attempted episode id list, in order. The baseline for the
    first is the `initial` snapshot, so seeded records are never counted as
    something the agent captured.
    """
    marks = checkpoints(run)
    previous = records(graph_at(run, "initial") or "")
    rows = []
    for episode_id in episodes:
        text = graph_at(run, f"episodes/{episode_id}/workspace")
        if text is None:
            rows.append({"episode": episode_id, "graph": False})
            continue
        current = records(text)
        prompt_path = Path(run) / f"episodes/{episode_id}/prompt.txt"
        prompt = prompt_path.read_text() if prompt_path.is_file() else None
        created = [iri for iri in current if iri not in previous]
        judged = {iri: validity(current[iri], prompt=prompt) for iri in created}
        mark = marks.get(episode_id) or {}
        rows.append({
            "episode": episode_id, "graph": True,
            "records_created": len(created),
            "records_valid": sum(1 for item in judged.values() if item["valid"]),
            "records": {iri: dict(judged[iri], kind=current[iri]["kind"],
                                  status=current[iri]["status"]) for iri in created},
            "records_pending": list(mark.get("pending") or []),
            "conforms": mark.get("conforms"),
            "revision": mark.get("revision"),
            "attempts": attempts(run, backend, episode_id),
        })
        previous = current
    return rows


def capture_summary(rows):
    """Run-level capture outcomes. `captured_episodes` is the AD's capture rate."""
    graphed = [row for row in rows if row.get("graph")]
    if not graphed:
        return {"episodes": 0, "captured_episodes": None, "capture_rate": None,
                "records_created": 0, "records_valid": 0, "validity_rate": None,
                "conforms": None, "pending": 0}
    created = sum(row["records_created"] for row in graphed)
    valid = sum(row["records_valid"] for row in graphed)
    captured = sum(1 for row in graphed if row["records_created"])
    conformance = [row["conforms"] for row in graphed if row["conforms"] is not None]
    return {"episodes": len(graphed), "captured_episodes": captured,
            "capture_rate": captured / len(graphed),
            "records_created": created, "records_valid": valid,
            "validity_rate": valid / created if created else None,
            "conforms": all(conformance) if conformance else None,
            "pending": sum(len(row["records_pending"]) for row in graphed)}
