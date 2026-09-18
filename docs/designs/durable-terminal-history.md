# Durable terminal history

Status: design, unbuilt. Owner: SUBC.

## The gap, stated as an observation

After the daemon cut on 2026-09-18, `ck module terminals` read **empty for every
module**. Not "no exits" — no *record* of exits. Nineteen modules had just been
torn down and restarted, and the instrument that exists to report exactly that
had nothing to say.

`TerminalRing` is in-memory, per daemon process, 32 entries per module
(`terminal_ring.rs:5`). So the daemon that observed those nineteen exits is the
daemon that died. The recorder dies with the record, at precisely the moment the
record is wanted.

That is the same failure BROCA measured on child stderr the same evening: the
daemon holds the read end of the child's pipe, so a *module* restart preserves
the last lines and a *daemon* cut loses them. Two instruments, one shape — the
observer is inside the blast radius of the event it observes.

## Why this matters more than the SIGTERM handler

Both items came out of the same night. It is worth stating the ranking, because
the SIGTERM handler is the more obvious piece of work and the less valuable one:

- a SIGTERM handler improves **one future event** — it buys drain notice on the
  next daemon death
- durable terminal history makes **every past event readable** — including the
  ones that already happened and are currently unrecoverable

An exit that nobody can read is indistinguishable from an exit that did not
happen. That ambiguity is what turns a real incident into "it was probably
fine".

## What must survive

One record per observed child exit, which is already the shape `TerminalRecord`
carries (`terminal_ring.rs:29`):

    exit_code            Option<i32>
    exit_signal          Option<i32>     <- the fleet-wide teardown discriminator
    at_ms                u64
    disposition          Stopped | Disabled | Failed | Restarting
    exit_kind            TerminalExitKind
    disposition_detail   Option<String>  <- names the budget AND its window

Plus one field the in-memory ring does not need and a durable one does: the
**daemon incarnation** that observed it. Without it, two exits from different
daemon lifetimes are indistinguishable in the file, and "the daemon restarted
between these two records" is exactly the fact a reader is trying to establish.

`exit_signal` is the load-bearing field. It is the fleet-wide teardown detector:
a module that stops itself on connection close exits cleanly with no signal; one
that does not gets SIGKILLed after the drain ceiling. That detector works for
hand-rolled modules with zero cooperation — three of the four seats implementing
connection-driven teardown do not link the SDK — but only if the observation
survives the restart that produced it.

## Shape

**Append-only JSONL under `run/`, one file for all modules, written at the same
site that pushes into the ring.**

    ~/.local/share/cortexkit/run/terminals.jsonl

Four properties, each for a reason:

1. **Append-only.** A terminal record is a fact about a moment. Nothing may
   rewrite one, so there is no consistency question and no lock beyond the
   append.

2. **One file, not per-module.** The question a reader asks after an incident is
   "what died, in what order" — which is a cross-module question, and
   per-module files make the ordering an inference across mtimes. `ck module
   terminals <id>` filters; the file does not pre-decide.

3. **Written where the ring is written**, in `record_terminal_with_detail`
   (`supervise.rs`). One site, so the two cannot disagree about what happened.
   The ring stays as the fast path for `ck module terminals`; the file is the
   durable one and is consulted when the ring's window does not cover the
   question.

4. **Not a store.** No schema version, no migration, no SQLite. The daemon holds
   no durable state today and this must not be the change that gives it one —
   a corrupt line should cost one record, not a boot. Readers skip lines that do
   not parse and **report the count of skipped lines** rather than silently
   dropping them.

## Retention

Size-based, via `cortexkit-log`'s `LineSink`, which already does rotation with
age pruning and is already a daemon dependency. Reusing it rather than
hand-rolling retention is the whole reason `LineSink` was made public
(commons `9973615`).

Default ceiling should be small — this is one line per module exit, so a fleet
restarting daily writes ~19 lines/day. A 32 MiB cap is absurd headroom; the
point of a cap is that an unnoticed crash loop cannot fill a disk.

## What this does NOT fix, stated so nobody reads it as more

**The stderr window is a separate gap with the same shape and a different fix.**
Durable terminals record *that* a module exited and how; they do not record what
it printed on the way out. BROCA's lost stderr needs the capture file to be
flushed on the daemon's own teardown path, which is the SIGTERM handler's job.
The two items are siblings, not substitutes.

**A daemon killed with SIGKILL still loses the in-flight record.** An append
that has not happened cannot be durable. The window shrinks from "every exit
this daemon observed" to "the exit being written at the instant of death", which
is the right trade and is not zero.

**It says nothing about exits the daemon never observed.** A child reaped by
something else, or an exit during a window where the supervision task itself had
died (which happened on 2026-08-15) leaves no record here either, because the
observer never saw it. The file is a durable copy of what the daemon knew, not a
second observer.

## Acceptance

The arm that matters is a **daemon restart with a module exit on either side**:

1. stop a module, read its terminal from `ck module terminals` (ring path)
2. bounce the daemon
3. read the same terminal again (file path, ring is empty)
4. the record is present, and carries a **different incarnation** from any exit
   recorded after the bounce

Step 4 is the one a weaker implementation passes vacuously: a file that survives
but cannot distinguish daemon lifetimes answers "did it exit" and not "did it
exit before or after the restart", and the second is the question incidents turn
on.

Control: with the file write removed, step 3 reads empty — which is today's
behaviour, and is what makes the arm non-vacuous.

## Open question for the operator

Nothing in this design needs a decision before it is built. It is recorded as
owed rather than built tonight because it wants a daemon cut to be observable,
and one was placed hours ago; it should ride the next window rather than justify
its own.
