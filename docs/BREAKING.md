# Breaking and behaviour changes

Every change that can make a program, a store or a reader built
against an earlier stepmeld behave differently, newest first: what
changed, who it affects, and what to do. Additive changes that nothing
existing notices are not listed.

While the engine is iterating, the v1 contracts change in place
rather than gaining a v2 beside them; this log is how a user of the
engine catches up.

## A placement recorded on a cloud Worker is the cloud

*Behaviour change, `stepmeld-core` (`Workflow::placement_among`), and
an addition (`Workflow::statuses_among`).*

**Before:** a Step placed on a cloud Worker by name before the cloud
became one placement (below) waited for a Performer: its go given, the
reader's status `Ready` (status alone does not check placement) while
no Run was ever minted. **Now:** a placement recorded in a locality
whose one way in is a pool with no Worker listed beside it is that
pool, whatever Performer it named, so such a Step runs on `cloud` and
its Run says so. A placement on a Worker of a locality that lists its
Workers (a machine that is gone) still waits for a person.
`statuses_among(performers)` is each Step's status with placement
checked, as the Gate sees it, over `Driver::faces()` (now public).

**Who notices:** a Workflow placed on a cloud Worker by name: it runs
rather than waiting. A Policy naming a cloud Worker still waits (a
Policy names no locality beside its Performer); place it on `cloud`.

**What to do:** nothing for placements. A reader that shows a
Workflow's status and knows the Performers on offer reads
`statuses_among` instead of `statuses`, so a Step no Performer can take
reads `Waiting(Performer)` rather than `Ready`.

## The cloud Workers are one placement, the pool `cloud`

*Behaviour change, `stepmeld-core` (`OrderPerformer::for_workers`,
`Workflow::placement_among`) and the CLI.*

**Before:** every published Worker was a placement by name, a cloud
Worker included, though an order placed on a cloud Worker was open to
every cloud Worker and the name recorded on the Run was whichever one
a person happened to pick. With one Worker offering a verb the Step
went to it unasked; a pool was taken only when named. **Now:** a cloud
Worker is not a placement by its own name. The cloud Workers that have
published themselves are one placement, the pool `cloud` (locality
`cloud`, offering what any of them not away offers), beside `any`. A
pool with no Worker listed beside it in its locality counts as a
Worker would, so a verb only the cloud offers still places itself as
it did on a lone cloud Worker, and one a Worker here and the cloud
both offer still waits for a choice; `any` stands beside the Workers
it pools, so it is still never chosen unasked. `stepmeld worker set
--name cloud` is refused as `any` is.

**Who notices:** a Workflow whose Policy names a cloud Worker: that
Performer no longer exists, so the Step waits for a Performer (a Step
placed on one by name is on `cloud` since the change above). A UI or script that listed cloud
Workers as placements, or placed on one by name.

**What to do:** place on `cloud` (`stepmeld place <wf> --step <s>
--worker cloud`; a Policy's `placement.performer` the same). Move an
Orchestrator and its Workers together with no Run live, as every
order-shape change asks (PLACEMENT.md §10, phase 6).

## A Workflow's own definition is edited by Commands

*Additive, `stepmeld-core` and the CLI.* `Command` gains `AddStep`,
`RemoveStep`, `Bind`, `Unbind`, `MoveBinding`, `SetGuard`, `Relabel`,
`SetOverride` and `Saved` (`stepmeld-core/src/edit.rs`): each edits the
Workflow's own WorkflowDefinition, lands only if it then holds together,
keeps the Step records in step (a step added gets its record as at
creation; one removed takes its record into the History), and leaves a
History entry holding the state before. A step inside a nested
definition is addressed by path (`publish/extra`) until that step
starts. `edit::definition_of` is the Workflow's definition as one to
keep in a Library, with `Keep` saying which of its tuning goes along;
`stepmeld definition <workflow> --name … --add` adds it. `stepmeld edit
<workflow> …` issues the edits.

**Who notices:** a `match` over `Command` that names every variant.
`engine::step_record` is new (a Step as `create` builds it).

**What to do:** add arms for the new variants, or a catch-all.

## Step entries set their use apart, and nest inline

*Contracts `stepmeld/workflow-definition.v1`, `stepmeld/workflow.v1`,
`stepmeld/step-definition.v1`, changed in place; the library API.*
**Before:** a step entry was a name, a verb by name and version and a
label; a nested WorkflowDefinition was looked up in the Library when its
child was made; a Policy's placement named a Performer or nobody; a
definition needed a step. **Now:** an entry may carry `parameters`
(values its use starts from instead of the verb's defaults, chosen by
the definition, which a `set` to none comes back to), `policy` (the
Step's rules in place of the verb's) and `definition` (the nested
WorkflowDefinition it is an occurrence of, inline; `step` names it).
`create` copies every nested definition in, so a Workflow is whole on
its own, and the nested Performer makes a child from the parent's copy.
A placement may name a `locality`: the driver chooses among the
Performers offering the verb there. A definition may have no steps.

**Who notices:** code building `StepEntry` by its fields
(`StepEntry::new`); a `Performer` built from `PerformerFace` by its
fields (`nests`, true for one that performs nested definitions);
callers of `Workflow::placement_among` and `placement_for` (a `nested`
flag, and a step name); a validator that refused a definition with no
steps; a reader of Workflow documents, which now carry nested
definitions inline.

**What to do:** nothing for existing documents: every new field is
optional, and Workflows made before run as they did. A Performer of
your own that performs nested definitions says `nests: true`.

## A Workflow carries its own WorkflowDefinition

*Contract `stepmeld/workflow.v1`, changed in place; `StateStore`; the
CLI.* **Before:** a Workflow named its WorkflowDefinition by name and
version (`definition`), and every reader looked it up in the Library on
every read, so a change to the Library's definition reached every
Workflow made from it. **Now:** the document carries
`workflow_definition`, its own copy made at creation, and `from`, the
Library's name and version it was made from (absent when made from
nothing). `definition` is gone, so a build from before refuses the new
document rather than reading it, and this build refuses a document
from before. The Library's WorkflowDefinition may change or go without
reaching a Workflow; its StepDefinitions may not go while a Workflow's
Steps are occurrences of them (`library remove` keeps them, and says
so).

