# Orchestrators and Workers

*2026-10-17. How stepmeld separates deciding what runs from running
it. Decisions are Max's unless labelled "my choice"; a label marks
what is open to change without a new ruling. Superseded in part by
`docs/PLACEMENT.md` (2026-10-20), which settles §4's flavors and
mechanisms and the first question of §6; this note still describes the
first form as built.*

## 1. Two roles

| Role | Does | Holds |
|---|---|---|
| **Orchestrator** | Advances Workflows: judges Gates, mints Runs, places them, applies what comes back. | Nothing of its own. It reads and writes the shared store. |
| **Worker** | Runs the work an Orchestrator ordered, and reports how it goes. | A private store: its name, its programs, where Runs keep their files. |

A machine may be either or both. A person's own machine is usually
both; a machine that only does work is usually a Worker alone.

- **Definitions and Workflows are machine-independent.** Any
  Orchestrator can advance any Workflow, whether or not its own machine
  can run any of the Workflow's steps.
- **A Worker never writes a Workflow.** It reads orders addressed to
  it and writes reports on them. Only Orchestrators write Workflows and
  their History.

## 2. Orders

An Orchestrator reaches a Worker by placing an **order** in the shared
store: the Run's request, addressed to the Worker by name. The Worker
takes its open orders, runs each through its own Performer, and writes
a **report** on the order: progress while it runs, the outcome when it
ends, and the log so far. A cancel is a request on the order, which the
Worker carries out.

This is the run directory protocol (`contracts/run-directory.v1.md`)
lifted from a folder to the shared store. It settles two questions the
design note had left open (`docs/DESIGN.md` §11):

- **How a Performer is reached on another machine.** On the
  Orchestrator's side, one Performer places orders and reads reports.
  On the Worker's side, the local Performer runs programs as before.
- **Who keeps a Run alive.** The Worker owns the execution; any
  Orchestrator can read the report. A Run no longer stalls because the
  Orchestrator that started it went away.

## 3. What a Worker keeps, and what it publishes

| | Kept by | Read by | Holds |
|---|---|---|---|
| **Private** | the Worker's own store | that Worker | its name, the command for each program it has, where Runs keep their files, whether it runs work in the cloud |
| **Published** | the shared store | every Orchestrator | its name, its host, whether it is a cloud Worker, the names of the programs it has, when it was last seen |

Machine-specific paths never reach the shared store, which several
machines may share.

## 4. Performers are global; configuration is data

The kinds of Performer are fixed for a deployment: the same code on
every Orchestrator and every Worker, with behaviour driven by
configuration read from the stores. Programs are the per-machine part.

**A StepDefinition names its program** (Max, 2026-10-18), and each
Worker maps that name to a command on its own machine. So a
StepDefinition says what does its work, and placement stays free: a
local Worker maps the name to the program itself, a cloud Worker to a
launcher that hands the same work to a queue. A Worker offers every
StepDefinition whose program it has; an Orchestrator works that out
from the Library and the Worker's published programs. (Registering a
command per StepDefinition was weighed and set aside: with the program
named in the StepDefinition it would repeat it.)

- **On an Orchestrator**, an order is the one way to reach a Worker
  (this note first called it a flavor and left room for others;
  `docs/PLACEMENT.md` §3 retires the word).
- **On a Worker**, a Performer decides *how* the work runs (first
  called a mechanism, also retired). stepmeld ships one: a local
  program per program name, through the run directory. The Docker and
  Batch Performers follow (`docs/PLACEMENT.md` §10).

**Locality comes from the Worker** (my choice of rule): a cloud Worker
is `cloud`; a Worker on the Orchestrator's own host is `this-machine`;
any other is `local-network`. So "confirm before cloud" always gets a
stable answer.

**A placement names the Worker.** Performer names
are Worker names, so a Run's record says which machine runs it, and an
Orchestrator that takes over the Workflow later reads the same answer.

## 5. In the code

| Piece | Where |
|---|---|
| `OrderBook`: the shared store's orders and published Workers, with an in-memory implementation and a conformance suite | `stepmeld-core/src/orders.rs`; the SQLite store implements it in the same file as the Workflows |
| `OrderPerformer`: the Orchestrator's Performer for one published Worker | `stepmeld-core/src/orders.rs` |
| `Worker`: the loop that takes orders, runs them through a Performer and reports | `stepmeld-core/src/orders.rs` |
| The Worker's private store | `stepmeld-sqlite` (`WorkerStore`), one SQLite file |
| Contracts | `stepmeld/order.v1`, `stepmeld/worker.v1` |
| The command | `stepmeld worker …` sets up this machine's Worker (`worker program set <program> -- <command…>`); `stepmeld work` runs it; `stepmeld tick` orchestrates |

My choices in the first form, each open to change:

- The private store's default file is `stepmeld-worker.sqlite` in the
  current folder, beside the shared store's `stepmeld.sqlite`, so the
  walkthrough needs no flags. A Worker's name defaults to the host name.
- A report carries the whole log so far, replaced on each report. Fine
  for small logs; a long one wants appending.
- A cancel asked before the Worker took the order ends it as canceled
  without starting it (since 2026-10-20 the Orchestrator ends it, so a
  Worker that is away does not hold it up).
- An order is addressed to one named Worker. "Any Worker that offers
  the StepDefinition" is not built.
- Since 2026-10-20 a Worker claims each order under a lease and its
  published entry is its heartbeat (`docs/PLACEMENT.md` §5–§6).

## 6. Not decided yet

Carried into `docs/PLACEMENT.md`: the first is settled there (§5, §6),
the others remain open (§11).

- What an Orchestrator does about a Worker that has stopped being seen
  while it holds a live Run (today: the Run stays live, as before).
- How a Workflow says where it is orchestrated, if it should say at all,
  and how an Orchestrator-only store works offline.
- Users and permissions on the shared store.
