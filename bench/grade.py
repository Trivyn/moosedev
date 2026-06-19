"""Programmatic grader for context-recovery Q&A: keyword coverage + citation."""


def grade(answer: str, gt: dict) -> dict:
    a = (answer or "").lower()
    groups = gt.get("must_include_any", [])
    hits = sum(
        1 for grp in groups
        if any(v.lower() in a for v in (grp if isinstance(grp, list) else [grp]))
    )
    coverage = hits / len(groups) if groups else 0.0
    title = gt.get("decision_title", "")
    cited = bool(title) and title.lower() in a
    # Currency diagnostic: markers of the SUPERSEDED answer in the response. Reported, not
    # auto-penalized — a stale-only answer already fails via must_include_any (it lacks the current
    # markers); this flags when an arm SURFACED the stale decision (B1 currency-blind) vs not (B2).
    stale = [m for m in gt.get("stale_answer_markers", []) if m.lower() in a]
    score = round(0.7 * coverage + 0.3 * (1.0 if cited else 0.0), 3)
    return {"score": score, "passed": score >= 0.7, "coverage": round(coverage, 3),
            "cited": cited, "stale": stale}
