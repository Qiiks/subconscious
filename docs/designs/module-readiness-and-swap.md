# Module readiness and blue/green swap

Status: design, r1. Not built. Athena review before slicing.

## Why

A supervised module restart today is: drain the incumbent, kill it, spawn the
replacement, relay `route.open` to it the instant it registers. For a module
whose per-route setup is expensive the replacement is slow to bind for a
window after registration, and during that window every caller eats the
setup cost inline.

Two incidents in two days measured what that costs when the module is `aft`,
which warms each project root lazily on its first bind:

    2026-09-19  fleet-wide stall, 99 roots, 261 bind-relay timeouts,
                every client on a shared connection starved (reader HOL)
    2026-09-20  operator-visible outage from a routine placement: aft
                restarted 13:17:30Z, 312 bind-relay timeouts by 13:21Z
                (6 / 123 / 139 / 44 per minute), a sibling seat's tool lane
                dark for four minutes, all 312 lines `module_id=aft`

The reader head-of-line fix (merged, 8ab8b201 + 52b00793) bounds the SECOND
mechanism — one module's slow binds no longer block unrelated frames on the
same connection. It does nothing about the first: aft's own callers still
pay the warmup window, as 12 s stalls today and as fast `module_timeout`
refusals once the breaker is placed.

The operator's ask, verbatim in spirit: start the new process, route nothing
to it, keep the old one serving, and only once the new one is ready and
healthy stop the old one gracefully and route to the new one.

This document is that, built on a smaller thing it needs anyway.

## What exists, read from source

**Registration is one endpoint per module id.** `Registry` keys on
`module_id`; a second HELLO for a live id is refused; `ForwardingTable`
mints a new endpoint generation on registration after removing the old
endpoint, and `commit_route_locked` re-resolves the endpoint and refuses
`StaleModuleEndpoint` if it was replaced mid-relay. That fence is what makes
route binding safe across a respawn and this design must not weaken it.

**"Warming" is an absence, not a declaration.** `handle_route_open` reaches
`module_warming` only in the `else` arm of `registry.get_module` — that is,
only while the module is NOT registered and the supervisor reports it as
`Starting | Running | Restarting` (`is_warming_with_snapshot_lock`). The
instant a module sends HELLO, every `route.open` is relayed into its
`on_bind`. There is no field, on HELLO or on `catalog.update`, by which a
registered module can say "not yet".

**The supervisor has one child slot per module.** `SupervisedModule` holds
`child: Option<SupervisedChild>`; `spawn_child` mints one launch nonce; the
reserved-id gate admits one live nonce per id. A restart is
`drain -> kill -> spawn` in that order, on one slot.

**Route state is module-local.** Sessions, subscriptions, and warmed roots
live in the module process. There is no transparent migration of a bound
route between processes and this design does not attempt one.

## The ladder

Each rung is independently shippable and each later rung needs the earlier
one.

### Rung 1 — reader head-of-line fix (done, unplaced)

Bounds a slow module's blast radius to its own callers. Not part of this
design; listed because rungs 2 and 3 are wrong to build without it, since
without it a "not ready" module that is nevertheless receiving relays still
starves everyone.

### Rung 2 — module-declared readiness

A registered module may declare itself **not ready**. While it is not ready,
`route.open` targeting it is refused with the existing `module_warming`
code — already typed, already retryable, already bound to the SDKs' 30 s
route-open deadline — instead of being relayed.

Wire:

- `ModuleManifest` (HELLO) gains `ready: Option<bool>`, default `true` when
  absent. Absent is "ready", so every deployed module keeps its current
  behaviour with no edit. Constructed through the builder, so no construct
  site breaks.
- `catalog.update` gains the same optional field, so a module can flip
  `false -> true` when its setup completes without re-registering.
- `catalog.list` mirrors it, so `ck catalog <id>` and consumers can see it.

Daemon:

- `Registration` carries `ready: bool`.
- `handle_route_open`: after `get_module` succeeds and before any relay,
  `if !registration.ready { refuse module_warming }`. Same code, same
  refusal-attestation log line, same closed-vocabulary counter key. Nothing
  new for clients to classify.
- The supervisor's health probe is unchanged. Readiness is orthogonal to
  health: a module can be healthy and not ready (warming), or ready and
  degraded. Conflating them is why Kubernetes has two probes; we already
  have the liveness half.

Module obligation (aft, first adopter): pre-warm at start from a persisted
root list rather than on first bind, register with `ready: false`, flip to
`ready: true` when the persisted set is warm. **Without this the rung is
inert** — a process that receives no traffic never warms, and readiness
would just be a longer version of today. This is aft's work and is named as
a precondition, not assumed.

What rung 2 changes for callers during a restart: today they see 12 s bind
stalls (or, with rung 1 placed, fast `module_timeout` after three). With
rung 2 they see fast `module_warming` from the first call, retry inside
their deadline, and land on a module that is actually ready. The window is
the same length; every call inside it is cheap and honest.

### Rung 3 — blue/green swap

`supervisor.swap { module_id }` (and `ck module restart --swap <id>`):

    1. spawn CANDIDATE alongside INCUMBENT       (second child slot,
                                                   second launch nonce)
    2. candidate registers with ready:false       (registered, UNROUTABLE)
    3. candidate warms, flips ready:true          (rung 2 signal)
    4. CUTOVER: active endpoint := candidate      (atomic under the
                                                   forwarding write lock)
    5. incumbent enters Draining: route.closing,
       quiescence wait, route.closed, GOODBYE      (existing drain, reason
                                                   `swap`)
    6. incumbent exits; slot freed

