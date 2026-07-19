"""Fixture for the tree-sitter Python fallback resolver."""

import collections

MAX_DEPTH = 3


def top_level(a, b):
    return a + b


def _decorate(func):
    return func


class Widget:
    label = "widget label"

    def render(self):
        local = 1
        return str(local)


def outer():
    def nested_fn():
        return 42

    return nested_fn


@_decorate
def decorated():
    return "decorated body"
