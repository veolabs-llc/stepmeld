"""The walkthrough's verb: add two integers, slowly enough to watch.

A tag is a label, not a type: the engine never looks inside a Value,
so the verb checks its own Inputs and refuses, in words, what it
cannot add.
"""

import sys
import time

from stepmeld import verb


def add(run: verb.Run) -> None:
    a, b = run.input("a"), run.input("b")
    if not all(isinstance(x, int) and not isinstance(x, bool) for x in (a, b)):
        raise verb.Refused(f"a and b must be integers, not {a!r} and {b!r}")
    beat = float(run.parameter("beat-seconds", 1))
    for i in range(1, 4):
        run.progress("adding", i, 3, "beats")
        run.log(f"thinking about {a} + {b} ({i}/3)")
        time.sleep(beat)
    run.succeed({"sum": a + b}, headline=f"{a} + {b} = {a + b}")


if __name__ == "__main__":
    sys.exit(verb.main({"add": add}))
