# subc wire batch, September 2026 — r1

Status: draft r1 for review (BROCA, MC, AFT, ALF). One lock wave carries every
item here; nothing in it ships alone, because each item alone re-stales the
same twelve path-dependency consumers and the per-bump cost is a human
reading `frame.rs` at BROCA, not a lockfile line.

**Frame layout: unchanged.** `crates/subc-protocol/src/frame.rs` was last
modified 2026-07-13 (`5c1e2d23`). Every item below is a JSON body field, a
new channel-0 push, an SDK function, or a transport-crate helper. Envelope
bytes do not move.

Crates and versions the wave bumps: `subc-protocol` 0.19.0 → 0.20.0,
`subc-control` 0.11.2 → 0.12.0, `subc-transport` 0.6.0 → 0.6.1,
`subc-client-rs` 0.12.1 → 0.13.0, `@cortexkit/subc-client` (minor). Daemon
ships first (rule #16822): every reader-side change below is deployed and
verified running before the omitting or newly-sending producer is announced.

---

## 1. `BindIdentity.project_id: Option<String>` (subc-protocol)

**Consumer:** BROCA (durable per-project lineage keyed on a rename-stable id).
**Producer:** harness plugins / prefrontal supplying it on `route.open`.

```rust
pub struct BindIdentity {
    pub project_root: PathBuf,
    pub harness: String,
    pub session: String,
    /// The entorhinal-registered project id (`pj-…`) for `project_root`,
    /// when the root is a REGISTERED project. Absent for implicit roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}
```

Semantics, normative:

- **Registered only.** A producer sends `project_id` only when entorhinal
  resolved the root to a registered project. Implicit roots derive an id
  from the path and re-roll on rename; they are never sent as `project_id`.
  Absent therefore means "no stable id, key on the triple", not "unknown".
- **Consistent per session.** A producer sends it on every bind of a session
  or on none. A consumer keying durable state on it splits its own lineage
  if binds alternate; at cold start a triple-keyed bind opens a fresh empty
  lineage byte-identical to a new session, so **no consumer-side fence can
  catch a producer that drops the id later**. This invariant is the only
  brace; it goes in the doc comment and the CONSUMER-IMPACT notice verbatim.
- **Daemon posture:** relayed verbatim on `route.bind`, unattested, same as
  `project_root`/`harness`/`session`. Consumers verify via entorhinal
  `resolve`. `ck routes` shows it when present.
- **Construct impact:** `BindIdentity` is a plain struct with struct-literal
  construction sites across the fleet; this is construct-breaking for them.
  The wave adds `BindIdentity::new(project_root, harness, session)` and
  marks the struct `#[non_exhaustive]` so the next field is not.

SDK plumbing: TS `routeOpen(..., { projectId })`; Rust `RouteTarget`/bind
options gain `project_id`. Both default to absent.

## 2. Drain: stop notice to the module, busy predicate, subscription-excluded quiescence

**Consumers:** BROCA (seal), AFT (StreamEnd held-open subscriptions), every
module with a graceful-stop hook.

Facts this stands on (supervise.rs, 2026-09-11): the daemon sends **no
SIGTERM** on any path. Restart = route drain (30 s ceiling) → `route.closed`
pushes → route GOODBYEs → module GOODBYE on channel 0 → wait ≤30 s for the
child to exit → SIGKILL. The graceful-stop signal a module gets is the
channel-0 GOODBYE, today at drain END. Broca's drains hit the 30 s ceiling
with zero runs in flight because `session.subscribe` holds a request credit
open; AFT's 328 held-open subscriptions do the same. `drained: false` is the
steady state for both, and it is a measurement of the predicate, not of the
modules.

Three changes, landing together or not at all:

**2a. Module-side stop notice at drain start.** The daemon sends the module
a new channel-0 control command when the drain begins:

```rust
// subc-protocol session.rs, ModuleControlCommand
Draining {
    reason: RouteCloseReason,   // reload | restart | disable
    deadline_ms: u64,           // the drain ceiling, from the daemon's clock
}
```

A module receiving it must stop admitting new work, `StreamEnd` its
held-open subscription lanes, and let in-flight requests settle. It is
advisory: a module that ignores it is treated as today. The module GOODBYE
at drain end is unchanged and remains the exit trigger.

**2b. Busy predicate from a declared gauge.** A module may declare, in its
manifest `self_signals`, one entry of a new kind:

```rust
SelfSignalKind::Busy   // anchored_to a health gauge name, e.g. "runs_in_flight"
```

When present, the drain's quiescence condition becomes
`wire_quiescent && health.metrics[gauge] == 0`, polled from the module's
`health.check` reply at the existing probe cadence. Undeclared modules keep
wire-only quiescence. The gauge must be a non-negative integer; anything else
reads as "busy" and is logged once per drain.

**2c. Subscription-excluded wire quiescence.** `endpoint_in_flight_count`
(forwarding.rs:1347) counts every acquired credit. The drain's quiescence
wait excludes credits whose request the **client declared a subscription at
open** — an explicit request-kind bit on the route request, never inferred.
AFT's review killed the inferred form: "first reply was `StreamData` with no
terminal yet" is not a discriminator, because a `bash` call with `wait:true`
on a 25-minute cargo run holds its permit exactly the same way for exactly as
long and MUST count as in flight (it is the class `drained:false` was right
about), and a provider that streams a long tool result would be misread the
same way. There is no body field the daemon parses and no stream-begin frame
in subc-protocol, so the only honest signal is the client saying so.

Wire: a `SUBSCRIPTION` flag bit on the `Request` envelope (the envelope's
remaining reserved bit, allocated here; TS/Swift constants and the golden
`protocol_constants.json` in the same bump). The SDKs set it from
`subscribe()`/`callStreamed`-style entry points; an ordinary `call()` never
sets it. The daemon reads it at credit acquisition and tags the credit.

The excluded set is **settled at drain start**: captured when the drain
begins; subscriptions opened after the notice are not excluded (they should
not exist; 2a asked the module to stop admitting). Excluded subscriptions
still receive their route GOODBYE at drain end as today.

`route.closed` gains `excluded_subscriptions: u32` beside `drained` and
`abandoned`, so a consumer can tell "drained because nothing was in flight"
(0) from "drained with N held-open subscriptions excluded" — the line that
says whether a consumer's wake lanes were live at close.

Ordering with 2a: the stop notice is written to the endpoint's channels
**before** the quiescence wait begins, so a consumer's cancel-on-closing
fires before any GOODBYE. If a drain completes early under 2c, the consumer
sees the notice, then GOODBYE — never GOODBYE alone. (A consumer that treats
GOODBYE as terminal regardless is safe either way; the stop-first order is
the cleaner one and is normative.)

The 30 s ceiling is never extended by any of the three. Consequently `Busy`
changes **when** the kill lands inside the ceiling, never **whether**: a
module whose gauge is still non-zero at 30 s is killed as today. A module
declares `Busy` to protect work that settles inside its own deadline
contract, not to hold the daemon.

Seal rules a module's stop hook must satisfy, cited from BROCA's design:
interrupted work is `Interrupted`, never `Cancelled`; a seal that drops the
indeterminate set is worse than none; when fsync cannot land, exit unsealed
and say so; **the success path logs too**, including "sealed 0 in flight" —
silence must never be a success shape.

## 3. `call_with_streamed_body` (SDK affordance; no daemon change)

**Consumer:** MC. The 82.7 MB cold seed that motivated this is gone (the
adapter now ships inventory deltas and boundary-scoped seeds); the realistic
streamed body is a `state_sync` cold seed for a session the module has never
seen (single-digit MB) and later the D5 `lineage.begin/put/finish` uploads,
which the D5 contract already chunks above 1 MiB with begin/put/finish
semantics of its own (R11). The affordance stays because the shape is
right; the motivating size is recorded here so nobody sizes buffers to it.

Facts: `MAX_FRAME_BODY_LEN` is 64 MiB (subc-protocol lib.rs:162). The
client→module data path is `route.module_sink.send(frame).await`
(router.rs:665): it **awaits** writer capacity, so a client streaming
chunks gets TCP backpressure through the daemon, never a drop. The
module→client direction is `try_send` with connection-close escalation on a
full sink (router.rs:450): a module streaming replies to a slow client is
the arm that drops. The spec below is client→module.

Shape: one `Request` frame carrying the JSON head (`op`, params, and
`streamed_body: { total_bytes?: u64, digest?: "sha256:…" }`), followed by
`StreamData` frames on the same `corr` carrying opaque bytes, terminated by
`StreamData` with the `LAST` flag. The module SDK reassembles against a
per-op ceiling the module declares in its manifest operation:

```rust
ManagementOperation {
    …,
    streamed_body_max_bytes: Option<u64>,
    streamed_body_digest: StreamedBodyDigest,   // Optional (default) | Required
}
```

The ceiling counts **body bytes only** — the head `Request` frame is bounded
by `MAX_FRAME_BODY_LEN` like any request and does not count against it.

Refusals by name, before any bytes are buffered past the ceiling:

- `streamed_body_over_cap` fires on **two arms**, both tested: (i) EARLY, on
  the head, when `total_bytes` is present and exceeds the ceiling — before
  any `StreamData` is read; (ii) LATE, on actual bytes crossing the ceiling,
  when `total_bytes` is absent or understates the body. A head that lies
  small does not buy a larger buffer.
- `streamed_body_unsupported` (op has no ceiling declared).
- `streamed_body_digest_required` (op declares `Required` and the head
  carries no digest) — refused on the head, before buffering. A module whose
  custody model is digest-bound (D5's begin/put/finish refuses a conflicting
  re-declaration with the original intact) cannot admit a body it cannot
  bind, and the SDK refusing beats the handler discovering it after
  reassembly.
- `streamed_body_digest_mismatch` (trailer digest present and wrong).
- `streamed_body_sender_closed` — the sender's connection closed, or the
  request was cancelled, before the `LAST` frame. Surfaced to the handler as a
  typed refusal on the reassembly, never as a hang: an abort after the head
  must not leave the module reassembling a body nobody will read.
  `on_stream_begin` admits on the head; this is the symmetric close.

No resume after a dropped chunk: under backpressure-not-drop a dropped chunk
is a dropped connection, and the consumer re-sends the whole body (MC's
bodies are idempotent by key — `state_sync` on ordinal/inventory, D5 uploads
on digest). One trailer digest over the whole body, not per chunk: these
bodies are consumed whole.

An `on_stream_begin` admission hook lets a module refuse on the head alone. Rust `SubcConsumer::call_with_streamed_body(target, head,
impl AsyncRead)`; TS `callStreamed(moduleId, head, ReadableStream)`. The
binary-body gap noted in Rust (`send_request` hardcodes `Flags::new(false,…)`)
and Swift (`beginRouteRequest` binary:false) closes in the same wave.

## 4. `connection_file::discovery_candidates` treats empty as unset (subc-transport)

`discover()` already filters `SUBC_CONNECTION_FILE=""` via
`non_empty_os_var`; the ladder function `discovery_candidates(explicit,
env_named)` does not, so a caller passing `Some("")` gets `PathBuf::from("")`
(a bare relative filename, i.e. cwd) as its only candidate. The filter moves
into the ladder for both `env_named` and `runtime_dir`; test: `Some("")`
behaves as `None` and every candidate is absolute. (ENGRAM fixture-vacuity;
FUSI #144.)

## 5. `pub const WIRE_CRATE_VERSION: &str = env!("CARGO_PKG_VERSION")` (subc-protocol)

For `ManifestProvenance.wire_crate_version`; PLEX and BROCA refuse to
hand-type it.

## 6. Daemon logging `with_ansi(false)` (subc-core, same window)

Not wire. The non-tty daemon log carries ANSI styling that breaks contiguous
ASCII matching on level words; the stall-hunt queries that pinned the
current rendering are closed. Lands in the daemon cut that carries 2a–2c.

---

## Order of operations

1. Daemon cut (2a sender, 2b/2c predicate, 4 reader-side, 6) placed and
   verified running here and on the alpha VMs.
2. One `#fleet-notices` post: CONSUMER-IMPACT per crate, the frame-layout
   line above for BROCA, the `project_id` producer invariants verbatim, the
   owed-set from `scripts/fleet/check-sibling-locks.sh`, the grep each
   consumer runs.
3. Producers move: prefrontal/plugins send `project_id`; broca and aft wire
   `Draining` into their stop hooks; MC adopts `call_with_streamed_body`.

## Consumer ledger (one line per seat, added as reviews arrive)

- **callosum** (CALLO): item 1 — one production construction site
  (`runtime.rs:11837`) plus eight test literals, all migrate to `::new`;
  as a producer on the serving-side `route.open` its `project_id` is always
  absent (no entorhinal resolution on that path), so consistency holds by
  construction. Item 2 — consumer of both 2a and 2b; 2a lets it refuse
  inbound admission at drain start (origin classifies `not_sent`, retries
  elsewhere) and 2b will anchor `Busy` to a new `ledgered_in_flight` gauge
  so the drain waits for exactly the calls whose interruption is an
  `ambiguous` reconciliation. Items 3–6: no exposure.

- **aft** (AFT): item 2c — the consumer whose 328 held-open wake
  subscriptions pinned every drain to the ceiling. r1 stands with one
  change, folded: the subscription discriminator is an EXPLICIT request-kind
  bit set by the client at open (their 1a), not inferred from the first reply
  frame (their 1b, which misreads a long `wait:true` tool call). Cancel-on-
  `route.closing` is safe against 2c in either ordering; stop-first is
  normative. `excluded_subscriptions` on `route.closed` added at their
  request. Items 1, 3–6: no exposure.

- **magic-context** (MC): item 3 — first consumer; r1 stands with the
  digest-required arm and the two ceiling sentences (body-bytes-only, and
  over-cap on both the early `total_bytes` arm and the late actual-bytes
  arm) folded above. Will always send the trailer digest. Items 1, 2, 4–6:
  no exposure.

- **alfonso-ios** (CKIOS): item 1 — the phone is a pure client and never
  sends `BindIdentity`; no consumer. Item 2a — a consumer seam, not a
  reviewer note: whatever `Draining{reason, deadline_ms}` becomes in the
  Swift SDK, it must not surface to the app as `FedConnectionState.disconnected`
  or as a retryable failure; the phone renders `disconnected` as "Lost the
  connection to your Mac", and a healthy drain rendered that way is
  "waiting is not failing" one layer down. Invisible is acceptable; a
  distinct state (`awaitingPeer`-shaped) is acceptable; error-shaped is
  not. Normative for the Swift SDK arm of 2a.

## Open for review

- 2b: gauge name conflicts — a module declaring `Busy` anchored to a gauge
  its health reply omits. Proposed: treat omission as busy for one drain
  and log; do not refuse HELLO.
- 3: ~~whether the trailer digest is mandatory~~ — settled by MC: optional
  by SDK default, verified when present, and **requirable per op** via
  `streamed_body_digest: Required` (refused as
  `streamed_body_digest_required` on the head).
- 1: whether `ck routes` should render `project_id` or only expose it in
  `--json`.
