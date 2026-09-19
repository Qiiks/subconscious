# route.open head-of-line blocking on the connection reader

Status: design settled, unimplemented. Athena consult
`ct_00000000-0000-403d-98d6-1b4419ee8fc8` (2 of 4 seats: one model unavailable,
one quota-exhausted, so `agreement_unmeasurable` — two independent readings, not
a quorum). Every claim it turned on has since been verified directly at source;
those verifications are recorded here, not the consult's citations, because a
design note whose evidence lives in another document is one re-read away from
being unfalsifiable.

## The defect

The connection reader loop is strictly serial per connection: it reads a frame,
then awaits its dispatch before reading the next. `route.open` awaits a bind
relay to the target module with a 12s budget. So **one module whose `on_bind` is
slow blocks every later frame on that connection**, including calls to unrelated
modules on other channels.

Measured on this host, 2026-09-19, during a live fleet stall:

    261  "route.bind relay timed out" for aft in 3000 log lines
    268  "slow control dispatch op=route.open elapsed_ms=12001/12002/11481"
      0  contended snapshot locks (so: not daemon-internal lock starvation)
    708  live routes on aft

Client calls to prefrontal-core with a 10s deadline timed out inside those bind
windows, to the second. The failure is self-sustaining: a client whose call
exceeds its deadline runs its liveness probe, convicts the socket, reconnects,
and reopens all cached routes — which re-issues the slow bind.

**The origin was not the daemon.** AFT traced it to their transient-cache sweep
reading the whole of `$TMPDIR` (290,697 entries; one bare `ls -f | wc -l` took
870s) before applying a 200-entry cap, which parked six of eight executor
workers. The daemon is the amplifier: it is why a slow `on_bind` in one module
reaches modules that have nothing to do with it. Both halves are real and they
have different owners.

## What was wrong in my framing

I believed serial dispatch was what protects `route.open` publication ordering,
and therefore that moving the wait off the reader risked reordering. **That is
false.** On the accept path:

```rust
Ok(Ok(RouteBindRelayOutcome::Accepted)) => {
    reservation.disarm();
    self.observe_route_open_accept(ctx, &target_module_id, &principal_label);
    Ok(Vec::new())                      // emits no frame
}
```

The response frame is pre-built at reservation time, carried on an egress permit
reserved *before* the relay, and published together with the forwarding-table
entry inside `commit_route_locked`'s single critical section — driven by the
**module** connection's handler, not the client's reader. The test that pins the
ordering drives the module ctx.

So the guarantee is held by **permit-plus-lock**, not by reader seriality, and
moving the client's wait off the reader leaves it untouched. This collapsed most
of the risk I had attributed to the change and made the fix considerably smaller
than I had scoped it.

## Settled preconditions

Three questions had to be answered before any shape was legal. All three are now
closed by reading the source, not by inference.

**1. Is channel-0 corr-FIFO a contract?** No. Both SDKs demux control replies by
hash lookup on the arriving frame's own correlation id:

- TypeScript: `pendingKey(handle, frame.header.corr)` → `"0:0:<corr>"` →
  `this.pending.get(key)`.
- Rust: `PendingKey { generation, channel: frame.header.channel, epoch:
  frame.header.epoch, corr: frame.header.corr }` → `HashMap` lookup.

Neither has a positional or arrival-order assumption for channel 0. The only
FIFO test in the suite, `single_client_pipelined_requests_preserve_corr_fifo_order`,
pins ordering on a **route** channel (`ack.route_channel`), not channel 0.
Out-of-arrival-order channel-0 refusals are a property of *every* shape that
removes head-of-line blocking, so this was a precondition rather than a
shape-selection criterion.

**2. Does an aborted spawned tail release its reservation?** Yes, and
synchronously, which is required because an aborted task cannot await:

```
Drop for RouteBindReservationGuard
  -> release_and_disarm()
       -> forwarding.abort_pending_relay(...)          sync
       -> send_goodbye_target_best_effort(...)         sink.try_send, no await
```

**3. Is the daemon the origin?** No — see above. A daemon-side fix contains the
blast radius; it does not make any module's `on_bind` fast. Keep that in view so
this is not mistaken for a cure.

## Shapes rejected

**Reserve-then-ack** (ack before the module binds; failure by control push) is
rejected. It changes an observable wire semantic: the `route.open` response
currently *proves* the module bound the route, and both SDKs install the
channel/epoch and immediately treat the route as live. Under this shape a client
can send data frames on a reserved-but-uncommitted channel, forcing the daemon
either to park them (unbounded memory, plus a state check on the splice path
that the zero-deserialization property forbids) or to reject them spuriously. It
also moves `module_timeout` out of the response and into a push, breaking the
closed retry classifier both SDKs share. That is a two-SDK semantics change
bought for a daemon-internal scheduling defect.

