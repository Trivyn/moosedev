"""Programmatic grader for context-recovery Q&A: keyword coverage + citation."""

import re

# Negation cues that flip a superseded-state marker from "asserted as current" to "contrasted
# away". A good CURRENT answer naturally names the old state to deny it ("it is NOT a temporary
# stub anymore", "you do NOT need to start --serve yourself") — those mentions must not count as
# the arm serving stale info. Cues are matched in a small window preceding each marker occurrence.
# NOTE: keyword staleness is inherently brittle — a current answer can also *narrate* the old
# state without an adjacent negation ("the old stub was intentionally temporary, now replaced").
# The reliable currency signal is `coverage` of the CURRENT markers (must_include_any); this
# diagnostic is a best-effort hint, not the verdict. See bench lessons on currency grading.
_NEGATION_CUES = (
    "not", "n't", "no longer", "never", "instead of", "rather than", "no need",
    "without", "stopped", "replaced", "retired", "deprecat", "superseded", "no more",
)
_NEG_WINDOW = 56  # chars before a marker scanned for a negation cue


def _normalize(s: str) -> str:
    """Lowercase; collapse markdown/punctuation to spaces so `**not**` reads as the cue `not`.
    Apostrophes are kept so contractions (n't) survive. Padded with spaces for word-boundary tests."""
    return " " + re.sub(r"[^a-z0-9']+", " ", s.lower()).strip() + " "


def _asserted_stale(marker: str, text: str) -> bool:
    """True iff `marker` appears at least once in `text` NOT negated by a preceding cue.

    Substring presence alone over-counts: the current answer contrasts against the old state,
    so we only flag a marker when some occurrence is asserted (no negation cue in its window).
    """
    m = marker.lower()
    start = 0
    while True:
        i = text.find(m, start)
        if i == -1:
            return False
        window = _normalize(text[max(0, i - _NEG_WINDOW):i])  # padded with spaces both ends
        negated = any(
            (cue in window) if "'" in cue else (f" {cue} " in window)
            for cue in _NEGATION_CUES
        )
        if not negated:
            return True  # an asserted (non-negated) occurrence → genuinely surfaced as current
        start = i + len(m)


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
    # Currency diagnostic: markers of the SUPERSEDED answer ASSERTED as current in the response.
    # Reported, not auto-penalized — a stale-only answer already fails via must_include_any (it
    # lacks the current markers); this flags when an arm SURFACED the stale decision (B1
    # currency-blind) vs not (B2). Negation-aware so a current answer that denies the old state
    # ("no longer a temporary stub") is not falsely flagged.
    stale = [m for m in gt.get("stale_answer_markers", []) if _asserted_stale(m, a)]
    score = round(0.7 * coverage + 0.3 * (1.0 if cited else 0.0), 3)
    return {"score": score, "passed": score >= 0.7, "coverage": round(coverage, 3),
            "cited": cited, "stale": stale}
