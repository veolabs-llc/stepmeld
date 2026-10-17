"""The walkthrough's verb (examples/hello) does what the README says."""

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

from stepmeld import verb

REPO = Path(__file__).resolve().parents[2]
HELLO = REPO / "examples/hello"


def load_add():
    spec = importlib.util.spec_from_file_location("hello_add", HELLO / "add.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.add


def run_dir(tmp_path, a, b):
    definition = json.loads((HELLO / "add.json").read_text())
    request = {
        "workflow": "hello-1",
        "step": "add",
        "run": "hello-1/add/1",
        "definition": {"name": "add", "version": 1},
        "inputs": {"a": a, "b": b},
        "parameters": {"beat-seconds": 0},
        "outputs": definition["outputs"],
    }
    (tmp_path / "request.json").write_text(json.dumps(request))
    return tmp_path


def test_it_adds_and_reports_progress(tmp_path):
    d = run_dir(tmp_path, 2, 3)
    assert verb.main({"add": load_add()}, [str(d)]) == 0
    out = json.loads((d / "outcome.json").read_text())
    assert out["outputs"] == {"sum": 5} and out["summary"]["headline"] == "2 + 3 = 5"
    assert json.loads((d / "progress.json").read_text())["done"] == 3


def test_a_tag_is_not_a_type_so_the_verb_checks(tmp_path):
    d = run_dir(tmp_path, 2, "three")
    assert verb.main({"add": load_add()}, [str(d)]) == 1
    out = json.loads((d / "outcome.json").read_text())
    assert out["class"] == "refused" and "must be integers" in out["reason"]


def test_a_verb_runs_without_jsonschema():
    # a verb never validates; the helper loads with jsonschema unavailable
    code = "import sys; sys.modules['jsonschema'] = None; import stepmeld.verb; print('ok')"
    done = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True)
    assert done.returncode == 0 and done.stdout.strip() == "ok", done.stderr
