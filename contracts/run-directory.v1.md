# The run directory protocol, v1

How a Performer and a program that does a Step's work speak to each
other on one machine: files in a directory, so a verb can be a program
in any language. It is the first wire form of the Performer contract
(`docs/DESIGN.md` §4.2); a wire form between machines is still open
(§11).

`stepmeld-local` is the Performer side. `stepmeld-verb` (Rust) and
`stepmeld.verb` (Python) are the program side, so a verb need not write
the files by hand.

## Invocation

```text
<program> [fixed args...] <run-dir>
```

A machine's performers file maps each verb (name and version) to a
command line; the Performer appends the Run's directory as the last
argument. Standard input is closed. Standard output and standard error
both go to `log.txt`. One directory per Run, named from the Run's id;
asked to start the same Run again, the Performer starts nothing and
returns the same handle.

## The files

| File | Written by | When | Contract |
|---|---|---|---|
| `request.json` | Performer | once, before the program starts | `stepmeld/run-request.v1` |
| `progress.json` | program | as it goes, replaced whole; optional | `stepmeld/run-progress.v1` |
| `outcome.json` | program, or the Performer when the program could not | once, when the Run ends | `stepmeld/run-outcome.v1` |
| `cancel.json` | Performer | once, when a cancel is asked | `stepmeld/run-cancel.v1` |
| `state.json` | program | when it has work elsewhere to pick back up; optional | the program's own: opaque |
| `resumable` | program | beside `state.json`: empty, says the program can be started again | — |
| `resumes` | Performer | how many times the program was started again | a count, as text |
| `log.txt` | the program's output | throughout | text; one line per message, stamped in UTC by the helpers |
| `pid` | Performer | at each start | the process id, as text |

Files another process may be reading (`progress.json`,
`outcome.json`, `cancel.json`, `state.json`) are written whole: to a
temporary name, then renamed.

The documents carry no `contract` field; the file's name says which
contract it is.

## How a Run ends

- **Success**: `outcome.json` with state `succeeded` and every Output
  the request lists. A success that lacks one is the Performer's
  `fault`.
- **Failure in words**: state `failed` with a class (the program's
  word; a retake Policy matches on it) and a reason. A program that
  cannot do the request at all says class `refused`.
- **Exit without an outcome**: the Performer writes one. Exit status 0
  with no Outputs owed is a success; exit 0 with Outputs owed is a
  `fault`; any other exit is a failure of class `exit`, its reason the
  status and the last line of the log.
- **Gone without an outcome** (killed, a reboot): a program that wrote
  `resumable` is started again on the same directory, up to three
  times, and reads its `state.json` to carry on. Otherwise, or after
  the third time, the Run is `lost`.

## Cancel

A cancel is a request with a deadline, not a kill:

1. The Performer writes `cancel.json`: the reason, when it was asked,
   and the deadline (thirty seconds later unless the Performer is
   configured otherwise).
2. The program, if it watches for the file, stops what it started
   (a job elsewhere, a child process) and writes its own outcome,
   normally state `canceled`, before the deadline. It may also finish:
   an outcome written first stands.
3. A program gone after a cancel, without an outcome, is canceled; it
   is never started again, whatever `resumable` says.
4. At the deadline the Performer stops the program and writes a
   canceled outcome whose reason says it was stopped there.

Until one of these ends it, the Run is still running: the engine asks
the Performer again on every tick, and every ask is short.

## Not in v1

- A wire form between machines (`docs/DESIGN.md` §11).
- An `attention` flag from the program; the local Performer reports
  none.
- The Performer's `estimate`; the local Performer answers none.
