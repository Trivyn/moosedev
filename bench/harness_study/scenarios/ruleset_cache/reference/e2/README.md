# Local scoring service

Python standard library only. `Ruleset(name, revision, multiplier)` is immutable;
`ScoringService(ruleset, evaluator=rules.evaluate)` scores integer inputs. An
injected evaluator follows `(value, ruleset) -> int` and lets callers measure
work. Public results are integers. No persistence or concurrent callers are needed.

Run visible tests with `python3 -m unittest discover -s tests -v`.
