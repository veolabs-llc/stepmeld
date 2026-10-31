# stepmeld

A generic workflow engine: a Library of StepDefinitions (each names the
program that does its work) and WorkflowDefinitions (Steps and the
Bindings between them); Workflows made of Steps, each run by a
Performer when its Gate opens; every Run recorded, nothing overwritten.

The engine knows nothing of any domain. Values are opaque; Performers
do the work; a UI reads documents and never gets called.

- `docs/DESIGN.md`: the design note: vocabulary, shape, rules,
  scenarios. Read it first.
- `docs/ORCHESTRATOR-WORKER.md`: the two roles. An Orchestrator
  advances Workflows; a Worker runs the work they order, through orders
  in the shared store.
- `docs/PLACEMENT.md`: where work runs. Workers on this machine, on
  fleet machines and in the cloud; orders, leases and requirements;
  providers as plugin crates; the phases that bring it in.
- `contracts/`: the contracts, as JSON schemas with golden fixtures,
  and the scenario fixtures every implementation is held to.
- `stepmeld-core`: the Rust implementation: documents, the pure core,
  the Performer and StateStore seams with in-memory implementations
  and their conformance suites, the Driver, the nested-workflow
  Performer; the design note's scenarios as tests.
- `stepmeld-sqlite`: the shared store (definitions, Workflows, orders,
  published Workers) in one SQLite file.
- `stepmeld-config`: a deployment and a machine as files: which store
  is shared, how each program runs (a command, and the kinds plugins
  add), secrets by name; the kinds a deployment may name.
- `stepmeld-local`: a Performer that runs the programs on this
  machine (files-in-a-directory protocol; any language), as a command
  or as a Docker image; it finds Docker and the machine's GPUs.
- `stepmeld-aws`: the AWS provider, a plugin the core never imports:
  the shared store in S3, a Performer that runs programs as AWS Batch
  jobs, secrets from Secrets Manager, registry sign-in, and
  `stepmeld-run`, the entrypoint a Batch image runs its program under.
- `stepmeld-verb`: the program side of that protocol, for a program
  written in Rust (the Python twin is `python/`'s `stepmeld.verb`).
- `stepmeld-cli`: the `stepmeld` command: `tick` orchestrates, `work`
  runs this machine's Worker (the crate is named `stepmeld`:
  `cargo install stepmeld`).

Build and test with `scripts/test.sh` (cargo).

## Walkthrough: add, then multiply

`examples/hello` is a workflow with two steps. The first adds two
integers; its sum flows into the second, which multiplies it by a
factor. Each reports progress as it goes. The example has four parts:

- `stepdef_add.json`, the StepDefinition `stepdef_add`: two Inputs, `a`
  and `b`; a Parameter, `beat-seconds`, with its default; one Output,
  `sum`; and the program that does the work, `hello-program`.
- `stepdef_multiply.json`, the StepDefinition `stepdef_multiply`: Inputs
  `value` and `factor`, the same Parameter, Output `product`, and the
  same program.
- `workflowdef_hello.json`, the WorkflowDefinition `workflowdef_hello`:
  a Step named `step_add` that uses `stepdef_add`, a Step named
  `step_multiply` that uses `stepdef_multiply`, and one Binding, from
  `step_add.sum` to `step_multiply.value`. That Binding is the only
  order there is: `step_multiply` waits until `step_add` has a sum to
  give it.
- `src/main.rs`, the program `hello-program`. It uses nothing of
  stepmeld, only a JSON library: the Worker runs it with a folder for
  the Run, it reads `request.json` there (which StepDefinition, which
  Inputs), writes `progress.json` three times and `outcome.json` at the
  end, and what it prints becomes the Run's log. That exchange of files
  is the whole contract between a Worker and a program
  (`contracts/run-directory.v1.md`), so a program can be written in any
  language. (`stepmeld-verb` for Rust and `python/`'s `stepmeld.verb`
  for Python do the same with more care: a crash still leaves an
  outcome.)

A **Step** is one use of a StepDefinition inside a WorkflowDefinition.
Its name is unique within that WorkflowDefinition and is how everything
else refers to it: Bindings (`step_add.sum`), and the command's
`--step`. That is also how one StepDefinition can be used twice: two
Steps with different names, say `step_first_sum` and `step_second_sum`,
both using `stepdef_add`.

A StepDefinition names its program; each Worker says which command
runs that program on its own machine. So the same StepDefinition can
run wherever a Worker has the program, and a Worker offers every
StepDefinition whose program it has.

