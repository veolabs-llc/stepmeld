# Breaking and behaviour changes

Every change that can make a program, a store or a reader built
against an earlier stepmeld behave differently, newest first: what
changed, who it affects, and what to do. Additive changes that nothing
existing notices are not listed.

While the engine is iterating, the v1 contracts change in place
rather than gaining a v2 beside them; this log is how a user of the
engine catches up.

## A cancel is a request with a deadline

*Behaviour change, `stepmeld-local`. Contract: `contracts/run-directory.v1.md`.*

**Before:** a cancel killed the program at once, and the Performer
wrote a canceled outcome. **Now:** the Performer writes `cancel.json`
(reason and deadline, thirty seconds by default) and the Run stays
running until the program ends or the deadline passes; then the
program is stopped and the outcome written as before.

**Who notices:** a verb program that does not watch for `cancel.json`
now runs on for up to the grace period after a cancel. A program that
exits in that window without an outcome is canceled, even when it
wrote `resumable`.

**What to do:** nothing, to keep the old effect a little later; or
check `run.cancel_asked()` (Python) / `Run::cancel_asked()` (Rust),
stop what the verb started elsewhere, and call `canceled(...)`. A
Performer that wants the old immediate stop is built with
`LocalPerformer::with_grace(Duration::ZERO)`.
