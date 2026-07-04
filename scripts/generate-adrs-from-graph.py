#!/usr/bin/env python3
"""Render MOOSEDev ArchitecturalDecision records as docs/adr markdown files."""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.error
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


COUNT_QUERY = """\
PREFIX : <https://trivyn.io/ontologies/software/architecture#>
SELECT (COUNT(?ad) AS ?n) WHERE {
  GRAPH <https://moosedev.dev/kg/project> { ?ad a :ArchitecturalDecision . }
}
"""


ENUM_QUERY = """\
PREFIX : <https://trivyn.io/ontologies/software/architecture#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?ad ?title ?status ?ts ?author WHERE {
  GRAPH <https://moosedev.dev/kg/project> {
    ?ad a :ArchitecturalDecision ;
        rdfs:label ?title ;
        :hasLifecycleStatus ?status ;
        :hasTimestamp ?ts .
    OPTIONAL { ?ad :hasAuthor ?author }
  }
} ORDER BY ?ts ?ad
"""


CLUSTER_QUERY = """\
PREFIX : <https://trivyn.io/ontologies/software/architecture#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?ad ?dir ?rel ?node ?nlabel ?ndesc WHERE {{
  GRAPH <https://moosedev.dev/kg/project> {{
    VALUES ?ad {{ {values} }}
    {{
      ?ad ?p ?node . FILTER(isIRI(?node))
      BIND("out" AS ?dir)
      BIND(REPLACE(STR(?p), "^.*[/#]", "") AS ?rel)
      FILTER(?rel IN ("isMotivatedBy","weighs","resultsIn","concerns",
                      "hasRationale","supersedes","isSupersededBy"))
      OPTIONAL {{ ?node rdfs:label ?nlabel }}
      OPTIONAL {{ ?node :hasDescription ?ndesc }}
    }} UNION {{
      ?node :constrains ?ad .
      BIND("in" AS ?dir) BIND("constrains" AS ?rel)
      OPTIONAL {{ ?node rdfs:label ?nlabel }}
      OPTIONAL {{ ?node :hasDescription ?ndesc }}
    }} UNION {{
      ?ad :hasDescription ?ndesc .
      BIND("self" AS ?dir) BIND("hasDescription" AS ?rel) BIND(?ad AS ?node)
    }}
  }}
}} ORDER BY ?ad ?dir ?rel
"""


