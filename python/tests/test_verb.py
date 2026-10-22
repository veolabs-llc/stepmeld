"""The verb protocol end to end over a run directory."""

import json

import pytest

import stepmeld
from stepmeld import verb


def request(tmp_path, name="count-lines", outputs=("counts",), **extra):
    doc = {"workflow": "wf", "step": "s", "run": "wf/s/1", "definition": {"name": name, "version": 1}, "inputs": {"files": ["a.txt", "b.txt"]}, "parameters": {"min-length": 12}, "outputs": [{"name": o, "tag": "t"} for o in outputs], **extra}
    tmp_path.mkdir(parents=True, exist_ok=True)
    (tmp_path / "request.json").write_text(json.dumps(doc))
    return tmp_path


def outcome(run_dir):
    return json.loads((run_dir / "outcome.json").read_text())


def test_a_verb_that_succeeds_writes_its_outputs_and_summary(tmp_path):
    def count(run):
        run.progress("counting", 1, len(run.input("files")), "files")
        run.succeed({"counts": [[1, 2.0, 3.0]]}, headline="1 count", metrics={"files": 2})

    assert verb.main({"count-lines": count}, [str(request(tmp_path))]) == 0
    out = outcome(tmp_path)
    assert out["state"] == "succeeded" and out["outputs"]["counts"] == [[1, 2.0, 3.0]] and out["summary"]["headline"] == "1 count"
    assert json.loads((tmp_path / "progress.json").read_text())["total"] == 2
    assert out["metrics"] == {"files": 2}


def test_a_refusal_and_a_crash_are_outcomes_too(tmp_path):
    def refuse(run):
        raise verb.Refused("no such file a.txt")

    assert verb.main({"count-lines": refuse}, [str(request(tmp_path))]) == 1
    assert outcome(tmp_path) == {"state": "failed", "class": "refused", "reason": "no such file a.txt", "summary": {"headline": "no such file a.txt"}}

    def crash(run):
        raise ValueError("boom")

    d = request(tmp_path / "crash")
    assert verb.main({"count-lines": crash}, [str(d)]) == 1
    out = outcome(d)
    assert out["class"] == "fault" and "boom" in out["reason"] and "Traceback" in out["summary"]["details"]["traceback"]


def test_owed_outputs_and_silence_are_faults(tmp_path):
    def forgets(run):
        run.succeed({})

    assert verb.main({"count-lines": forgets}, [str(request(tmp_path))]) == 1
    assert outcome(tmp_path)["class"] == "fault" and "counts" in outcome(tmp_path)["reason"]

    def silent(run):
        pass

    d = request(tmp_path / "silent")
    verb.main({"count-lines": silent}, [str(d)])
    assert "without saying" in outcome(d)["reason"]


def test_an_unknown_verb_is_refused(tmp_path):
    d = request(tmp_path, name="nope")
    assert verb.main({"count-lines": lambda r: None}, [str(d)]) == 1
    assert outcome(d)["class"] == "refused" and "count-lines" in outcome(d)["reason"]


def test_a_missing_input_is_a_refusal(tmp_path):
    def needs(run):
        run.input("pages")

    verb.main({"count-lines": needs}, [str(request(tmp_path))])
    assert outcome(tmp_path)["class"] == "refused"


@pytest.mark.parametrize("state", ["succeeded"])
def test_the_outcome_validates_against_the_contracts_outcome(tmp_path, state):
    import stepmeld

    def ok(run):
        run.succeed({"counts": 1}, headline="h")

    verb.main({"count-lines": ok}, [str(request(tmp_path))])
    # an Outcome is the `outcome` definition of the workflow contract
    schema = stepmeld.schema("stepmeld/workflow.v1")
    import jsonschema

    jsonschema.Draft202012Validator({"$ref": "#/$defs/outcome", "$defs": schema["$defs"]}).validate(outcome(tmp_path))


