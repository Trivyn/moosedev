"""Build the CAPABILITY question suite with GRAPH-DERIVED ground truth (trivyn-temporal).

Each question carries a SPARQL that computes its exact answer set; we run it against the store and
freeze `expected_set`/`expected_size` into a capability_qa task JSON. Ground truth is derived, never
hand-authored. Supersession/multi-hop anchors are chosen by a witness-discovery query (pick a record
whose path is non-empty), then the prompt is instantiated with that real title. Private corpus →
tasks land under BENCH_HOME via config.corpus_tasks_path.

  .venv/bin/python capability_build.py            # build + verify
  .venv/bin/python capability_build.py --dry-run  # just print derived sizes
"""
import argparse
import asyncio
import json
from pathlib import Path

import config
from mcp_client import call_tool
from scale_build import start_serve, stop_serve, env_for

CORPUS = "trivyn-temporal"
STORE = config.CORPORA[CORPUS]["data_dir"]
G = "https://moosedev.dev/kg/project"
LABEL = "http://www.w3.org/2000/01/rdf-schema#label"


def sparql(query: str):
    out = asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"],
                                env_for(Path(STORE)), "sparql", {"query": query}))
    return json.loads(out).get("results", {}).get("bindings", [])


def aset(bindings):
    return [{"iri": b["s"]["value"], "title": b.get("title", {}).get("value", "")} for b in bindings]


# ---- fixed-SPARQL questions (set-completeness + negation) -------------------------------------
def type_set(local, status=None):
    f = f'FILTER(STRENDS(STR(?k),"{local}"))'
    s = (f'?s ?sp ?st . FILTER(STRENDS(STR(?sp),"hasLifecycleStatus")) FILTER(LCASE(STR(?st))="{status}")'
         if status else "")
    return (f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ ?s a ?k ; <{LABEL}> ?title . {f} {s} }} }} '
            f'ORDER BY ?title')


def neg_no_edge(local, edge, status=None):
    s = (f'?s ?sp ?st . FILTER(STRENDS(STR(?sp),"hasLifecycleStatus")) FILTER(LCASE(STR(?st))="{status}")'
         if status else "")
    return (f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ ?s a ?k ; <{LABEL}> ?title . '
            f'FILTER(STRENDS(STR(?k),"{local}")) {s} '
            f'FILTER NOT EXISTS {{ ?s ?ep ?o . FILTER(STRENDS(STR(?ep),"{edge}")) }} }} }} ORDER BY ?title')


SUPERSEDED_SET = (f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ '
                  f'?h ?p ?s . FILTER(STRENDS(STR(?p),"supersedes")) ?s <{LABEL}> ?title }} }} ORDER BY ?title')

