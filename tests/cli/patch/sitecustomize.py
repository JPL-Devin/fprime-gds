"""Loaded by the Python of `fprime-cli` in tests/cli/check.py only.

`fprime-cli events --json` stops with "Object of type ... is not JSON serializable" on
the first event that has an enumeration argument. This writes such values as text
instead, so that the remaining events can be compared.
"""
import json

_dumps = json.dumps


def dumps(obj, **options):
    options.setdefault("default", lambda value: value.val if hasattr(value, "val") else str(value))
    return _dumps(obj, **options)


json.dumps = dumps