Failure arms, which are the point:

- Candidate never registers, never flips ready, or fails its first health
  probe within a budget: **kill the candidate, incumbent untouched, swap
  reported failed with the candidate's terminal record**. A bad card no
  longer takes service down. This is the property the operator is buying.
- Candidate registers but the incumbent dies during the swap: promote the
  candidate immediately (it is the only live process); if it is not ready,
  callers get `module_warming` as in a plain restart.
- Daemon restart mid-swap: the swap is not durable; both children observe
  EOF and exit; next boot spawns one child as today. Acceptable — the
  operator restarting the daemon during a module swap is the rarer of the
  two and the failure mode is a plain cold start.

What callers see: bound routes on the incumbent get `route.closing` /
`route.closed` / `GOODBYE` at step 5 exactly as on a restart today, because
sessions are module-local. The difference is that their reopen (step 5 to
6 overlaps the candidate already serving) lands on a **warm** process
immediately. The unavailability window collapses from "warmup time" to
"one reopen round trip".

### Registry shape for rung 3

Today: `module_id -> Registration`. Rung 3: `module_id -> { active:
Registration, candidate: Option<Registration> }`.

- `route.open` resolves through `active` only. A candidate is registered —
  it has an endpoint, a generation, a connection, it answers `catalog.list`
  with `ready:false` — and is never selected for a route.
- HELLO on an id that already has an `active`: refused today
  (`already_registered`). Rung 3: admitted as `candidate` **only if the
  supervisor has an open swap for that id and the HELLO's launch nonce is
  the candidate slot's nonce**. An unsolicited second HELLO is still
  refused. This keeps the reserved-id gate exact: two live nonces per id
  exist only while the supervisor has minted two.
- Cutover swaps `active` and `candidate` under the forwarding write lock and
  bumps the generation the way registration does today, so an in-flight
  relay to the old active hits `StaleModuleEndpoint` exactly as it would
  across a respawn. The existing fence covers the new transition without a
  new fence.

### Who can use rung 3

**Two processes on one module's state is a data hazard, not a scheduling
one.** Broca seals a WAL on stop; engram holds captures; cerebellum holds
browser sessions; aft holds a resident index behind a writer barrier. Most
modules are single-writer on their store and overlapping them corrupts it.

So rung 3 is **opt-in by manifest declaration**:

    overlap: "exclusive" | "safe"        default "exclusive"

`supervisor.swap` on an `exclusive` module is refused with a typed error
naming the declaration. `ck module restart --swap` on one prints the same
and suggests plain restart. Making a module overlap-safe is module work —
for aft, at minimum, the candidate must open the store read-only or on a
separate writer lease until cutover. That is aft's design, not this one's;
this design only guarantees the daemon never overlaps a module that has not
said it can be.

### What rung 3 does not do

- No transparent route migration. Routes close and reopen.
- No swap across a daemon restart.
- No automatic swap on crash. The restart budget path is unchanged; swap is
  an operator or placement-tool verb.
- No health-based automatic rollback after cutover. If the candidate goes
  bad after promotion, that is a normal unhealthy module and the existing
  restart policy applies. (A post-cutover soak with automatic rollback to
  the incumbent is a later rung; it needs the incumbent kept alive past
  cutover, which doubles the overlap window and is not obviously worth it.)

## Slicing

    A. rung 2 — wire field + registry flag + route.open refusal + catalog
       mirror + `ck catalog` rendering. Golden fixtures both directions
       (absent == ready). One slice, subc-protocol minor bump.
    B. rung 3 registry — active/candidate pair, HELLO admission keyed on
       an open swap, cutover under the write lock, StaleModuleEndpoint
       covers the transition. One slice, daemon only.
    C. rung 3 supervisor — second child slot, swap state machine with the
       failure arms above, `supervisor.swap` control op, `--swap` on the
       CLI, `overlap` manifest declaration and its refusal. One slice.
    D. aft adopts: persisted-root pre-warm, ready:false/true, overlap
       declaration. AFT's slice, in their repo, after A lands.

A is worth shipping alone: it is small, it is the readiness half of the
operator's ask, and rung 3 without it has no cutover gate.

## Mutation controls the slices must carry

- A: with the `ready` check removed from `handle_route_open`, a registered
  `ready:false` module receives a relayed bind — the test must red on the
  relay reaching the stub, not on the client's error code alone.
- B: with cutover done outside the forwarding write lock, an in-flight relay
  to the old active commits against it after promotion — must red on the
  route landing on the incumbent.
- C: with the failure arm removed, a candidate that never becomes ready
  leaves the incumbent drained — must red on the incumbent's route.closing
  having been sent. And: an unsolicited second HELLO with a nonce the
  supervisor did not mint must still be refused.

## Open questions for review

1. Is `module_warming` the right code for "registered but not ready", or
   does reusing it lose information a client would act on differently?
   Reuse costs nothing on the wire; a new code needs a client-tolerance
   phase first (SDKs treat unknown route-open codes as terminal).
2. Should `ready` be allowed to go `true -> false` after registration (a
   module that must rebuild an index while serving)? The design says yes
   because the check is per-route-open and costs nothing, but a module
   flapping it would look like a restart storm to callers.
3. Is the candidate's health probe the same probe as the incumbent's, and
   does a candidate failing it count against the module's restart budget?
   Design says: same probe, and no — a failed swap is not a crash.
