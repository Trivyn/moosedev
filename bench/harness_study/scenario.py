"""Load immutable scenario inputs without exposing reference material to agents."""
from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path
from .artifacts import sha256_file


SCENARIOS = Path(__file__).parent / "scenarios"
MAINTENANCE = "display_labels_maintenance"
HORIZONS = ("pilot", "long")
PROBE_KINDS = ("task", "retained", "retention", "currency")
RETENTION_MEASURES = ("correctness", "cost")
LONG_EPISODE_COUNTS = (4, 5)
_LONG_FIELDS = {"schema_version", "id", "track", "title", "horizon", "initial_facts", "episodes",
                "negative_checks", "source_mapping", "dependency_map"}
_LONG_EPISODE_FIELDS = {"id", "prompt", "clarifications", "expected_fact_ids", "stale_fact_ids",
                        "visible_checks", "hidden_test", "reference", "allowed_paths", "probes",
                        "retired_tests"}
_PROBE_FIELDS = {"id", "kind", "test", "fact_ids", "decided_by", "measures"}
_NEGATIVE_FIELDS = {"id", "episode", "base_reference", "overlay", "expected", "visible", "fails_probes"}
_GOLD_FIELDS = {"schema_version", "scenario_id", "facts", "forbidden_claims", "review_status"}
_GOLD_FACT_FIELDS = {"id", "claim", "kind", "evidence", "introduced_episode", "seeded",
                     "superseded_episode", "superseded_by", "current_scope"}
_FORBIDDEN_FIELDS = {"claim", "applies_from_episode", "applies_through_episode"}
_SEED_FIELDS = {"id", "kind", "title", "description", "component", "relations", "evidence"}
_TEST_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*\.test[A-Za-z0-9_]*$")
_DECIDED_BY = re.compile(r"^scenario\.json#/(episodes|initial_facts)/(\d+)(/prompt)?$")


def relative_file(root: Path, name: str) -> Path:
    """Resolve a package file, refusing aliases and traversal in authored inputs."""
    path = Path(name)
    if path.is_absolute() or not path.parts or any(p in (".", "..") for p in path.parts):
        raise ValueError(f"unsafe scenario path: {name}")
    current = root
    for part in path.parts:
        current = current / part
        if current.is_symlink():
            raise ValueError(f"scenario path is a symlink: {name}")
    if not current.exists():
        raise ValueError(f"scenario path is missing: {name}")
    return current


def tree_manifest(root: Path) -> dict[str, str]:
    entries = {}
    for path in sorted(root.rglob("*")):
        if "__pycache__" in path.parts:
            continue
        if path.is_symlink():
            raise ValueError(f"scenario contains symlink: {path}")
        if path.is_file():
            entries[path.relative_to(root).as_posix()] = sha256_file(path)
    return entries


def package_hash(root: Path) -> str:
    return hashlib.sha256(json.dumps(tree_manifest(root), sort_keys=True).encode()).hexdigest()


def load_scenario(name: str, directory: Path = SCENARIOS) -> dict:
    root = relative_file(directory, name)
    scenario = json.loads((root / "scenario.json").read_text())
    if scenario.get("schema_version") == 2 and scenario.get("id") == name:
        return _load_long(root, scenario)
    if scenario.get("schema_version") != 1 or scenario.get("id") != name:
        raise ValueError("scenario identity/schema mismatch")
    if scenario.get("track") not in ("inherited", "accumulation"):
        raise ValueError("unknown study track")
    episodes = scenario.get("episodes", [])
    expected_episodes = 1 if name == MAINTENANCE else 3
    if len(episodes) != expected_episodes or len({e["id"] for e in episodes}) != expected_episodes:
        raise ValueError(f"scenario requires {expected_episodes} uniquely identified episodes")
    relative_file(root, "project")
    gold = json.loads(relative_file(root, "gold.json").read_text())
    if gold.get("scenario_id") != name or gold.get("schema_version") != 1:
        raise ValueError("gold identity/schema mismatch")
    fact_ids = [fact["id"] for fact in gold["facts"]]
    if len(set(fact_ids)) != len(fact_ids):
        raise ValueError("duplicate expected fact identity")
    for episode in episodes:
        if not episode.get("prompt", "").strip() or not episode.get("visible_checks"):
            raise ValueError("episode requires a prompt and visible verification commands")
        relative_file(root, episode["hidden_test"])
        relative_file(root, episode["reference"])
        if not set(episode["expected_fact_ids"]).issubset(fact_ids):
            raise ValueError("episode references an unknown expected fact")
    seed_ids = [fact["id"] for fact in scenario.get("initial_facts", [])]
    if len(seed_ids) != len(set(seed_ids)) or not set(seed_ids).issubset(fact_ids):
        raise ValueError("invalid seed fact identities")
    if scenario["track"] == "accumulation" and seed_ids:
        raise ValueError("accumulation scenario must not seed scored knowledge")
    # Hash all input files, including reference code: approval cannot survive an edit.
    scenario["package_sha256"] = package_hash(root)
    scenario["gold_sha256"] = hashlib.sha256((root / "gold.json").read_bytes()).hexdigest()
    return scenario


