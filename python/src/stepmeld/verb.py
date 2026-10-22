"""The verb protocol: what a program speaks to stepmeld's local
Performer (`stepmeld-local`).

    <program> [args...] <run-dir>
      <run-dir>/request.json    read: the StartRequest
      <run-dir>/progress.json   written as the work goes (optional)
      <run-dir>/outcome.json    written once, when the work ends
      <run-dir>/cancel.json     read: a cancel asked for, and its deadline
      <run-dir>/log.txt         stdout and stderr, kept by the Performer;
                                every line stamped in UTC (`main`)

A verb whose work goes on elsewhere (a launcher following a job on a
cloud queue) saves what it would need to pick the work back up with
`run.save_state(...)`; if the program is then gone without an outcome,
the Performer starts it again on the same directory and `run.state`
holds what was saved.

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
import re
import sys
import threading
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
        state = self.dir / "state.json"
        self.state: dict = json.loads(state.read_text()) if state.exists() else {}
        resumes = self.dir / "resumes"
        #: how many times the Performer has started this program again
        self.resumed: int = int(resumes.read_text().strip() or 0) if resumes.exists() else 0

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

    # ---- what is kept for a resume ----

    def save_state(self, state: dict) -> None:
        """Record what a fresh start of this program would need to carry
        on (a job id, a run id), and tell the Performer the program may
        be started again if it dies. `run.state` holds it on the next
        life."""
        self.state = dict(state)
        _write(self.dir / "state.json", self.state)
        (self.dir / "resumable").touch()

    # ---- what is said back ----

    def log(self, message: str) -> None:
        """One line of the Run's log, stamped with the moment (UTC, the
        one spelling every stepmeld record uses; a reader shows local
        time). A message of several lines is several stamped lines."""
        stamp = _now()
        for line in message.splitlines() or [""]:
            print(f"{stamp} {line}", flush=True)

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

    def cancel_asked(self) -> str | None:
        """The reason a cancel was asked for, once one has been. The verb
        stops what it started (a job elsewhere), then calls `canceled`,
        before the deadline in `cancel.json`, after which the Performer
        stops the program."""
        path = self.dir / "cancel.json"
        if not path.exists():
            return None
        try:
            return json.loads(path.read_text()).get("reason") or "canceled"
        except (OSError, ValueError):
            return "canceled"

    def canceled(self, reason: str) -> None:
        """End the Run as canceled, in the verb's own words."""
        self._end({"state": "canceled", "class": "canceled", "reason": reason})

    def _end(self, doc: dict) -> None:
        if self.ended:
            return
        _write(self.dir / "outcome.json", doc)
        self.ended = True


def _now() -> str:
    import datetime

    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


Verb = Callable[[Run], None]


_STAMPED = re.compile(rb"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ(?: |$)")


def stamped(line: bytes) -> bytes:
    """A line of output as the log keeps it: stamped with the moment
    it was read, unless it already carries a stamp (`Run.log`'s)."""
    return line if _STAMPED.match(line) else _now().encode() + b" " + line


class _Stamper:
    """Everything this process writes to stdout and stderr, at the file
    descriptors (so a C library's logging, glog in COLMAP, and every
    child process are covered too), stamped line by line on its way to
    where stdout went before (the Run's `log.txt`). A log a reader
    shows with one clock per line (contracts/run-directory.v1.md)."""

    def __init__(self) -> None:
        for f in (sys.stdout, sys.stderr):
            f.flush()
        self.out = os.dup(1)
        read, write = os.pipe()
        self.saved_err = os.dup(2)
        os.dup2(write, 1)
        os.dup2(write, 2)
        os.close(write)
        for f in (sys.stdout, sys.stderr):
            try:
                f.reconfigure(line_buffering=True)  # type: ignore[attr-defined]
            except (AttributeError, ValueError):
                pass
        self.thread = threading.Thread(target=self._pump, args=(read,), daemon=True)
        self.thread.start()

    def _pump(self, read: int) -> None:
        pending = b""
        while chunk := os.read(read, 65536):
            pending += chunk
            *lines, pending = pending.split(b"\n")
            if lines:
                os.write(self.out, b"".join(stamped(l) + b"\n" for l in lines))
        if pending:
            os.write(self.out, stamped(pending) + b"\n")
        os.close(read)

    def stop(self) -> None:
        """Put stdout and stderr back and let the last lines through. A
        child process still holding the pipe is not waited for long."""
        for f in (sys.stdout, sys.stderr):
            try:
                f.flush()
            except (OSError, ValueError):
                pass
        os.dup2(self.out, 1)
        os.dup2(self.saved_err, 2)
        self.thread.join(timeout=5)
        os.close(self.out)
        os.close(self.saved_err)


def main(verbs: dict[str, Verb], argv: list[str] | None = None) -> int:
    """Run the verb the request names. The last argument is the run
    directory; anything before it is the program's own. Every line
    the program writes while it runs is stamped (`_Stamper`)."""
    argv = sys.argv[1:] if argv is None else argv
    if not argv:
        print("usage: <program> <run-dir>", file=sys.stderr)
        return 2
    stamper = _Stamper()
    try:
        return _main(verbs, argv)
    finally:
        stamper.stop()


def _main(verbs: dict[str, Verb], argv: list[str]) -> int:
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
