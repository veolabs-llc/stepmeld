# stepmeld

A generic workflow engine: a Library of verbs (StepDefinitions) and
recipes (WorkflowDefinitions); Workflows made of Steps, each run by a
Performer when its Gate opens; every Run recorded, nothing overwritten.

The engine knows nothing of any domain. Values are opaque; Performers
do the work; a UI reads documents and never gets called.

- `docs/DESIGN.md`: the design note: vocabulary, shape, rules,
  scenarios. Read it first.
- `contracts/`: the contracts, as JSON schemas with golden fixtures,
  and the scenario fixtures every implementation is held to.
- `stepmeld-core`: the Rust implementation: documents, the pure core,
  the Performer and StateStore seams with in-memory implementations
  and their conformance suites, the Driver, the nested-workflow
  Performer; the design note's scenarios as tests.
- `stepmeld-sqlite`: the StateStore in one SQLite file.
- `stepmeld-local`: a Performer that runs a program per verb on this
  machine (files-in-a-directory protocol; any language).
- `stepmeld-verb`: the program side of that protocol for a verb
  written in Rust (the Python twin is `python/`'s `stepmeld.verb`).
- `stepmeld-cli`: the `stepmeld` command over the two (the crate is
  named `stepmeld`: `cargo install stepmeld`).

Build and test with `scripts/test.sh` (cargo).

## Walkthrough: add two integers

`examples/hello` is a workflow with one step that adds two integers,
reporting progress as it goes. It has four files:

- `add.json`, the verb (a StepDefinition): two Inputs, `a` and `b`; a
  Parameter, `beat-seconds`, with its default; one Output, `sum`.
- `hello.json`, the recipe (a WorkflowDefinition): one step, named
  `add`, that uses the verb.
- `add.py`, the program that does the work. It reads its Inputs,
  writes progress three times, and writes its Output, through the
  Python helper `stepmeld.verb`.
- `performers.json`, which tells this machine which program performs
  the `add` verb, and where each Run keeps its files.

A tag such as `integer` is a label, not a type: the engine never looks
inside a value. A verb checks its own Inputs, as `add.py` does, and
refuses in words what it cannot use.

Install the command and the Python helper (from the repository root):

```bash
cargo install --path stepmeld-cli
```
```bash
python3 -m venv .venv && . .venv/bin/activate && pip install ./python
```

Then, from `examples/hello`, with the virtual environment active (the
performers file runs `python3 add.py`, so `python3` must find the
helper):

```bash
cd examples/hello
alias sm='stepmeld --store hello.sqlite --performers performers.json'
```
```bash
sm add add.json hello.json         # put the verb and the recipe in the Library
sm create hello --id hello-1       # a Workflow from the recipe
sm set hello-1 add --input a --value 2
sm set hello-1 add --input b --value 3
sm start hello-1                   # a new Workflow runs nothing until started
sm tick --watch                    # the driver: start, observe, apply, until done
```

Nothing runs between ticks: `tick --watch` drives the Workflow until
nothing is running, and each tick reads the program's progress. While
it runs, `sm show hello-1` in another terminal shows the progress the
last tick read (`adding 2 of 3 beats`); when it is done:

```text
$ sm show hello-1
hello-1  (hello v1, succeeded)
  add              succeeded
      in   a              2
      in   b              3
      par  beat-seconds   1  (default)
      out  sum            5
      run 1 on local by policy start-automatically: succeeded: 2 + 3 = 5
```

`sm log hello-1 add` prints the program's output and `sm history
hello-1` every event in order. Each Run's files (`request.json`,
`progress.json`, `outcome.json`, `log.txt`) are in `runs/`; the
protocol is `contracts/run-directory.v1.md`.

## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](LICENSE-MIT)), at your option. Built by VEO Labs.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any
additional terms or conditions.
