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
  and their conformance suites.

Build and test with `scripts/test.sh` (cargo).

Licence: to be chosen with exlumen's (Apache-2.0 or MPL-2.0); private
until then. Built by VEO Labs; first user is the exlumen engine.
