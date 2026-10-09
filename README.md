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

Build and test with `scripts/test.sh` (cargo). Try it:

```bash
cargo run -q -p stepmeld -- performers        # what a performers file looks like
cargo run -q -p stepmeld -- add contracts/fixtures/step-definition.v1/detect-targets.json contracts/fixtures/workflow-definition.v1/find-targets.json
cargo run -q -p stepmeld -- create find-targets --id wf-1
cargo run -q -p stepmeld -- show wf-1
```

## Licence

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT licence
([LICENSE-MIT](LICENSE-MIT)), at your option. Built by VEO Labs.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the
Apache-2.0 licence, shall be dual licensed as above, without any
additional terms or conditions.