def test_saved_state_comes_back_on_the_next_life(tmp_path):
    d = request(tmp_path)

    def first(run):
        assert run.state == {} and run.resumed == 0
        run.save_state({"job": "j-9"})
        raise SystemExit(1)  # dies before an outcome

    with pytest.raises(SystemExit):
        verb.main({"count-lines": first}, [str(d)])
    assert (d / "resumable").exists() and not (d / "outcome.json").exists()
    (d / "resumes").write_text("1")

    def second(run):
        assert run.state == {"job": "j-9"} and run.resumed == 1
        run.succeed({"counts": []}, headline="picked up j-9")

    assert verb.main({"count-lines": second}, [str(d)]) == 0
    assert outcome(d)["summary"]["headline"] == "picked up j-9"


def test_every_log_line_is_stamped(tmp_path, capsys):
    """A reader of log.txt sees when each line was said: a UTC stamp in
    stepmeld's one spelling, one per line even for a message of several."""
    import re

    def chatty(run):
        run.log("one")
        run.log("two\nthree")
        run.succeed({"counts": []})

    assert verb.main({"count-lines": chatty}, [str(request(tmp_path))]) == 0
    lines = [ln for ln in capsys.readouterr().out.splitlines() if ln.strip()]
    stamped = [re.fullmatch(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ) (.*)", ln) for ln in lines]
    assert all(stamped), lines
    assert [m.group(2) for m in stamped] == ["one", "two", "three"]


def test_the_files_hold_to_their_contracts_and_a_cancel_is_heard(tmp_path):
    def count(run):
        run.progress("counting", 1, 2, "lines")
        run.succeed({"counts": {"file": "counts.json"}}, headline="counted", metrics={"lines": 2})

    d = request(tmp_path / "ok", name="count")
    verb.main({"count": count}, [str(d)])
    for file, contract in [("request.json", "stepmeld/run-request.v1"), ("progress.json", "stepmeld/run-progress.v1"), ("outcome.json", "stepmeld/run-outcome.v1")]:
        assert stepmeld.check(json.loads((d / file).read_text()), contract) is None, file

    def waits(run):
        while (reason := run.cancel_asked()) is None:
            pass
        run.canceled(f"stopped its job ({reason})")

    d = request(tmp_path / "cancel", name="waits", outputs=())
    (d / "cancel.json").write_text(json.dumps({"reason": "asked by person:ann", "at": "2026-10-17T14:05:00Z", "deadline": "2026-10-17T14:05:30Z"}))
    assert verb.main({"waits": waits}, [str(d)]) == 1
    assert outcome(d) == {"state": "canceled", "class": "canceled", "reason": "stopped its job (asked by person:ann)"}
    assert stepmeld.check(outcome(d), "stepmeld/run-outcome.v1") is None


def test_a_verb_runs_without_jsonschema():
    # a verb never validates; the helper loads with jsonschema unavailable
    import subprocess
    import sys

    code = "import sys; sys.modules['jsonschema'] = None; import stepmeld.verb; print('ok')"
    done = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True)
    assert done.returncode == 0 and done.stdout.strip() == "ok", done.stderr


def test_every_line_the_program_writes_is_stamped_once(tmp_path):
    """Run as the Performer runs it: a program whose stdout and stderr
    are the log. Its prints, a raw write to fd 2 (a C library's
    logging), a child process and `run.log` all land one stamp each."""
    import re
    import subprocess
    import sys
    import textwrap

    d = request(tmp_path / "run")
    program = tmp_path / "prog.py"
    program.write_text(textwrap.dedent("""
        import os, subprocess, sys
        from stepmeld import verb

        def count(run):
            print("a print")
            os.write(2, b"I20261022 13:02:04.090663 87 image.cc:377] glog\\n")
            subprocess.run([sys.executable, "-c", "print('a child')"], check=True)
            run.log("said twice\\nby run.log")
            sys.stdout.write("no newline at the end")
            run.succeed({"counts": []})

        sys.exit(verb.main({"count-lines": count}))
    """))
    with open(d / "log.txt", "w") as log:
        code = subprocess.run([sys.executable, str(program), str(d)], stdout=log, stderr=log).returncode
    assert code == 0
    lines = (d / "log.txt").read_text().splitlines()
    stamp = r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ "
    assert all(re.match(stamp, l) for l in lines), lines
    assert not any(re.match(stamp + stamp, l) for l in lines), lines
    bodies = [l[21:] for l in lines]
    assert bodies == ["a print", "I20261022 13:02:04.090663 87 image.cc:377] glog", "a child", "said twice", "by run.log", "no newline at the end"], bodies
