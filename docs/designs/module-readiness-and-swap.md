# Module readiness and blue/green swap

Status: design, r2 after Athena review (ct_…e9e0c01e76e8, 4-seat panel).
Rung 2 is judged shippable as written. Rung 3 r1 was wrong in five places
the panel found from source; r2 rewrites it. Not built.

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

**Registration is one endpoint per module id, in two places.** `Registry`
keys `ModuleRegistration` on `module_id`; a second HELLO for a live id is
refused with `duplicate_module_id` (control.rs:1557-1564, registry.rs:77-79).
Separately, `ForwardingInner.modules_by_id` is `module_id -> single
ModuleConnection`, and `register_module_connection` inserts with NO
duplicate check (forwarding.rs:439-447) — a second same-id registration
silently overwrites the active entry. Every consumer resolves by id: relay
reservation, `module_is_draining`, `has_live_module_connection`,
`begin_module_drain`. So "registered but unroutable" is not expressible
by touching the registry alone; the forwarding layer needs its own slot.

**The `StaleModuleEndpoint` fence is safe in placement and fatal in
disposition.** `commit_route_locked` re-resolves the endpoint and refuses
`StaleModuleEndpoint` if it was replaced mid-relay (forwarding.rs:1911-1916),
so a relay reserved against one process cannot commit against another. But
that `Err` propagates out of `complete_pending_relay` (forwarding.rs:997) to
`refuse_to_end_module_connection_for_a_client` (control.rs:3939-3960),
whose doc comment names "a stale module endpoint" as a statement about THIS
connection and ends it. Across a respawn the acking connection is the dead
old process and ending it is a no-op. Across a cutover it is the live
incumbent still carrying every other client's routes — the 09-06 class
fenced in 0.17.16, reopened from the other side. The pending client also
receives no response frame (the sender drops before the reply is built).
The fence covers cutover only with a NEW non-fatal arm.

**"Warming" is an absence, not a declaration.** `handle_route_open` reaches
`module_warming` only in the `else` arm of `registry.get_module` — that is,
only while the module is NOT registered and the supervisor reports it as
`Starting | Running | Restarting` (`is_warming_with_snapshot_lock`). The
instant a module sends HELLO, every `route.open` is relayed into its
`on_bind`. There is no field, on HELLO or on `catalog.update`, by which a
registered module can say "not yet".

**The supervisor has one child slot per module, and one nonce.**
`SupervisedModule` holds `child: Option<SupervisedChild>`; `spawn_nonces`
and `reserved_nonces` are `module_id -> single value` and `spawn_child`
REPLACES both on every spawn (supervise.rs:808-824, 4077-4084); `retire`
wipes them. Two live nonces per id cannot be represented today, and the
nonce is load-bearing beyond HELLO: it backs consumer `route.open`
attestation (`Principal::Reserved`). A restart is `drain -> kill -> wait for
registry release -> backoff -> spawn` (`restart_child`, supervise.rs:3490-3531),
and the drain tail `wait_for_registration_release` polls until
`registry.get_module(id).is_none()` (supervise.rs:5184-5190). Cgroup paths
and stderr capture files are also keyed on bare `module_id`
(supervise.rs:4087-4119).

**The reserved gate does not nonce-check unreserved ids.**
`reserved_hello_rejection` returns authorized for any id with no
`reserved_nonces` entry and no reserved prefix (supervise.rs:871-912). The
only thing stopping a squatter on a live unreserved id is the
`duplicate_module_id` refusal — which is exactly the refusal a swap lifts.
And in `handle_hello` the reserved gate runs BEFORE duplicate detection
(control.rs:1494-1515 vs 1550-1565).

**`handle_route_open` is a sequence of snapshots, not one.** `get_module`
(2176), `module_is_draining` (2255), `process_live` (2269),
`has_live_module_connection` (2284), then the relay — each under its own
lock. Only the reservation step is atomic with cutover.

**`RouteCloseReason` is closed:** `Reload | Restart | Disable | Crash |
CapabilityDenied`. Both SDKs classify an unknown reason as `must_not_reopen`.

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

- `ModuleRegistration` carries `ready: bool`.
- `handle_route_open`: after `get_module` succeeds and before any relay,
  `if !registration.ready { refuse module_warming }`. Same wire code, so
  nothing new for clients to classify. NOT the same log line or counter
  key: today's `module_warming` is emitted only by
  `supervised_absent_route_open_refusal_frame`, which stamps
  state/enabled/live from the supervisor snapshot (control.rs:2108-2135);
  a registered-not-ready module has no such status. Reusing that builder
  would make "absent" and "declared not ready" indistinguishable in the
  counter and in the log — on exactly the incident class this rung exists
  to diagnose. So: same `code`, a distinct message, `ErrorBody.detail =
  {"reason": "declared_not_ready"}` (the field exists and is `None` on
  other paths), and a distinct log field `reason=declared_not_ready`.
  The closed counter vocabulary gains one key for it.
