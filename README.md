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
  published Workers) in one SQLite file, and a Worker's own store (its
  name and programs) in another.
- `stepmeld-local`: a Performer that runs the programs on this
  machine (files-in-a-directory protocol; any language).
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
Worker carries it out and reports. The Worker keeps its own settings
and programs in `stepmeld-worker.sqlite`. Both files are made in the
current folder the first time a command needs them.

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
`walkthrough/stepmeld.sqlite`; the second, the Worker's store,
`walkthrough/stepmeld-worker.sqlite`. `stepmeld worker` now shows the Worker's program and what it offers:
`stepdef_add v1, stepdef_multiply v1`.

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
  and `stepmeld library remove workflowdef_hello` takes one out (once
  no Workflow uses it), asking whether to remove the StepDefinitions no
  other WorkflowDefinition uses (`--with-orphans` or `--keep-orphans`
  answers in advance);
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


## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](LICENSE-MIT)), at your option. Built by VEO Labs.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any
additional terms or conditions.
