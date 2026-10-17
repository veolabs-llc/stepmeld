# stepmeld

A generic workflow engine: a Library of verbs (StepDefinitions) and
recipes (WorkflowDefinitions); Workflows made of Steps, each run by a
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
- `stepmeld-local`: a Performer that runs a program per verb on this
  machine (files-in-a-directory protocol; any language).
- `stepmeld-verb`: the program side of that protocol for a verb
  written in Rust (the Python twin is `python/`'s `stepmeld.verb`).
- `stepmeld-cli`: the `stepmeld` command: `tick` orchestrates, `work`
  runs this machine's Worker (the crate is named `stepmeld`:
  `cargo install stepmeld`).

Build and test with `scripts/test.sh` (cargo).

## Walkthrough: add two integers

`examples/hello` is a workflow with one step that adds two integers,
reporting progress as it goes. It has three files:

- `add.json`, the verb (a StepDefinition): two Inputs, `a` and `b`; a
  Parameter, `beat-seconds`, with its default; one Output, `sum`.
- `hello.json`, the recipe (a WorkflowDefinition): one step, named
  `add`, that uses the verb.
- `add.py`, the program that does the work. It reads its Inputs,
  writes progress three times, and writes its Output, through the
  Python helper `stepmeld.verb`.

A tag such as `integer` is a label, not a type: the engine never looks
inside a value. A verb checks its own Inputs, as `add.py` does, and
refuses in words what it cannot use.

Two roles take part (`docs/ORCHESTRATOR-WORKER.md`). The
**Orchestrator** (`stepmeld tick`) advances Workflows. The **Worker**
(`stepmeld work`) runs the programs on this machine. They meet in the
shared store, `stepmeld.sqlite`: the Orchestrator places an order, the
Worker carries it out and reports. The Worker keeps its own settings
and programs in `stepmeld-worker.sqlite`. Both files are made in the
current folder the first time a command needs them.

Install the command and the Python helper (from the repository root):

```bash
cargo install --path stepmeld-cli
```
```bash
python3 -m venv .venv && . .venv/bin/activate && pip install ./python
```

Then, from `examples/hello`, with the virtual environment active (the
Worker runs `python3 add.py`, so `python3` must find the helper):

```bash
cd examples/hello
stepmeld add add.json hello.json                    # the verb and the recipe, into the Library
stepmeld worker program add add -- python3 add.py   # this machine's Worker can perform `add`
```

Start the Worker in a second terminal (same folder, same virtual
environment) and leave it running:

```bash
stepmeld work --watch
```

Back in the first terminal, make a Workflow and orchestrate it:

```bash
stepmeld create hello --id hello-1                  # a Workflow from the recipe
stepmeld set hello-1 add --input a --value 2
stepmeld set hello-1 add --input b --value 3
stepmeld start hello-1                              # a new Workflow runs nothing until started
stepmeld tick --watch                               # orchestrate until nothing is running
```

`tick --watch` places the step on the Worker, which takes the order
and runs `add.py`; each tick reads the Worker's latest report. While it
runs, `stepmeld show hello-1` shows the progress the last tick read
(`adding 2 of 3 beats`); when it is done (the Worker is named after
your machine):

```text
$ stepmeld show hello-1
hello-1  (hello v1, succeeded)
  add              succeeded
      in   a              2
      in   b              3
      par  beat-seconds   1  (default)
      out  sum            5
      run 1 on my-laptop by policy start-automatically: succeeded: 2 + 3 = 5
```

`stepmeld log hello-1 add` prints the program's output, `stepmeld
history hello-1` every event in order, and `stepmeld worker` the
Worker's name and programs. Without a Worker, `tick` says what it
waits for: `add: no Worker offers it`. Each Run's files (`request.json`,
`progress.json`, `outcome.json`, `log.txt`) are in the Worker's `runs/`;
the protocol is `contracts/run-directory.v1.md`.

## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](LICENSE-MIT)), at your option. Built by VEO Labs.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any
additional terms or conditions.