- **Best-effort, not an invariant.** `handle_route_open` reads readiness
  under one lock and reserves the relay under another; a module can flip
  `ready` between the two, and under rung 3 the check can read the
  pre-cutover active while the reservation lands on the post-cutover one.
  So a module MUST still tolerate an `on_bind` while not ready. The rung
  removes the common case, not the possibility; document it that way on
  the manifest field.
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

    1. spawn CANDIDATE alongside INCUMBENT       (candidate slot: own child,
                                                   own nonce, own cgroup and
                                                   capture-file suffix)
    2. candidate registers with ready:false       (candidate slot in registry
                                                   AND forwarding; unroutable)
    3. candidate warms, flips ready:true          (rung 2 signal)
    4. CUTOVER: swap active/candidate slots       (one forwarding write lock;
                                                   generation bumps)
    5. incumbent drains BY ENDPOINT: route.closing,
       quiescence wait, route.closed, GOODBYE      (reason `Restart` on the
                                                   wire; see below)
    6. incumbent exits; candidate slot freed;
       registry/forwarding candidate := None

Failure arms, which are the point:

- Candidate never registers, never flips ready, or fails its first health
  probe within a budget: **kill the candidate, incumbent untouched, swap
  reported failed with the candidate's terminal record**. A bad card no
  longer takes service down. This is the property the operator is buying,
  and r1 got it wrong at step 1: "untouched" requires the candidate's
  spawn to leave the incumbent's nonce, cgroup, capture file and
  registration alone, none of which the existing `spawn_child` does.
- Candidate registers but the incumbent dies during the swap: promote the
  candidate immediately (it is the only live process); if it is not ready,
  callers get `module_warming` as in a plain restart.
- Daemon restart mid-swap: the swap is not durable; both children observe
  EOF and exit; next boot spawns one child as today.

What callers see: bound routes on the incumbent get `route.closing` /
`route.closed` / `GOODBYE` at step 5 exactly as on a restart today, because
sessions are module-local. Their reopen lands on a **warm** process. The
unavailability window collapses from "warmup time" to "one reopen round
trip".

### What rung 3 actually has to change (r2, from the review)

r1 called this "a registry-shaped change". It is not; it is a slot-shaped
change across four subsystems, and each of the following is a place where
r1 would have shipped a defect:

**Forwarding candidate slot.** `ForwardingInner` gains a per-id candidate
entry beside `modules_by_id`. `register_module_connection` for an id with a
live active entry and an open swap goes into the candidate slot; today it
silently overwrites, which would make the candidate routable the instant it
registered. Every by-id consumer (`module_is_draining`,
`has_live_module_connection`, relay reservation) keeps resolving the
active slot only.

**Endpoint-keyed drain.** `begin_module_drain` takes `module_id` and
resolves `modules_by_id[id]` — after cutover that is the CANDIDATE, so r1's
step 5 would have marked the new active as draining and left neither
process routable. Rung 3 needs `begin_endpoint_drain(ModuleEndpointId)`,
and step 5 calls it with the incumbent's endpoint captured before cutover.

**Slot-keyed registration waits.** `drain_child_to_state` ends with
`wait_for_registration_release`, polling `get_module(id).is_none()` — which
a successful swap guarantees never happens, so every swap would end in
`RegistrationStillActive` after 30 s. Its mirror
`wait_for_registration_after_reload` would report the incumbent's
registration as the candidate's and collapse the never-registers failure
arm. Both must key on the endpoint/slot, not the id.

