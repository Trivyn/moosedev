"""Export a corpus's project KG to per-record text chunks — the B1 free-text corpus.

Content parity: these chunks ARE the records B2 reads, just serialized as text. Pulled with a
deterministic SPARQL SELECT over the whole project graph — NOT get_relevant_context, which is a
relevance-ranked retrieval surface hard-capped at 100 (src/mcp/mod.rs: clamp(1,100)); a complete
B1 corpus needs EVERY record. (The product-side equivalent is the planned KG dump, Requirement
d459cac2; sparql is the existing bulk-read primitive, so the bench uses it now.)

Predicates are matched by local-name (STRENDS) so the ontology namespace isn't hardcoded
(decouple-code-from-ontology-ttl).
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
    ?s a ?kind ; ?tp ?title .
    FILTER(STRENDS(STR(?tp), "hasTitle"))
    OPTIONAL {{ ?s ?dp ?desc .   FILTER(STRENDS(STR(?dp), "hasDescription")) }}
    OPTIONAL {{ ?s ?sp ?status . FILTER(STRENDS(STR(?sp), "hasLifecycleStatus")) }}
  }}
}} ORDER BY ?title
"""


def _local(iri: str) -> str:
    return iri.rsplit("/", 1)[-1].rsplit("#", 1)[-1]


def rows_to_chunks(sparql_json: str) -> list[dict]:
    data = json.loads(sparql_json)
    chunks, seen = [], set()
    for b in data["results"]["bindings"]:
        iri = b["s"]["value"]
        if iri in seen:  # a record with >1 rdf:type would duplicate; keep the first
            continue
        seen.add(iri)
        title = b["title"]["value"]
        desc = b.get("desc", {}).get("value", "")
        chunks.append({
            "iri": iri,
            "title": title,
            "kind": _local(b["kind"]["value"]),
            "status": b.get("status", {}).get("value", ""),
            "text": f"# {title}\n\n{desc}".rstrip(),  # self-contained: title + rationale
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


async def main(corpus: str = "moosedev") -> None:
    out_json = await call_tool(config.MOOSEDEV_BIN, ["--connect"], corpus_env(corpus),
                               "sparql", {"query": QUERY})
    chunks = rows_to_chunks(out_json)
    out = config.corpus_chunks_path(corpus)
    out.write_text(json.dumps(chunks, indent=2))
    print(f"exported {len(chunks)} records -> {out}")


if __name__ == "__main__":
    asyncio.run(main(*(sys.argv[1:] or ["moosedev"])))