A tag such as `integer` is a label, not a type: the engine never looks
inside a value. A program checks its own Inputs, as `hello-program`
does, and refuses in words what it cannot use.

Two roles take part (`docs/ORCHESTRATOR-WORKER.md`). The
**Orchestrator** (`stepmeld tick`) advances Workflows. The **Worker**
(`stepmeld work`) runs the programs on this machine. They meet in the
shared store, `stepmeld.sqlite`: the Orchestrator places an order, the
Worker claims it, carries it out and reports. The Worker keeps its own
settings and programs in its machine file, `stepmeld-machine.toml`.
Both files are made in the current folder the first time a command
needs them. (A deployment file, `stepmeld.toml`, says which store is
shared; without one it is `stepmeld.sqlite` in the current folder. The
fleet variation below uses one.)

**Source and state are kept apart.** `examples/hello` holds the
example's source, under version control. Everything the walkthrough
makes (the stores, each Run's files) goes in one folder at the top of
the repository, `walkthrough/`, which git ignores. To start over from a
blank slate, delete that folder. Every command below is run from
inside it.

The `stepmeld` command is the framework, so it is installed on your
`PATH`. Nothing of the walkthrough is: its program is built into the
repository's own `target/release` and registered by a relative path,
`../target/release/hello-program`, and its definitions are read from
`../examples/hello/…`. The command prints when it makes a new store,
and where, so one run in the wrong folder shows itself.

From the top of the repository, install the command (into
`~/.cargo/bin`), build the example's program, and make the
walkthrough's folder:

```bash
cargo install --path stepmeld-cli
```
```bash
cargo build --release -p stepmeld-example-hello
```
```bash
mkdir walkthrough && cd walkthrough
```

Put the definitions in the Library, and tell this machine's Worker
which command runs `hello-program`:

```bash
stepmeld library add ../examples/hello/stepdef_add.json ../examples/hello/stepdef_multiply.json ../examples/hello/workflowdef_hello.json
stepmeld worker program set hello-program -- ../target/release/hello-program
```

The first command prints that it made the shared store,
`walkthrough/stepmeld.sqlite`; the second, this machine's file,
`walkthrough/stepmeld-machine.toml`. `stepmeld worker` now shows the
Worker's program and what it offers: `stepdef_add v1, stepdef_multiply
v1`.

Start the Worker in a second terminal, in the same folder, and leave
it running:

```bash
cd walkthrough
stepmeld work --watch
```

Back in the first terminal, make a Workflow, give it the Inputs nobody
else feeds, and orchestrate it:

```bash
stepmeld create workflowdef_hello --id hello-1
stepmeld set hello-1 --step step_add --input a --value 2
stepmeld set hello-1 --step step_add --input b --value 3
stepmeld set hello-1 --step step_multiply --input factor --value 4
stepmeld start hello-1        # a new Workflow runs nothing until started
stepmeld tick --watch         # orchestrate until nothing is running
```

`--step` names a Step, not a StepDefinition. `step_multiply`'s `value`
is not set by hand (`set` refuses: it is fed by a Binding). `tick
--watch` places `step_add` on the Worker, which runs `hello-program`;
when `step_add` succeeds its sum is carried to `step_multiply`, which
then runs in turn. `tick` prints a line whenever a Step or the Workflow
changes, with its progress or its result (the Worker is named after
your machine; which progress lines appear depends on timing; a
Workflow already finished stays quiet):

```text
hello-1          step_add         running    on my-laptop, not yet taken
hello-1          step_multiply    not-ready
hello-1          (workflow)       running
hello-1          step_add         running    adding 2 of 3 beats
hello-1          step_add         succeeded  2 + 3 = 5
hello-1          step_multiply    running    on my-laptop, not yet taken
hello-1          step_multiply    running    multiplying 1 of 3 beats
hello-1          step_multiply    running    multiplying 3 of 3 beats
hello-1          step_multiply    succeeded  5 × 4 = 20
hello-1          (workflow)       succeeded
```

The Worker's terminal says the same from its side, one line as it
takes each order and one as it ends (`hello-1/step_add/1  succeeded
2 + 3 = 5`). `stepmeld show hello-1` gives the whole picture at any
time:

```text
$ stepmeld show hello-1
hello-1  (workflowdef_hello v1, succeeded)
  step_add         (stepdef_add v1)         succeeded
      in   a              2
      in   b              3
      par  beat-seconds   1  (default)
      out  sum            5
      run 1 on my-laptop by policy start-automatically: succeeded: 2 + 3 = 5
  step_multiply    (stepdef_multiply v1)    succeeded
      in   factor         4
      in   value          5
      par  beat-seconds   1  (default)
      out  product        20
      run 1 on my-laptop by policy start-automatically: succeeded: 5 × 4 = 20
```

Then, still in `walkthrough/`:

- `stepmeld log hello-1 --step step_multiply` prints that Step's
  program output, and `stepmeld history hello-1` every event in order;
- `stepmeld library` lists the StepDefinitions and WorkflowDefinitions,
  and `stepmeld library remove workflowdef_hello` takes one out, asking
  whether to remove the StepDefinitions no other WorkflowDefinition uses
  (`--with-orphans` or `--keep-orphans` answers in advance). `hello-1`
  is not touched: a Workflow carries its own copy of the
  WorkflowDefinition it was made from, so only a StepDefinition its
  Steps still use is kept;
- `stepmeld edit hello-1 …` changes that copy (`add-step`,
  `remove-step`, `bind`, `unbind`, `guard`, `relabel`, `override`: the
  value a Step's Parameter starts from), each edit recorded in the
  History; `stepmeld definition hello-1 --name workflowdef_hello_2
  --add` keeps the copy in the Library as a WorkflowDefinition of its
  own;
- without a Worker, `tick` says what it waits for: `step_add: no Worker
  offers it`;
- each Run's files (`request.json`, `progress.json`, `outcome.json`,
  `log.txt`) are in `walkthrough/runs/`; the protocol is
  `contracts/run-directory.v1.md`.

To start over, stop the Worker (Ctrl-C) and, from the top of the
repository:

```bash
rm -rf walkthrough
```

### A fleet, on one machine

The same Workflow, with the work done by two Workers, standing in for
two fleet machines on the local network (`docs/PLACEMENT.md` §2). Each
gets a folder of its own, as a machine would have: its own machine
file, its own folder for Runs, and a deployment file that points at
the shared store (on a real fleet, every machine's deployment file is
the same and names a store they all reach).

Stop the Worker from the walkthrough above if it is still running.
From `walkthrough/`, make the two Workers, `bench-a` and `bench-b`:

```bash
for b in bench-a bench-b; do mkdir $b; printf '[stores.shared]\nkind = "sqlite"\npath = "../stepmeld.sqlite"\n' > $b/stepmeld.toml; done
(cd bench-a && stepmeld worker set --name bench-a && stepmeld worker program set hello-program -- ../../target/release/hello-program)
(cd bench-b && stepmeld worker set --name bench-b && stepmeld worker program set hello-program -- ../../target/release/hello-program)
```

Start each in a terminal of its own and leave them running:

```bash
cd walkthrough/bench-a && stepmeld work --watch
```
```bash
cd walkthrough/bench-b && stepmeld work --watch
```

Back in `walkthrough/`, make a second Workflow and start it:

```bash
stepmeld create workflowdef_hello --id hello-2
stepmeld set hello-2 --step step_add --input a --value 2
stepmeld set hello-2 --step step_add --input b --value 3
stepmeld set hello-2 --step step_multiply --input factor --value 4
stepmeld start hello-2
stepmeld tick
```

With two Workers offering the work, nothing is chosen for you:

```text
hello-2          step_add         waiting    waiting for a placement: several Workers offer it (`place`)
```

Name a Worker, or `any` for the first one free to take it:

```bash
stepmeld place hello-2 --step step_add --worker any
stepmeld place hello-2 --step step_multiply --worker any
stepmeld tick --watch
```

Each order is open; whichever Worker looks first claims it, and its
terminal says so. `tick` says who took it (`running    on bench-a`).
A Worker that stops (Ctrl-C) while it holds a Run keeps its claim for
five minutes; back within that time it carries on, and after it the
Run is lost and its Step's Policy decides what follows. A Worker unheard
for three minutes is away, and nothing new is placed on it.

### The fleet, with the program as an image

The same two Workers, running `hello-program` as a container instead
of a command: what a fleet does when every machine should run the same
code, however it is set up. The image is built once, from the top of
the repository (it compiles the program inside, so the first build
takes a few minutes):

```bash
docker build -f examples/hello/Dockerfile -t stepmeld-hello:1 .
```

Stop both Workers. In `walkthrough/`, give the program an image in each
deployment file, and take the command away from each machine, so the
image is how they run it:

```bash
for b in bench-a bench-b; do printf '\n[programs.hello-program.docker]\nimage = "stepmeld-hello:1"\n' >> $b/stepmeld.toml; (cd $b && stepmeld worker program remove hello-program); done
(cd bench-a && stepmeld worker)
```

`stepmeld worker` now says `program  hello-program  (docker)`, and
`has  docker` (what it found on the machine). Start the two Workers again
(`stepmeld work --watch` in each folder), then run a third Workflow as
before, placed on `any`:

```bash
stepmeld create workflowdef_hello --id hello-3
stepmeld set hello-3 --step step_add --input a --value 2
stepmeld set hello-3 --step step_add --input b --value 3
stepmeld set hello-3 --step step_multiply --input factor --value 4
stepmeld start hello-3
stepmeld place hello-3 --step step_add --worker any
stepmeld place hello-3 --step step_multiply --worker any
stepmeld tick --watch
```

Each Run is a `docker run` of the image with the Run's folder mounted
at `/stepmeld/run`; `docker ps` shows it while it runs, named after the
Run. A Docker that runs in a virtual machine (Docker Desktop, Colima)
mounts only the folders it shares, usually the user's own; the
walkthrough lives in the repository, so it is shared. On Linux the
container writes the Run's files as root.

### In the cloud: AWS Batch, by hand

The same Workflow with the work done as AWS Batch jobs, the shared store
in S3, and a cloud Worker launching and watching the jobs
(`docs/PLACEMENT.md`; `stepmeld-aws`). It needs an AWS account, so it is
run by hand: nothing here runs it nightly. You need, in one region:

- an S3 bucket (`my-bucket` below) for the shared store and the Runs'
  folders;
- an ECR repository for the image;
- a Batch compute environment and job queue (`my-queue`);
- a job definition (`stepmeld-hello`) for the image, whose job role may
  read and write `s3://my-bucket/stepmeld/`;
- an AWS profile (`my-profile`) that may use all of these.

Build the image, `hello-program` under `stepmeld-run` (which keeps the
Run's folder in S3 in step with a copy the program works on), and push
it:

```bash
docker build -f examples/hello/Dockerfile.batch -t <account>.dkr.ecr.<region>.amazonaws.com/stepmeld-hello:1 .
```
```bash
aws ecr get-login-password --profile my-profile | docker login --username AWS --password-stdin <account>.dkr.ecr.<region>.amazonaws.com
```
```bash
docker push <account>.dkr.ecr.<region>.amazonaws.com/stepmeld-hello:1
```

In a folder of its own (`walkthrough/cloud/`), the deployment file:

```toml
store = "shared"

[providers.aws]
region = "<region>"
profile = "my-profile"
runs = "s3://my-bucket/stepmeld/runs/"

[stores.shared]
kind = "s3"
bucket = "my-bucket"

[programs.hello-program.batch]
definition = "stepmeld-hello"
queues = { on-demand = "my-queue" }
```

Then the cloud Worker, in a terminal of its own, and the Workflow, from
`walkthrough/cloud/`:

```bash
stepmeld worker set --name queue --cloud true
stepmeld work --watch
```
```bash
stepmeld library add ../../examples/hello/stepdef_add.json ../../examples/hello/stepdef_multiply.json ../../examples/hello/workflowdef_hello.json
stepmeld create workflowdef_hello --id hello-cloud
stepmeld set hello-cloud --step step_add --input a --value 2
stepmeld set hello-cloud --step step_add --input b --value 3
stepmeld set hello-cloud --step step_multiply --input factor --value 4
stepmeld start hello-cloud
stepmeld tick --watch
```

The cloud is one placement, the pool `cloud`, which the Step takes on
its own here since nothing else offers its verb. Each Step's order is
open to any cloud Worker; `queue` claims it, submits a job with
`STEPMELD_RUN` naming the Run's folder in S3, and watches it. A second
cloud Worker on another machine (`--name queue-2`) is the same
placement: it would take over a job whose launcher went away, and go
on watching the same job. A Step's `needs` (`memory_gib`, `vcpus`,
`capacity`) pick the queue and the size, from `ladder` when the program
gives one; a deployment may also size each request by formulas over its
Inputs and Parameters (`[programs.<name>.needs]`), and `stepmeld needs
<run-folder>` prints what one Run's request comes to; a Policy with `confirm = ["cloud"]` holds each Step for a
person before it is launched.


## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](LICENSE-MIT)), at your option. Built by VEO Labs.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any
additional terms or conditions.
