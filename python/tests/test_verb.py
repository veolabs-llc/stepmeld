"""The verb protocol end to end over a run directory."""

import json

import pytest

from stepmeld import verb


def request(tmp_path, name="detect-targets", outputs=("targets",), **extra):
    doc = {"workflow": "wf", "step": "s", "run": "wf/s/1", "definition": {"name": name, "version": 1}, "inputs": {"images": ["a.jpg", "b.jpg"]}, "parameters": {"min-size": 12}, "outputs": [{"name": o, "tag": "t"} for o in outputs], **extra}
    tmp_path.mkdir(parents=True, exist_ok=True)
    (tmp_path / "request.json").write_text(json.dumps(doc))
    return tmp_path


def outcome(run_dir):
    return json.loads((run_dir / "outcome.json").read_text())


def test_a_verb_that_succeeds_writes_its_outputs_and_summary(tmp_path):
    def detect(run):
        run.progress("detecting", 1, len(run.input("images")), "images")
        run.succeed({"targets": [[1, 2.0, 3.0]]}, headline="1 target", metrics={"images": 2})

    assert verb.main({"detect-targets": detect}, [str(request(tmp_path))]) == 0
    out = outcome(tmp_path)
    assert out["state"] == "succeeded" and out["outputs"]["targets"] == [[1, 2.0, 3.0]] and out["summary"]["headline"] == "1 target"
    assert json.loads((tmp_path / "progress.json").read_text())["total"] == 2
    assert out["metrics"] == {"images": 2}


def test_a_refusal_and_a_crash_are_outcomes_too(tmp_path):
    def refuse(run):
        raise verb.Refused("no such file a.jpg")

    assert verb.main({"detect-targets": refuse}, [str(request(tmp_path))]) == 1
    assert outcome(tmp_path) == {"state": "failed", "class": "refused", "reason": "no such file a.jpg", "summary": {"headline": "no such file a.jpg"}}

    def crash(run):
        raise ValueError("boom")

    d = request(tmp_path / "crash")
    assert verb.main({"detect-targets": crash}, [str(d)]) == 1
    out = outcome(d)
    assert out["class"] == "fault" and "boom" in out["reason"] and "Traceback" in out["summary"]["details"]["traceback"]


def test_owed_outputs_and_silence_are_faults(tmp_path):
    def forgets(run):
        run.succeed({})

    assert verb.main({"detect-targets": forgets}, [str(request(tmp_path))]) == 1
    assert outcome(tmp_path)["class"] == "fault" and "targets" in outcome(tmp_path)["reason"]

    def silent(run):
        pass

    d = request(tmp_path / "silent")
    verb.main({"detect-targets": silent}, [str(d)])
    assert "without saying" in outcome(d)["reason"]


def test_an_unknown_verb_is_refused(tmp_path):
    d = request(tmp_path, name="nope")
    assert verb.main({"detect-targets": lambda r: None}, [str(d)]) == 1
    assert outcome(d)["class"] == "refused" and "detect-targets" in outcome(d)["reason"]


def test_a_missing_input_is_a_refusal(tmp_path):
    def needs(run):
        run.input("photos")

    verb.main({"detect-targets": needs}, [str(request(tmp_path))])
    assert outcome(tmp_path)["class"] == "refused"


@pytest.mark.parametrize("state", ["succeeded"])
def test_the_outcome_validates_against_the_contracts_outcome(tmp_path, state):
    import stepmeld

    def ok(run):
        run.succeed({"targets": 1}, headline="h")

    verb.main({"detect-targets": ok}, [str(request(tmp_path))])
    # an Outcome is the `outcome` definition of the workflow contract
    schema = stepmeld.schema("stepmeld/workflow.v1")
    import jsonschema

    jsonschema.Draft202012Validator({"$ref": "#/$defs/outcome", "$defs": schema["$defs"]}).validate(outcome(tmp_path))
