"""Entity-anchored CURRENCY questions: the one Q&A shape push cannot win by tautology.

Asking "which records apply to this entity" is not a test of push — the harness
pushes exactly that entity's dossier, so it scores 1.0 by construction and the
question merely restates the delivery. The same objection sinks the graph-wide
"list every Constraint" questions for a file-anchored walk.

A currency question does not have that defect. It anchors on an entity where BOTH
a record and the record it supersedes are linked, so the WRONG answer is present
in the corpus and equally retrievable:

  - the harness's push is working-set filtered, so the superseded record never
    arrives and the model cannot cite it;
  - BM25 over exported text is currency-blind and surfaces both;
  - an agent holding the MCP tools must know to filter by lifecycle itself.

The discriminating measure is therefore not F1 but the CURRENCY ERROR: naming the
superseded record as if it still applied. That is the failure MOOSEDev claims to
prevent, and it is what the NeSy work measured.
"""
import collections
import json
import pathlib
import re
import sys

import config

QUAD = re.compile(r'^<(\S+?)>\s+<(\S+?)>\s+(.*?)\s+<\S+?>\s*\.$')
RECORD_KINDS = {"ArchitecturalDecision", "Constraint", "Lesson", "Requirement",
                "Pattern", "AntiPattern"}


def read_graph(path: pathlib.Path) -> dict:
    """Minimal N-Quads scan: enough for supersession, lifecycle and code links."""
    g = {"supersedes": {}, "status": {}, "label": {}, "kind": {},
         "path": {}, "file": {}, "linked": collections.defaultdict(list)}
    for line in path.read_text(errors="replace").splitlines():
        m = QUAD.match(line.strip())
        if not m:
            continue
        s, p, o = m.groups()
        local = p.rsplit("#", 1)[-1].rsplit("/", 1)[-1]
        if local == "supersedes":
            g["supersedes"][s] = o.strip("<>")
        elif local == "hasLifecycleStatus":
            g["status"][s] = o.strip('"')
        elif local == "label":
            g["label"][s] = o.strip('"')
        elif local == "type":
            k = o.strip("<>").rsplit("#", 1)[-1]
            if k in RECORD_KINDS:
                g["kind"][s] = k
        elif local == "hasLogicalPath":
            g["path"][s] = o.strip('"')
        elif local == "definedInPath":
            # the harness pushes per FILE, so the file is what the walk arm needs
            g["file"][s] = o.strip('"')
        elif local in ("concerns", "constrains") and "/CodeEntity/" in o:
            g["linked"][o.strip("<>")].append(s)
    return g


def traps(g: dict) -> list[dict]:
    """Entities carrying a current record AND the one it replaced."""
    out = []
    for entity, records in g["linked"].items():
        here = set(records)
        for new, old in g["supersedes"].items():
            if new not in here or old not in here:
                continue
            # only a live successor makes the question well-posed: if the
            # replacement is itself superseded there is no single current answer
            if g["status"].get(new) != "accepted":
                continue
            if g["status"].get(old) != "superseded":
                continue
            out.append({"entity": entity, "current": new, "superseded": old})
    return out


def main() -> None:
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="moosedev-currency-2026-09")
    args = ap.parse_args()

    store = pathlib.Path(config.CORPORA[args.corpus]["data_dir"]) / "kg.nq"
    g = read_graph(store)
    found = traps(g)
    tasks = config.corpus_tasks_path(args.corpus)
    tasks.mkdir(parents=True, exist_ok=True)

    seen, written = set(), 0
    for t in found:
        where = g["path"].get(t["entity"], "")
        cur, old = g["label"].get(t["current"]), g["label"].get(t["superseded"])
        if not (where and cur and old) or cur == old:
            # identical titles cannot be graded: the wrong answer is the right string
            continue
        key = (where, cur)
        if key in seen:
            continue
        seen.add(key)
        name = re.sub(r"[^a-z0-9]+", "_", where.lower()).strip("_")[:48]
        task = {
            "id": f"cur_{name}",
            "type": "capability_qa",
            "capability_class": "currency",
            "materialize_tree": False,
            "anchor": where,
            # the push arm selects the dossier by IRI: a logical path is not a
            # SCIP symbol and get_entity_dossier must not have to guess
            "anchor_iri": t["entity"],
            "anchor_file": g["file"].get(t["entity"], ""),
            "prompt": (f"In this project, what rule CURRENTLY governs `{where}`? "
                       f"Give the exact title of the decision or constraint in force. "
                       f"Do not cite knowledge that has been superseded."),
            "ground_truth": {
                "answer_kind": "scalar",
                "expected_set": [{"iri": t["current"], "title": cur}],
                "expected_size": 1,
                # graded beyond F1: citing this is a currency error, the failure
                # the whole push mechanism exists to prevent
                "stale_distractor": {"iri": t["superseded"], "title": old},
            },
        }
        (tasks / f"{task['id']}.json").write_text(json.dumps(task, indent=2))
        written += 1

    print(f"supersession traps found : {len(found)}")
    print(f"currency tasks written   : {written} -> {tasks}")
    for f in sorted(tasks.glob("cur_*.json"))[:6]:
        t = json.loads(f.read_text())
        print(f"  {t['id']:<44} current={t['ground_truth']['expected_set'][0]['title'][:38]!r}")
        print(f"  {'':<44} stale  ={t['ground_truth']['stale_distractor']['title'][:38]!r}")


if __name__ == "__main__":
    main()
