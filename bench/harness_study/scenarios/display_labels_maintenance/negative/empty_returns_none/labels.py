"""Display-name formatting with stable scalar and batch APIs."""


def _normalize_name(name):
    stripped = name.strip()
    return stripped if stripped else "(unnamed)"


def render_name(name):
    return _normalize_name(name)


def render_names(names):
    return [_normalize_name(name) for name in names] or None
