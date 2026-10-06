"""The schemas here are the repository's, and the fixtures validate."""

import json
from pathlib import Path

import pytest

import stepmeld

REPO = Path(__file__).resolve().parents[2]


def test_the_package_schemas_are_the_contracts():
    ours = {p.name: p.read_text() for p in stepmeld.root().glob("*.schema.json")}
    theirs = {p.name: p.read_text() for p in (REPO / "contracts/schemas").glob("*.schema.json")}
    assert ours == theirs, "python/src/stepmeld/schemas is a copy of contracts/schemas; copy again"


@pytest.mark.parametrize("path", sorted((REPO / "contracts/fixtures").rglob("*.json")))
def test_every_fixture_validates(path):
    name = "stepmeld/" + path.parent.name
    if path.parent.name == "scenarios":
        pytest.skip("scenario fixtures are not documents")
    doc = json.loads(path.read_text())
    problem = stepmeld.check(doc, name)
    if path.name.endswith(".refused.json"):
        assert problem is not None
    else:
        assert problem is None, f"{path}: {problem}"


def test_an_unknown_contract_is_named():
    with pytest.raises(KeyError, match="no contract"):
        stepmeld.schema("stepmeld/nope.v1")
