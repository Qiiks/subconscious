# Start-over audit, 2026-09-06: the five we are doing

Six independent model audits of this repo answered "if you started over, what
would you do differently". Their claims were checked against source before this
list was written; the ones that survived and were approved are below, in the
order they will be worked. Each item is a standing task for idle time on this
seat, not a campaign with a deadline.

Verified findings the list rests on (numbers read from the tree, not the audits):

- `subc-core` is one crate holding the daemon (`lib.rs`), the operator CLI
  (`bin/ck.rs`, 7,031 lines), and the setup/upgrade engine (`setup/`, 23 files,
  `#[path]`-included into `ck.rs` only). The crate therefore carries `rusqlite`
  (bundled), `reqwest`, and `ed25519-dalek` for code the daemon never links.
  Twelve sibling modules dev-depend on `subc-core` for `bootstrap::run_with_config`
  (an in-process daemon for their tests), `Frame`, `ServerError`.
- `ModuleManifest` requires `trust_tier`, `consumes`, and `bindings` on the wire
  and in the builder; the daemon reads none of them on any production path.
- The four wire crates are `publish = true` and on crates.io, but the registry is
  behind the tree by several minor versions (protocol 0.10.0 published, 0.18.0
  here). Twenty-seven sibling checkouts path-depend on them instead.
- Timing budgets are synchronized by comment: the SDK route-open retry deadline
  (30s) "matches" the daemon drain ceiling (30s); liveness probe windows and the
  bind-relay budget (12s) are copies. Nothing fails when one side moves.
- Retry/close decision tables are hand-mirrored in TS, Rust, and Swift with
  comments saying "kept identical to".
- The connection reader awaits routing inline (`server.rs`) and a request awaits
  route credit on that same task (`router.rs`); a saturated route blocks CANCEL
  and unrelated routes on the connection. The loom-checked single-owner design in
  `dispatch_spike/` is `#[cfg(test)]`, waiting on production evidence.

## The five

| # | Item | Shape | Fleet cost | Status |
|---|------|-------|-----------|--------|
| 1 | **Contract tables as fixtures, not comments.** Budgets (drain, retry deadline, bind relay, probe windows, auth deadline, arbitration grace) and decision tables (retryable route-open codes, close-reason dispositions) live in `crates/subc-protocol/tests/golden/`; daemon and all three SDKs assert against them the way `MAX_FRAME_BODY_LEN` is asserted today. Codegen only if drift recurs after the fixtures exist. | fixture + parity tests | none (tests only) | **DONE** 2026-09-06, `ba8395de` |
| 2 | **Publish on every bump.** The wire-crate release chain publishes to crates.io as part of the version bump, so a consumer can pin the registry instead of the sibling path. Path deps stay supported; the notice invites, never forces. | release script + CI workflow | one notice | **DONE** 2026-09-12 — closed a nine-version registry gap in one pass; registry now serves protocol 0.21.0, transport 0.7.0, control 0.13.1, client-rs 0.15.1 |
| 3 | **Manifest diet.** `trust_tier`, `consumes`, `bindings` become `Option` + `serde(default)`; the builder stops requiring them; the daemon keeps decoding old manifests that carry them. | protocol bump | one lock wave (builder signature) | **DONE** 2026-09-06, `ba8395de` |
| 4 | **Crate split.** `subc-core` keeps the daemon library (which is what the twelve dev-dep consumers use); `ck`, `ck-under-test`, `setup/`, `fleet_lint`, `subc-probe`, `fake-aft-stub` move to a new `ck` crate. No behaviour change; the daemon's dependency graph loses the installer's. | workspace move | one `subc-core` bump for dev-dep consumers | **BLOCKED on a decision**, [ask_c1829aad](ask_c1829aad) — see below |
| 5 | **Module lifecycle authority.** One record per module with incarnation-fenced transitions (registered, admission closed, drained, exited, replaced); `handle_route_open`'s **six separately-locked state reads** become one; public status is a projection. Not the per-frame actor. | design room → athena → spec campaign | daemon-internal | needs a room — premise re-measured 2026-09-19, see below |

Batched into waves so each fleet cost is paid once:

- **Wave A** = items 1 + 3 → one `subc-protocol` bump, one lock wave.
- **Wave B** = item 4 → one `subc-core` bump.
- **Wave C** = item 2 → publish chain, then the invitation notice.
- **Wave D** = item 5 → room first.

### Wave B acquired a second reason, and a decision it did not originally need

ENGRAM asked to consume the daemon from crates.io (2026-09-18) after my shared
checkout put their `--locked` gates through three version flips in an afternoon.
Registry pins are immune to that by construction; path deps are not. But
`subc-core` is `publish = false`, so it is the one crate that cannot be pinned
— which is the standing argument for the split arriving from a consumer rather
than from tidiness.

The cost is the same either way and was miscounted twice before it was right:
**four permanent crates.io names** on both paths, because `cortexkit-log` and
`subc-jsonc` are used by the daemon half and would need publishing regardless.
So the count does not discriminate, and the decision is purely about which crate
is permanent: `subc-core` as it stands (carrying `ck` and the setup engine
forever) or a small `subc-daemon` split out first.

That is [ask_c1829aad](ask_c1829aad). Nothing is stuck while it waits.

### Item 5's premise, re-measured rather than recalled (2026-09-19)

`handle_route_open` (control.rs:1647-2000) takes **six** state reads across
**three** different authorities, each acquiring its own lock:

    registry.get_module                    x2
    supervisor.removal_tombstone_age_ms    x1
    forwarding.module_is_draining          x1
    forwarding.has_live_module_connection  x1
    forwarding.begin_route_bind_relay_for  x1

So the count in the row is right, and the sharper statement is that they span
THREE AUTHORITIES rather than six reads of one — which is why no single lock
makes them consistent, and why the fix is a record rather than a coarser lock.
Between the first `get_module` and the final bind relay, a module can register,
deregister, begin draining, or be removed, and each read sees a different
instant.

METHOD NOTE, because it nearly produced a false premise: a line-based
`grep -oE "registry\.[a-z_]+\("` over that span returns **zero**. rustfmt wraps
the chains (`self\n    .registry\n    .get_module(`), so the call and its
receiver are on different lines and a line-oriented pattern cannot match. The
zero was caught only by a known-present control in the same span (43 `module_id`
mentions, proving the range was right and non-empty); without it the reading is
"no registry calls here", which is well-formed and false. Count with
`tr '\n' ' '` first when the question is about call sites in Rust.

## Audit claims that were checked and declined

- Direct peer IPC instead of the splice router: trades the channel table for N×M
  socket management and moves `Principal` attestation out of the daemon; the
  epoch/handle machinery it blames is what makes teardown honest.
- gRPC / Cap'n Proto instead of the 21-byte envelope over opaque JSON: the daemon
  never parsing bodies is the design; a schema-owning daemon is a different product.
- Unix sockets + peer credentials instead of loopback TCP + key: correct
  analysis; ruled "stay on TCP" on 2026-08-10, and the remote story is Noise.
- Machine-owned config + human override file: right at t=0; the comment-preserving
  editor already exists and is small.
