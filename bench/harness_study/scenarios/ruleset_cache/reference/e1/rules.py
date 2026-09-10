from dataclasses import dataclass

@dataclass(frozen=True)
class Ruleset:
    name: str
    revision: int
    multiplier: int

def evaluate(value: int, ruleset: Ruleset) -> int:
    return value * ruleset.multiplier
