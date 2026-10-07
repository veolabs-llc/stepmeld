# stepmeld — design note

*Moved here from veokit/docs/STEPMELD-DESIGN.md on 2026-10-06. Drafted 2026-10-06 from the design conversation of 2026-10-04 to
2026-10-06 (Max and Claude). It records agreed vocabulary and shape;
nothing is built. Where a point is still my recommendation rather than
Max's ruling it is labelled. The note lives here until the `stepmeld`
repository exists, then moves there; veokit issues 17 and 18 refer to
it.*

## 1. Why

The review of `conductor-core` (veokit 17) found that there is no
workflow runner to extract: what issue 18 calls the runner is a set of
per-step launch functions and one fold function that knows every step,
every route and the publish chain by name. Rather than carve that into
a runner, the decision is to design a generic workflow engine and build
it in a repository of its own, open source, upstream of exlumen:

```
stepmeld  <-  exlumen  <-  veokit  <-  gt, kima
```

The engine is domain-agnostic. gt will need an expressive, multi-user
workflow engine; kima's pipelines and a render farm fit the same model;
exlumen needs an open runner for its open workflows. One engine, three
users, which is the second-domain test a generic design needs.

The name: `stepmeld`, free on crates.io, PyPI, npm and GitHub as of
2026-10-06.

## 2. Goals and non-goals

The engine must:

- know nothing of the domain, and almost nothing of the data that flows
  through it;
- define success, failure and exceptions once;
- define workflow and step status once, derived and never stored
  separately;
- carry per-step policies for whether a person must act;
- log and measure in one shape, enough for a UI to show status and for
  a reader to estimate time left;
- be agnostic about the tools a step uses and where it runs, while
  knowing whether a step runs on this machine, the local network or in
  the cloud;
- keep the UI entirely outside: the engine ships documents, never
  screens.

Out of scope now, and not to be obstructed: expressive permissions,
concurrent multi-user use with contention resolution, nested workflows
beyond what §6 gives, fan-out over lists.

Branching and optional steps are in v1: they would be annoying to
retrofit.

## 3. Vocabulary

Two kinds of thing throughout: what is in the Library (definitions,
recipes; never any values) and what exists once a workflow is minted
(instances; the only place values live).

### 3.1 Library side

| Term | Meaning |
|---|---|
| **Library** | The WorkflowDefinitions and StepDefinitions available for composing and running. Running never writes to it. |
| **StepDefinition** | A verb: its InputDefinitions, ParameterDefinitions and OutputDefinitions. What a step's implementation uses (a tool, a program, an image) is hidden inside it; the engine never sees it. |
| **WorkflowDefinition** | A recipe: a set of named step entries, each a name and the StepDefinition it refers to (with version), plus Bindings. Versioned, immutable. It declares no inputs or outputs of its own (§5.3). |
| **Binding** | A connection from one named step's Output to another named step's Input, optionally guarded. The only structure in a recipe; there is no step order. |
| **InputDefinition** | A name, a tag, whether required. |
| **ParameterDefinition** | The same, plus a default. |
| **OutputDefinition** | A name and a tag. A **Decision** is an OutputDefinition whose options are declared (`accept`, `refine`) and whose Value the engine may read. |
| **Guard** | A condition on a Binding, over facts the engine holds: step statuses and Decisions. Composed from a few fixed forms; no expression language. |
| **Policy** | Per-step rules from a fixed menu (§5.6). |
| **Tag** | An opaque label (`file-list`, `camera-pose`) on a definition and its Value; compared for equality, used to look up presenters and Performers. Not a type system. |

Every WorkflowDefinition is also offered by the Library as a
StepDefinition with its derived face (§6), so recipes nest.

### 3.2 Workflow side

| Term | Meaning |
|---|---|
| **Workflow** | One occurrence of a WorkflowDefinition, with its Steps. The only place Values live. |
| **Step** | One occurrence of a StepDefinition inside a Workflow, carrying the name the recipe gave it. |
| **Input, Parameter, Output** | A Step's slots, each holding a Value or empty. |
| **Value** | An opaque blob the engine never reads, usually a reference into a store. Compared for equality. |
| **Run** | One execution of a Step: its Inputs and Parameters as resolved, its Performer and placement, its handle, its progress, its Outcome, its metrics. Append-only: a retake is a new Run. A data-layer run (§8) is the durable trace of one. |
| **Gate** | A Step's go/no-go: the conjunction of its Predicates (§5.5). Derived each time the engine looks. |
| **Status** | Derived, never stored. Step: not ready, ready, running, waiting on a person (with the Predicate that is closed), succeeded, failed, skipped, stale. Workflow: follows from its Steps. |
| **Stale** | A succeeded Step whose Inputs or Parameters now differ from its current Run's record (§5.7). |

