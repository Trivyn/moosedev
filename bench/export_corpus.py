"""Export a corpus's project KG to per-record text chunks — the B1 free-text corpus.

Content parity: these chunks ARE the records B2 reads, just serialized as text. Pulled with a
deterministic SPARQL SELECT over the whole project graph — NOT get_relevant_context, which is a
relevance-ranked retrieval surface hard-capped at 100 (src/mcp/mod.rs: clamp(1,100)); a complete
B1 corpus needs EVERY record. (The product-side equivalent is the planned KG dump, Requirement
d459cac2; sparql is the existing bulk-read primitive, so the bench uses it now.)

The title is read from rdfs:label — the canonical label every record carries, including
SystemComponents (whose typed label property is hasComponentName, not hasTitle) and the same
field B2's get_relevant_context searches. Other predicates are matched by local-name (STRENDS)
so the ontology namespace isn't hardcoded (decouple-code-from-ontology-ttl).
"""
import asyncio
import json
import sys

import config
from mcp_client import call_tool

PROJECT_GRAPH = "https://moosedev.dev/kg/project"

QUERY = f"""
SELECT ?s ?kind ?title ?desc ?status WHERE {{
  GRAPH <{PROJECT_GRAPH}> {{
    ?s a ?kind ; <http://www.w3.org/2000/01/rdf-schema#label> ?title .
    OPTIONAL {{ ?s ?dp ?desc .   FILTER(STRENDS(STR(?dp), "hasDescription")) }}
    OPTIONAL {{ ?s ?sp ?status . FILTER(STRENDS(STR(?sp), "hasLifecycleStatus")) }}
  }}
}} ORDER BY ?title
"""


def _local(iri: str) -> str:
    return iri.rsplit("/", 1)[-1].rsplit("#", 1)[-1]


def rows_to_chunks(sparql_json: str, include_status: bool = True) -> list[dict]:
    data = json.loads(sparql_json)
    chunks, seen = [], set()
    for b in data["results"]["bindings"]:
        iri = b["s"]["value"]
        if iri in seen:  # a record with >1 rdf:type would duplicate; keep the first
            continue
        seen.add(iri)
        title = b["title"]["value"]
        desc = b.get("desc", {}).get("value", "")
        status = b.get("status", {}).get("value", "")
        # Status in the searchable text gives B1 the same currency info B2 has (a currency win then
        # reflects whether the delivery ACTS on the lifecycle). With include_status=False
        # (--no-status), B1 is CURRENCY-BLIND: the faithful append-only free-text baseline that
        # accumulates superseded + current entries with no machine-readable lifecycle.
        status_line = f"\n\nLifecycle status: {status}" if (status and include_status) else ""
        chunks.append({
            "iri": iri,
            "title": title,
            "kind": _local(b["kind"]["value"]),
            "status": status,
            "text": f"# {title}{status_line}\n\n{desc}".rstrip(),  # self-contained: title + status + rationale
        })
    return chunks


def corpus_env(corpus: str) -> dict:
    c = config.CORPORA[corpus]
    return {
        "MOOSEDEV_DATA_DIR": c["data_dir"],
        "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
        "MOOSEDEV_NO_AUTOSPAWN": "1",
        "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
        "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY,
        "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL,
    }


async def main(corpus: str = "moosedev", include_status: bool = True) -> None:
    out_json = await call_tool(config.MOOSEDEV_BIN, ["--connect"], corpus_env(corpus),
                               "sparql", {"query": QUERY})
    chunks = rows_to_chunks(out_json, include_status)
    out = config.corpus_chunks_path(corpus)
    out.write_text(json.dumps(chunks, indent=2))
    print(f"exported {len(chunks)} records -> {out}{'' if include_status else ' (currency-blind, no status)'}")


if __name__ == "__main__":
    _argv = sys.argv[1:]
    _no_status = "--no-status" in _argv
    _pos = [a for a in _argv if not a.startswith("--")] or ["moosedev"]
    asyncio.run(main(_pos[0], not _no_status))
