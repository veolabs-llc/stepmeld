# Placement

*2026-10-20. How stepmeld places work on this machine, on fleet
machines and in the cloud, and what it asks of a deployment. Decisions
are Max's unless labelled "my choice"; a label marks what is open to
change without a new ruling. This note decides; the code follows in
the phases of §10. It supersedes `docs/ORCHESTRATOR-WORKER.md` §4's
flavors and mechanisms, and the passages of `docs/DESIGN.md` that left
locality to the engine's users.*

## 1. Why

Until now stepmeld chose a Performer and left the rest to its users.
The users built the rest themselves: machine heartbeats, orders that
fleet machines claim under a lease, a Docker runner with GPU probing,
an AWS Batch launcher, a deployment file naming stores, executors and
secrets, and an execution contract with its own executors. Every new
user would build them again.

**stepmeld understands and manages Orchestrator and Worker locality**
(Max, 2026-10-20). The split between placement (stepmeld's) and
locality (the users') bought nothing, and it made questions such as a
Worker that stops being seen awkward to answer on either side of it.
When this note is carried out, the engine's users mint images and
programs and configure stepmeld; they no longer control execution.

**A refactor, not new capability.** What moves is what the users do
today, made generic, with the smallest adjustments that make it so.
"The cloud" means AWS Batch as it runs today.

## 2. Deployment shapes

Out of the box, stepmeld works in each of these:

| Shape | Orchestrators | Workers | Store |
|---|---|---|---|
| One machine | one | one, on the same machine | one SQLite file |
| A fleet | one | one or more on fleet machines on the local network | one every machine reaches |
| Many Orchestrators | several, on several machines, advancing one Workflow without side effects | any | one every machine reaches |
| The cloud | any | a Worker whose Performer runs the program as a Batch job | any |

A Worker runs a program in one of three ways: as a command, by loading
and running a Docker image, or as an AWS Batch job that loads and runs
a Docker image.

## 3. Words

No new words. Three existing ones, kept apart:

| Word | Answers | Example |
|---|---|---|
| **placement** | Which Worker runs a Run, or, for an open order, what any Worker taking it must have. A decision, by an Actor or a Policy. | "on `workstation`", "any Worker with `cuda`" |
| **locality** | Where the work happens, as seen from the Orchestrator. A fact derived from the placement, never declared. | this-machine, local-network, cloud |
| **Performer** | How a Worker runs the program it was ordered to run. | a command, a Docker image, a Batch job |

Locality cannot name the third row: a fleet machine may run the same
program as a command or as a Docker image, and both are
`local-network`. A command or a Docker image takes its locality from
the Worker's host as the Orchestrator sees it; a Batch job is `cloud`.

Retired: **mechanism** (a Worker's way of running work: it is a
Performer) and **flavor** (an Orchestrator's way of reaching a Worker:
there is only one, an order).

New for requirements (§7): **requires**, **needs**, **capability**.

## 4. Providers are plugins

- **stepmeld may name a provider** (Max, 2026-10-20), reversing "nothing
  here names a provider". AWS now; Azure and others later.
- **Each provider's code is its own crate**, a plugin: `stepmeld-aws`
  first. The core never imports one; the core, the in-memory
  implementations and the local ones name no provider.
- **What a provider crate supplies**, each registered by kind: a store,
  a Performer (Batch), a kind of secret (Secrets Manager), and signing
  a container in (a registry login, credentials for the job).
- **The `stepmeld` command links provider crates as cargo features**
  (my choice). No dynamic loading.
- Unchanged: nothing names a domain, a UI, or a downstream repository,
  product or machine, and examples stay neutral.

## 5. Orders take over from dispatch

The orders of 2026-10-17 were the first step of moving the users'
dispatch into stepmeld (Max, 2026-10-20); this is the rest of it.

- **Pinned or open.** A pinned order names one Worker, which keeps a
  stable record of where a Run ran. An open order names what a Worker
  must have (its program and every word its StepDefinition `requires`,
  and a locality if the placement says one); any Worker that has it may
  take it. "Next available" is an open order. A cloud order is always
  open.
- **A Worker claims an order** with a conditional write and holds it
  under a lease it renews while the work goes on. The claim records the
  Performer's handle. The lease's length and renewal start where the
  users' dispatch has them, five minutes renewed every minute (my
  choice).
- **When a claim's lease lapses**, what happens depends on whether its
  handle means anything on another machine:

  | Handle | Example | A lapsed lease |
  |---|---|---|
  | portable | a Batch job id | Any Worker with the same Performer takes the claim over and goes on observing. Nothing is lost. |
  | local | a process or a container on one host | The order ends `lost`: re-issued, never revived. The Step's on-failure Policy decides what follows. |

  A Performer says which its handles are.
- **One pattern for both.** An Orchestrator's lease on a Workflow and a
  Worker's lease on an order are the same rule (Max, 2026-10-20): hold
  while working, renew, and another may take over once it lapses.
- **A cancel is a request** on the order, standing until the order ends
  (as built).
- **Slots.** A Worker takes as many orders at once as each of its
  Performers has slots: by default one for a command and one for a
  Docker image, no limit for Batch (my choice).
- **Logs go in the store**, appended as they grow (Max, 2026-10-20),
  rather than replaced whole on each report.

## 6. Workers advertise themselves

- **A Worker's published entry is its heartbeat**, rewritten about once
  a minute: its name, its host, its capabilities, the programs each of
  its Performers has, its slots, the orders it holds, and when it said
  so. A Worker unseen for three beats is away (both numbers from the
  users' heartbeat, my choice to keep).
- **An away Worker is not placed on.** An open order is never taken by
  it; a pinned order it has not claimed waits, and its Step says the
  Worker is away. An order it holds is settled by its lease (§5).
- **A Worker detects some capabilities itself**: `docker` when Docker
  answers, `cuda` and its GPUs when a GPU container can run, and the
  GPU memory in use. This probing is generic (Max, 2026-10-20) and
  comes with the Docker Performer.

## 7. Requirements and resources are opaque

Like a tag, a requirement is a string compared for equality (Max
deferred the design, 2026-10-20; agreed as below).

- **`requires`**, on a StepDefinition: a list of words
  (`["cuda", "vram-24g"]`). A Worker may take the order only if it
  offers every one.
- **`needs`**, on a StepDefinition: an opaque map
  (`{"memory_gib": 32, "gpus": 1}`). The engine never reads it; it hands
  it to the Performer, which sizes the work by it (a Batch job size, a
  Docker `--gpus` and shared memory) or refuses in words what it cannot
  meet.
- **Capabilities** are the words a Worker offers: those it detects
  (§6) and those its machine file adds.
- **No arithmetic.** A deployment that wants to match on size says so
  in a word (`vram-24g`). The engine compares strings, as it does tags.

## 8. Configuration

Two files, in the shape the users' deployment and machine files have
today.

- **The deployment file** is the same on every machine of a deployment:
  the store (its kind and settings), each provider's settings, the
  secrets by name, and for each program what is the same everywhere
  (its image, its Batch job). Found by `STEPMELD_CONFIG`, else in the
  user's configuration folder (my choice of names).
- **The machine file** belongs to one machine: its Worker's name (the
  host name by default), where Runs keep their files, the command for
  each program it runs as a command, capabilities it adds, and slots.
  `stepmeld worker program set` writes it. The SQLite Worker store of
  2026-10-17 retires.
- **A StepDefinition says only what does its work** (its program) and
  what that needs. Which Performer runs the program is each Worker's
  configuration.
- **Secrets** are named, with a kind: an environment variable or a file
  built in, Secrets Manager in `stepmeld-aws`. A program's
  configuration lists the secrets it is given.
- **An embedder** may hand stepmeld its configuration through a trait
  instead of files.
- **`TRIPOD_*` names are renamed** `STEPMELD_*` (Max, 2026-10-20).

## 9. What moves, from where

| Today | Moves to |
|---|---|
| Dispatch orders, their claimant and its lease | Orders (§5) |
| The machine heartbeat and its roster | A Worker's published entry (§6) |
| The capability table keyed by verb | `requires` on StepDefinitions (§7) |
| The Docker runner and GPU probing | The Docker Performer (§6) |
| The Batch launcher; a Performer pinned to the machine that launched | The Batch Performer in `stepmeld-aws`, with portable handles (§5) |
| A shared store on S3 | A store in `stepmeld-aws` |
| A Performer that mirrors logs into the store | Logs in the store (§5) |
| Ticking only the Workflows that moved | The Orchestrator |
| The deployment file, secrets and container sign-in | Configuration (§8), and `stepmeld-aws` for what names AWS |
| An execution contract, a job contract and their executors | Superseded: a program in an image, placed by stepmeld |

## 10. Phases

Small PRs, upstream first. The v1 contracts change in place, each break
in `docs/BREAKING.md`.

0. This note; the rule on providers in `CLAUDE.md`.
1. *(built 2026-10-20)* Orders take over from dispatch: claims and
   leases, pinned and open orders (the pool `any`; cloud orders open
   and taken over), `requires` and capabilities, the heartbeat,
   takeover or lost, logs in the store, and a harness case with two
   Orchestrators on one store. The walkthrough variation with two
   Workers on one machine waits for the machine file (phase 2), which
   changes how a Worker is set up.
2. Plugins and configuration: the deployment and machine files, kinds
   registered for stores, Performers, secrets and sign-in; SQLite,
   commands, and environment and file secrets built in.
3. The Docker Performer and GPU probing; the walkthrough's program as
   an image on the stand-in fleet.
4. `stepmeld-aws`: the S3 store, the Batch Performer, Secrets Manager,
   container sign-in; a walkthrough variation in the cloud, run by
   hand.
5. The Orchestrator ticks only what moved.
6. Downstream, after phase 4: the users bump, configure stepmeld, and
   delete what moved. Orders already running under the old dispatch
   finish first; the two never run side by side.

## 11. Not decided yet

- Whether a Step with several Workers that could run it waits for a
  placement (as now) or goes to an open order by default.
- What happens to a pinned order on an away Worker beyond waiting:
  nothing, re-placement by Policy, or attention.
- A store every machine reaches without a cloud account. Stores are
  plugins; whether stepmeld ships such a one is open.
- Who builds and publishes the images programs run in.
- Users and permissions on the shared store.
