# Contracts

Language-neutral, versioned. Each contract is a JSON schema in
`schemas/` (draft 2020-12), named `stepmeld/<name>.v<n>`, with golden
fixtures in `fixtures/<name>.v<n>/` that every implementation must
read and validate. Scenario fixtures (`fixtures/scenarios/`) hold the
core: a sequence of commands and observations and the state expected
after each.

| Contract | Kind |
|---|---|
| `stepmeld/step-definition.v1` | data: a verb |
| `stepmeld/workflow-definition.v1` | data: a recipe |
| `stepmeld/workflow.v1` | data: one occurrence, with its Steps and Runs |
| `stepmeld/history.v1` | data: one append-only record |
| `stepmeld/run-request.v1` | the run directory: what a program is handed |
| `stepmeld/run-progress.v1` | the run directory: how far a program is |
| `stepmeld/run-outcome.v1` | the run directory: how a Run ended (the same shape as a Workflow's outcome) |
| `stepmeld/run-cancel.v1` | the run directory: a cancel asked for, and its deadline |

The run directory protocol, how a Performer and a program that does a
Step's work speak on one machine, is `run-directory.v1.md`; the four
`run-*` schemas are its files.

Behavioural contracts (Commands, Performer, StateStore) are described
in `docs/DESIGN.md` §4 and held by the conformance suites in
`stepmeld-core`. A change to a contract is expand, then contract.