**A non-fatal superseded-endpoint arm.** This is the load-bearing one.
When `commit_route_locked` returns `StaleModuleEndpoint` for a relay that
was reserved against the incumbent and is being acked after cutover, the
current disposition ends the acking module connection — correct across a
respawn (the acker is dead), catastrophic across a cutover (the acker is
the live incumbent mid-drain, carrying every other client's routes). Rung 3
adds an arm in `refuse_to_end_module_connection_for_a_client`'s caller
that, for a superseded endpoint: releases the reservation, answers the
waiting client `module_reloading` (retryable; the client reopens onto the
new active), sends a channel-scoped GOODBYE to the incumbent for the
binding it just created, and keeps the incumbent's connection alive.
Without this arm, one in-flight bind at cutover reopens the 09-06 class.

**Per-slot nonces and a mandatory swap-token check.** `spawn_nonces` and
`reserved_nonces` become per-slot. And the candidate's HELLO is admitted by
a NEW check — constant-time compare against the candidate slot's nonce,
applied to reserved and unreserved ids alike, refusing an absent nonce —
not by delegation to `reserved_hello_rejection`, which never nonce-checks
an unreserved id. r1 said "this keeps the reserved-id gate exact"; the
gate has no such property, and the only thing protecting a live unreserved
id today is the `duplicate_module_id` refusal that a swap lifts. The check
must also run BEFORE the reserved gate in `handle_hello`, or a reserved
module's candidate is refused as `reserved_module` before swap admission
is reached.

**Per-slot cgroup path and capture file.** Both are keyed on bare
`module_id` today; a candidate would join the incumbent's cgroup (one kill
domain) and interleave into its capture file.

**Close reason stays `Restart`.** `RouteCloseReason` is a closed enum and
both SDKs map an unknown reason to `must_not_reopen`; a `Swap` variant is a
two-phase wire change with nothing to gain, since the client's correct
response is identical. The journal's swap record is the discriminator, as
it already is for daemon-cut versus restart.

**Overlap is frozen and config-sourced.** `overlap` must be in the
`catalog.update` frozen set (the machinery exists:
`catalog_update_frozen_field_message`, control.rs:1795-1803), or a candidate
can declare itself safe after admission. And the manifest is only readable
while the module is registered, so `supervisor.swap` on a module that is
down has nothing to consult: `overlap` is declared in `ModuleSpec`
(`subc.jsonc`) as the authority, mirrored on the manifest for `ck catalog`,
and swap is refused outright when the incumbent is not registered.

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

### The warm set: which roots a candidate should load (r3, with AFT)

A candidate that flips ready before loading anything has made the swap
useless: the first route to each root lands cold. The module cannot
answer "which roots" itself, because its persisted state lists every root it
has ever seen, not the ones live now (AFT's side of this is
`docs/design/subc-readiness-warmup.md` in the aft repository). The daemon
routes every bind, so it is the only party that knows the live set exactly.

**What exists, read from source.** It does not keep that set today.
`route.open` canonicalizes the project root (`control.rs`, the
`ProjectRootId::from_path_allowing_missing` call in `handle_route_open`),
relays it to the module in the bind, and then drops it: `RouteBinding`
(`forwarding.rs`) holds channels, epochs, the principal and `bound_at`, and
no root.

**Daemon obligation 1: keep the root.** Store the canonical root on
`RouteBinding` and on the pending reservation, as the `ProjectRootId` the
open already computed. The type is `Option` on purpose. A binding created
before the daemon carrying this change has no root, and it must stay
*unknown*, never become *absent* (see below).

**Daemon obligation 2: the query.** A new channel-0 op:

    request   supervisor.live_roots { module_id }
    reply     LiveRoots {
                module_id,
                roots: [ { project_root, bound, pending } ],   // counts per root
                unknown_root_bindings,                          // bound + pending with no stored root
                total_bindings,
              }

The reply has to distinguish three answers, and it does so by construction.
The shape is AFT's requirement, and it is the right one:

| roots | unknown_root_bindings | meaning | candidate does |
| --- | --- | --- | --- |
| non-empty | any | these roots are live | warm them under the budget |
| empty | > 0 | routes exist, roots unrecorded | warm lazily; this is NOT "nothing to warm" |
| empty | 0, with `total_bindings == 0` | no routes at all | flip ready at once |

If pre-change bindings were simply left out, "unknown" and "none" would be
the same empty list. The first swap after the daemon cut would then warm
nothing, flip ready immediately, and look exactly like a successful warm
swap: a cold cutover that reports itself as fine. `total_bindings` exists so
the third row is a positive statement, not an inference from two zeros.

Authority (corrected by the r3 review; the first version of this paragraph
was wrong). The caller is the module itself, on its own module connection,
and `supervisor.routes` cannot serve it: module-originated frames go through
`is_known_module_request_op` and a dispatcher that accepts only
`CatalogUpdate`, and `supervisor.routes` is documented as control-plane only,
not addressable by agent-facing modules. So the query is a new
module-originated op, `ModuleControlRequestFromModule::LiveRoots {}`, with
no module id in the request: the daemon answers for the caller's own
registration, so a module can see only its own routes. That scoping is the
authority rule. It is a new, narrow trust edge, and it is a subc-protocol
wire addition, so slice E is not daemon-only.