**Who notices:** anything that reads a Workflow document
(`wf.definition` is now `wf.workflow_definition`, with `wf.from` beside
it); a store holding Workflows made before this build; a `StateStore`
of your own (one new required method, `document`: the Workflow's
document as held, for a reader that cannot read it).

**What to do:** `stepmeld adopt` (one Workflow, or every one that
needs it) copies the Library's definition each names onto it and notes
it in the History; run it before `tick`. In code, `Workflow::adopt(doc,
&lib)` and `Driver::adopt`. Drop any Library lookup of a Workflow's
definition and read the document's.

## Removing a Workflow takes its orders and their logs

*Behaviour, every `StateStore` that keeps orders (memory, SQLite, the
object store).* **Before:** `remove` forgot the Workflow, its History
and its lease; the orders its Runs placed (`orders/`, `open/`) and their
logs (`logs/`) stayed, and had to be removed by hand. **Now:** they go
with it. A Workflow with an order that has not ended is refused in
words, before anything is forgotten.

**Who notices:** a store that removed a Workflow while an order of its
was still live; nothing that removes finished Workflows.

**What to do:** nothing. Orders left by earlier removals stay until
removed by hand.

## A canceled Batch job's program is stopped at the deadline

*Behaviour, `stepmeld-aws` (`stepmeld-run`).* **Before:** `stepmeld-run`
copied `cancel.json` down and nothing more. Batch's terminate at the
deadline sent SIGTERM to the wrapper, not the program, so a program
that did not watch `cancel.json` ran on to its end, published what it
made and the job could end SUCCEEDED. **Now:** the wrapper keeps the
cancel as a machine does. At the deadline it sends SIGTERM to the
program's process group (the program and whatever it started), then
SIGKILL ten seconds later (`STOP_GRACE`). It passes a SIGTERM of its own
straight on. A program that ends after a cancel without an outcome of
its own ends canceled. The program now runs in its own process group.

**Who notices:** a program that ignored cancels in a Batch job is now
stopped. One that stops cleanly on SIGTERM can still do so. `wrapped`
keeps its signature; `wrapped_until` also takes the flag
`stepmeld-run` sets on SIGTERM.

**What to do:** rebuild the images that carry `stepmeld-run`.

## A container's `before` runs without the container's variables

*Behaviour.* The shell that starts a container holds its variables for
`docker run -e` to pass by name; the image's `before` ran in that shell
and saw them, so a variable meant for inside the container (a cache's
mount point) steered the machine's own command. It now runs with each of
them unset (`env -u`). `container_command` takes the variables.

## A machine may make a container ready, and mount its own folders

*Additive.* An image may name `before`, a command run on the machine
first with the Run's folder appended (the container starts only if it
succeeds; the start does not wait for it), and `volumes`, folders
mounted with `-v`. A machine file's `[docker]` section (`before`,
`volumes`, `env`) adds these to every container that machine runs: its
`before` where the image names none, its folders after the image's, its
variables beside the program's. `stepmeld_local::container_command` is
the command a container's Run starts with.

## A machine runs one container at a time

*Behaviour.* A Worker built from a machine file now runs at most one
order at once among the programs it runs as Docker images (PLACEMENT.md
§5's slot for the Docker Performer), whatever its `slots`; `[worker]
docker_slots = n` says otherwise. Before, a Worker with several slots
could start several containers at once. `Worker::with_limit` is the
rule for any embedder: at most so many orders of some programs at once.

## A program may be given variables that are no secret

*Additive.* `[programs.<name>] env = { NAME = "value" }` hands a program
variables beside its secrets: as its environment for a command, by name
(`-e`) for a container, and with the submit for a Batch job (never its
secrets: a Batch job takes those from its job definition, as before). A name may be a secret or in `env`, not both.
`ProgramPlan` gains `vars`, the `env` part of its `env`;
`BatchProgram` gains `env`, filled from it.

## An Orchestrator ticks only what moved

*Additive.* `Driver::tick_moved` ticks a Workflow only when its version
changed, it has a Run live or a Step ready, or the Workers on offer
changed; `StateStore::listing` and `Objects::listing` give versions at
once (defaults read each one). `stepmeld tick --watch` uses it.

## Programs may run as AWS Batch jobs

*Additive.* `stepmeld-aws` adds the cloud Performer kind `batch`
(`[programs.<name>.batch]`: `definition`, `queues`, `capacity`,
`ladder`; the Runs' folders at `[providers.aws] runs`) and the
`stepmeld-run` wrapper a Batch image runs its program under.

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
