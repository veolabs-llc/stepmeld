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
  a Step named `add` that uses `stepdef_add`, a Step named `multiply`
  that uses `stepdef_multiply`, and one Binding, from `add.sum` to
  `multiply.value`. That Binding is the only order there is: `multiply`
  waits until `add` has a sum to give it.
- `src/main.rs`, the program `hello-program`. It does the work of both
  StepDefinitions: it reads which one the request names, checks the
  Inputs, writes progress three times and writes the Output, through
  the Rust helper `stepmeld-verb`. (A program can be written in any
  language; `python/`'s `stepmeld.verb` is the same helper for Python.)

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

Install the command and the example's program (from the repository
root; both land in `~/.cargo/bin`, which must be on your `PATH`):

```bash
cargo install --path stepmeld-cli
```
```bash
cargo install --path examples/hello
```

Then, from `examples/hello`, put the definitions in the Library and
tell this machine's Worker which command runs `hello-program`:

```bash
cd examples/hello
stepmeld library add stepdef_add.json stepdef_multiply.json workflowdef_hello.json
stepmeld worker program set hello-program -- hello-program
```

(`stepmeld worker` now shows the Worker's program and what it offers:
`stepdef_add v1, stepdef_multiply v1`.)

Start the Worker in a second terminal (same folder) and leave it
running:

```bash
stepmeld work --watch
```

Back in the first terminal, make a Workflow, give it the Inputs nobody
else feeds, and orchestrate it:

```bash
stepmeld create workflowdef_hello --id hello-1
stepmeld set hello-1 add --input a --value 2
stepmeld set hello-1 add --input b --value 3
stepmeld set hello-1 multiply --input factor --value 4
stepmeld start hello-1        # a new Workflow runs nothing until started
stepmeld tick --watch         # orchestrate until nothing is running
```

`set` names a Step (`add`, `multiply`), not a StepDefinition.
`multiply`'s `value` is not set by hand (`set` refuses: it is fed by a
Binding). `tick --watch` places `add` on the Worker, which runs
`hello-program`; when `add` succeeds its sum is carried to `multiply`,
which then runs in turn. While it runs, `stepmeld show hello-1` shows
the progress the last tick read (`adding 2 of 3 beats`); when it is
done (the Worker is named after your machine):

```text
$ stepmeld show hello-1
hello-1  (workflowdef_hello v1, succeeded)
  add              succeeded
      in   a              2
      in   b              3
      par  beat-seconds   1  (default)
      out  sum            5
      run 1 on my-laptop by policy start-automatically: succeeded: 2 + 3 = 5
  multiply         succeeded
      in   factor         4
      in   value          5
      par  beat-seconds   1  (default)
      out  product        20
      run 1 on my-laptop by policy start-automatically: succeeded: 5 × 4 = 20
```

`stepmeld log hello-1 multiply` prints that Step's program output,
`stepmeld history hello-1` every event in order, and `stepmeld library`
the StepDefinitions and WorkflowDefinitions. Without a Worker, `tick`
says what it waits for: `add: no Worker offers it`. Each Run's files
(`request.json`, `progress.json`, `outcome.json`, `log.txt`) are in the
Worker's `runs/`; the protocol is `contracts/run-directory.v1.md`.

## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](LICENSE-MIT)), at your option. Built by VEO Labs.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any
additional terms or conditions.
