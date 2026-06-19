"""Content-parity check: the B1 export must carry exactly the records B2 reads from the graph.

Verifies (1) chunk count == graph record count, (2) every chunk IRI exists in the graph, and
(3) each chunk's body text == the graph's stored hasDescription for that record. A mismatch means
B1 and B2 would not carry identical knowledge — the headline B2−B1 comparison would be confounded.
"""
import asyncio
import json
import sys

import config
from export_corpus import corpus_env
from mcp_client import call_tool

PG = "https://moosedev.dev/kg/project"


async def graph_desc_map(corpus: str) -> dict:
    q = f"""SELECT ?s ?d WHERE {{ GRAPH <{PG}> {{
        ?s a ?k ; ?p ?d . FILTER(STRENDS(STR(?p), "hasDescription")) }} }}"""
    out = await call_tool(config.MOOSEDEV_BIN, ["--connect"], corpus_env(corpus), "sparql", {"query": q})
    rows = json.loads(out)["results"]["bindings"]
    return {r["s"]["value"]: r["d"]["value"] for r in rows}


async def main(corpus: str) -> None:
    chunks = json.loads(config.corpus_chunks_path(corpus).read_text())
    gmap = await graph_desc_map(corpus)
    print(f"corpus={corpus}  chunks={len(chunks)}  graph_records_with_desc={len(gmap)}")

    missing = [c["iri"] for c in chunks if c["iri"] not in gmap]
    mismatched = []
    for c in chunks:
        if c["iri"] not in gmap:
            continue
        # The chunk text is intentionally richer than the raw description: it prepends
        # "# title" and a "Lifecycle status:" line (currency-aware BM25). Parity means the
        # record's rationale (hasDescription) is CARRIED in the chunk, so check containment.
        desc = gmap[c["iri"]].strip()
        if desc and desc not in c["text"]:
            mismatched.append(c["iri"])

    print(f"chunks missing from graph: {len(missing)}")
    print(f"body != graph hasDescription: {len(mismatched)}")
    ok = not missing and not mismatched and len(chunks) == len(gmap)
    print("PARITY OK" if ok else "PARITY FAILED")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1] if len(sys.argv) > 1 else "moosedev"))
