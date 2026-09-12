"""Unscored Stage 2 readiness proof, with no agent or helper chat requests."""
import hashlib
import uuid

from .seed import seed_iri


CANARY_SOURCE = '''"""Disposable evolution preflight; never a scored workspace."""


def _preflight_normalize_name(name):
    stripped = name.strip()
    return stripped if stripped else "(unnamed)"


def _preflight_identity(value):
    return value


def render_name(name):
    return _preflight_normalize_name(name)


def render_names(names):
    return [_preflight_normalize_name(name) for name in names]
'''


def _pages(daemon, route, request, receipts):
    """Keep exact requests, including failures, and reject non-progressing cursors."""
    cursor, seen, identities, candidates = None, set(), set(), []
    snapshot = None
    purpose = route.endswith("/purpose/candidates")
    handles = set()
    for _ in range(256):
        payload = dict(request, cursor=cursor, limit=1)
        receipt = {"request": payload}
        receipts.append(receipt)
        response = daemon._request(route, payload)
        receipt["response"] = response
        identity = ((response.get("revision"),) if purpose else (
            response.get("knowledge_revision"), response.get("index", {}).get("revision"),
            response.get("scope_digest"), response.get("index", {}).get("status")))
        if not all(isinstance(value, str) and value for value in identity):
            raise RuntimeError("candidate page lacks a complete snapshot identity")
        if snapshot is not None and identity != snapshot:
            raise RuntimeError("candidate pagination changed snapshot identity")
        snapshot = identity
        if not purpose:
            expected_refresh = ("refreshed" if cursor is None
                                and request["refresh_policy"] == "supported_frozen" else "not_requested")
            if response.get("index", {}).get("refresh_action") != expected_refresh:
                raise RuntimeError("candidate paging did not preserve the single-refresh contract")
        page = response.get("candidates")
        if not isinstance(page, list) or len(page) > 1:
            raise RuntimeError("candidate endpoint did not honor the bounded page contract")
        for candidate in page:
            key = candidate.get("iri" if purpose else "id")
            if not isinstance(key, str) or not key or key in identities:
                raise RuntimeError("candidate pagination repeated or omitted an identity")
            identities.add(key)
            if purpose:
                handle = candidate.get("handle")
                if not isinstance(handle, str) or not handle or handle in handles:
                    raise RuntimeError("purpose pagination repeated or omitted a choice handle")
                handles.add(handle)
        candidates.extend(page)
        cursor = response.get("next_cursor")
        if cursor is None:
            return candidates
        if not page or not isinstance(cursor, str) or not cursor or cursor in seen:
            raise RuntimeError("candidate pagination did not make progress")
        seen.add(cursor)
    raise RuntimeError("disposable candidate inventory exceeded the preflight page bound")


def _helper(candidates):
    matches = [candidate for candidate in candidates
               if candidate.get("name") == "_preflight_normalize_name"
               or (candidate.get("symbol") or "").endswith("/_preflight_normalize_name().")]
    if len(matches) != 1:
        raise RuntimeError("fresh index must discover exactly one newly written private helper")
    candidate = matches[0]
    if (not candidate.get("symbol") or not candidate.get("definition_range")
            or candidate.get("scope_basis") not in {"changed_definition", "enclosing_definition"}):
        raise RuntimeError("helper discovery lacks a proven current definition")
    return candidate