QUESTIONS = [
    ("set_all_lessons", "set_completeness", "set",
     "In this project, list EVERY Lesson that has been recorded. Give each lesson's title; be exhaustive.",
     type_set("Lesson")),
    ("set_deprecated_records", "set_completeness", "set",
     "List every recorded item whose lifecycle status is 'deprecated'. Be exhaustive; give each title.",
     f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ ?s <{LABEL}> ?title ; ?sp ?st . '
     f'FILTER(STRENDS(STR(?sp),"hasLifecycleStatus")) FILTER(LCASE(STR(?st))="deprecated") }} }} ORDER BY ?title'),
    ("set_accepted_constraints", "set_completeness", "set",
     "List every Constraint whose status is 'accepted'. Be exhaustive; give each constraint's title.",
     type_set("Constraint", "accepted")),
    ("set_all_constraints", "set_completeness", "set",
     "List EVERY Constraint recorded for this project. Be exhaustive; give each constraint's title.",
     type_set("Constraint")),
    ("set_all_requirements", "set_completeness", "set",
     "List EVERY Requirement recorded for this project. Be exhaustive; give each requirement's title.",
     type_set("Requirement")),
    ("set_superseded_ads", "set_completeness", "set",
     "List every architectural decision that has been SUPERSEDED (replaced by a later decision). Be exhaustive.",
     f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ ?s a ?k ; <{LABEL}> ?title . '
     f'FILTER(STRENDS(STR(?k),"ArchitecturalDecision")) '
     f'FILTER EXISTS {{ ?h ?p ?s . FILTER(STRENDS(STR(?p),"supersedes")) }} }} }} ORDER BY ?title'),
    ("neg_accepted_ads_no_motivation", "negation", "set",
     "Which ACCEPTED architectural decisions have NO recorded motivation (no link to a requirement or "
     "constraint that motivated them)? List them all.",
     neg_no_edge("ArchitecturalDecision", "isMotivatedBy", "accepted")),
    ("neg_constraints_no_rationale", "negation", "set",
     "Which Constraints have NO recorded rationale? List them all.",
     neg_no_edge("Constraint", "hasRationale")),
    ("sup_all_superseded", "supersession", "set",
     "List every recorded decision/item that has since been superseded by a newer version. Be exhaustive.",
     SUPERSEDED_SET),
    # --- added to strengthen the thin classes (large-class negation, more set/supersession) ---
    ("neg_accepted_ads_no_rationale", "negation", "set",
     "Which ACCEPTED architectural decisions have NO recorded rationale? List them all.",
     neg_no_edge("ArchitecturalDecision", "hasRationale", "accepted")),
    ("set_proposed_ads", "set_completeness", "set",
     "List EVERY architectural decision whose status is 'proposed'. Be exhaustive; give each title.",
     type_set("ArchitecturalDecision", "proposed")),
    ("sup_current_replacements", "supersession", "set",
     "List every decision that REPLACED an earlier one (i.e., that supersedes a previous decision). Be exhaustive.",
     f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ ?s ?p ?o . '
     f'FILTER(STRENDS(STR(?p),"supersedes")) ?s <{LABEL}> ?title }} }} ORDER BY ?title'),
]

# ---- witness-discovery questions (supersession head, multi-hop) -------------------------------
SUP_HEAD_DISCOVER = (f'SELECT ?old ?oldTitle ?head ?headTitle WHERE {{ GRAPH <{G}> {{ '
                     f'?head ?p ?old . FILTER(STRENDS(STR(?p),"supersedes")) '
                     f'?old <{LABEL}> ?oldTitle . ?head <{LABEL}> ?headTitle . '
                     f'FILTER NOT EXISTS {{ ?newer ?p2 ?head . FILTER(STRENDS(STR(?p2),"supersedes")) }} '
                     f'FILTER NOT EXISTS {{ ?old ?p3 ?older . FILTER(STRENDS(STR(?p3),"supersedes")) }} '
                     f'}} }} ORDER BY ?oldTitle')

MH_DISCOVER = (f'SELECT ?c ?cTitle ?req ?reqTitle WHERE {{ GRAPH <{G}> {{ '
               f'?c a ?ck . FILTER(STRENDS(STR(?ck),"Constraint")) ?c <{LABEL}> ?cTitle . '
               f'?c ?cp ?ad . FILTER(STRENDS(STR(?cp),"constrains")) '
               f'?ad ?mp ?req . FILTER(STRENDS(STR(?mp),"isMotivatedBy")) '
               f'?req a ?rk . FILTER(STRENDS(STR(?rk),"Requirement")) ?req <{LABEL}> ?reqTitle }} }}')


def build_sup_head():
    b = sparql(SUP_HEAD_DISCOVER)
    if not b:
        return None
    w = b[0]  # first leaf->head pair (a length-1 chain)
    old_t, head = w["oldTitle"]["value"], {"iri": w["head"]["value"], "title": w["headTitle"]["value"]}
    q = (f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ '
         f'?old <{LABEL}> "{old_t}" . ?s ?p ?old . FILTER(STRENDS(STR(?p),"supersedes")) '
         f'?s <{LABEL}> ?title }} }}')
    return ("sup_head_of_reversal", "supersession", "scalar",
            f"The decision titled \"{old_t}\" was later reversed. What is the CURRENT decision that "
            f"replaced it? Give its exact title.", q)


