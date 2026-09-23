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

~~**Children are not killed with the daemon.**~~ **CORRECTED 2026-09-23: this
was not established by the evidence above, and at a measured cut it was false.**
Modules ran in the daemon's process group (every module's pgid was the daemon's
pid), and the launchd plist sets no `AbandonProcessGroup`, so when the daemon
exited launchd killed the whole group. "Zero orphans" is exactly what a group
kill leaves, and BROCA's seal came from its own SIGTERM handler, so neither
item could tell "tore down on EOF" from "killed with the group". At the
16:52:57Z cut, astrocyte and callosum, which log a teardown pair on EOF and do
so on `ck module stop`, logged nothing. systemd's default
`KillMode=control-group` had the same shape on Linux.

What is built now:

- each supervised module leads its own process group (`process_group(0)` at
  spawn), so the service manager's group kill does not reach it;
- after the notice and the drain, the daemon closes every connection, so each
  module sees EOF while the daemon is still alive, and sends SIGTERM to every
  `protocol: "none"` child, which has no connection to see EOF on;
- it then waits at most 1 s for children to exit, sends SIGTERM to any still
  running, waits at most 0.5 s more, and sends SIGKILL (bounds in
  `child_roster.rs`; a second SIGTERM goes straight to the kill). Without this
  a child that ignores EOF would outlive the daemon, and a surviving
  `nats-server` would fight the next daemon's for its port;
- the systemd unit sets `KillMode=mixed`: SIGTERM to the daemon only, and
  SIGKILL for the rest of the cgroup only after the daemon exits.

The acceptance test for this asserts the module's own EOF and
teardown-complete markers after a simulated group kill, not process absence,
because absence is what the group kill produced too.

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

## The wire: reuse the existing lanes, with reason `Restart`

Both notices already exist and the module-restart path already sends both
(`supervise.rs:4399` module.draining to the provider, `:4404` route.closing to
consumers). Use them unchanged.

**Do not add a `Shutdown` variant to `RouteCloseReason`**, and the reason is
behavioural rather than a compatibility preference. Both SDKs classify the
reason, and both map an unrecognised one to the strictest disposition
(`clients/subc-client/src/client.ts:437` — "Unknown close reasons take the
strictest action and must not trigger a reopen"). So a new variant reaches every
consumer built before it as **never reopen this route** — exactly wrong for a
daemon shutdown, which is the case where the route should come back once the
daemon does. A more accurate noun bought at the cost of correct behaviour on
every un-updated consumer is not a trade worth making.

`Restart` classifies as `may_reopen`, which produces the right behaviour on both
sides: a consumer expects the route back, a provider stops admitting work.

The daemon-cut distinction lives in the **journal marker** instead. That is the
right allocation rather than a compromise: the wire field steers live behaviour,
the journal records history, and the question "was this a daemon cut or a module
restart" is asked after the fact by an operator reading `ck module terminals`.

Also do not invent a provider-side `route.closing`. It is not an existing module
command, and adding one breaks decoders fleet-wide for no behavioural gain.

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

- **It does not make a cut graceful by itself.** The notice makes it
  *announced*; the EOF teardown is graceful only because the daemon now
  delivers the EOF and keeps modules out of its process group (see the
  correction above).
- **It does not guarantee any module finishes.** See the budgets.
- **It does not help a SIGKILLed daemon.** Nothing runs. The journal's
  durability is what covers that case, and covers it only up to the last
  completed append.
- **It does not establish launchd's ceiling.** That number is still owed by
  whoever measures it first, and PLEX is the seat whose row changes when
  someone does.
