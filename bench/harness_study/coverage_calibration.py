"""Offline calibration of the harness plan-coverage check.

A port of the pure scorer in src/harness/coverage.rs (tokens from
src/harness/daemon/reconcile_score.rs), run over recorded plan summaries
against the governing Constraints of a scenario package read at run time: the
accepted Constraints of the deciding fact's component, which the linked-evidence
walk reaches from that component's code. Reports, for each threshold pair,
how often each rule would return a plan. Diagnostics only: no daemon, no model.

    python -m bench.harness_study.coverage_calibration --scenario late_fees_crowded
        --deciding-fact fees-np7 --plans target/harness-crowded-probe-v1/tiers-v1-plans.json
"""
import argparse
import json
from pathlib import Path
import re

from .artifacts import canonical_json
from .scenario import load_scenario
from .seed import episode_prompt, seed_iri

# Mirrors reconcile_score.rs STOPWORDS.
STOPWORDS = frozenset((
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "in", "is", "it", "its",
    "of", "on", "or", "that", "the", "this", "to", "was", "were", "with", "when", "which", "will",
    "not", "no", "we", "our", "so", "than", "then", "into", "over", "each", "every", "any", "all",
    "can", "must", "should", "never", "always", "only", "also"))
# Mirrors coverage.rs defaults.
DEFAULT_LABEL_MIN = 2
DEFAULT_CLAIM_MIN = 2
LABEL_MINS = (1, 2, 3, 4)
CLAIM_MINS = (1, 2, 3, 4)


def tokens(text):
    """Lowercased alphanumeric runs of at least two bytes, without stopwords."""
    words = (word.lower() for word in re.split(r"[\W_]+", text))
    return {word for word in words if len(word.encode()) >= 2 and word not in STOPWORDS}


def stem(token):
    """The frozen suffix fold (byte lengths, as in Rust)."""
    n = len(token.encode())
    if n > 4 and token.endswith("ies"):
        return token[:-3] + "y"
    if n > 5 and token.endswith("ing"):
        return token[:-3]
    if n > 4 and (token.endswith("ed") or token.endswith("es")):
        return token[:-2]
    if n > 3 and token.endswith("s") and not token.endswith("ss"):
        return token[:-1]
    return token


def stemmed_tokens(text):
    return {stem(token) for token in tokens(" ".join(word for word in text.split() if "://" not in word))}


def claim_values(claim):
    """Each `key: value` line without its predicate name."""
    lines = []
    for line in claim.splitlines():
        key, separator, value = line.partition(": ")
        lines.append(value if separator and key and all(ch.isalnum() or ch == "_" for ch in key) else line)
    return "\n".join(lines)


def assess(summary, background, iri, label, claim, label_min=DEFAULT_LABEL_MIN, claim_min=DEFAULT_CLAIM_MIN):
    background_tokens = stemmed_tokens(background)
    plan = stemmed_tokens(summary)
    label_distinctive = stemmed_tokens(label) - background_tokens
    claim_distinctive = stemmed_tokens(claim_values(claim)) - background_tokens
    label_matched = sorted(label_distinctive & plan)
    claim_matched = sorted(claim_distinctive & plan)
    covered = ((not label_distinctive and not claim_distinctive)
               or (bool(label_distinctive) and len(label_matched) >= min(label_min, len(label_distinctive)))
               or (bool(claim_distinctive) and len(claim_matched) >= min(claim_min, len(claim_distinctive))))
    return {"iri": iri, "label": label, "covered": covered, "label_matched": label_matched,
            "label_distinctive": len(label_distinctive), "claim_matched": claim_matched,
            "claim_distinctive": len(claim_distinctive), "label_min": label_min, "claim_min": claim_min}


def governing_rules(scenario, deciding_fact):
    """The accepted Constraints of the deciding fact's component, with claims rendered as the daemon renders them."""
    facts = scenario["initial_facts"]
    component = next(fact for fact in facts if fact["id"] == deciding_fact)["component"]
    rules = []
    for fact in sorted((fact for fact in facts if fact["kind"] == "Constraint" and fact["component"] == component),
                       key=lambda fact: fact["id"]):
        # Literal claims first, then links; URL words are ignored by the scorer.
        claim = "hasDescription: " + fact["description"] + "\n"
        claim += "concerns: " + seed_iri(scenario["id"] + "/component/" + component) + "\n"
        for relation in fact.get("relations", []):
            claim += relation["predicate"] + ": " + seed_iri(scenario["id"] + "/fact/" + relation["target"]) + "\n"
        rules.append({"fact": fact["id"], "iri": seed_iri(scenario["id"] + "/fact/" + fact["id"]),
                      "label": fact["title"], "claim": claim})
    return rules


def calibrate(scenario_id, deciding_fact, plans_path, guidance=""):
    scenario = load_scenario(scenario_id)
    # The runner's background: the task objective, then the human guidance.
    background = episode_prompt(scenario["episodes"][0], "harness") + " " + guidance
    rules = governing_rules(scenario, deciding_fact)
    plans = json.loads(Path(plans_path).read_text())
    matrix = []
    for label_min in LABEL_MINS:
        for claim_min in CLAIM_MINS:
            returns = {rule["fact"]: 0 for rule in rules}
            for plan in plans:
                for rule in rules:
                    if not assess(plan["summary"], background, rule["iri"], rule["label"], rule["claim"],
                                  label_min, claim_min)["covered"]:
                        returns[rule["fact"]] += 1
            matrix.append({"label_min": label_min, "claim_min": claim_min, "plans": len(plans),
                           "deciding_returns": returns[deciding_fact],
                           "distractor_returns": sum(count for fact, count in returns.items() if fact != deciding_fact),
                           "returns": returns})
    receipts = [{"plan": plan.get("name"), "receipts": [dict(assess(plan["summary"], background, rule["iri"],
                                                                     rule["label"], rule["claim"]), fact=rule["fact"])
                                                         for rule in rules]} for plan in plans]
    return {"scenario_id": scenario_id, "package_sha256": scenario["package_sha256"], "deciding_fact": deciding_fact,
            "plans_path": str(plans_path), "rules": [rule["fact"] for rule in rules],
            "defaults": {"label_min": DEFAULT_LABEL_MIN, "claim_min": DEFAULT_CLAIM_MIN},
            "matrix": matrix, "default_receipts": receipts}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--deciding-fact", required=True)
    parser.add_argument("--plans", type=Path, required=True)
    parser.add_argument("--guidance", default="")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = calibrate(args.scenario, args.deciding_fact, args.plans, args.guidance)
    if args.output:
        args.output.write_bytes(canonical_json(result))
    for cell in result["matrix"]:
        print(f"label_min={cell['label_min']} claim_min={cell['claim_min']}: deciding {cell['deciding_returns']}/"
              f"{cell['plans']}, distractor returns {cell['distractor_returns']} {cell['returns']}")


if __name__ == "__main__":
    main()
