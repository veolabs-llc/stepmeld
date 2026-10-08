# stepmeld

The generic workflow engine. `README.md` is the map; `docs/DESIGN.md`
is the design and the vocabulary, and the words there are the words
here (a Run, not an attempt; a Performer, not an executor; a Gate, not
a scheduler).

- Rules are patterns, not laws (Max, 2026-09-04): everything in this
  file and the docs is a preference that worked once. When one bites,
  name the incident and negotiate it.
- This repository is public (MIT or Apache-2.0) and upstream of
  everything that uses it. Nothing here names a domain (no photos,
  runs-as-artifacts, scenes), a provider, a UI, or a downstream
  repository, product or machine. A Value is opaque; a tag is a string
  compared for equality.
- The documents are the contract, not the Rust types: `contracts/`
  holds the JSON schemas and golden fixtures, and `stepmeld-core` is
  held to them by test. Scenario fixtures hold the core itself; a
  second implementation in another language is held to the same files.
  Contract changes are expand, then contract.
- The core is a pure function over documents: no clock, no I/O, no
  threads inside it. Time is passed in. Persistence, Performers and
  drivers sit at the edge.
- Every seam (Performer, StateStore) ships an in-memory implementation
  and a conformance suite beside the trait.
- Tests: `scripts/test.sh` before a PR, every time; no GitHub gate, by
  choice. Suites stay well under a minute.
- Edit in a worktree; the main checkout stays on `main`. Small PRs.
