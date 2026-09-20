"""Capture fidelity: does a captured record describe the decision the episode made.

This is the one place in the study that asks a model anything about meaning, and
it is deliberately narrow. The split is symbolic first:

- `capture.py` deterministically extracts which records an episode created and
  the exact lines of the sealed `kg.nq` that assert them. The judge never
  invents an evidence span, which is what keeps `grading._validate_spans`
  satisfiable and keeps the model a sensor rather than the controller.
- The model is asked only for a verdict per expected fact, against the gold
  claim and the record text.

The output is an ordinary judgment in the schema `review` already ingests and
`regrade` already replays. It is NOT a new scoring path: semantic truth still
comes exclusively from retained judgments, and a judgment written by a model is
labelled as such by its `reviewer_id` so it can never be mistaken for a human's.

The judge is BLIND to arm: the prompt carries gold claims and record text, never
the backend, condition or model that produced them.
"""
import json
from pathlib import Path
import urllib.error
import urllib.request

from . import capture
from .artifacts import ArtifactStore

TIMEOUT = 180
SCHEMA = {
    "type": "object", "additionalProperties": False,
    "required": ["assessments", "forbidden"],
    "properties": {
        "assessments": {"type": "array", "items": {
            "type": "object", "additionalProperties": False,
            "required": ["fact_id", "verdict", "record", "rationale"],
            "properties": {
                "fact_id": {"type": "string"},
                "verdict": {"type": "string", "enum": ["supported", "missing"]},
                "record": {"type": ["string", "null"]},
                "rationale": {"type": "string"},
            }}},
        "forbidden": {"type": "array", "items": {
            "type": "object", "additionalProperties": False,
            "required": ["claim", "asserted_by", "rationale"],
            "properties": {
                "claim": {"type": "string"},
                "asserted_by": {"type": ["string", "null"]},
                "rationale": {"type": "string"},
            }}},
    },
}

INSTRUCTION = (
    "You are grading whether a software project's captured knowledge records express specific "
    "expected claims. For each expected claim, answer `supported` only if some record states that "
    "claim in substance -- a paraphrase counts, and a record may support a claim it does not quote. "
    "Answer `missing` if no record states it. Name the supporting record by its exact id, or null. "
    "Then, for each forbidden claim, say which record asserts it, or null if none does. "
    "Judge only what the records say. Do not reward a well-formed record that says nothing relevant."
)


def resolve_model(base_url, model, api_key):
    """Fail closed if the literal model id is not served.

    A judge silently substituted for another model makes every judgment it wrote
    unattributable, and a deleted model has already cost this project one
    unidentifiable grading pass.
    """
    request = urllib.request.Request(base_url.rstrip("/") + "/models",
                                     headers={"Authorization": f"Bearer {api_key}"})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            served = json.loads(response.read())
    except (urllib.error.URLError, ValueError) as error:
        raise ValueError(f"judge provider did not answer a model listing: {error}") from error
    names = {item.get("id") for item in served.get("data") or [] if isinstance(item, dict)}
    if model not in names:
        raise ValueError(f"judge model {model!r} is not served by {base_url}; refusing to substitute")
    return model


