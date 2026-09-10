from cache import ResultCache
from rules import evaluate

class ScoringService:
    def __init__(self, ruleset, evaluator=evaluate):
        self.ruleset = ruleset
        self.evaluator = evaluator
        self.cache = ResultCache()

    def score(self, value):
        return self.cache.get_or_compute(self.ruleset, value, self.evaluator)