def build_mh():
    b = sparql(MH_DISCOVER)
    if not b:
        return None
    c_t = b[0]["cTitle"]["value"]
    q = (f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ '
         f'?c <{LABEL}> "{c_t}" . ?c ?cp ?ad . FILTER(STRENDS(STR(?cp),"constrains")) '
         f'?ad ?mp ?s . FILTER(STRENDS(STR(?mp),"isMotivatedBy")) '
         f'?s a ?rk . FILTER(STRENDS(STR(?rk),"Requirement")) ?s <{LABEL}> ?title }} }}')
    return ("mh_req_through_constraint", "multi_hop", "set",
            f"For the constraint titled \"{c_t}\", find the requirement(s) that motivated the "
            f"architectural decision this constraint shaped. List them.", q)


# multi-hop #2: X <-supersedes- head -isMotivatedBy-> motivation (supersedes ∘ isMotivatedBy)
MH_SUPER_DISCOVER = (f'SELECT ?old ?oldTitle ?m ?mTitle WHERE {{ GRAPH <{G}> {{ '
                     f'?head ?sp ?old . FILTER(STRENDS(STR(?sp),"supersedes")) ?old <{LABEL}> ?oldTitle . '
                     f'?head ?mp ?m . FILTER(STRENDS(STR(?mp),"isMotivatedBy")) ?m <{LABEL}> ?mTitle }} }}')


def build_mh_superseder():
    b = sparql(MH_SUPER_DISCOVER)
    if not b:
        return None
    old_t = b[0]["oldTitle"]["value"]
    q = (f'SELECT DISTINCT ?s ?title WHERE {{ GRAPH <{G}> {{ '
         f'?old <{LABEL}> "{old_t}" . ?head ?sp ?old . FILTER(STRENDS(STR(?sp),"supersedes")) '
         f'?head ?mp ?s . FILTER(STRENDS(STR(?mp),"isMotivatedBy")) ?s <{LABEL}> ?title }} }}')
    return ("mh_motivation_of_superseder", "multi_hop", "set",
            f"What requirement or constraint motivated the decision that REPLACED \"{old_t}\"? List them.", q)


def census():
    out = {}
    for e in ("isMotivatedBy", "supersedes", "constrains", "hasRationale"):
        b = sparql(f'SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{G}> {{ ?s ?p ?o . '
                   f'FILTER(STRENDS(STR(?p),"{e}")) }} }}')
        out[e] = int(b[0]["n"]["value"]) if b else 0
    return out


def main():
    global CORPUS, STORE
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="trivyn-temporal")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    CORPUS = a.corpus
    STORE = config.CORPORA[CORPUS]["data_dir"]
    proc = start_serve(Path(STORE), Path("/tmp/capability_build_serve.log"))
    try:
        c = census()
        print(f"edge census: {c}")
        qs = list(QUESTIONS)
        for fn in (build_sup_head, build_mh, build_mh_superseder):
            w = fn()
            if w:
                qs.append(w)
            else:
                print(f"  WARN witness empty for {fn.__name__}")
        tasks_dir = config.corpus_tasks_path(CORPUS)
        tasks_dir.mkdir(parents=True, exist_ok=True)
        print(f"\n{'id':<32}{'class':<18}{'kind':<8}{'size':>6}")
        for qid, klass, kind, prompt, q in qs:
            es = aset(sparql(q))
            print(f"{qid:<32}{klass:<18}{kind:<8}{len(es):>6}")
            if a.dry_run:
                continue
            if not es:  # degenerate empty-set question on THIS corpus — no set to enumerate; drop it
                print(f"  SKIP {qid}: empty ground truth")
                (tasks_dir / f"{qid}.json").unlink(missing_ok=True)
                continue
            task = {"id": qid, "type": "capability_qa", "capability_class": klass,
                    "materialize_tree": False, "prompt": prompt,
                    "ground_truth": {"answer_kind": kind, "sparql": q,
                                     "expected_set": es, "expected_size": len(es)}}
            (tasks_dir / f"{qid}.json").write_text(json.dumps(task, indent=2))
        if not a.dry_run:
            print(f"\nwrote {len(qs)} capability tasks -> {tasks_dir}")
    finally:
        stop_serve(proc)


if __name__ == "__main__":
    main()