### 3.3 Execution side

| Term | Meaning |
|---|---|
| **Performer** | What does a Step's work (§4.2). Declares its verbs and its locality; chosen per Step. |
| **Locality** | A Performer's answer to where: this machine, the local network, the cloud. A Workflow has none of its own. |
| **Placement** | The act and record of choosing a Performer for a Run, by an Actor or a Policy. |
| **Handle** | The Performer's own id for a Run's work. |
| **Outcome** | How a Run ended: succeeded, failed, canceled or lost, with a class, a reason, a Summary and, on success, the Outputs. Refused is a start that never ran. "Could not be asked" is never an Outcome. |
| **Summary** | The Performer's account of what it did: a headline and opaque details. |
| **Actor** | Who is responsible for a decision: a person, an agent, or a Policy (which names the recipe version it came from). On every record. Max: the label may yet change. |
| **Command** | What an Actor issues: create, set, start, approve, retry, skip, cancel, complete. With Performer reports, the engine's only input. |
| **History** | The append-only record of Commands, Placements, progress and Outcomes, each with its Actor and time. Not "events": that word is kept for an external signal, should one be needed. |
| **StateStore** | The engine's persistence: read, conditional write, append, list, and a lease per Workflow. SQLite first; a server later. |

Retired words: Definition and Template (for the recipe), Attempt and
Take (for a Run), chain, route, tool (as an engine noun).

## 4. Shape

### 4.1 A pure core

The engine is a function:

```
(Workflow, History, Command | observation, now)  ->  (Workflow', History', effects)
```

where the effects are Performer calls to make (`start`, `observe`,
`cancel`). Persistence, clocks and Performers sit at the edge. The core
is held to scenario fixtures (§7), which any language can run.

The loop a driver runs:

1. evaluate every Step's Gate;
2. start every open one (mint a Run, place it, call `start`);
3. observe every live Run;
4. apply what came back (Outputs, Outcomes, progress) to the Workflow
   and the History;
5. repeat.

Parallelism is not a feature; it is the absence of order. Two Steps
whose Gates are open start together, in different localities. A Step
fed by two branches waits for both by its Inputs Predicate; one Output
feeding five Steps opens five Gates.

