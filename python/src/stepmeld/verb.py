"""The verb protocol: what a program speaks to stepmeld's local
Performer (`stepmeld-local`).

    <program> [args...] <run-dir>
      <run-dir>/request.json    read: the StartRequest
      <run-dir>/progress.json   written as the work goes (optional)
      <run-dir>/outcome.json    written once, when the work ends
      <run-dir>/log.txt         stdout and stderr, kept by the Performer

A verb is a function of a `Run`. `main` reads the request, picks the
verb by the definition's name, runs it, and makes sure an outcome is
written whatever happens: a `Refused` becomes class `refused`, any
other exception class `fault` with the traceback in the details, a verb
that returns without saying anything class `fault` too. Files are
written atomically (the Performer polls them).
"""

from __future__ import annotations

import json
import os
import sys
import traceback
from pathlib import Path
from typing import Any, Callable


class Refused(Exception):
    """The verb cannot do this request, in words a person reads: a file
    that is not there, an option it does not know. Nothing ran."""


def _write(path: Path, doc: dict) -> None:
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(doc, indent=2) + "\n")
    os.replace(tmp, path)


class Run:
    """One Run of a verb, as the Performer handed it over."""

    def __init__(self, run_dir: str | os.PathLike):
        self.dir = Path(run_dir)
        self.request: dict = json.loads((self.dir / "request.json").read_text())
        self.ended = False

    # ---- what was asked ----

    @property
    def verb(self) -> str:
        return self.request["definition"]["name"]

    @property
    def inputs(self) -> dict[str, Any]:
        return self.request.get("inputs", {})

    @property
    def parameters(self) -> dict[str, Any]:
        return self.request.get("parameters", {})

    @property
    def declared_outputs(self) -> list[str]:
        return [o["name"] for o in self.request.get("outputs", [])]

    def input(self, name: str) -> Any:
        if name not in self.inputs:
            raise Refused(f"input {name!r} was not given")
        return self.inputs[name]

    def parameter(self, name: str, default: Any = None) -> Any:
        return self.parameters.get(name, default)

    # ---- what is said back ----

    def log(self, message: str) -> None:
        print(message, flush=True)

    def progress(self, phase: str | None = None, done: int | None = None, total: int | None = None, unit: str | None = None) -> None:
        doc = {"at": _now()}
        if phase is not None:
            doc["phase"] = phase
        if done is not None:
            doc["done"] = int(done)
        if total is not None:
            doc["total"] = int(total)
        if unit is not None:
            doc["unit"] = unit
        _write(self.dir / "progress.json", doc)

    def succeed(self, outputs: dict[str, Any] | None = None, headline: str | None = None, details: Any = None, metrics: Any = None) -> None:
        outputs = outputs or {}
        missing = [o for o in self.declared_outputs if o not in outputs]
        if missing:
            raise RuntimeError(f"the verb owes outputs {missing} and did not produce them")
        doc: dict = {"state": "succeeded", "outputs": outputs}
        if headline is not None:
            doc["summary"] = {"headline": headline, **({"details": details} if details is not None else {})}
        if metrics is not None:
            doc["metrics"] = metrics
        self._end(doc)

    def fail(self, class_: str, reason: str, headline: str | None = None, details: Any = None) -> None:
        doc: dict = {"state": "failed", "class": class_, "reason": reason}
        doc["summary"] = {"headline": headline or reason, **({"details": details} if details is not None else {})}
        self._end(doc)

    def _end(self, doc: dict) -> None:
        if self.ended:
            return
        _write(self.dir / "outcome.json", doc)
        self.ended = True


def _now() -> str:
    import datetime

    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


Verb = Callable[[Run], None]


def main(verbs: dict[str, Verb], argv: list[str] | None = None) -> int:
    """Run the verb the request names. The last argument is the run
    directory; anything before it is the program's own."""
    argv = sys.argv[1:] if argv is None else argv
    if not argv:
        print("usage: <program> <run-dir>", file=sys.stderr)
        return 2
    run = Run(argv[-1])
    verb = verbs.get(run.verb)
    if verb is None:
        run.fail("refused", f"this program has no verb {run.verb!r}; it has {sorted(verbs)}")
        return 1
    try:
        verb(run)
        if not run.ended:
            run.fail("fault", f"verb {run.verb!r} returned without saying how it ended")
    except Refused as e:
        run.fail("refused", str(e))
    except Exception as e:  # noqa: BLE001 - every failure becomes an outcome
        run.fail("fault", f"{type(e).__name__}: {e}", details={"traceback": traceback.format_exc()})
    return 0 if json.loads((run.dir / "outcome.json").read_text())["state"] == "succeeded" else 1
