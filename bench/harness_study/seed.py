"""One factual seed, represented as either notes or canonical graph input."""
import json
import uuid

from .scenario import seed_notes

ARCH = "https://trivyn.io/ontologies/software/architecture#"
GRAPH = "https://moosedev.dev/kg/project"
RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
LABEL = "http://www.w3.org/2000/01/rdf-schema#label"


def seed_iri(name):
    return "https://moosedev.dev/kg/study/" + str(uuid.uuid5(uuid.NAMESPACE_URL, name))


def seed_graph(scenario):
    """No helper LLM authors initial knowledge; stable IDs preserve exact parity."""
    triples = []

    def add(subject, predicate, value, iri=False):
        obj = f"<{value}>" if iri else json.dumps(value, ensure_ascii=False)
        triples.append(f"<{subject}> <{predicate}> {obj} <{GRAPH}> .")

    facts = scenario["initial_facts"]
    components = sorted({f["component"] for f in facts} or {"Project"})
    for component in components:
        subject = seed_iri(scenario["id"] + "/component/" + component)
        add(subject, RDF_TYPE, ARCH + "SystemComponent", True)
        add(subject, LABEL, component)
        add(subject, ARCH + "hasComponentName", component)
    for fact in facts:
        subject = seed_iri(scenario["id"] + "/fact/" + fact["id"])
        add(subject, RDF_TYPE, ARCH + fact["kind"], True)
        add(subject, LABEL, fact["title"])
        add(subject, ARCH + "hasTitle", fact["title"])
        add(subject, ARCH + "hasDescription", fact["description"])
        add(subject, ARCH + "hasLifecycleStatus", "accepted")
        add(subject, ARCH + "hasAuthor", "study-reviewed-seed")
        triples.append(f'<{subject}> <{ARCH}hasTimestamp> "2026-09-07T00:00:00Z"^^<http://www.w3.org/2001/XMLSchema#dateTime> <{GRAPH}> .')
        add(subject, ARCH + "concerns", seed_iri(scenario["id"] + "/component/" + fact["component"]), True)
        for relation in fact.get("relations", []):
            add(subject, ARCH + relation["predicate"],
                seed_iri(scenario["id"] + "/fact/" + relation["target"]), True)
    return "\n".join(sorted(triples)) + "\n"


GUIDANCE = {
    "without": "Project knowledge is in PROJECT_NOTES.md. Read it before changing code; update ordinary project notes when useful understanding changes.",
    "codex_mcp": "MOOSEDev is the project's persistent memory. Recall project knowledge before work; inspect entity dossiers before edits and capture durable decisions, requirements, constraints and lessons. Correct obsolete knowledge with supersede/retract. Use the available MOOSEDev tools.",
    "harness": "Use the harness's persistent project knowledge and ordinary coding workflow. Read relevant knowledge and preserve useful understanding when it changes.",
}
# Both MCP arms are offered the same tools and must be asked the same thing; sharing
# the string keeps a later edit from silently making the two conditions incomparable.
GUIDANCE["opencode_mcp"] = GUIDANCE["codex_mcp"]


def prepare_workspace(workspace, scenario, condition):
    if condition == "without":
        (workspace / "PROJECT_NOTES.md").write_text(seed_notes(scenario["initial_facts"]))
    else:
        (workspace / ".moosedev").mkdir()
        (workspace / ".moosedev" / "kg.nq").write_text(seed_graph(scenario))


def episode_prompt(episode, condition):
    # Every clarification is supplied upfront. No adaptive judge consults gold.
    return "\n\n".join([episode["prompt"], *episode["clarifications"].values(),
                         GUIDANCE[condition], "Visible verification: " + "; ".join(episode["visible_checks"])])