With a server, several processes may hold the same StateStore. One
driver advances a Workflow at a time, under a lease held in the
StateStore. (The publish chain's lease, one level up.)

### 4.2 Performer

The engine's only way to make something happen outside itself.

```
describe()               -> name, locality, the verbs it performs
start(step)              -> handle | refused (in words; nothing ran)
observe(handle)          -> running(progress, attention?) | ended(outcome) | absent | unreachable
cancel(handle, reason)      canceling ended work is not an error
log(handle)              -> raw output, while it keeps it
estimate(step)           -> optional: cost or duration, changing nothing
```

- Every call is short; the Performer is asked repeatedly. A Step that
  takes six hours elsewhere costs the engine nothing while it waits.
- Absent, unreachable and refused are three answers. Unreachable
  changes nothing.
- The Performer decides success (Max's ruling), subject to one engine
  rule: a success must supply the Outputs the StepDefinition declares,
  or it is a fault of the Performer. A Step declaring no Outputs
  succeeds on the Performer's word.
- `start` must be idempotent: asked twice for the same Run, it returns
  the same handle.
- Lost is the Performer's word for work it once started and cannot find.
  Final; a lost Run is retaken as a new Run, never revived.
- A running report may carry an **attention** flag with a reason
  ("blocked inside: layers failed"; "quota exceeded, waiting"), for a
  Performer whose work needs a person without having ended.
- A Performer never reads or writes a Workflow, never retries, never
  holds for review. It reports; the engine and its Policies decide.
- Capacity is the Performer's: if five Gates open and a GPU takes one
  job, the Performer accepts five and reports four queued.

A Step a person completes has no Performer; it is finished by a
`complete` Command carrying its Outputs.

Kinds expected: a local process, a container on this machine (what
exlumen's `execution.v1` local executor is), fleet orders (veokit),
AWS Batch (veokit), and the nested-workflow Performer, which lives
inside the engine because it issues Commands (§6).

The Performer contract gets a wire form, not only a Rust trait, so a
Performer in another language or process can serve a Rust engine.

### 4.3 Contracts

Language-neutral, versioned, with golden fixtures and conformance
suites, as `exlumen/contracts` does today.

| Contract | Kind | States |
|---|---|---|
| WorkflowDefinition, StepDefinition | data | §3.1 |
| Workflow, Run | data | §3.2; also what a UI reads |
| History | data | §3.3 |
| Outcome, Status | vocabulary | cited by the others |
| Commands | behavioural | the engine's own face |
| Performer | behavioural | §4.2, with a wire form |
| StateStore | behavioural | read, conditional write, append, list, lease |

The documents, not the Rust types, are the contract: JSON schemas from
day one, so a second implementation reads the same StateStore. Each
behavioural contract ships an in-memory implementation and a
conformance suite beside it.

## 5. Rules decided

1. **Values exist only on the Workflow side.** The Library describes
   what can be done; a Workflow records what was done and with what.
   Parameter defaults live in the Library as part of describing the
   verb (my recommendation, accepted by silence); Inputs have no
   defaults.
2. **The engine never reads inside a Value.** It knows a name, a tag,
   whether required, and whether a Value is present. It compares Values
   for equality and passes them through. Present and edit callbacks are
   the presentation layer's, registered by tag; the engine never calls
   them. The one exception is a Decision, whose Value is one of its
   declared options.
3. **A workflow's face is derived.** Required Inputs no Binding feeds
   are what a user must provide; every Step's Output is addressable by
   path (`dense.cloud`). A value two Steps need comes from a Step with
   no Inputs (today's `sources` step), never from the Workflow itself.
4. **Names are the only per-use difference in a recipe.** Two entries
   for the same StepDefinition differ by name; everything else that
   makes the Steps differ arrives after minting. A step entry may carry
   a display label. (Per-recipe overrides of defaults and policy were
   withdrawn; add them if a real case appears.)
5. **The Gate is a conjunction of Predicates**, checked and reported in
   this order:
   1. no live Run, and either not yet succeeded or stale;
   2. on a live path: at least one guarded Binding into it applies.
      When none can ever apply the Step is skipped, which is final;
   3. every required Input holds a Value;
   4. every required Parameter holds a Value;
   5. go given: the Policy starts automatically, or an Actor issued
      `start`;
   6. placed: a Performer is chosen, and confirmed where the Policy
      demands it for that locality. Confirmations are evaluated per
      Step at any depth of nesting, shown at the top.

   Predicates 1-4 are flipped by facts (Outcomes filling Outputs,
   Bindings carrying them on); 5-6 by Commands or Policies. Status is
   the Gate's answer, and its reason is the first closed Predicate.
6. **All rules are known ahead of time; there is no runtime referee.**
   Guards are fixed forms over step statuses and Decisions. Policies
   are a menu: start automatically or on Command; on success accept or
   hold for review; on failure stop or retake up to N times for named
   classes; on stale wait or rerun; placement fixed, chosen, or
   confirmed before a locality. Anything the engine cannot judge from
   what it holds becomes a Step whose Run is the judge: a Performer's
   check or a person's review producing a Decision. A callback would be
   an invisible second way to run domain code; a Decision Step is the
   same judgement with a Run, an Actor, a Summary and a place in the
   History.
7. **Stale.** A Step is stale when its Inputs' or Parameters' Values
   differ from its current Run's record. A stale Step keeps its Outputs
   (its Run's artifacts are real and still bound downstream). A stale
   Step never starts on its own: "start automatically" applies to a
   Step that has not succeeded; the recipe may say `on stale: rerun`
   per Step. Stale spreads only on success: Outputs are replaced when a
   new Run succeeds, and only then do the Steps bound to them become
   stale.
8. **Current Run.** A Step's status follows its latest Run; its Outputs
   follow its latest succeeded Run. After a failed retake they differ,
   and a UI shows both.
9. **Time is injected.** Timeouts and schedules are Policies that need
   the engine's clock; the engine is told the time, so the fixtures stay
   deterministic.
10. **Rerunning a verb to run a different verb is not a loop.** A
    rejected solve is "change something and retake solve", by Command.
    No cycles in v1.

## 6. Nesting

Every WorkflowDefinition is offered as a StepDefinition with its derived
face, names by path: `chunk.cloud`, `layers.layer`. The nested-workflow
Performer performs all such verbs: `start` issues `create` for a child
Workflow (Actor: the parent's Run), the handle is the child's id,
`observe` reads the child's status and derives progress, `cancel`
cancels the child, and the child's Outputs map back by path. A retake of
the parent Step is a new child Workflow; the old one stays in the
History, linked by its handle. A child that needs attention is reported
running with the attention flag, so the person fixes it inside the
child and the parent proceeds when the child does.

The publish chain becomes a child recipe: `chunk` then `layers` and
`warm` in parallel. Its `commit` is the dense Run's success and its
`pull` is the chunk Performer's own fetching. Leases, capabilities,
`waiting`, `blocked` and `interrupted` were the chain's own scheduler;
here they are Gates, Performers and the attention flag.

Nesting also answers issue 18's hardest question: exlumen ships
"photogrammetry", which ends at dense; veokit's "photogrammetry layer"
nests it and adds publish. The open recipe never names a viewer.

## 7. The scenarios

Four walks tested the vocabulary; they become the first conformance
fixtures.

- **A. Photogrammetry layer.** `photos` (a person) -> `solve` ->
  `review` (a person; Decision accept|refine) -> `refine` (guarded) ->
  `dense` (Input `model` bound to `refine.model`, else `solve.model`;
  confirm before cloud; retake once on lost) -> `publish`. Dense's Run 1
  lost (Policy retakes), Run 2 failed out of memory (Policy stops; a
  person sets image-size and starts Run 3). Found: "waiting on a person"
  needs its reason; a Decision is just a readable Output; Policy as
  Actor reads fine.
- **B. Rerunning upstream.** Changing `solve.matcher` after everything
  succeeded. Found: the stale rules (§5.7), the current-Run rule (§5.8),
  "superseded" as a data-layer word for a Run a later Run replaced (the
  sweep can reclaim one nothing references), and that re-reviewing may be
  unwanted (`on stale: keep`, left out until asked for).
- **C. Publish as a nested workflow.** §6. Found: the attention flag;
  confirmations per Step at any depth; dotted names are the only new
  syntax.
- **D. The same verb twice.** Two photo sets, `detect-early` and
  `detect-late` in parallel in two localities with different Parameters,
  joined by `compare` (Decision stable|moved) guarding `flag`. One side
  fails and is retaken alone. Found: nothing new in the vocabulary;
  stale is per Input; step entries want display labels; "any number of
  sets" is fan-out, deferred (Steps minted at run time with generated
  names; the id scheme allows for it from the start).

## 8. Relation to what exists

| Today | Under stepmeld |
|---|---|
| A data-layer run (id, manifest, prefix) | The durable trace of one Run: the run id is the Run's, the manifest is its record, the prefix is where its Outputs live. A failed Run is an uncommitted prefix; a retake is a new run id, as now. Sharded dense is one Run whose Performer runs a job plan. An executor's retry stays beneath one Run. |
| The `runs` space also holding status, job, workflow, chain, dispatch and machine documents | Most of it becomes the StateStore |
| `workflow.v1` with its hard-coded kinds, routes and publish fields | WorkflowDefinition + Workflow documents; `aws-batch`, `dispatch`, `conductor-local` become Performers |
| `Compute`, `execution.v1` | `execution.v1` stays exlumen's contract for container jobs; one adapter presents an executor as a Performer; the engine never sees a job |
| The publish chain (`chain-state.v2`) | A child recipe (§6) |
| `fold_workflow` and the five step tables in conductor-core | The loop (§4.1) and the Library |
| The glossary's "run": a computation output | One execution of a Step and what it produced; same referent, wider sense |

veokit 17's refactoring plan is unchanged in its first phases (carving
the document helpers and the leaves off `Direct`) and changes in its
later ones: instead of building a runner inside conductor-core, the
launch and fold code is retired in favour of Performers for stepmeld.
No backward compatibility is owed to today's workflow logic.

## 9. Ease of use

The complexity sits with recipe authors, not with people who run
recipes. A user picks a WorkflowDefinition and sees Steps with reasons
("needs photos", "waiting for your OK", "running on RyzenBox"); they
never meet a Binding. Authors meet Bindings, but the recipe format may
infer the obvious ones (one Input below one Output of the same tag) as
sugar compiled to explicit Bindings before anything runs. Defaults cover
the rest: start automatically, place on this machine, stop on failure.
A simple recipe is a list of verbs.

## 10. Prior art

- **TigerTool** (Max's 2025 engine, `WORKFLOW-ENGINE-BRIEF.md`): the
  whole machine in one readable place; relational spine with opaque
  payload; atomic compare-and-swap transitions; transition-time inputs
  on the record, not merged into state (our Runs); named registered
  predicates, no expression language (our Guards and Policies); validate
  definitions on save; delete any config field with no reader; a
  generic engine is cheap and a generic application is expensive, so
  the UI stays out and a second domain exercises the claims.
- **wfe** (`github.com/sunbeamdotpt/wfe`, MIT): not built on (needs a
  live host and a SQL database; control flow over one shared blob; its
  CI/CD domain leaks into its core; one author). Borrowed: one return
  type that tells the engine what to do next (our `observe`); "wait for
  a named event" as the single unblocking mechanism, which our
  nested-workflow Performer uses in spirit; a conformance suite beside
  each trait with in-memory implementations; Suspend as a failure
  policy (our attention); validation at load and schemas generated from
  types; cleanup hooks and compensation, noted for later.

## 10a. Decided while building (2026-10-06)

The first implementation (`stepmeld-core`, `stepmeld-sqlite`,
`stepmeld-local`, `stepmeld-cli`) settled these; each is in the code
and its tests, and none contradicts the rules above.

1. **Verbs are one namespace.** A WorkflowDefinition may not take a
   StepDefinition's name and version, since the Library offers both as
   verbs. Labels may be equal ("Detect OmniTargets" twice), names not.
2. **Port names.** Inputs and Parameters share a namespace (both are
   given); Outputs have their own (a refine takes `model` and gives
   `model`). A port name on a nested recipe's face is dotted
   (`chunk.cloud`); a step name never is, so `step.port` splits at the
   first dot.
3. **Alternative Bindings are taken in order, and an earlier one that
   cannot be told yet holds the answer.** `dense.model <- refine.model,
   else solve.model` waits while the review is undecided rather than
   taking solve's model early.
4. **A source that succeeded once keeps its Outputs while it is
   retaken**, so downstream Inputs hold the last good Value until the
   new Run succeeds (rule 7's "stale spreads only on success").
5. **A confirmation stands until the placement changes.** A Policy
   retake of the same placement is not asked about again; a `place`
   Command clears it. A person's `start` counts as a confirmation.
6. **The driver notes a prospective placement** on a Step waiting for
   confirmation, so a reader with only the document sees the same
   status the driver does.
7. **Children tick before parents**, so a parent sees what its child
   did this tick; a child created mid-tick is ticked in the same call.
8. **Policies live on the verb and on the Step**, not on the recipe:
   the StepDefinition carries defaults; an Actor changes a Workflow's
   Step by `set-policy`. Per-recipe overrides stay withdrawn (rule 4).
9. **The core is synchronous.** No async runtime; every Performer call
   is short by contract, and a driver may thread as it likes.
10. **A person's Run has no locality**, and its Performer word is the
    person.
12. **A Workflow is removed by a person, never archived by the
    engine.** `remove` (the StateStore's, through the driver) forgets
    a Workflow, its History, its lease and the children its Runs made;
    one with a Run still live is refused until canceled. A driver
    ticks every Workflow in the store, finished ones included (a lease
    write each), so a long-lived store is kept short by removing.
11. **The local Performer's protocol is files in a directory**
    (`request.json` in; `progress.json`, `outcome.json`, `log.txt`
    out), so a verb can be a program in any language: the first wire
    form of the Performer contract.

## 11. Open

- A better label than Actor (Max's instinct; none proposed yet).
- Fan-out over lists; roles for human Steps; cross-workflow references;
  `on stale: keep`; a Decision option that marks a Step stale (a
  structured "reject"); timeouts in the Policy menu. All additive.
- The Performer wire form: where it lives (the server is the natural
  place) and its transport.
- The exact fixed forms a Guard may take, and whether a numeric
  engine-readable Output kind joins Decision for thresholds.

## 12. Next steps

1. *(done 2026-10-06)* The `stepmeld` repository, private, licence to
   match exlumen's choice.
2. *(done)* The four data contracts as JSON schemas with golden
   fixtures; scenarios A-D as tests in `stepmeld-core/tests`, which
   stand in for scenario fixtures until a second implementation needs
   language-neutral ones.
3. *(done)* The Rust core as a pure function; in-memory StateStore and
   Performer with conformance suites.
4. *(done)* `stepmeld-sqlite` and `stepmeld-local`.
5. *(done)* "Detect OmniTargets" end to end through the `stepmeld`
   command, with a shell script as the verb.
6. exlumen: the real verbs (`groundtruth solve`, `dense`, OmniTarget
   detection) as programs speaking the local Performer's protocol, and
   the open recipes; veokit: the fleet and Batch Performers and the
   publish child recipe; then veokit 18 closes.
7. The Performer wire form beyond this machine (a server), and the
   scenario fixtures as language-neutral files.
