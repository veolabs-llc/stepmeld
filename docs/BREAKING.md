# Breaking and behaviour changes

Every change that can make a program, a store or a reader built
against an earlier stepmeld behave differently, newest first: what
changed, who it affects, and what to do. Additive changes that nothing
existing notices are not listed.

While the engine is iterating, the v1 contracts change in place
rather than gaining a v2 beside them; this log is how a user of the
engine catches up.

## Orchestrators and Workers; the command's performers file is gone

*Breaking for the command; additive for the library. Note:
`docs/ORCHESTRATOR-WORKER.md`. Contracts: `stepmeld/order.v1`,
`stepmeld/worker.v1`.*

**Before:** `stepmeld --performers <file>` told the command this
machine's programs, and `tick` ran them in the same process. **Now:**
a Worker registers its programs in its own store
(`stepmeld worker program add <verb> -- <command...>`, kept in
`stepmeld-worker.sqlite`), `stepmeld work` runs it, and `tick` places
each Run as an order on a Worker that published itself to the shared
store. `place` takes a Worker's name and no locality: locality comes
from the Worker.

**Who notices:** anything that calls the command with `--performers`
or `stepmeld performers` (both are gone), or `place` with a locality.
A shared store file from before gains its new tables when opened. In
the library, `StateStore` is unchanged; `OrderBook` is a new trait,
implemented by the memory and SQLite stores. `stepmeld_local::file`
still reads a performers file, but the command no longer does.

**What to do:** register each program once with `stepmeld worker
program add`, run `stepmeld work --watch` on each machine that does
work, and orchestrate with `stepmeld tick` as before. A driver built in
code reaches Workers with `OrderPerformer::for_workers` and runs one
with `orders::Worker`.

## A cancel is a request with a deadline

*Behaviour change, `stepmeld-local`. Contract: `contracts/run-directory.v1.md`.*

**Before:** a cancel killed the program at once, and the Performer
wrote a canceled outcome. **Now:** the Performer writes `cancel.json`
(reason and deadline, thirty seconds by default) and the Run stays
running until the program ends or the deadline passes; then the
program is stopped and the outcome written as before.

**Who notices:** a verb program that does not watch for `cancel.json`
now runs on for up to the grace period after a cancel. A program that
exits in that window without an outcome is canceled, even when it
wrote `resumable`.

**What to do:** nothing, to keep the old effect a little later; or
check `run.cancel_asked()` (Python) / `Run::cancel_asked()` (Rust),
stop what the verb started elsewhere, and call `canceled(...)`. A
Performer that wants the old immediate stop is built with
`LocalPerformer::with_grace(Duration::ZERO)`.
