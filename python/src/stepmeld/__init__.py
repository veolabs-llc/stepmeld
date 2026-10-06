"""stepmeld for Python: the contracts' schemas, found by name, and the
verb protocol (`stepmeld.verb`).

The schemas here are a copy of `contracts/schemas` in the stepmeld
repository, kept equal by a test; the contracts are the documents, not
these files.
"""

from __future__ import annotations

import json
from functools import lru_cache
from pathlib import Path

import jsonschema

PREFIX = "stepmeld/"


def root() -> Path:
    return Path(__file__).parent / "schemas"


def schema_names() -> list[str]:
    return sorted(PREFIX + p.name.removesuffix(".schema.json") for p in root().glob("*.schema.json"))


def schema(name: str) -> dict:
    """The schema of a contract named `stepmeld/<name>.v<n>`."""
    short = name.removeprefix(PREFIX)
    path = root() / f"{short}.schema.json"
    if not path.exists():
        raise KeyError(f"no contract {name!r}; there are {schema_names()}")
    return json.loads(path.read_text())


@lru_cache(maxsize=None)
def _validator(name: str):
    return jsonschema.Draft202012Validator(schema(name))


def validate(doc: dict, name: str) -> None:
    """Hold `doc` to its contract; raises jsonschema.ValidationError
    saying where it departs."""
    _validator(name).validate(doc)


def check(doc: dict, name: str) -> str | None:
    """As `validate`, answering the first departure as text, or None."""
    errors = sorted(_validator(name).iter_errors(doc), key=lambda e: list(e.path))
    if not errors:
        return None
    e = errors[0]
    where = "/".join(str(p) for p in e.path) or "(root)"
    return f"{e.message} at {where}"
