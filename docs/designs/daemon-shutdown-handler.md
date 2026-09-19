# Daemon shutdown handler

Status: design, unbuilt. Owner: SUBC.
Sibling: `durable-terminal-history.md` (built, merged at e0da800b).

## What the daemon does today, measured rather than assumed

There are **zero signal handlers in `subc-core`**. `launchctl bootout` sends
SIGTERM, the default disposition kills the process immediately, and no teardown
runs.

My first statement of this was too strong and is corrected here, because the
correction is what sizes the work. I said a daemon cut was "an ungraceful stop
for all nineteen modules at once". Three pieces of evidence say otherwise, taken
after the 2026-09-18 cut:

- process parentage: 19/19 modules were children of the *new* daemon, **zero
  orphans survived**
- BROCA's WAL seal was appended ~20 s after bootout — the child was alive and
  working
- CEREB's teardown is connection-driven and completes six browser sessions in
  ~100 ms

**Children are not killed with the daemon.** They observe EOF on their control
connection, run their own graceful stop, and exit — the shape specified on
2026-09-06, which five seats have now independently confirmed they implement.

So "the daemon sends nothing" is true and "nothing graceful happens" is false,
and they are different statements.

## What is actually lost on a cut

A short list, which is the honest one:

1. **Advance notice.** `route.closing` arrives while a module still has a wire
   to answer on, so it can stop accepting new work before it loses the ability
   to say anything. On a cut it learns by EOF, which is simultaneous with losing
   the channel.
2. **The drain.** The daemon does not wait for in-flight requests. A client
   request in flight at cut time is settled by the module if it can and lost
   otherwise.
3. **The last lines.** The daemon holds the read end of each child's
   stdout/stderr pipe, so the reader dies first and the child's final output is
   unrecorded. BROCA measured this.

That list reprices the work: **a handler buys notice and a bounded drain, not
the difference between graceful and ungraceful.** Worth building; not urgent.

## The budgets, and why they rule out the obvious design

Five rows from the teardown ledger, each read from source by its owner:

| seat | trigger | bound |
|---|---|---|
| CEREB | connection-driven | ~100 ms, six browser sessions |
| ASTRO | connection-driven | one in-progress tick, single SQLite txn |
| SYNAPSE | connection-driven, two tiers | worker sockets retire children in 0.63 s |
| BROCA | connection-driven | **fixed** ~30 s (10 s grace + 20 s seal) |
| PLEX | connection-driven | **one poll pass**: 2.1 s typical, 31 s max this process, **99.6 s measured** on an earlier one |

Three orders of magnitude between the fastest and the slowest, and PLEX's is not
a constant — it is bounded by *their own work*, so it grows with their watch
count.

The obvious design is a total budget: notify everyone, wait N seconds, exit.
**No N is both safe for PLEX and acceptable to launchd.** A value that lets a
99.6 s poll pass finish is a value that hangs the shutdown; a value launchd
tolerates cuts PLEX mid-pass on a bad day.

And the ceiling is **unmeasured**: nobody has established how long launchd
grants a LaunchAgent's grandchildren before SIGKILL. Today's cut never reached
it. Designing against an unmeasured ceiling with a made-up constant is how a
budget becomes folklore.

## Therefore: the handler promises notice, never completion

    on SIGTERM:
      1. stamp the terminal journal with a daemon-shutdown marker
      2. broadcast route.closing on channel 0 to every connection with routes
      3. wait for quiescence up to a SHORT bounded budget
      4. exit, whether or not step 3 completed

A third step, "flush the child capture sinks", stood here and is DELETED: there
is no buffer to flush, so it would have recovered zero bytes while reading as
though it addressed the third loss. See that item above.

Step 4 is the design. The handler **must not** try to outlast launchd, because
a handler that is killed mid-way is strictly worse than one that exited on
time: it holds the process alive past the point where its work is being
recorded, and produces a partial teardown nobody can distinguish from a
complete one.

This makes PLEX's own formulation the standing requirement for every module,
and it is adopted here rather than merely noted:

> **Not fitting must be survivable, or say why not.**

Three of five rows already state it explicitly — PLEX's fenced
cursor-plus-events transaction, BROCA's WAL replay recovery, ASTRO's durable
cursor with insert-once dedupe. CEREB's is the row where it should be stated
rather than inferred.

## Ordering, which is the part that earns the change

Notice **before** the drain, and the drain bounded separately from the notice.

A module that receives `route.closing` can stop admitting work in microseconds
even if its current unit of work runs for ninety seconds. So the notice is
useful to PLEX *precisely because* it does not wait for them: they stop taking
new watches immediately and finish the pass they are in, and whether that pass
completes before launchd's ceiling is a question the notice does not need to
answer.

There is no capture-flush step to order, for the reason given above: nothing is
held, so nothing can be flushed. The third loss is not addressed by this handler
and the document no longer claims it is.

## Escalation

A second SIGTERM **escalates**, never restarts: cut the wait, go straight to
exit. An operator sending it twice is saying "stop waiting", and a handler that
ignores the second signal makes them reach for SIGKILL, which loses the journal
stamp the first signal was about to write.

## What the journal stamp buys

Durable terminal history now survives a daemon restart, so a shutdown marker in
the same journal makes the *cause* of a module's exit readable after the fact:
an exit with no preceding daemon marker is the module's own, and one after it is
part of a cut. Today those are indistinguishable once the ring is gone.

That is also why this is second and terminals were first: a handler improves one
future event, durable history makes every past one readable.

## Acceptance

The arm that decides it:

1. start the daemon with modules running
2. send SIGTERM
3. a module that logs on `route.closing` records it **before** its EOF
4. the terminal journal carries a daemon-shutdown marker
5. the daemon exits within the bounded budget even with a module that
   deliberately refuses to quiesce

Step 3 is the one a weaker implementation passes vacuously: a handler that never
delivers the notice still lets every module exit cleanly by EOF, so the other
arms all pass while the feature does nothing. **The ordering must be asserted
against a module's own observation, not against the daemon's intent** — and the
module must observe the SEQUENCE, since "received both" proves nothing without
which came first.

Controls, and both break the property as the module experiences it:

- with the broadcast removed, step 3 fails by name
- with established connections closed *before* the broadcast, EOF precedes the
  notice and step 3 fails by name

The second is the real failure mode rather than a synthetic one: a handler that
exits or tears connections down without broadcasting is exactly what this
feature exists to prevent, and it is reachable by ordinary mis-edit.

**AN EARLIER REVISION NAMED THE WRONG MUTATION HERE**, and the error is the one
this very paragraph warns against. It said "a handler that broadcasts after
closing the listener delivers nothing". False: `handle_connection` is spawned
detached (`server.rs:126` drops the JoinHandle) and `AbortTasksOnDrop` holds only
the accept-loop tasks (`server.rs:149-154`), so closing the listener aborts
accepts and leaves every established connection running. A test forced red by
moving the listener close would have asserted a daemon-internal ordering with no
consequence for any module — reasoning about the daemon's structure while the
property is about the module's experience. Caught by the mason implementing this,
reading source against the prose.

## Non-goals, stated so nobody reads this as more

- **It does not make a cut graceful.** It already is, for modules that hang
  teardown off connection close. It makes it *announced*.
- **It does not guarantee any module finishes.** See the budgets.
- **It does not help a SIGKILLed daemon.** Nothing runs. The journal's
  durability is what covers that case, and covers it only up to the last
  completed append.
- **It does not establish launchd's ceiling.** That number is still owed by
  whoever measures it first, and PLEX is the seat whose row changes when
  someone does.
