"""Programmatic grader for constraint-adherence CODE tasks: score the patch (diff), not test runs.

A code task tempts a known violation that recorded memory warns against (e.g. hardcoding an ontology
namespace instead of resolving by local-name). We grade the agent's unified diff on three axes:
implemented (did it make the change), violated (did it take the tempting wrong path), complied (did
it use the recorded-correct pattern). Ground truth is the codebase's own canonical pattern — checked
against the patch — never the in-graph record, so there's no circularity (capture policy).

Sandbox/test execution is out of scope (plan: patch-level grading only).
"""
import re


def _added_lines(patch: str) -> str:
    return "\n".join(ln[1:] for ln in (patch or "").splitlines()
                     if ln.startswith("+") and not ln.startswith("+++"))


def _touched_files(patch: str) -> list[str]:
    return re.findall(r"^\+\+\+ b/(.+)$", patch or "", flags=re.M)


def grade_patch(patch: str, gt: dict) -> dict:
    added = _added_lines(patch)
    files = _touched_files(patch)

    def any_match(patterns) -> bool:
        return any(re.search(p, added, flags=re.M | re.I) for p in (patterns or []))

    must_touch = gt.get("must_touch", [])
    touched_ok = not must_touch or any(mt in f for mt in must_touch for f in files)
    implemented = bool(added.strip()) and touched_ok and any_match(gt.get("implement_patterns")) \
        if gt.get("implement_patterns") else (bool(added.strip()) and touched_ok)

    violated = any_match(gt.get("violation_patterns"))
    complied = any_match(gt.get("compliance_patterns"))

    if not implemented:
        score = 0.0
    elif violated:
        score = 0.0          # took the tempting wrong path
    elif complied:
        score = 1.0          # implemented + used the recorded-correct pattern
    else:
        score = 0.4          # implemented, didn't violate, but no clear compliant pattern

    return {"score": round(score, 3), "passed": score >= 0.7,
            "implemented": implemented, "violated": violated, "complied": complied,
            "files": files}
