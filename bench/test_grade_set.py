"""Regression tests for grade_set's identifier matching.

The grader's contract (its own docstring) is that an expected item "counts as found if its
IRI appears". A model that writes IRIs the natural markdown way — inside code spans — broke
that silently: the IRI pattern did not stop at a backtick, so every extracted id carried a
trailing ` and matched nothing, while `_norm` deleted the IRIs when building titles so those
lines produced no title either. Both match paths failed at once and a perfect answer scored
0.000 (gemma-4-26b-a4b, neg_constraints_no_rationale, 2026-09-19: 74 predicted, 0 matched,
recall 1.000 once fixed).
"""
import grade_set

GT = {
    "answer_kind": "set",
    "expected_set": [
        {"iri": "https://moosedev.dev/kg/Constraint/6cccbe63-e3bd-4991-8ba2-f5928e7b37ae",
         "title": "Serialized, fail-closed quota admission"},
        {"iri": "https://moosedev.dev/kg/Constraint/2692d146-3279-490c-91b4-5414ae488fc6",
         "title": "Geospatial ingest currently degrades to strings"},
    ],
}


def test_backticked_iris_match():
    """A markdown code span is the normal way to write an IRI; it must not zero the answer."""
    answer = (
        "The following Constraints have no recorded rationale:\n\n"
        "- `https://moosedev.dev/kg/Constraint/6cccbe63-e3bd-4991-8ba2-f5928e7b37ae`\n"
        "- `https://moosedev.dev/kg/Constraint/2692d146-3279-490c-91b4-5414ae488fc6`\n"
    )
    r = grade_set.grade_set(answer, GT)
    assert r["n_matched"] == 2, f"backticked IRIs must match: {r}"
    assert r["recall"] == 1.0


def test_bare_iris_still_match():
    answer = (
        "- https://moosedev.dev/kg/Constraint/6cccbe63-e3bd-4991-8ba2-f5928e7b37ae\n"
        "- https://moosedev.dev/kg/Constraint/2692d146-3279-490c-91b4-5414ae488fc6\n"
    )
    assert grade_set.grade_set(answer, GT)["n_matched"] == 2


def test_punctuated_iris_still_match():
    """The trailing-punctuation strip that predates the backtick fix must survive it."""
    answer = (
        "- (https://moosedev.dev/kg/Constraint/6cccbe63-e3bd-4991-8ba2-f5928e7b37ae);\n"
        "- https://moosedev.dev/kg/Constraint/2692d146-3279-490c-91b4-5414ae488fc6.\n"
    )
    assert grade_set.grade_set(answer, GT)["n_matched"] == 2


def test_titles_still_match():
    answer = (
        "- Serialized, fail-closed quota admission\n"
        "- Geospatial ingest currently degrades to strings\n"
    )
    assert grade_set.grade_set(answer, GT)["n_matched"] == 2


def test_wrong_answer_still_scores_zero():
    """The fix must not make matching so lenient that a wrong answer passes."""
    answer = "- `https://moosedev.dev/kg/Constraint/00000000-0000-0000-0000-000000000000`\n"
    assert grade_set.grade_set(answer, GT)["n_matched"] == 0