def _fields(value, allowed, what):
    if not isinstance(value, dict):
        raise ValueError(f"{what} must be an object")
    unknown = set(value) - allowed
    if unknown:
        raise ValueError(f"unknown {what} fields: {sorted(unknown)}")


def _load_long(root: Path, scenario: dict) -> dict:
    """Schema 2: several episodes whose later probes are decided by earlier reasons."""
    _fields(scenario, _LONG_FIELDS, "long-horizon scenario")
    if scenario.get("horizon") != "long":
        raise ValueError("schema 2 scenarios must declare the long horizon")
    if scenario.get("track") not in ("inherited", "accumulation"):
        raise ValueError("unknown study track")
    episodes = scenario.get("episodes", [])
    if len(episodes) not in LONG_EPISODE_COUNTS or len({e.get("id") for e in episodes}) != len(episodes):
        raise ValueError("long-horizon scenario requires 4 or 5 uniquely identified episodes")
    relative_file(root, "project")
    relative_file(root, scenario["source_mapping"])
    relative_file(root, scenario["dependency_map"])
    gold = json.loads(relative_file(root, "gold.json").read_text())
    _fields(gold, _GOLD_FIELDS, "gold")
    if gold.get("scenario_id") != scenario["id"] or gold.get("schema_version") != 2:
        raise ValueError("gold identity/schema mismatch")
    facts = {}
    for fact in gold["facts"]:
        _fields(fact, _GOLD_FACT_FIELDS, "gold fact")
        if fact["id"] in facts:
            raise ValueError("duplicate expected fact identity")
        facts[fact["id"]] = fact
    count = len(episodes)
    for fact in facts.values():
        introduced = fact["introduced_episode"]
        if not isinstance(introduced, int) or not 0 <= introduced <= count or (introduced == 0) != fact["seeded"]:
            raise ValueError(f"fact {fact['id']} has an invalid introduction")
        superseded = fact.get("superseded_episode")
        if superseded is not None:
            if not isinstance(superseded, int) or not introduced < superseded <= count:
                raise ValueError(f"fact {fact['id']} is superseded outside its lifetime")
            if fact.get("superseded_by") not in facts:
                raise ValueError(f"fact {fact['id']} is superseded by an unknown fact")
        elif "superseded_by" in fact or "current_scope" in fact:
            raise ValueError(f"fact {fact['id']} names a successor or scope without a supersession")
    for claim in gold["forbidden_claims"]:
        _fields(claim, _FORBIDDEN_FIELDS, "forbidden claim")
        start, end = claim["applies_from_episode"], claim.get("applies_through_episode")
        if not claim.get("claim", "").strip() or not 1 <= start <= count or (end is not None and not start <= end <= count):
            raise ValueError("forbidden claim has an invalid episode range")
    seeds = scenario.get("initial_facts", [])
    for seed in seeds:
        _fields(seed, _SEED_FIELDS, "seed fact")
    seed_ids = [seed["id"] for seed in seeds]
    if len(seed_ids) != len(set(seed_ids)) or {fid for fid, f in facts.items() if f["seeded"]} != set(seed_ids):
        raise ValueError("invalid seed fact identities")
    if (scenario["track"] == "accumulation") == bool(seed_ids):
        raise ValueError("accumulation scenarios seed nothing; inherited scenarios seed knowledge")
    probes = {}
    for index, episode in enumerate(episodes):
        number = index + 1
        _fields(episode, _LONG_EPISODE_FIELDS, "long-horizon episode")
        if not episode.get("prompt", "").strip() or not episode.get("visible_checks"):
            raise ValueError("episode requires a prompt and visible verification commands")
        relative_file(root, episode["hidden_test"])
        relative_file(root, episode["reference"])
        expected, stale = set(episode["expected_fact_ids"]), set(episode["stale_fact_ids"])
        if not (expected | stale) <= set(facts) or expected & stale:
            raise ValueError(f"episode {episode['id']} has unknown or contradictory fact identities")
        for fid, fact in facts.items():
            if fid in expected and fact["introduced_episode"] > number:
                raise ValueError(f"episode {episode['id']} expects a future fact: {fid}")
            retired = fact.get("superseded_episode") is not None and fact["superseded_episode"] <= number
            unscoped = retired and "current_scope" not in fact
            if (fid in stale) != unscoped or (fid in expected and unscoped):
                raise ValueError(f"episode {episode['id']} misclassifies superseded fact {fid}")
        kinds, tests = set(), set()
        for probe in episode["probes"]:
            _fields(probe, _PROBE_FIELDS, "probe")
            if probe["id"] in probes or probe["kind"] not in PROBE_KINDS:
                raise ValueError(f"duplicate or unknown probe: {probe.get('id')}")
            if (probe.get("measures") in RETENTION_MEASURES) != (probe["kind"] == "retention") or \
                    ("measures" in probe and probe["kind"] != "retention"):
                raise ValueError(f"probe {probe['id']}: retention probes, and only they, measure correctness or cost")
            if not _TEST_NAME.match(probe["test"]) or probe["test"] in tests:
                raise ValueError(f"probe {probe['id']} needs one unique Class.test_method")
            if not set(probe["fact_ids"]) <= set(facts) or not probe["fact_ids"]:
                raise ValueError(f"probe {probe['id']} cites unknown facts")
            decided = _DECIDED_BY.match(probe["decided_by"])
            if decided is None:
                raise ValueError(f"probe {probe['id']} has an unreadable deciding pointer")
            source, position = decided.group(1), int(decided.group(2))
            if source == "episodes" and (decided.group(3) is None or position >= count):
                raise ValueError(f"probe {probe['id']} must cite an episode prompt")
            if source == "initial_facts" and (decided.group(3) is not None or position >= len(seeds)):
                raise ValueError(f"probe {probe['id']} cites an unknown seed")
            earlier = source == "initial_facts" or position < index
            if (probe["kind"] == "task") != (source == "episodes" and position == index) or \
                    (probe["kind"] != "task" and not earlier):
                raise ValueError(f"probe {probe['id']} is not decided where its kind requires")
            kinds.add(probe["kind"])
            tests.add(probe["test"])
            probes[probe["id"]] = dict(probe, episode=episode["id"])
        if "task" not in kinds or (index and not kinds & {"retention", "currency"}):
            raise ValueError(f"episode {episode['id']} lacks its required task or retention/currency probe")
        previous = {p["test"] for p in episodes[index - 1]["probes"]} if index else set()
        if not set(episode["retired_tests"]) <= previous:
            raise ValueError(f"episode {episode['id']} retires a test the previous episode never ran")
    covered = set()
    by_id = {episode["id"]: episode for episode in episodes}
    for negative in scenario.get("negative_checks", []):
        _fields(negative, _NEGATIVE_FIELDS, "negative check")
        episode = by_id.get(negative["episode"])
        if (episode is None or negative["base_reference"] != episode["reference"]
                or negative["expected"] != "fail" or negative["visible"] != "pass"):
            raise ValueError(f"negative {negative.get('id')} must overlay its episode reference and fail hidden checks only")
        relative_file(root, negative["overlay"])
        own = {p["id"] for p in episode["probes"]}
        if not negative["fails_probes"] or not set(negative["fails_probes"]) <= own:
            raise ValueError(f"negative {negative['id']} must fail probes of its own episode")
        covered.update(negative["fails_probes"])
    uncovered = sorted(pid for pid, probe in probes.items()
                       if probe["kind"] in ("retention", "currency") and pid not in covered)
    if uncovered:
        raise ValueError(f"retention and currency probes need a negative: {uncovered}")
    scenario["package_sha256"] = package_hash(root)
    scenario["gold_sha256"] = hashlib.sha256((root / "gold.json").read_bytes()).hexdigest()
    return scenario


def list_scenarios(directory: Path = SCENARIOS, *, horizon: str = "pilot") -> list[str]:
    """Packages of one horizon; long-horizon packages never enter pilot consumers."""
    if horizon not in HORIZONS:
        raise ValueError(f"unknown scenario horizon: {horizon}")
    return sorted(p.parent.name for p in directory.glob("*/scenario.json")
                  if json.loads(p.read_text()).get("horizon", "pilot") == horizon)


def seed_notes(facts: list[dict]) -> str:
    """Render exactly the seed content also given to the graph condition."""
    blocks = ["# Starting project knowledge", "You may update these notes as the project changes."]
    if not facts:
        blocks.append("Component: Project")
    for fact in facts:
        blocks.extend([f"## {fact['id']}: {fact['title']}", f"Kind: {fact['kind']}",
                       f"Component: {fact['component']}", "Status: accepted", fact["description"]])
        blocks.extend(f"Relation: {r['predicate']} -> {r['target']}" for r in fact.get("relations", []))
        # Source mapping belongs to the private review package, not either agent
        # input. Both representations contain the same reviewed facts.
    return "\n\n".join(blocks) + "\n"