**Per-channel ordering lanes keyed on channel id** is degenerate: every control
frame in question *is* channel 0, so the key collapses all control work into one
serial lane — today's behaviour with a queue bolted on. Rekeying on module or
corr makes it the escape hatch plus a queue, and the queue falsifies the "no
queue segment by construction" invariant that gives the `elapsed_ms=12001`
diagnostic its meaning. It would also make the reader run ahead of dispatch,
weakening the read-backpressure and close-cancellation coupling the loop
currently gets from awaiting dispatch inside its `select`.

## The fix, in order

**Stage 1 — non-structural, zero ordering risk, no SDK change.** Ship first,
because it addresses the measured incident on its own.

The bind budget is 12s. **Correction to the consult's framing, which I had
repeated:** 10s is not a client-SDK default. Both SDKs default to 30s
(`DEFAULT_REQUEST_TIMEOUT_MS`, `DEFAULT_CALL_TIMEOUT`), and the 10s figure in
the incident was one caller's own per-call deadline — my board calls. So "the
budget exceeds the client deadline" is true of a 10s caller and false of a
default one, and stating it as a universal would have put a wrong premise in
the argument for the fix.

The defensible form is narrower and still decisive: **callers choose their own
deadlines and the daemon cannot see them, so a budget that any common caller
undercuts is spent holding a reader for an answer nobody will read.** The cost
is asymmetric — a bind that would have succeeded at 11s is rare, while 268
measured stalls at the ceiling each blocked every later frame on their
connection. Size the budget against the shortest deadline in practice, not
against the SDK default, and let per-module config raise it for a module that
genuinely needs longer.

- Cap the bind budget below the shortest client deadline. It is already
  per-module overridable, so this is reachable by config before it is reachable
  by code.
- Add per-module fast refusal after *k* consecutive relay timeouts, emitting
  `module_timeout`, which both SDKs already classify as retryable with capped
  backoff under a 30s deadline. This converts each 12s stall into a microsecond
  refusal.
- Cap in-flight binds per module endpoint, so a wedged module cannot accumulate
  reservations once the reader's accidental one-bind-per-12s throttle is gone.

### The budget's own doc comment records the reasoning that no longer holds

Worth reading before changing the number, because it is a third instance of a
pattern this fleet hit twice more the same day (ENGRAM: a comment three lines
above the carve-out that broke it; CEREB: a per-instance clock whose own doc
comment said so):

> The default is generous because rejecting a VALID bind is far worse than
> waiting on a slow one; a consumer that wants a tighter bound retries the bind
> itself.

**That reasoning is correct and was written when the wait cost only the
caller.** Under serial dispatch it now costs every later frame on that
connection, so "waiting on a slow one" means blocking consumers who never asked
for this module. The tradeoff the comment weighs is real; one side of it grew by
a factor nobody re-measured, because the growth happened in a different file.

So the number is not a free parameter to tune down. Lowering it alone trades
head-of-line cost for false rejections of legitimately slow binds — and AFT's
`on_bind` genuinely does a bounded project walk, which is exactly the valid-but-
slow case the comment protects. **The mechanism that makes a lower budget safe
is per-module fast refusal**: a module that is persistently slow gets refused
in microseconds rather than waited on for the full budget every time, so the
budget stops being paid repeatedly for a condition already known.

Sequence within Stage 1, therefore: fast refusal first, budget reduction second
and only once refusal makes it safe. Reversing that order ships the false
rejections without the protection.

**Stage 2 — the scoped escape hatch.** Spawn **`route.open` only**.

Not "ops that await another process": there are nine such client control ops
(`route.open` plus seven supervisor ops), and nobody has audited their ordering
semantics. `route.open` is the one op with measured head-of-line cost and the
one whose accept-path publication is already off the reader task. Scoping to it
reduces the new reordering surface to one op and one frame shape; widen only
when slow-dispatch logs implicate another op.

Constraints on the bound:

- **Enforce by non-blocking admission and fast refusal, never by awaiting a
  semaphore in the reader.** Awaiting a permit there is the same head-of-line
  defect with a smaller constant.
- **The ceiling must sit well below `CONNECTION_EGRESS_BUFFER`.** Each pending
  bind holds an `OwnedPermit` in the connection's bounded egress for the whole
  bind window, and that queue is shared with the data plane. N concurrent binds
  cost N egress slots; if N approaches the buffer size the connection starves
  its own responses — a self-inflicted stall the current design cannot produce.
  Derive the number from that constant, not from an intuition about client
  parallelism.
- **Also cap per target module.** Serial dispatch is an accidental rate limiter
  of one bind per 12s; removing it points 708 reopens at one module at once.
- **Cancellation must be connection-scoped**, via a `JoinSet` owned by the
  connection task and aborted at exit — not by dropping detached handles. The
  commit path already refuses to publish into a closing client, so a leaked task
  settles harmlessly, but it holds a permit, a slot pair and a corr for the full
  budget, which is exactly the resource being rationed.

## The guard to write

Extend the existing idiom rather than inventing one: a `route.open` against a
never-replying module, concurrent on the same socket with a data call to a
*second*, already-bound module, asserting the second module's response arrives
**before** the open's refusal. Prove it structurally by frame order rather than
with a latency bound — a timing assertion here flakes on loaded CI, which has
already happened once in this file.
