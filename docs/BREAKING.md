# Breaking and behaviour changes

Every change that can make a program, a store or a reader built
against an earlier stepmeld behave differently, newest first: what
changed, who it affects, and what to do. Additive changes that nothing
existing notices are not listed.

While the engine is iterating, the v1 contracts change in place
rather than gaining a v2 beside them; this log is how a user of the
engine catches up.

## A Worker's round says how each order ended

*Breaking for the library.* `orders::Worker::tick` returns
`Vec<Moved>` (`order`, `word`, and `detail`: the headline it ended with
or why it failed) instead of `Vec<(String, String)>`. Read `m.order`
and `m.word` where the pair was used. The command's `tick` and `work`
now print only what changed, with each result; their output is not a
contract.

## The command names a Step with `--step`

*Breaking for the command.*

**Before:** `stepmeld set <workflow> <step> …`, and likewise `start`,
`complete`, `confirm`, `policy`, `place`, `skip` and `log`, took the
Step as a bare second word, easy to mistake for a StepDefinition's
name. **Now:** each takes `--step <name>`, the Step's name in its
WorkflowDefinition; `place` also takes `--worker <name>`. `show` prints
each Step's StepDefinition beside its name.

**What to do:** add `--step` (and `--worker` for `place`) to scripts
that call these commands.

## A StepDefinition names its program; Workers publish programs

*Breaking. Contracts: `stepmeld/step-definition.v1`,
`stepmeld/run-request.v1`, `stepmeld/order.v1`, `stepmeld/worker.v1`.*

**Before:** a Worker registered a command per StepDefinition and
published the StepDefinitions it offered; a StepDefinition said nothing
of what did its work. **Now:** a StepDefinition names its program
(`"program": "hello-program"`), required unless a person does the
work; a request and an order carry that name; a Worker registers a
command per program (`stepmeld worker program set <program> --
<command...>`) and publishes the names of its programs (`programs`,
replacing `verbs` in `worker.v1`); an Orchestrator offers a Worker
every StepDefinition in the Library whose program it has.

**Who notices:** every StepDefinition a Performer does needs a
`program`, or `library add` refuses it. In the library:
`stepmeld_local::Program` is keyed by `name` (was `verb`) and the local
Performer runs the program the request names; `Worker::new` takes the
program names; `OrderPerformer::for_workers` takes the Library;
`WorkerStore::set_program`/`remove_program` take a program name;
`stepmeld_local::file` (the performers file) is gone. The command:
`stepmeld add` is now `stepmeld library add`, `stepmeld library` shows
the Library (also `library show`), and `worker program add <verb>` is
now `worker program set <program>`.

**What to do:** add `"program"` to each StepDefinition, register one
command per program on each Worker, and use the new subcommands.

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