Snapshot contract: resolve the routable endpoint and count bound and pending
bindings under one forwarding read lock, the same linearization point
cutover's write lock uses, so a reply describes either the incumbent before
cutover or the candidate after it, never a mix. The reply must satisfy
`total_bindings == sum(bound + pending) + unknown_root_bindings`.

**Swap semantics.** Incumbent and candidate share a module id, so the query
needs a rule for which one it describes. It reports the bindings of the
endpoint that is routable when the query is answered. Before cutover that is
the incumbent, which is the set that is about to move and so the one worth
warming. The reply is a snapshot. Routes the incumbent takes after the query
are not in it, and they warm lazily after cutover, as every root does today.
A candidate may re-query before flipping. It has no reason to poll.

**Where `module_warming` is visible.** During a swap, the candidate's
not-ready state is invisible to callers: the incumbent stays routable until
cutover, so nobody's `route.open` is refused while the candidate warms. The
warm-up budget costs callers nothing in the swap case. It costs them only on
a plain restart (no incumbent), where route.open answers `module_warming`
for the whole budget. The SDKs retry that code only until their route-open
retry deadline, 30 s by default. **So on a plain restart the budget must
stay well under 30 s, or opens fail rather than wait.** The budget value
itself is the module's, measured from per-root load times. It is a module
constant, not daemon config, because only the module knows what one root
costs. AFT measured about 45 s to warm its live set after a restart, so it
sets two budgets: 10 s on a plain restart (then flip, and the rest warm
lazily) and a 90 s ceiling as a swap candidate, where nobody waits.

**Telling a swap from a restart.** Two budgets only work if the module
knows which case it is in, and it has to know before HELLO, since that is
when it plans its warm-up. The supervisor decides this at spawn, so the
signal is an environment variable set on the candidate's spawn:

    SUBC_SPAWN_ROLE=swap_candidate

It is absent on every other spawn, and **absence means plain restart**, the
safe reading: a module that misses it uses the short budget and gives up
some warmth, never correctness. Of the three possible carriers it is the
only one available before HELLO, and it needs no wire change. It says only
which budget applies. It is not an authority: the swap-token check at HELLO
(slice C) stays the thing that proves a candidate is the one the supervisor
minted, and nothing in the daemon trusts this variable.

### r3 review corrections to B and C (binding on the slice briefs)

An independent panel read r2 against the shipped code (three seats, two
quota-exhausted). It confirmed findings 1 to 4 closed by the r2 design, and
that an incumbent ack arriving after cutover cannot commit to the wrong
endpoint: `commit_route_locked` requires the reservation's endpoint to equal
the active one. It found these, all fixable inside their slice:

1. **The superseded-endpoint arm is in the wrong place (blocks B as r2
   specified it).** r2 put it in `refuse_to_end_module_connection_for_a_client`'s
   caller in control.rs. By then `complete_pending_relay` has removed the
   pending entry and `commit_route_locked` has consumed it. The reservation
   indexes are removed before the endpoint check fails, and the pending
   sender is dropped unanswered. The caller holds only a connection id and a
   corr, so it can keep the connection alive but cannot answer the waiting
   client or send the channel-scoped GOODBYE. The arm moves into
   `complete_pending_relay` / `commit_route_locked`, which must detect a
   superseded endpoint BEFORE stripping state, answer the client
   `module_reloading` through `pending.sender`, release the reservation pair,
   and return the abandoned target in the completion so the caller emits one
   GOODBYE on that channel. The control.rs caller keeps only the "do not end
   the connection" half. Test: force an incumbent ack between promotion and
   drain; the incumbent's other routes survive, the client gets a retryable
   refusal, the reservation is released, and the pending-to-bound transition
   is counted once.
2. **Registry candidate slot must cover connection-keyed lookups (B).**
   `replace_catalog_for_connection` and `deregister_connection` search
   registrations by connection id. With a candidate slot they must search it
   too, or the candidate's `catalog.update(ready: true)` returns `Ok(None)`
   silently and it never becomes ready.
3. **Consumer attestation during overlap (C).** `spawned_consumer_authorized`
   must accept both slots' nonces while the swap is open. Otherwise a
   consumer identity presented by the incumbent, or a process it spawned,
   fails `route.open` the moment the candidate is spawned.
4. **Candidate exits need their own reap path (C).** The existing reap
   (`classify_reaped_child_exit` into the module's snapshot, then
   `next_crash_restart`) would both spend a restart unit and flip the
   incumbent's state. The candidate slot gets a separate classification that
   kills and reaps only the candidate and releases only its slot and nonce.
