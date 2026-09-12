"""Display-name formatting with stable scalar and batch APIs."""


def render_name(name):
    stripped = name.strip()
    return stripped if stripped else "(unnamed)"


def render_names(names):
    result = []
    for name in names:
        stripped = name.strip()
        result.append(stripped if stripped else "(unnamed)")
    return result
