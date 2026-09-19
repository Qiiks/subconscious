# Module lifecycle authority

Status: DESIGN, unimplemented. Item 5 of `docs/plans/audit-2026-09-06-program.md`.
Next step is an Athena review of this note, not a mason.

## The problem, measured rather than recalled

`handle_route_open` (control.rs:1647-2000) answers one question — *may this
caller bind to this module right now* — by taking **six state reads across three
independent authorities**, each acquiring its own lock:

    registry.get_module                    x2
    supervisor.removal_tombstone_age_ms    x1
    forwarding.module_is_draining          x1
    forwarding.has_live_module_connection  x1
    forwarding.begin_route_bind_relay_for  x1

Between the first `get_module` and the final relay, a module can register,
deregister, begin draining, be removed by a rescan, or crash and respawn. Each
read sees a different instant, and the decision is assembled from instants that
never coexisted.

**A coarser lock does not fix this**, which is the whole reason the item exists.
The three authorities are separate structures with separate lifetimes: the
registry is the wire catalog, forwarding is the routing table, the supervisor is
the process manager. One lock over all three would serialise the daemon's three
busiest paths against each other to make one decision consistent. The fix is a
single record that *is* the answer, not a bigger lock around three that are not.

## The evidence that this class bites, rather than an argument that it could

**2026-09-06, a 4.5-hour AFT outage.** A client socket closed in the same
instant AFT's `RouteBindAck` arrived. `commit_route_locked` returned
`ConnectionClosing{client}` — a *client*-scoped error — and the `?` sat on the
**module** connection's frame handler, so a shared connection carrying ~170
routes was torn down over one dying client's bind. The sibling arm one line above
already handled "client sink closed" gracefully; "client marked closing but sink
still open" — a ~100 µs window — fell through to the fatal path. Fixed in
0.17.16 with a fence, but the shape is the same: a decision assembled from two
views of a connection that had already diverged.

**2026-09-18, 282 bind-relay timeouts.** All `aft`, clustered at 17:xx (a daemon
cut), 01:xx and 02:xx (module placements). Not this defect — the module was
alive and slow — but it is the same window, and it is the window in which the
six reads disagree most.

**The same day, a message with two producers.** `"module '<id>' connection
closed during route.bind relay"` (forwarding.rs:2008) is emitted by
`remove_module_connection_locked`, which is called both when a module connection
genuinely goes away (forwarding.rs:1498) **and** from inside
`register_module_connection` when a new registration replaces an old endpoint on
the same connection id (forwarding.rs:403). The text asserts a mechanism that is
wrong for one of its two callers. A lifecycle record with named transitions
makes that distinction structural rather than a string someone has to keep
honest.

## Shape

One record per module id, holding:

    incarnation        monotonic, bumped on every (re)registration
    state              Registered | AdmissionClosed | Drained | Exited | Replaced
    since_ms
    reason             why the current state was entered

Every transition is **incarnation-fenced**: a caller holding incarnation N
cannot move a record that has advanced to N+1. That is the same fence already
used for route epochs (`stale_route_epoch`) and for the `ExitKind::ProbeInduced`
pid+start-time severance, and it exists for the same reason — an actor that
went away and came back is a *different* actor, and the daemon already knows
this in two other places.

`handle_route_open` then takes **one** read and gets an answer that was true at
a single instant. Public status (`ck module status`, `supervisor.list`) becomes
a projection of the record rather than a join across three authorities performed
at render time.

## What this is NOT

**Not a per-frame actor.** The data plane stays as it is: the router splices
frames by header with no lifecycle involvement, and nothing on the forwarding
hot path acquires the lifecycle record. This is admission and status only.

**Not a replacement for the three authorities.** The registry still holds
manifests, forwarding still holds routes, the supervisor still owns processes.
The record holds the *lifecycle facts those three currently each keep a partial
copy of*, and they consult it rather than each other.

**Not a durability change.** The record is in-memory, like the forwarding table.
Terminal history is already durable via the journal (0.18.9); this is about
consistency within a daemon lifetime, not across one.

## Acceptance, and the mutation that decides it

The arm a weaker implementation passes vacuously is **the fence**, because a
record that is merely centralised still answers every single-threaded test
correctly.

    the decider: a route.open holding incarnation N, against a record that
    advanced to N+1 between its read and its bind, must be REFUSED --
    not served against the new incarnation, and not served against the old one

    the mutation: drop the incarnation comparison. Every ordinary test stays
    green; that one arm must fail by name.

Second arm, from the 09-06 outage: a client-scoped failure during a bind must
never terminate the module connection. Already fenced in 0.17.16, and the
lifecycle record must not reopen it — so that fence's test runs unchanged
against the new path.

## Open questions for review

1. **Does the record subsume `removal_tombstones`?** Tombstones answer "this
   module was intentionally removed, N ms ago" and are consulted only by
   route.open. `Exited{reason: Removed}` with `since_ms` carries the same fact.
   If it does subsume them, that is a deletion rather than an addition, which
   would make the change net-negative in surface area.

2. **Where does the record live?** A fourth structure adds a lock; putting it
   inside `ForwardingTable` (which already holds `daemon_draining` and
   `draining_endpoints`) avoids one but grows a structure that is already 18
   maps under one `RwLock`.

3. **What happens to a caller mid-relay when the record advances?** The bind
   relay is the one lifecycle-sensitive operation with a 12 s budget. The fence
   says refuse; the alternative is to let an in-flight relay complete against
   the incarnation it started with. Both are defensible and they differ in what
   a consumer sees on a module restart.

4. **Is `Replaced` distinct from `Exited`?** forwarding.rs:403 replaces an
   endpoint without the process exiting. If those collapse, the message defect
   above stays possible.