5. **`SUBC_SPAWN_ROLE` must be actively absent on other spawns (C).** Spawn
   applies arbitrary `env` entries from the module spec, so "set it only on
   candidates" does not make it absent elsewhere. Plain spawns remove it
   explicitly, and the variable is refused as a configured `env` key.

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

    A. rung 2 — wire field + registration flag + route.open refusal with
       its own message/detail/log field/counter key + catalog mirror +
       `ck catalog` rendering. Golden fixtures both directions (absent ==
       ready). One slice, subc-protocol minor bump. JUDGED SHIPPABLE.
    B. rung 3 forwarding + registry — candidate slot in ForwardingInner
       and Registry, endpoint-keyed drain entry point, slot-keyed
       registration waits, the non-fatal superseded-endpoint arm, cutover
       under the write lock. One slice, daemon only, and it is the
       concurrency-critical one.
    C. rung 3 supervisor — candidate child slot with per-slot nonce,
       cgroup suffix and capture file, `SUBC_SPAWN_ROLE=swap_candidate` on
       the candidate's spawn only, mandatory swap-token HELLO check
       ordered before the reserved gate, swap state machine with the
       failure arms, `supervisor.swap` control op, `--swap` on the CLI,
       `overlap` in ModuleSpec + frozen on catalog.update + refusal.
    E. warm set — `Option<ProjectRootId>` on RouteBinding and the pending
       reservation, module-originated `LiveRoots {}` scoped to the caller's
       own registration, the three-way reply, the Rust module-handle call.
       A subc-protocol minor bump (new module op). Independent of B/C, can
       ship before them (useful on a plain restart too).
    D. aft adopts: warm the `supervisor.live_roots` set under a module-side
       budget, ready:false/true, overlap declaration. AFT's slice, in
       their repo, after A and E land.

A ships alone. B and C do not ship without each other and B goes first
because C's failure arms are tested against B's slots.

## Mutation controls the slices must carry

- A: with the `ready` check removed from `handle_route_open`, a registered
  `ready:false` module receives a relayed bind — must red on the relay
  reaching the stub, not on the client's error code alone. And: with the
  detail/log discriminator removed, a registered-not-ready refusal and a
  supervised-absent refusal produce identical log lines — must red by name.
- B: (i) with cutover done outside the forwarding write lock, an in-flight
  relay to the old active commits after promotion — must red on the route
  landing on the incumbent. (ii) With the superseded-endpoint arm removed,
  a relay acked by the incumbent after cutover ENDS THE INCUMBENT'S
  CONNECTION — must red on the incumbent's other routes receiving GOODBYE.
  (iii) With `begin_module_drain` used instead of the endpoint-keyed one,
  step 5 marks the candidate draining — must red on a post-cutover
  route.open refusing `module_reloading`.
- E: (i) with the root dropped when the binding is created, a module with
  live routes answers `roots: []` and `unknown_root_bindings: 0`. This
  must red on the root-known arm. (ii) With `unknown_root_bindings` folded
  into "no bindings", a binding with no stored root makes the reply read as
  row 3. This must red by name on the unknown-root arm, which is the
  silent-cold-swap arm.
- C: (i) with the failure arm removed, a candidate that never becomes
  ready leaves the incumbent drained — must red on the incumbent's
  route.closing having been sent. (ii) An unsolicited second HELLO with a
  nonce the supervisor did not mint, on an UNRESERVED id with an open
  swap, must be refused — this is the arm the reserved gate cannot supply.
  (iii) With the swap check ordered after the reserved gate, a reserved
  module's candidate is refused `reserved_module` — must red by code.

## Settled by review

1. `module_warming` is the right code; unanimous, decisive (closed
   retryable set in protocol crate and both SDKs, unknown codes terminal).
   Discriminate in message, `ErrorBody.detail`, and a log field, never in
   the code.
2. `ready` may go `true -> false` after registration; the check is
   best-effort per route.open and costs nothing. A module flapping it
   looks like a restart storm to callers, which is that module's defect.
3. Overlap is a manifest opt-in, default exclusive, frozen on
   catalog.update, sourced from ModuleSpec so it is readable when the
   module is down.

## Still open

- Health probe and restart budget for the candidate slot: same probe;
  a failed swap does not spend a restart-budget unit. Not judged by the
  panel (that code was out of range); C's brief must state it and test it.
- The HELLO-to-forwarding handoff in control.rs was inferred by the panel
  rather than read; B's worker reads it first.
