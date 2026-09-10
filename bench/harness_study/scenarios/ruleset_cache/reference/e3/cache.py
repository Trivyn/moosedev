class ResultCache:
    """Results partitioned by immutable ruleset identity and input."""
    def __init__(self):
        self.entries = {}

    def get_or_compute(self, ruleset, value, evaluator):
        key = (ruleset.name, ruleset.revision, value)
        if key not in self.entries:
            self.entries[key] = evaluator(value, ruleset)
        return self.entries[key]