Binding = dict[str, dict[str, Any]]
Meta = dict[str, str]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate docs/adr from ArchitecturalDecision records in the MOOSEDev graph."
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path.cwd(),
        help="repository root containing .moosedev/http.addr and docs/ (default: cwd)",
    )
    parser.add_argument(
        "--addr",
        help="backend HTTP address, e.g. 127.0.0.1:7474 (default: read .moosedev/http.addr)",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        help="ADR output directory (default: <repo-root>/docs/adr)",
    )
    parser.add_argument(
        "--batch-size",
        type=int,
        default=20,
        help="number of decisions to fetch per cluster query (default: 20)",
    )
    parser.add_argument(
        "--no-check",
        action="store_true",
        help="skip the post-generation coverage/lifecycle check (it runs by default)",
    )
    # Deprecated: verification now runs by default. Accepted as a no-op so existing
    # callers (the skill, CI) keep working; use --no-check to opt out.
    parser.add_argument("--check", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument(
        "--json",
        action="store_true",
        help="emit a machine-readable summary",
    )
    return parser.parse_args()


def backend_addr(repo_root: Path, explicit_addr: str | None) -> str:
    if explicit_addr:
        return explicit_addr
    addr_path = repo_root / ".moosedev" / "http.addr"
    try:
        return addr_path.read_text(encoding="utf-8").strip()
    except FileNotFoundError as exc:
        raise SystemExit(
            f"missing {addr_path}; start the backend with `moosedev --serve` or pass --addr"
        ) from exc


def query(endpoint: str, sparql: str) -> list[Binding]:
    payload = json.dumps({"query": sparql}).encode("utf-8")
    request = urllib.request.Request(
        endpoint,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read().decode("utf-8")
    except urllib.error.URLError as exc:
        raise SystemExit(f"SPARQL request failed at {endpoint}: {exc}") from exc
    try:
        body = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SystemExit(
            f"SPARQL endpoint {endpoint} returned a non-JSON response "
            f"(is --addr pointing at the MOOSEDev backend?): {exc}"
        ) from exc
    # A 200 that is not a SPARQL SELECT result (e.g. a stale server, a proxy, or
    # the wrong endpoint) must not be silently read as "zero rows" — that would
    # let a misrouted --addr empty docs/adr. Require the SELECT result shape.
    results = body.get("results") if isinstance(body, dict) else None
    bindings = results.get("bindings") if isinstance(results, dict) else None
    if not isinstance(bindings, list):
        raise SystemExit(
            f"SPARQL endpoint {endpoint} returned a 200 response with no "
            "results.bindings; refusing to treat a non-SELECT response as an empty "
            "graph (check that --addr points at the MOOSEDev backend)."
        )
    return bindings


def value(row: Binding, key: str, default: str = "") -> str:
    term = row.get(key)
    if not term:
        return default
    raw = term.get("value", default)
    return str(raw)


def slugify(title: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")
    return slug or "decision"


def date_only(timestamp: str) -> str:
    return timestamp.split("T", 1)[0] if "T" in timestamp else timestamp[:10]


def md_cell(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def filename(meta: Meta) -> str:
    return f"{meta['num']}-{meta['slug']}.md"


def adr_link(meta: Meta) -> str:
    return f"[ADR-{meta['num']}]({filename(meta)})"


def node_bullet(row: Binding) -> str:
    label = value(row, "nlabel")
    desc = value(row, "ndesc")
    node = value(row, "node")
    if label and desc:
        return f"- {label}: {desc} (`{node}`)"
    if label:
        return f"- {label} (`{node}`)"
    if desc:
        return f"- {desc} (`{node}`)"
    return f"- `{node}`"


def render_status(
    meta: Meta,
    clusters: dict[str, dict[str, list[Binding]]],
    by_iri: dict[str, Meta],
) -> str:
    status = meta["status"].lower()
    if status == "accepted":
        return "Accepted"
    if status == "proposed":
        return "Proposed"
    if status == "deprecated":
        return "Deprecated"
    if status == "superseded":
        successors = [
            by_iri[value(row, "node")]
            for row in clusters[meta["iri"]]["isSupersededBy"]
            if value(row, "node") in by_iri
        ]
        if successors:
            return f"Superseded by {adr_link(successors[0])}"
        return "Superseded (successor not recorded)"
    return status.capitalize() if status else "not recorded"


def render_adr(
    meta: Meta,
    clusters: dict[str, dict[str, list[Binding]]],
    by_iri: dict[str, Meta],
) -> str:
    rows = clusters[meta["iri"]]
    lines = [
        f"# {meta['num']}. {meta['title']}",
        "",
        f"- Status: {render_status(meta, clusters, by_iri)}",
        f"- Date: {date_only(meta['ts'])}",
        f"- Author: {meta['author'] or 'not recorded'}",
    ]

    supersedes = [
        by_iri[value(row, "node")]
        for row in rows["supersedes"]
        if value(row, "node") in by_iri
    ]
    for older in supersedes:
        lines.append(f"- Supersedes: {adr_link(older)}")

    lines.extend(["", "## Context"])
    context_rows = rows["isMotivatedBy"] + rows["constrains"]
    if context_rows:
        lines.extend(node_bullet(row) for row in context_rows)
    else:
        lines.append("No motivating requirement or constraint recorded.")

    lines.extend(["", "## Decision"])
    self_descs = [value(row, "ndesc") for row in rows["hasDescription"] if value(row, "ndesc")]
    rationales = [value(row, "ndesc") for row in rows["hasRationale"] if value(row, "ndesc")]
    if self_descs:
        lines.append("\n\n".join(self_descs))
    else:
        lines.append("No decision description recorded.")
    lines.append("")
    if rationales:
        lines.append("\n\n".join(rationales))
    else:
        lines.append("No separate rationale recorded.")

    lines.extend(["", "## Considered Options"])
    if rows["weighs"]:
        lines.extend(node_bullet(row) for row in rows["weighs"])
    else:
        lines.append("No alternatives recorded.")

    lines.extend(["", "## Consequences"])
    if rows["resultsIn"]:
        lines.extend(node_bullet(row) for row in rows["resultsIn"])
    else:
        lines.append("No consequences recorded.")

    if rows["concerns"]:
        lines.extend(["", "## Affects"])
        for row in rows["concerns"]:
            label = value(row, "nlabel") or value(row, "node")
            lines.append(f"- {label} (`{value(row, 'node')}`)")

    lines.extend(
        [
            "",
            "---",
            f"Source: graph record `{meta['iri']}`. Generated view - regenerate from the graph; do not hand-edit.",
            "",
        ]
    )
    return "\n".join(lines)


def render_index(
    records: list[Meta],
    clusters: dict[str, dict[str, list[Binding]]],
    by_iri: dict[str, Meta],
) -> str:
    today = datetime.now(timezone.utc).date().isoformat()
    lines = [
        "# Architecture Decision Records",
        "",
        f"> **Generated view.** Rendered from the MOOSEDev knowledge graph on {today}.",
        "> The graph is the source of truth - **regenerate, do not hand-edit.** Scope: architectural",
        "> decisions only; constraints, patterns, and lessons are rendered by sibling artifact skills.",
        "",
        "| # | Title | Status | Date |",
        "|---|-------|--------|------|",
    ]
    for meta in records:
        status = render_status(meta, clusters, by_iri)
        status = re.sub(r"\[ADR-(\d{4})\]\([^)]+\)", r"ADR-\1", status)
        lines.append(
            f"| {meta['num']} | [{md_cell(meta['title'])}]({filename(meta)}) | "
            f"{md_cell(status)} | {date_only(meta['ts'])} |"
        )
    lines.append("")
    return "\n".join(lines)


def enumerate_records(endpoint: str) -> tuple[int, list[Meta]]:
    count_rows = query(endpoint, COUNT_QUERY)
    count = int(value(count_rows[0], "n", "0")) if count_rows else 0
    if count == 0:
        return 0, []

    records: list[Meta] = []
    seen_slugs: dict[str, int] = defaultdict(int)
    for idx, row in enumerate(query(endpoint, ENUM_QUERY), start=1):
        title = value(row, "title")
        base_slug = slugify(title)
        seen_slugs[base_slug] += 1
        slug = base_slug if seen_slugs[base_slug] == 1 else f"{base_slug}-{seen_slugs[base_slug]}"
        records.append(
            {
                "num": f"{idx:04d}",
                "iri": value(row, "ad"),
                "title": title,
                "status": value(row, "status"),
                "ts": value(row, "ts"),
                "author": value(row, "author"),
                "slug": slug,
            }
        )
    return count, records


def fetch_clusters(
    endpoint: str,
    records: list[Meta],
    batch_size: int,
) -> dict[str, dict[str, list[Binding]]]:
    clusters: dict[str, dict[str, list[Binding]]] = defaultdict(lambda: defaultdict(list))
    for record in records:
        _ = clusters[record["iri"]]
    for start in range(0, len(records), batch_size):
        batch = records[start : start + batch_size]
        values = " ".join(f"<{record['iri']}>" for record in batch)
        for row in query(endpoint, CLUSTER_QUERY.format(values=values)):
            clusters[value(row, "ad")][value(row, "rel")].append(row)
    return clusters


def render_all(
    records: list[Meta],
    clusters: dict[str, dict[str, list[Binding]]],
    by_iri: dict[str, Meta],
) -> dict[str, str]:
    """Render every output file to an in-memory {filename: content} map.

    Rendering happens before any filesystem mutation, so a render error aborts
    the run without leaving docs/adr half-written.
    """
    if not records:
        return {
            "0000-index.md": "# Architecture Decision Records\n\n"
            "No architectural decisions recorded yet.\n"
        }
    rendered = {filename(meta): render_adr(meta, clusters, by_iri) for meta in records}
    rendered["0000-index.md"] = render_index(records, clusters, by_iri)
    return rendered


def write_files(out_dir: Path, rendered: dict[str, str]) -> None:
    """Materialize the rendered ADR set into out_dir without a destructive window.

    Each file is written to a temp sibling and atomically renamed into place, so
    an interrupted write never corrupts an existing file. Stale ADR files that
    were not regenerated are pruned only after the new set is in place, so a
    failure leaves extra files behind, never a gap or an emptied directory.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, content in rendered.items():
        target = out_dir / name
        tmp = target.with_name(f".{name}.tmp")
        tmp.write_text(content, encoding="utf-8")
        tmp.replace(target)
    keep = set(rendered)
    for stale in out_dir.glob("[0-9][0-9][0-9][0-9]-*.md"):
        if stale.name not in keep:
            stale.unlink()


def summarize(
    out_dir: Path,
    count: int,
    records: list[Meta],
    clusters: dict[str, dict[str, list[Binding]]],
    by_iri: dict[str, Meta],
) -> dict[str, Any]:
    supersede_chains: list[str] = []
    missing_context: list[str] = []
    missing_decision: list[str] = []
    missing_successor: list[str] = []
    missing_reciprocal: list[str] = []

    for meta in records:
        rows = clusters[meta["iri"]]
        for row in rows["supersedes"]:
            older = by_iri.get(value(row, "node"))
            if older:
                supersede_chains.append(f"{older['num']} -> {meta['num']}")
                inverse_rows = clusters[older["iri"]]["isSupersededBy"]
                if not any(value(inverse, "node") == meta["iri"] for inverse in inverse_rows):
                    missing_reciprocal.append(f"{older['num']} -> {meta['num']}")
        if not (rows["isMotivatedBy"] or rows["constrains"]):
            missing_context.append(meta["num"])
        if not rows["hasDescription"]:
            missing_decision.append(meta["num"])
        if meta["status"].lower() == "superseded" and not rows["isSupersededBy"]:
            missing_successor.append(meta["num"])

    # adr_files is the count we are about to write (one per record); computed in
    # memory so verification can run BEFORE docs/adr is touched.
    return {
        "graph_decisions": count,
        "enumerated": len(records),
        "adr_files": len(records),
        "index_rows": len(records),
        "index": str(out_dir / "0000-index.md"),
        "supersede_chains": supersede_chains,
        "missing_context": missing_context,
        "missing_decision": missing_decision,
        "missing_successor": missing_successor,
        "missing_reciprocal": missing_reciprocal,
    }


def verify(summary: dict[str, Any]) -> None:
    if summary["graph_decisions"] != summary["adr_files"]:
        raise SystemExit(
            "coverage failed: graph decisions "
            f"{summary['graph_decisions']} != ADR files {summary['adr_files']}"
        )
    if summary["graph_decisions"] != summary["index_rows"]:
        raise SystemExit(
            "coverage failed: graph decisions "
            f"{summary['graph_decisions']} != index rows {summary['index_rows']}"
        )
    if summary["missing_successor"]:
        raise SystemExit(
            "lifecycle failed: superseded ADRs without isSupersededBy rows: "
            + ", ".join(summary["missing_successor"])
        )
    if summary["missing_reciprocal"]:
        raise SystemExit(
            "lifecycle failed: supersede chains without reciprocal inverse rows: "
            + ", ".join(summary["missing_reciprocal"])
        )


def main() -> None:
    args = parse_args()
    if args.batch_size < 1:
        raise SystemExit("--batch-size must be >= 1")

    repo_root = args.repo_root.resolve()
    out_dir = args.out_dir.resolve() if args.out_dir else repo_root / "docs" / "adr"
    endpoint = f"http://{backend_addr(repo_root, args.addr)}/api/v1/sparql/query"

    count, records = enumerate_records(endpoint)
    by_iri = {record["iri"]: record for record in records}
    clusters = fetch_clusters(endpoint, records, args.batch_size) if records else {}

    # Render and verify BEFORE touching docs/adr, so a failed check or a render
    # error leaves the previous good output in place. Verification is the default;
    # pass --no-check to skip it.
    rendered = render_all(records, clusters, by_iri)
    summary = summarize(out_dir, count, records, clusters, by_iri)
    if not args.no_check:
        verify(summary)
    write_files(out_dir, rendered)

    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print(
            "Generated {adr_files} ADR files from {graph_decisions} graph decisions. "
            "Index: {index}".format(**summary)
        )
        if summary["supersede_chains"]:
            print("Supersede chains: " + ", ".join(summary["supersede_chains"]))
        if summary["missing_context"]:
            print("Missing Context rows: " + ", ".join(summary["missing_context"]))
        if summary["missing_decision"]:
            print("Missing Decision descriptions: " + ", ".join(summary["missing_decision"]))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
