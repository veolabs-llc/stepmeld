# Breaking and behaviour changes

Every change that can make a program, a store or a reader built
against an earlier stepmeld behave differently, newest first: what
changed, who it affects, and what to do. Additive changes that nothing
existing notices are not listed.

While the engine is iterating, the v1 contracts change in place
rather than gaining a v2 beside them; this log is how a user of the
engine catches up.

## The shared store may be any object store; S3 comes with the command

*Additive.* `stepmeld_core::objects` keeps a whole shared store in an
object store with conditional writes (`ObjectStore` over `Objects`).
`stepmeld-aws` implements it for S3 and adds the kinds `s3` (store),
`aws-secretsmanager` (secret) and `aws` (sign-in: an ECR login and the
deployment's credentials for a container), read with
`[providers.aws]` (`region`, `profile`, `endpoint`). The `stepmeld`
command includes them by default (`aws` feature; leave it out with
`--no-default-features`).

## A program may be an image; a Worker probes Docker and its GPUs

*Additive for the contracts (`stepmeld/worker.v1` gains `gpus`);
breaking for code that builds the changed structs. Note:
`docs/PLACEMENT.md` §6.*

**Now:** a deployment may give a program an image,
`[programs.<name>.docker]` (`image`, `args`, `gpus`, `shm_gib`,
`pull_always`, `sign_in`); a Worker with no command for the program
runs it as `docker run` with the Run's folder mounted at
`/stepmeld/run`, secrets passed by name, the container named after the
Run and removed when the Run is stopped or its client is gone. A
machine's Worker offers `docker` and `cuda` when it finds them, and
publishes its GPUs' memory with each beat.

**Who notices:** `stepmeld_local::Program` gains `image` (use
`Program::new`); `WorkerFace` gains `gpus`; `stepmeld_config::WorkerPlan`
gains `kinds`. New: `stepmeld_local::{Image, SignedIn, SignIn,
docker_run, probe, gpus}`, `LocalPerformer::with_sign_in`,
`Kinds::with_sign_in`, `orders::Gpu`, `Worker::with_gpus`.

## A deployment file and a machine file; the Worker store retires

*Breaking for the command and the library. Note: `docs/PLACEMENT.md`
§8.*

**Before:** the shared store was `--store` (default `stepmeld.sqlite`)
and a Worker kept its name and programs in a SQLite file of its own,
`--worker-store` (default `stepmeld-worker.sqlite`). **Now:** a
deployment file (`--deployment`, `STEPMELD_CONFIG`, `stepmeld.toml`
here, or in the user's configuration folder) says which store is
shared, each program's settings and its secrets; without one the store
is `stepmeld.sqlite` here, as before, and `--store` still names a
SQLite file outright. A machine file (`--machine`, `STEPMELD_MACHINE`,
`stepmeld-machine.toml` here, or `machine.toml` in the configuration
folder) holds the Worker's name, Runs folder, cloud flag, slots,
capabilities and program commands. `stepmeld worker set` gains
`--runs` (was `--root`), `--slots` and `--capability`.

**Who notices:** `--worker-store` is gone; a Worker's settings in an
old `stepmeld-worker.sqlite` are not read (set them again with
`stepmeld worker set` and `worker program set`). In the library,
`stepmeld_sqlite::WorkerStore` is gone; `SqliteStore::definitions` is
now `StateStore::definitions` (a default, from the Library);
`stepmeld_local::Program` gains `env` (build it with `Program::new`).
New: `stepmeld-config` (`Deployment`, `Machine`, `Kinds`),
`store::SharedStore`, `performer::Router`.

**What to do:** register programs again on each Worker; pass
`--machine` where `--worker-store` was.

## A Run's log is kept beside its order and appended

*Breaking for the library and the contract `stepmeld/order.v1` (its
report loses `log`).*

**Before:** each report carried the whole log so far, replaced every
time. **Now:** the log is kept beside the order and grows by
appending: only the claiming Worker appends, only while the order is
open, and only at the length the store holds, so no chunk is written
twice and a Worker restarted mid-Run carries on from what the store
has.

**Who notices:** `Report` has no `log`; `OrderBook` gains `log` and
`append_log`. A shared store file from before gains its `log` column
when opened; logs already inside reports are not carried over.

**What to do:** read a Run's log with `OrderBook::log` (or the
Orchestrator's `Performer::log`, as `stepmeld log` does).

## Open orders, the pool `any`, and what a StepDefinition requires

*Breaking for the library; additive for the contracts. Note:
`docs/PLACEMENT.md` §5, §7. Contracts: `stepmeld/step-definition.v1`
gains `requires` and `needs`; `stepmeld/run-request.v1` and the order's
request gain `needs`; `stepmeld/order.v1` is pinned (`worker`) or open
(`open`: `cloud`, `requires`), exactly one.*

**Now:** a StepDefinition may say what a Worker must offer
(`requires`, opaque words) and what the work needs (`needs`, an opaque
map the Performer sizes the work by). A Worker offers a StepDefinition
only when it has the program and every required word. Every
Orchestrator has a pool Performer, `any`: placed there, a Run's order is
open, and the first Worker free to take it does; the pool is never
chosen on its own. A cloud Worker's orders are always open to every
cloud Worker, so when the one that launched a job stops renewing,
another takes the claim over and goes on observing the same job.

**Who notices:** `Order::worker` is an `Option`, beside `Order::open`;
`OrderBook::open_orders` also lists every open order, which the Worker
judges with `Order::for_worker`. `PerformerFace` gains `pool`;
`StepDefinition` gains `requires` and `needs`; `StartRequest` gains
`needs`. A Worker may not be named `any`. The command's Workers have no
capabilities yet (they arrive with the machine file), so a
StepDefinition that requires a word is not offered by them.

**What to do:** build `Order`, `PerformerFace`, `StepDefinition` and
`StartRequest` with the new fields (or `..`); read `order.worker` as an
`Option`.

## Orders are claimed under a lease; a Worker's entry is its heartbeat

*Breaking for the library; additive for the contracts and the command.
Note: `docs/PLACEMENT.md` §5–§6. Contracts: `stepmeld/order.v1` gains
`claim`; `stepmeld/worker.v1` gains `capabilities`, `slots`, `holds`,
`beat_s`.*

**Before:** a Worker started every open order addressed to it each
round, nothing said it held one, and a Run on a Worker that went away
stayed running for ever. **Now:** a Worker claims an order by a
conditional write and renews the claim (five minutes, every minute);
it takes as many orders as it has slots (one by default) and publishes
itself at least every beat (a minute), with what it holds. An
Orchestrator places nothing on a Worker unheard for three beats; it
ends lost a Run whose Worker stopped renewing a claim on local work,
and ends canceled at once an order nobody took. A Worker back too late
finds its order ended and stops the work it left running. A Worker back
within its lease carries on.

**Who notices:** in the library, `OrderBook` gains `claim` and `end`,
and `report` takes the reporting Worker's name and refuses any other;
`WorkerFace::performer_face` takes the time; `Order` and `WorkerFace`
gain fields (`Order { claim: None, .. }`); `Performer` gains
`portable()` (default `false`); `orders::Worker` gains `with_slots`,
`with_capabilities`, `with_lease`, `with_beat`. A shared store file
from before gains its `claim` column when opened. The command: `tick
--watch` reads the Workers again each tick, and says when a Run's
order is not yet taken or its Worker is away.

**What to do:** pass the Worker's name to `report`; build `Order` and
`WorkerFace` with the new fields (or `..`); a Performer whose handles
mean something on another machine (a cloud job id) returns `true` from
`portable()`.

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