def ask(base_url, model, api_key, prompt):
    """One judging call against an OpenAI-compatible endpoint, temperature zero."""
    body = json.dumps({
        "model": model, "temperature": 0,
        "messages": [{"role": "system", "content": INSTRUCTION},
                     {"role": "user", "content": prompt}],
        "response_format": {"type": "json_schema",
                            "json_schema": {"name": "capture_fidelity", "strict": True, "schema": SCHEMA}},
    }).encode()
    request = urllib.request.Request(
        base_url.rstrip("/") + "/chat/completions", data=body,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"})
    with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
        payload = json.loads(response.read())
    return json.loads(payload["choices"][0]["message"]["content"])


def spans(text, iri, path):
    """Contiguous sealed line ranges asserting one record, 1-based and inclusive."""
    lines = [index for index, line in enumerate(text.splitlines(), 1)
             if line.startswith(f"<{iri}> ")]
    ranges = []
    for line in lines:
        if ranges and line == ranges[-1]["end_line"] + 1:
            ranges[-1]["end_line"] = line
        else:
            ranges.append({"path": path, "start_line": line, "end_line": line})
    return ranges


def episode_question(gold, scenario, episode_id, created):
    """The blind prompt for one episode, plus the facts and forbidden claims it covers."""
    index = [episode["id"] for episode in scenario["episodes"]].index(episode_id)
    expected = set(scenario["episodes"][index]["expected_fact_ids"])
    facts = [fact for fact in gold["facts"] if fact["id"] in expected]
    forbidden = [item for item in gold.get("forbidden_claims", [])
                 if item.get("applies_from_episode", 1) <= index + 1
                 and (item.get("applies_through_episode") is None
                      or item["applies_through_episode"] >= index + 1)]
    lines = ["RECORDS CAPTURED SO FAR:"]
    if created:
        for iri, record in created.items():
            lines.append(f"\n- id: {iri}\n  kind: {record.get('kind')}\n  title: {record.get('title')}"
                         f"\n  description: {record.get('description')}")
    else:
        lines.append("\n(none)")
    lines.append("\n\nEXPECTED CLAIMS:")
    for fact in facts:
        lines.append(f"\n- fact_id: {fact['id']}\n  kind: {fact['kind']}\n  claim: {fact['claim']}")
    lines.append("\n\nFORBIDDEN CLAIMS (these must NOT be asserted):")
    lines.extend(f"\n- {item['claim']}" for item in forbidden) or lines.append("\n(none)")
    return "".join(lines), facts, forbidden


def judge_run(store_root, run_id, *, model, base_url, api_key, reviewer_id=None, caller=None):
    """Produce a judgment for one sealed run. `caller` is injected in tests."""
    store = ArtifactStore(Path(store_root))
    run = store.run_path(store.root / "runs" / run_id)
    seal = store._verify(run)
    manifest = json.loads((run / "manifest.json").read_text())
    scenario = json.loads((run / "scenario.json").read_text())
    gold = json.loads((run / "scenario" / "gold.json").read_text())
    outcome = json.loads((run / "outcome.json").read_text())
    episodes = [episode["id"] for episode in outcome.get("episodes", [])
                if episode.get("status") != "unattempted"]
    if caller is None:
        resolve_model(base_url, model, api_key)
        caller = lambda prompt: ask(base_url, model, api_key, prompt)

    previous, claims = capture.records(capture.graph_at(run, "initial") or ""), []
    for episode_id in episodes:
        text = capture.graph_at(run, f"episodes/{episode_id}/workspace")
        if text is None:
            continue
        current = capture.records(text)
        created = {iri: current[iri] for iri in current if iri not in previous}
        previous = current
        path = f"episodes/{episode_id}/workspace/.moosedev/kg.nq"
        if path not in seal["files"]:
            continue
        prompt, facts, forbidden = episode_question(gold, scenario, episode_id, created)
        answer = caller(prompt)
        known = {fact["id"] for fact in facts}
        for item in answer.get("assessments", []):
            if item.get("fact_id") not in known:
                continue
            evidence = spans(text, item["record"], path) if item.get("record") in created else []
            verdict = "supported" if (item.get("verdict") == "supported" and evidence) else "missing"
            claims.append({
                "claim_id": f"{episode_id}:{item['fact_id']}", "verdict": verdict,
                "episode_id": episode_id, "fact_id": item["fact_id"], "evidence": evidence,
                "rationale": item.get("rationale") or "no record stated this claim",
            })
        for position, item in enumerate(answer.get("forbidden", [])):
            evidence = spans(text, item.get("asserted_by"), path) if item.get("asserted_by") in created else []
            if not evidence:
                continue
            claims.append({
                "claim_id": f"{episode_id}:forbidden:{position}", "verdict": "unsupported",
                "episode_id": episode_id, "evidence": evidence,
                "rationale": item.get("rationale") or "asserts a forbidden claim",
            })
    if not claims:
        raise ValueError("no judgeable captured knowledge in this run; do not file an empty judgment")
    return {"reviewer_id": reviewer_id or f"judge:{model}",
            "scenario_gold_sha256": manifest.get("scenario_gold_sha256"),
            "judge": {"model": model, "base_url": base_url, "blind_to_arm": True},
            "claims": claims}
