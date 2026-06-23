"""Set-membership grader for capability_qa: recall / precision / F1 of an answer vs a graph-derived
expected set. Mirrors grade.grade's (answer, gt)->dict signature so run.py / regrade.py call it
identically. Singletons (chain/scalar answers) are size-1 sets, so this one grader covers
set-completeness / negation / supersession / multi-hop.

Matching is deliberately lenient on RECALL (the completeness signal we care about): an expected item
counts as found if its IRI appears, its title fuzzy-matches a predicted list item (difflib >= 0.88),
or its (distinctive, multi-word) title is contained in the answer prose. PRECISION guards against an
agent that dumps everything to game recall: predicted list items that match no expected item are
spurious. Stdlib only (difflib) — no new dependency.
"""
import difflib
import re

KG_IRI = re.compile(r"https?://[^\s)\]]*moosedev\.dev/kg/[^\s)\],]+")
_RATIO = 0.88


def _norm(s: str) -> str:
    """Lowercase; drop markdown emphasis/backticks and KG IRIs; collapse non-alnum to single spaces."""
    s = KG_IRI.sub(" ", s.lower()).replace("**", "").replace("`", "")
    return re.sub(r"[^a-z0-9]+", " ", s).strip()


def _is_table_sep(line: str) -> bool:
    """A markdown table separator row, e.g. `|---|---|` or `| :-- | --: |`."""
    return bool(re.match(r"^\s*\|?[\s:|-]*-{2,}[\s:|-]*\|?\s*$", line)) and "-" in line


def _predicted(text: str):
    """Pull candidate answer items out of a free-text / markdown answer.
    Returns (iris:set, titles:list[str]) — `titles` is one normalized title per list/table line."""
    iris = {m.group(0).rstrip(".,);") for m in KG_IRI.finditer(text)}
    titles = []
    lines = text.splitlines()
    for i, raw in enumerate(lines):
        if _is_table_sep(raw):
            continue  # separator row
        if "|" in raw and i + 1 < len(lines) and _is_table_sep(lines[i + 1]):
            continue  # table header row (the line directly above a separator)
        m = re.match(r"^\s*(?:[-*+]|\d+[.)]|\|)\s*(.+)$", raw)
        if not m:
            continue
        cell = m.group(1)
        if "|" in cell:  # markdown table row: take the first non-empty cell
            parts = [c.strip() for c in cell.split("|") if c.strip().strip("-")]
            cell = parts[0] if parts else cell
        cell = re.split(r"\s+[—–-]\s+|:\s", cell, 1)[0]  # cut a trailing " — desc" / ": desc"
        cell = re.sub(r"\s*\([^)]*\)\s*$", "", cell)  # cut a trailing "(accepted)"
        t = _norm(cell)
        if t:
            titles.append(t)
    return iris, titles


def grade_set(answer: str, gt: dict) -> dict:
    answer = answer or ""
    expected = gt.get("expected_set", [])
    n_expected = len(expected)
    pred_iris, pred_titles = _predicted(answer)
    norm_answer = _norm(answer)

    used = [False] * len(pred_titles)
    matched, missed = 0, []
    for e in expected:
        eid, et = e.get("iri", ""), _norm(e.get("title", ""))
        if eid and eid in pred_iris:  # 1) exact record id
            matched += 1
            continue
        best_j, best_r = -1, 0.0  # 2) best unused fuzzy title match
        for j, pt in enumerate(pred_titles):
            if used[j]:
                continue
            r = 1.0 if pt == et else difflib.SequenceMatcher(None, et, pt).ratio()
            if r > best_r:
                best_r, best_j = r, j
        if et and best_r >= _RATIO:
            used[best_j] = True
            matched += 1
            continue
        if et and len(et) >= 12 and et in norm_answer:  # 3) distinctive title embedded in prose
            matched += 1
            continue
        missed.append(e.get("title") or eid)

    # Predicted-item count for precision. A prose / scalar answer names items WITHOUT list formatting
    # ("The current decision is **X**") — those are still predictions (matched via containment), so the
    # count floors at `matched`; otherwise a correct one-line answer gets n_predicted=0 → precision 0 →
    # F1 0 (a grading artifact that scored exact-correct B1+B2 chain-head answers as failures).
    n_predicted = max(len(pred_titles), len(pred_iris), matched)
    recall = matched / n_expected if n_expected else 0.0
    precision = min(1.0, matched / n_predicted) if n_predicted else 0.0
    f1 = (2 * recall * precision / (recall + precision)) if (recall + precision) else 0.0
    return {
        "score": round(f1, 3), "passed": f1 >= 0.8,
        "recall": round(recall, 3), "precision": round(precision, 3), "f1": round(f1, 3),
        "n_expected": n_expected, "n_predicted": n_predicted, "n_matched": matched,
        "missed": missed[:25],
    }
