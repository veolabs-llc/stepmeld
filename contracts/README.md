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

Behavioural contracts (Commands, Performer, StateStore) are described
in `docs/DESIGN.md` §4 and held by the conformance suites in
`stepmeld-core`. A change to a contract is expand, then contract.