def probe_evolution(daemon, workspace, scenario, observed):
    """Exercise discovery and existing reviewed-link operations on known canary code."""
    context_request = observed["context_request"] = {
        "topic": "display name normalization and maintenance", "files": ["labels.py"]}
    context = daemon._request("/api/v1/harness/context", context_request)
    observed["context"] = context
    if 2 not in (context.get("intent_contracts") or []):
        raise RuntimeError("Stage 2 requires daemon intent contract 2")
    expected = {seed_iri(scenario["id"] + "/fact/" + fact["id"]): fact
                for fact in scenario["initial_facts"]}
    purpose_receipts = observed["purpose_pages"] = []
    purposes = _pages(daemon, "/api/v1/harness/intent/purpose/candidates", {
        "objective": "Preserve display name normalization and iterable maintenance behavior",
        "files": ["labels.py"]}, purpose_receipts)
    by_iri = {candidate["iri"]: candidate for candidate in purposes}
    if not expected.keys() <= by_iri.keys():
        raise RuntimeError("purpose discovery omitted applicable accepted seed records")
    for iri, fact in expected.items():
        candidate = by_iri[iri]
        values = {literal.get("value") for literal in candidate.get("claim", {}).get("literals", [])}
        if candidate.get("lifecycle") != "accepted" or fact["description"] not in values:
            raise RuntimeError("purpose choice must include the complete accepted seed claim")

    source = workspace / "labels.py"
    before = source.read_bytes()
    after = CANARY_SOURCE.encode()
    observed["source"] = {"before": before.decode(), "after": CANARY_SOURCE}
    source.write_bytes(after)
    changed = {"file": "labels.py", "before_digest": hashlib.sha256(before).hexdigest(),
               "after_digest": hashlib.sha256(after).hexdigest(),
               "changed_ranges": [{"start": {"line": 0, "col": 0},
                                   "end": {"line": CANARY_SOURCE.count("\n"), "col": 0}}]}
    receipts = observed["postedit_pages"] = []
    candidates = _pages(daemon, "/api/v1/harness/intent/candidates", {
        "files": [changed], "refresh_policy": "supported_frozen"}, receipts)
    if len(receipts) < 2:
        raise RuntimeError("the multi-definition canary must exercise continuation paging")
    for receipt in receipts:
        page = receipt["response"]
        if page.get("unresolved") or page.get("index", {}).get("status") != "current":
            raise RuntimeError("post-edit canary did not resolve against a current source index")
    helper = _helper(candidates)
    choices = {choice["iri"]: choice for choice in helper.get("record_choices", [])}
    if helper.get("source_digest") != changed["after_digest"] or not expected.keys() <= choices.keys():
        raise RuntimeError("new helper lacks its current source proof or both seed choices")
    for iri, fact in expected.items():
        predicate = "constrains" if fact["kind"] == "Constraint" else "concerns"
        if predicate not in choices[iri].get("legal_predicates", []):
            raise RuntimeError("candidate omitted the seed's legal association predicate")

    request = {"operation_id": str(uuid.uuid4()),
               "revision": receipts[-1]["response"]["knowledge_revision"],
               "bindings": [{"record_iri": iri, "file": "labels.py", "symbol": helper["symbol"],
                             "source_digest": helper["source_digest"]} for iri in sorted(expected)]}
    observed["link_request"] = request
    staged = observed["link_response"] = daemon._request("/api/v1/harness/intent/link", request)
    if staged.get("unresolved") or len(staged.get("links", [])) != len(expected):
        raise RuntimeError("canary helper associations did not produce the expected exact links")
    fields = ("record_iri", "file", "symbol", "source_digest")
    expected_bindings = {tuple(binding.get(field) for field in fields) for binding in request["bindings"]}
    resolved_bindings = {tuple(binding.get(field) for field in fields) for binding in staged.get("resolved", [])}
    if len(set(staged["links"])) != len(expected) or resolved_bindings != expected_bindings:
        raise RuntimeError("link response did not bind the exact requested helper associations")
    review_request = observed["link_review_request"] = {"operation_id": request["operation_id"], "accept": True}
    reviewed = observed["link_review"] = daemon._request("/api/v1/harness/intent/review", review_request)
    if reviewed.get("conforms") is not True or reviewed.get("durable") is not True or reviewed.get("pending"):
        raise RuntimeError("canary associations did not reach a durable reviewed checkpoint")
    observed["link_replay_request"] = request
    replay = observed["link_replay"] = daemon._request("/api/v1/harness/intent/link", request)
    if replay != staged:
        raise RuntimeError("reviewed link operation did not replay its exact retained response")
    reused_receipts = observed["linked_candidate_pages"] = []
    linked = _helper(_pages(daemon, "/api/v1/harness/intent/candidates", {
        "files": [changed], "refresh_policy": "none"}, reused_receipts))
    if not expected.keys() <= set(linked.get("existing_record_iris", [])):
        raise RuntimeError("candidate discovery failed to expose already reviewed helper links")
    observed["passed"] = True
