"""Load immutable scenario inputs without exposing reference material to agents."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
from .artifacts import sha256_file


SCENARIOS = Path(__file__).parent / "scenarios"


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
    if scenario.get("schema_version") != 1 or scenario.get("id") != name:
        raise ValueError("scenario identity/schema mismatch")
    if scenario.get("track") not in ("inherited", "accumulation"):
        raise ValueError("unknown study track")
    episodes = scenario.get("episodes", [])
    if len(episodes) != 3 or len({e["id"] for e in episodes}) != 3:
        raise ValueError("pilot scenarios require three uniquely identified episodes")
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


def list_scenarios(directory: Path = SCENARIOS) -> list[str]:
    return sorted(p.parent.name for p in directory.glob("*/scenario.json"))


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
