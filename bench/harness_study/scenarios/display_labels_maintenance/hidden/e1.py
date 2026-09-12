"""Private review tests. Execute with candidate workspace as the working directory."""
import importlib.util
import inspect
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("candidate_labels", Path.cwd() / "labels.py")
labels = importlib.util.module_from_spec(spec)
spec.loader.exec_module(labels)


class Behavior(unittest.TestCase):
    def test_scalar_examples(self):
        for value, expected in [("", "(unnamed)"), ("\t\n", "(unnamed)"),
                                ("  Ada  Lovelace  ", "Ada  Lovelace"),
                                ("\u2003Élodie 李\u2003", "Élodie 李"),
                                ("MiXeD", "MiXeD"), ("(unnamed)", "(unnamed)")]:
            with self.subTest(value=value):
                self.assertEqual(labels.render_name(value), expected)

    def test_batch_order_duplicates_and_input_preservation(self):
        values = [" z ", " A ", " z ", "", "\u2003李\u2003"]
        original = values.copy()
        self.assertEqual(labels.render_names(values), ["z", "A", "z", "(unnamed)", "李"])
        self.assertEqual(values, original)

    def test_generator_and_empty(self):
        seen = []
        def names():
            for name in [" A ", "", " A "]:
                seen.append(name)
                yield name
        self.assertEqual(labels.render_names(names()), ["A", "(unnamed)", "A"])
        self.assertEqual(seen, [" A ", "", " A "])
        self.assertEqual(labels.render_names(iter(())), [])

    def test_iterator_failure_propagates(self):
        sentinel = RuntimeError("input interrupted")
        def names():
            yield " A "
            raise sentinel
        with self.assertRaises(RuntimeError) as caught:
            labels.render_names(names())
        self.assertIs(caught.exception, sentinel)

    def test_public_signatures(self):
        for function, name in [(labels.render_name, "name"), (labels.render_names, "names")]:
            parameters = list(inspect.signature(function).parameters.values())
            self.assertEqual([(p.name, p.kind, p.default) for p in parameters],
                             [(name, inspect.Parameter.POSITIONAL_OR_KEYWORD,
                               inspect.Parameter.empty)])


class Extraction(unittest.TestCase):
    def test_both_entry_points_share_private_module_helper(self):
        candidates = [(name, function) for name, function in vars(labels).items()
                      if name.startswith("_") and not name.startswith("__")
                      and inspect.isfunction(function) and function.__module__ == labels.__name__]
        observed = []
        for name, original in candidates:
            calls = []
            marker = object()
            def replacement(value, *args, **kwargs):
                calls.append(value)
                return marker
            setattr(labels, name, replacement)
            try:
                scalar = labels.render_name(" scalar ")
                scalar_calls = calls.copy()
                calls.clear()
                batch = labels.render_names(iter([" first ", " second ", " first "]))
                shared = (scalar is marker and scalar_calls == [" scalar "]
                          and isinstance(batch, list) and len(batch) == 3
                          and all(value is marker for value in batch)
                          and calls == [" first ", " second ", " first "])
                observed.append((name, shared))
            except Exception:
                observed.append((name, False))
            finally:
                setattr(labels, name, original)
        self.assertTrue(any(shared for _, shared in observed),
                        "Both public APIs must delegate once per name to a shared private module-level helper; "
                        + repr(observed))


if __name__ == "__main__":
    unittest.main()
