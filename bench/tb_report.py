"""Tally a temporally-bootstrapped MOOSEDev store: nodes by kind, edges by predicate, supersede
chains, timeline span, and look-back correctness (new.hasTimestamp >= old.hasTimestamp). Reuses the
temporal_bootstrap serve/query helpers; namespace-decoupled (matches predicates/types by local name
per the decouple-from-ontology-TTL discipline).

Usage:  python tb_report.py --data-dir ~/.moosedev-stores/burrow-temporal
"""
import argparse
import json

import temporal_bootstrap as tb
from temporal_bootstrap import PROJECT_GRAPH, RDFS_LABEL

G = f"GRAPH <{PROJECT_GRAPH}>"


def select(data_dir, q):
    out = tb.mcp(data_dir, "sparql", {"query": q})
    try:
        return json.loads(out)["results"]["bindings"]
    except (ValueError, KeyError):
        return []


def v(b, k):
    return b.get(k, {}).get("value")


def report(data_dir):
    serve = tb.start_serve(data_dir)
    try:
        nodes = int(v(select(data_dir,
            f"SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {G} {{ ?s <{RDFS_LABEL}> ?l }} }}")[0], "n"))

        kinds = select(data_dir,
            f'SELECT ?k (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {G} {{ ?s a ?t . '
            f'?s <{RDFS_LABEL}> ?l . BIND(REPLACE(STR(?t),"^.*[/#]","") AS ?k) }} }} '
            f"GROUP BY ?k ORDER BY DESC(?n)")

        # Inter-record edges: object is another minted /kg/ record (excludes rdf:type -> ontology
        # classes and all literal datatype properties).
        EDGE = 'isIRI(?o) && CONTAINS(STR(?o),"/kg/")'
        edges_total = int(v(select(data_dir,
            f"SELECT (COUNT(*) AS ?n) WHERE {{ {G} {{ ?s ?p ?o . FILTER({EDGE}) }} }}")[0], "n"))

        edge_hist = select(data_dir,
            f'SELECT ?pred (COUNT(*) AS ?n) WHERE {{ {G} {{ ?s ?p ?o . FILTER({EDGE}) '
            f'BIND(REPLACE(STR(?p),"^.*[/#]","") AS ?pred) }} }} GROUP BY ?pred ORDER BY DESC(?n)')

        span = select(data_dir,
            f'SELECT (MIN(?t) AS ?lo) (MAX(?t) AS ?hi) WHERE {{ {G} {{ ?s ?p ?t . '
            f'FILTER(STRENDS(STR(?p),"hasTimestamp")) }} }}')[0]

        # Supersede chains with both endpoints' titles + timestamps, to check look-back correctness.
        chains = select(data_dir,
            f'SELECT ?newL ?nt ?oldL ?ot WHERE {{ {G} {{ '
            f'?new ?sp ?old . FILTER(STRENDS(STR(?sp),"supersedes")) '
            f'?new <{RDFS_LABEL}> ?newL . ?old <{RDFS_LABEL}> ?oldL . '
            f'?new ?ntp ?nt . FILTER(STRENDS(STR(?ntp),"hasTimestamp")) '
            f'?old ?otp ?ot . FILTER(STRENDS(STR(?otp),"hasTimestamp")) }} }}')

        validate = tb.mcp(data_dir, "validate_against_architecture", {}).splitlines()[0]
    finally:
        serve.terminate()
        try:
            serve.wait(timeout=10)
        except Exception:
            serve.kill()

    print(f"\n=== {data_dir} ===")
    print(f"nodes: {nodes}   edges: {edges_total}   edge:node = {edges_total/nodes:.2f}" if nodes else "empty")
    print(f"timeline: {v(span,'lo')}  ..  {v(span,'hi')}")
    print(f"validate: {validate}")
    print("\nby kind:")
    for b in kinds:
        print(f"  {v(b,'n'):>3} {v(b,'k')}")
    print("\nedges by predicate:")
    for b in edge_hist:
        print(f"  {v(b,'n'):>3} {v(b,'pred')}")
    bad = [c for c in chains if v(c, "nt") < v(c, "ot")]
    print(f"\nsupersede chains: {len(chains)}  (look-back violations new<old: {len(bad)})")
    for c in chains:
        ok = "ok" if v(c, "nt") >= v(c, "ot") else "BAD"
        print(f"  [{ok}] {v(c,'oldL')[:54]!r}\n        -> {v(c,'newL')[:54]!r}  ({v(c,'ot')[:10]} -> {v(c,'nt')[:10]})")
    return {"nodes": nodes, "edges": edges_total, "supersedes": len(chains), "lookback_bad": len(bad)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data-dir", required=True)
    report(ap.parse_args().data_dir)


if __name__ == "__main__":
    main()
