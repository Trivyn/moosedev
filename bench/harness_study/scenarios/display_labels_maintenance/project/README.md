# Display labels

Public APIs: `render_name(name)` and `render_names(names)` in `labels.py`.
Inputs are strings. Trim surrounding whitespace using Python `str.strip()`;
use `(unnamed)` if the trimmed name is empty. Keep internal whitespace and
letter case. Batch input may be any iterable of strings; return a list in
input order, including duplicates, without changing input collections.
Iterator failures propagate when encountered, without consuming later values.
Run visible checks with `python3 -m unittest discover -s tests -v`.
