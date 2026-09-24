# Upgrading the daemon without restarting the fleet

Status: design r2, for review before implementation. r1 was reviewed by a
three-member panel against the source at `ecad727e`; r2 folds that review in
after re-checking each finding against the current tree. File and line
references are to that tree.

## Changes in r2

- **Module connections are quiesced differently.** The reader is buffered and
  keeps a partially read frame alive across `route.open` completions, so
  "park at a frame boundary, the next bytes stay in the kernel" was false. The
  park signal is now observed only when no frame read is in progress, and each
  connection's unconsumed `BufReader` bytes travel in the handoff record.
- **Quiescence is a per-connection checklist**, not "egress queue empty": it
  covers the producers that can still enqueue on a module sink (late GOODBYE
  tasks, `route.bind` guard drops, orphan-route GOODBYEs, client teardown) and
  ends with a writer-acknowledged flush and an explicit fd recovery.
- **Allocator state is carried** (channel, epoch, control correlation,
  endpoint generation, connection id high-water marks) so the new image never
  reissues an identity a module may still hold.
- **Adoption registers without a HELLO_ACK** through a dedicated path.
- **Preflight compares HELLO_ACK facts**, not only the handoff schema.
- **Reexec is refused** while a swap, restart, drain or unregistered spawn is
  in progress (open question 2, settled).
- **Child reaping by pid is settled** against tokio 1.52.3's process driver
  (open question 3), with the conditions it needs.
- **The orphan sweep skips adopted children**, uses a separate atomically
  replaced record, checks executable identity, and states its crash window.
  On macOS it needs a process-identity source the daemon does not have yet.
- **Fallback no longer depends on the record**: fd numbers and kinds travel in
  the environment, secrets in the 0600 file.
- **CLOEXEC is restored on every adopted fd before any spawn**, and nothing
  spawns while it is cleared.
- **Exec-failure resume** is specified (drain flags, accept loop, holds), the
  journal distinguishes attempted / adopted / failed / fallback, and the
  operator surface no longer promises a reply on the closed control connection.
- **Reexec does not use the #121 shutdown machinery**: that flag is one-way
  and the shutdown drain tells modules to seal. Reexec has its own clearable
  hold and a route-only drain.
- **New sections**: stated losses, the unsafe-code boundary, cgroups, stderr
  pumps, TS provider assumption, what remains unsettled.
- **Slices**: slice 3 split into 3a (quiesce and resume, no exec) and 3b
  (record, adoption, resume after failed exec); the spawn-stream contract note
  moves ahead of any adoption; tests added.

## The problem

Every daemon upgrade today restarts every module. The daemon stops its children
before it exits, and the new daemon spawns fresh ones. Measured costs from one
week (2026-09-17 to 09-24):

- Magic Context Rust-mode sessions rebuilt 122K to 436K tokens of cache per cut.
- engram lost upload batches (one cut killed a five-hour publish at batch 70 of
  ~115).
- broca had sessions in flight at each cut.
- A cut is a shared, announced event, so upgrades get batched and delayed, and
  daemon fixes sit on master until someone pays for a window.

Upgrading the daemon should not restart the modules. Modules are separate
programs with their own release cycles; the daemon is the router between them.

## Goal and non-goals

Goal: replace the running daemon's code while every supervised module keeps
running, keeps its process, its memory and its connection to the daemon, and
the module needs no change to take part.

Not a goal (this design): keeping client routes alive across the upgrade. Routes
close with reason `Restart` exactly as they do today, and callers reopen them;
every SDK already handles that. Carrying live routes means serializing the
forwarding table (bindings, channel maps, epochs, credits, pending relays) and is
a separate, much harder step. It is listed at the end as a possible follow-up.

Not a goal: Windows. Windows has no exec that keeps the process; the upgrade
falls back to today's full restart there.

## Approach: the daemon execs its new binary in place

On Unix, `execve` replaces the program in the current process. The process id,
the parent (launchd or systemd) and the children are unchanged, and any file
descriptor without `FD_CLOEXEC` stays open across it. That gives three things for
free:

1. The modules stay the daemon's children, so the new image can still reap them
   and nothing is orphaned.
2. The service manager sees no exit, so no launchd `KeepAlive` race, no systemd
   `KillMode`, no bootstrap step that can fail (the cause of the 2026-09-24
   07:19Z outage).
3. If the exec itself fails (bad binary, missing file), `execve` returns an error
   and the old image is still running with all its state; it resumes.

`execve` runs no destructors, so the `kill_on_drop(true)` set on every child
(`supervise.rs:5630`) does not fire across it. The old image must still never
drop a child handle during the quiesce, because on the exec-failure path it
keeps running and a dropped handle kills the child.

### The unsafe-code boundary

Both `subc-daemon` and `subc-core` forbid unsafe code (`subc-daemon/src/lib.rs:7`,
`subc-core/src/lib.rs:6`). The old image's half is reachable safely:

- `std::os::unix::process::CommandExt::exec` performs the `execve` without
  forking.
- `rustix::io::fcntl_setfd` (the `io` module is unconditional in rustix 1.1.5,
  `src/lib.rs:224`; the function takes `AsFd`, `src/io/fcntl.rs:68`) sets and
  clears `FD_CLOEXEC`.
- A module socket becomes an `OwnedFd` without unsafe code (see "Recovering the
  fd" below), and a child pipe does too through tokio's
  `ChildStderr::into_owned_fd` / `ChildStdout::into_owned_fd`
  (tokio 1.52.3 `src/process/mod.rs:1637`).

The new image's half is not. Turning an inherited fd *number* into an owned
socket or pipe is `OwnedFd::from_raw_fd`, which is unsafe, and so is closing an
fd number the process does not own. There is no safe std or rustix API for it.
r2 proposes one small crate (working name `subc-fd-inherit`) whose only unsafe
function is "take ownership of this inherited fd number after checking with
`fcntl(F_GETFD)` that it is open", with every caller in the daemon crates staying
safe. This is a deliberate exception to the forbid and needs an explicit review
decision (see "Unsettled").

## What has to cross the exec

| Thing | Why it must survive | How |
|---|---|---|
| Listening sockets | the port in the connection file stays valid; no bind race | inherit the fd |
| Each child's stdout and stderr pipe read ends | if the daemon closes them, the child's next write gets `SIGPIPE`/`EPIPE` and it dies | inherit the fds; the new image restarts the pumps on them |
| Each pump's partial line | bytes already read from the pipe but not yet a complete line (`stderr_tail.rs:618-667`) | record |
| Each subc module's TCP connection to the daemon | Rust `serve()` returns on EOF and the module exits (`subc-client-rs` lib.rs `module_loop`: "Clean EOF: the daemon closed the connection. `return Ok(())`"), and hand-rolled modules do the same. Dropping these connections restarts the modules, which is the thing we are removing | inherit the fds, quiesced (below) |
| Each module connection's unconsumed read-ahead | `BufReader` bytes already pulled from the socket (`server.rs:371`) | record |
| Per-module supervisor state | restart budget, `spawn_generation`, pid, process start time, spawn facts (`spawned_from`, `spawned_file_identity`, `spawned_at_ms`), cgroup slot (primary or `_swap`), last exit, enabled/state | record |
| Launch-nonce tables | `spawn_nonces`, `reserved_nonces`, `reserved_prefix_owners` (`supervise.rs:1298-1325`) | record (secret) |
| Registration state of each module connection | manifest, negotiated wire version, readiness, capabilities, and the HELLO_ACK facts served (below) | record |
| Allocator high-water marks | see "Identities the new image must not reissue" | record |
| Stderr ring contents | `ck module stderr` history of the running processes | record (bounded: 200 lines, 64 KiB per module, `stderr_tail.rs:37-47`); the capture files on disk are unaffected |

Not carried: client connections (closed, see below), routes, pending
`route.bind` relays, pending module-control RPCs (settled or cancelled first),
health-probe tombstones, the bind-relay breaker state, the spawn-event ring, the
in-memory terminal ring, the listener accept backlog beyond what the kernel
keeps. Each of these is either empty by the time the record is written or a
stated loss (see "Stated losses").

Launch nonces: a nonce is minted per spawn, handed to the child in
`SUBC_LAUNCH_NONCE` (`supervise.rs:5453`) and checked at HELLO against
`reserved_nonces` / reserved prefixes (`supervise.rs:1525`
`reserved_hello_rejection`) and at consumer `route.open` attestation against
`spawn_nonces` (`supervise.rs:1315-1318`) without being consumed. The new image
needs all three tables so reserved-id gates and consumer attestation keep
working for the adopted processes. A `None` entry in `reserved_nonces` (reserved
and never spawned) is carried as `None`, not dropped: dropping it would reopen
the id to squatters. The record is secret material and is handled like the
connection key (below).

## Identities the new image must not reissue

Ordinary registration resets a module endpoint's allocators:
`register_module_connection_inner` bumps `next_generation` and inserts
`next_module_channel = 1` and `next_control_corr = 1` for the new endpoint
(`forwarding.rs:663-673`), and `module_slot_epochs` is keyed by
`ModuleRouteKey { endpoint, channel }` (`forwarding.rs:57-60`, `467`), so a new
endpoint starts every channel at epoch 1 (`forwarding.rs:2484-2491`).
`Router::next_connection_id` starts at 1 (`router.rs:604`, `622`, `736-737`).

That reset is safe today because a new registration is a new TCP connection and
a new module process. It is not safe for an adopted connection:

- The daemon accepts, by design, that a module can miss a route GOODBYE and
  keep the route (`forwarding.rs:95-122`). Module SDKs match a route by
  (channel, epoch) (`forwarding.rs:206-210`). If the new image reissues a
  (channel, epoch) the module still holds, the module merges two routes.
- A module-control reply that arrives after the exec for a correlation id the
  new image has reissued would be taken as the answer to the new request. Only
  a *different* op is caught (`UnexpectedOp`, `forwarding.rs:1573-1579`); a
  health reply matching a new health probe would be misattributed.

So the record carries, and the new image starts strictly above:

| Scope | State | New image starts at |
|---|---|---|
| per module endpoint | `next_module_channel` (`forwarding.rs:465`) | the carried value |
| per module endpoint | `module_slot_epochs` (`forwarding.rs:467`) | the highest epoch ever issued on that endpoint, + 1, for every channel |
| per module endpoint | `next_control_corr` (`forwarding.rs:473`) | the carried value; it is above every corr any pending RPC or `health_probe_tombstones` entry (`forwarding.rs:475`) holds, so those need no separate mark |
| global | forwarding `next_generation` (`forwarding.rs:461`) | the carried value, so an adopted endpoint's generation is never reused |
| global | router `next_connection_id` (`router.rs:604`) | the carried value; adopted connections keep their ids, which keeps them unique in the close registry, `endpoint_by_connection`, and logs spanning the upgrade |

Carrying one epoch mark per endpoint instead of the whole per-channel map keeps
the record small (the map can hold up to 65535 entries per endpoint) at the cost
of spending epochs faster; at 2^32 epochs per channel that cost is not
reachable in practice.

With the correlation mark carried, a late reply to an old-image RPC finds
neither a pending entry nor a tombstone, completes as `Unknown`
(`forwarding.rs:1586-1590`), and is dropped at debug level
(`control.rs:4624-4631`). That is the "discarded by the normal path" behaviour
r1 claimed; r1 was only right once the correlation mark is carried.

## The sequence

Old image, on `supervisor.reexec` (operator control op). Steps 1-3 are the
admission gate; steps 4-8 are the quiesce; 9-11 are the handoff.

1. **Preflight** (nothing changes if it fails). Resolve the target binary (the
   daemon's own configured path, which the placement has just replaced), check
   it exists and is executable, and run it with `--reexec-preflight`. The target
   prints, as JSON: the handoff schema versions it reads; its
   `PROTOCOL_VERSION`; its `module_subc_ops()` and subc capabilities; and, from
   the current daemon config, the storage descriptor it would serve each
   configured module and its machine id. Refuse unless:
   - this image's handoff schema is in the target's range;
   - the target's protocol version equals every adopted connection's
     negotiated version (negotiation is exact, `control.rs:65-70`,
     `1633-1642`, so an adopted module cannot be on another version);
   - every storage descriptor and the machine id equal what this image served
     each module in its HELLO_ACK (`control.rs:1954-1963`);
   - no subc op or capability this image advertised is missing from the
     target. Additions are allowed and are a stated loss: adopted modules do not
     learn of them until their next restart.

   Facts served at HELLO_ACK are frozen for an adopted module, because it never
   sends HELLO again. Refusing on a difference is what keeps a module from
   running under a storage policy or wire version the daemon no longer uses.
   The file can be replaced again between this check and the exec; the new
   image repeats the checks against its own build and config at step 12, and a
   difference there falls back per module (below), so the race can cost a
   module restart but never a module running on stale facts.

2. **Refuse while anything is in transition** (open question 2). Refuse with a
   retryable `reexec_busy` while any of these holds:
   - the forwarding table has a swap candidate, a superseded endpoint, or a
     draining endpoint (`candidates_by_id`, `superseded_endpoints`,
     `draining_endpoints`, `forwarding.rs:447-459`);
   - any module's supervise loop is restarting, swapping, in restart backoff,
     or has a spawned subc child that has not registered yet (a child spawned
     before the new connection file exists authenticates with the old key and
     would fail against the new image, burning a restart-budget slot);
   - another reexec, a daemon shutdown, or an operator `supervisor.restart` /
     swap is in progress.

   The check is serialized with the admission of those operations, not just
   read once. Swaps and restarts both run inside the module's own supervise
   loop (`supervise_swap.rs:18-19`), so the gate is a **reexec hold** command
   sent to every module's supervise loop: a loop acknowledges only from its idle
   state, and once held it refuses restart, swap, spawn and respawn with the
   retryable error until the hold is released. The forwarding half is checked
   under the forwarding write lock in the same critical section that sets the
   forwarding reexec gate (step 3), so a candidate cannot register between the
   check and the gate. If any loop does not acknowledge within a bound, release
   every hold and refuse.

3. **Stop admitting.** Stop the accept loops and keep the listeners. Today
   `serve_listener` takes the listener by value and the accept tasks are
   aborted on drop (`server.rs:250-253`, `260-279`), which would close the
   listener fd; the accept loop needs a stop signal that returns the
   `TcpListener` instead. Refuse new `route.open` with the retryable
   `module_reloading`. Set the forwarding reexec gate (refuses registration and
   cutover, like `daemon_draining` does at `forwarding.rs:653` and in
   `cutover_candidate`, `forwarding.rs:830`).

   **This does not reuse the #121 shutdown flag.** `begin_daemon_shutdown`
   closes the child roster (`supervise.rs:1954-1959`), whose flag is "set once
   ... and never cleared" (`child_roster.rs:44-62`); once set, every exit is
   recorded with disposition `DaemonShutdown` (`supervise.rs:5075-5085`,
   `5265-5293`) and any spawn is killed at birth (`supervise.rs:5688-5700`).
   Reexec must be able to resume after a failed exec, and exits during it are
   real exits, so it uses the per-loop hold above instead. It does not stamp
   `daemon_shutdown` in the journal either.

   Each held supervise loop also **stops polling its child's `wait()`** (tokio's
   `Child::wait` is cancel-safe; tokio reaps only when a child future is polled
   or dropped, see "Adopting children"). From here until the exec, the set of
   live pids is frozen: a child that exits stays a zombie of this process and
   is reaped either by the new image at adoption or by the old image after a
   failed exec.

4. **Drain routes, not modules.** For every endpoint, run the per-endpoint
   route drain with reason `Restart` and the usual bounded budget:
   `route.closing` to clients, wait for in-flight requests, `route.closed`, then
   release every binding, sending route GOODBYEs to modules. **Do not send
   `module.draining`.** The existing daemon-shutdown drain
   (`drain_for_daemon_shutdown`, `supervise.rs:1965-2035`) sends each module
   `ModuleControlCommand::Draining` (`supervise.rs:1993-1996`;
   `subc-protocol/src/session.rs:94-114`), which tells a module to seal because
   it is about to be stopped. There is no command that un-seals, so reexec uses
   the endpoint drain without that notice.

   Routes where a **module connection is the consumer** are drained too.
   Modules open routes to other modules (reserved principals,
   `forwarding.rs:154-161`), and a module connection runs the same
   `connection_loop` with its own `route_open_tasks` as any client
   (`server.rs:380`, `542-579`). Those routes are closed with `route.closed`
   (reason `Restart`) on the module's connection, and that connection's
   `route_open_tasks` are aborted and joined before step 7.

5. **Close client connections, and join them.** Request close on every
   non-module connection. `request_connection_close` only sends a oneshot
   (`forwarding.rs:570-592`); the real teardown happens later in that
   connection's `handle_connection`, which aborts and then joins its
   `route_open_tasks` (`server.rs:390-397`). Aborting a `route.open` drops its
   `RouteBindReservationGuard`, whose drop sends a GOODBYE to the target
   *module's* sink (`control.rs:310-337`). So step 5 finishes only when every
   client `handle_connection` has returned, not when close was requested. The
   connection tasks spawned by the accept loop are detached today
   (`server.rs:198-206`); the router needs a count of live non-module
   connections with a notification at zero, awaited with a bound.

6. **Settle producers that can still write to a module sink.** Per module
   connection, all of:
   - **Late GOODBYE tasks.** When a module's egress refuses a route GOODBYE,
     `send_module_route_goodbye` spawns a detached task that retries for up to
     `LATE_MODULE_GOODBYE_DEADLINE` (= the default drain timeout,
     `forwarding.rs:199`, `212-280`). These tasks are untracked today. They get
     a per-connection `JoinSet` (or counter plus notify) and are awaited with a
     bound. At the bound the remaining tasks are aborted and their GOODBYEs are
     lost; that is the existing accepted-lossy case (`forwarding.rs:95-122`)
     and is a stated loss, not a failure.
   - **`route.bind` reservation guards.** Covered by steps 4-5: every
     `route.open` that could hold a guard on this module has been joined, and
     `pending_relays` (`forwarding.rs:472`) for the endpoint is empty. The step
     checks that it is.
   - **Pending module-control RPCs** (`pending_control_rpcs`,
     `forwarding.rs:474`: health probes, route.bind relays, catalog updates).
     Wait for replies with a bound, then cancel what is left. Late replies are
     harmless once the correlation mark is carried (above).
   - **Orphan-route GOODBYEs.** The router answers a module frame on a route
     it does not hold with a GOODBYE, from that module connection's own read
     path (`router.rs:681-734`, `try_send` at `713`). It stops when the reader
     parks (step 7), so it is ordered before the final flush by construction.

7. **Park the module reader, then flush and stop the writer.**
   - **Reader.** The park signal is observed only at the top of
     `connection_loop`, where no `read_frame` future is alive. Today the loop
     keeps one `read_frame` future across `route.open` completions because it
     owns partial header and body buffers (`server.rs:519-539`), and waiting for
     a close request inside that `select!` discards them (`server.rs:524-527`).
     To keep the idle wait interruptible without that loss, the loop waits for
     readability with `AsyncBufReadExt::fill_buf`, which consumes nothing and is
     cancel-safe (tokio 1.52.3 `src/io/util/async_buf_read_ext.rs:270-272`),
     and creates the `read_frame` future only once bytes are buffered. The park
     signal races only the `fill_buf` wait, never a started frame. A module that
     stalls in the middle of a frame blocks the park; the park has a bound, and
     reaching it aborts the reexec (resume, step 13).

     **Read-ahead is carried, not avoided.** Once parked, the bytes the
     `BufReader` has already taken from the socket but not handed to a frame
     (`BufReader::buffer()`, tokio `src/io/util/buf_reader.rs:87`) are copied
     into the record; the new image prepends them before anything it reads from
     the socket. The alternative, reading module connections unbuffered, puts
     two extra syscalls per frame on every module connection's hot path for
     the whole life of the daemon, to avoid carrying at most one buffer (8 KiB
     by default) per connection once per upgrade. Carrying is cheaper and keeps
     the reader identical for clients and modules.
   - **Writer.** Seal the module's `FrameSink` so later sends fail instead of
     queueing frames nothing will write (a sealed-sink send is counted like a
     dropped late GOODBYE). Then enqueue a flush barrier and wait for the writer
     to acknowledge it. `send_flushed` (`router.rs:391-408`) already provides a
     writer acknowledgment sent only after `BufWriter::flush`
     (`server.rs:732-735`), but it needs a frame; the barrier is the same
     mechanism with no frame. An empty queue is not proof on its own: frames sit
     in the writer's `BufWriter` (`server.rs:685-691`) after they leave the
     queue.
   - **Recovering the fd, in safe code.** The writer task exits after the
     barrier and returns its `WriteHalf` (`BufWriter::into_inner` after the
     flush); `drain_writer` owns it today (`server.rs:373`, `678-695`) and needs
     to return it. The reader returns its `ReadHalf` through
     `BufReader::into_inner` after the buffer is copied. Then
     `ReadHalf::unsplit(write_half)` (tokio `src/io/split.rs:84`, checked first
     with `is_pair_of`) gives back the `tokio::net::TcpStream`;
     `TcpStream::into_std` (tokio `src/net/tcp/stream.rs:257`) deregisters it
     from the reactor; `OwnedFd::from(std::net::TcpStream)` is a safe
     conversion. This needs `handle_connection`, which is generic over the
     stream (`server.rs:294-300`), to know it holds a `TcpStream` on the path
     that parks; production connections are always `TcpStream`
     (`server.rs:150-153`, `199`).

   Module connections that are not fully registered (authenticated, HELLO not
   yet acknowledged) are treated as clients and closed in step 5; step 2
   already refused while a supervised child was unregistered, so this only
   catches unsupervised peers.

8. **Stop the stdout and stderr pumps**, keeping the pipes and the partial
   lines. `pump_lines_into` owns the pipe and a `pending` buffer of bytes that
   are not yet a complete line (`stderr_tail.rs:605-671`); at EOF today it
   emits that buffer as an incomplete line (`665-667`). The pump gets a stop
   signal raced against its (cancel-safe) `read`, and on stop returns the pipe
   and `pending` instead of emitting it. The pipe becomes an `OwnedFd` through
   `into_owned_fd`. The pending bytes and the ring generation go in the record,
   and the new pump starts with them as its `pending` buffer, so a line split
   across the exec comes out whole.

9. **Write the handoff record** (below), atomically, then clear `FD_CLOEXEC` on
   exactly the listed fds. **No child may be spawned from this point until the
   exec returns or succeeds**: a child spawned now would inherit every listener,
   module socket and pipe. Step 2's hold already refuses spawns; this step
   asserts it.
10. **Stamp the terminal journal** with `daemon_reexec_attempted`
    (incarnation, target build, time).
11. **`exec`** the target with the handoff environment (below).

New image, at startup, if `SUBC_REEXEC_FDS` is set:

12. **Validate.** Read the fd list from the environment and the record from the
    file it names. Check the record's schema version, checksum, owner-only mode
    and owner. For each listed fd: it is open, it is the listed kind (socket of
    the right family and type, or FIFO), and for a module socket its peer
    address is the one the record lists for that connection. Re-run the
    preflight fact checks against this build and config. Delete the file.
    Anything that fails sends the affected part (or all of it) to the fallback.
13. **Restore `FD_CLOEXEC` on every adopted fd**, first, before anything else
    runs that could spawn. Rust opens its own fds with `CLOEXEC` by default, so
    after this step no inherited fd can leak into a module the new image spawns.
    Remove `SUBC_REEXEC_FDS` and `SUBC_REEXEC_HANDOFF` from every child's
    environment (`Command::env_remove` on the spawn path), so the new image's
    children never see them.
14. **Adopt the listeners** and publish a new connection file (new key, new
    `daemon_id`), exactly as a fresh boot does; module connections are already
    authenticated and do not need the key. The daemon incarnation is derived from
    the `daemon_id` (`bootstrap.rs:636-639`), so it changes.
15. **Rebuild the supervisor entries** from the record, each child alive at its
    pid, re-admitted to the child roster (so the new image's own shutdown stop
    finds it, `child_roster.rs:1-13`), with its cgroup name. The pids cannot
    have been reused: an unreaped child keeps its pid until its parent reaps it,
    and the parent is this process. Start one reaper per adopted child
    ("Adopting children"). A child that exited after step 3 is a zombie; its
    reaper returns at once and its exit goes through the normal
    `on_child_exit` path, recorded once, with the normal restart policy.
16. **Re-register each module connection through the adoption path** (below),
    with the carried allocator marks, then start its reader with the carried
    read-ahead bytes in front of the socket. Run the capability census once.
17. **Restart the pumps** on the inherited pipes with their pending bytes, and
    restore the stderr rings.
18. **Stamp `daemon_reexec_adopted`** (new incarnation, previous incarnation,
    adopted module ids, modules sent to fallback) and resume. `route.open` works
    for a target as soon as step 16 is done for it.

### The adoption registration path

Normal registration queues HELLO_ACK as the first frame on the sink inside the
critical section that makes the endpoint visible
(`register_module_connection_acked`, `forwarding.rs:612-641`, used by the HELLO
handler at `control.rs:1802`). An adopted module sent its HELLO to the old image
and got its ack; another one would arrive in the middle of its stream, and a
module exits on an unexpected frame. The ack-less
`register_module_connection` (`forwarding.rs:594-610`) is not the answer either:
it shares `register_module_connection_inner`, which resets the allocators
(`forwarding.rs:663-673`) and the bind-relay breaker (`697-706`).

Adoption gets its own entry point that inserts the endpoint with no ack, with
the carried generation, channel, epoch and correlation marks, the carried
negotiated version and concurrency, and the registry entry (manifest,
readiness, capabilities) as declared, and that does not touch the breaker. It is
refused, like every registration, while the forwarding gate is set, so the new
image adopts before it opens admission.

### Exec-failure resume

If `exec` returns, the old image logs the error and, in this order:

- restores `FD_CLOEXEC` on every fd it cleared;
- stamps `daemon_reexec_failed` in the journal;
- unseals the module sinks, restarts each module's writer on its `WriteHalf`
  and its reader on its `BufReader` (which still holds the read-ahead), or, if
  the fd was already recovered, re-registers the `TcpStream` with the reactor
  via `from_std` and puts the carried read-ahead back in front of it;
- restarts the pumps with their pending bytes;
- clears the forwarding reexec gate and every mark the drain left: the
  endpoints in `draining_endpoints` and any `closing_connections` entries for
  module connections (both refuse registration and routing,
  `forwarding.rs:653`). Nothing clears `daemon_draining` today;
  `begin_daemon_drain` only ever sets it (`forwarding.rs:1875-1904`), which is
  one more reason reexec must not use it;
- re-arms the accept loops with the retained listeners;
- releases the supervise-loop holds. Each loop resumes polling `wait()`, which
  reaps any zombie and applies the normal restart policy to it.

Clients were already closed and reconnect as usual. There is no reply to send:
the initiating control connection was a client and was closed at step 5. The
result is in the daemon log, the journal marker, and `ck daemon` (below).

## Adopting children

The new image has no tokio `Child` for an adopted process. It waits by pid:
`rustix::process::waitpid(Some(pid), WaitOptions::empty())` on a blocking
thread, never `waitpid(-1)`, which would steal the exits of children the new
image spawns itself.

Open question 3 is settled from tokio 1.52.3's unix process driver
(`src/process/unix/{mod,orphan,reap,pidfd_reaper}.rs`): it reaps only through
`std::process::Child::try_wait` on children it spawned (`reap.rs:89`, `123`;
`pidfd_reaper.rs:108`, `205`) or on its queue of dropped children
(`orphan.rs:118`). Nothing under `src/process` calls `waitpid`, `waitid` or
`wait` on a wildcard pid. So a per-pid wait on a pid tokio never spawned cannot
race tokio. Conditions:

- Loop on `EINTR`: tokio installs a `SIGCHLD` handler once the new image spawns
  anything, and the signal can interrupt the blocking wait.
- `SIGCHLD` must not be `SIG_IGN` in the new image. `execve` resets caught
  signals to the default but keeps ignored ones ignored, and with `SIGCHLD`
  ignored the kernel reaps children itself and `waitpid` fails with `ECHILD`.
  The old image has a handler installed (tokio installs one on its first
  spawn), so this holds in practice; the new image checks it at step 12 and
  falls back if it does not.
- A zombie from step 3 onward is reaped immediately by its reaper (above).
- `ECHILD` from the reaper means something else reaped the pid; the exit is
  recorded with an unknown status rather than ignored.

One blocking thread per adopted child (19 on this host) is acceptable; a pidfd
(Linux) or `EVFILT_PROC` (macOS) reaper is an optimization for later.

The adopted child's supervisor handle is a variant of `SupervisedChild`
(`supervise.rs:113-133`) whose `wait` is the pid reaper and whose `start_kill`
signals the pid through rustix. The existing `wait` releases the roster entry
and, on Linux, removes the module's cgroup (`supervise.rs:144-158`); the adopted
variant does both.

**Cgroups (Linux).** Placement is done child-side before exec
(`supervise.rs:5638-5641`; `subc-cgroup/src/lib.rs:80-108`), so an adopted
child is still in its cgroup after the daemon's exec. The daemon's own cgroup
does not change across exec either, so the new image's `prepare_current`
reopens the same `subc-modules` subtree (`subc-cgroup/src/lib.rs:53-78`,
tolerating `AlreadyExists`). What the record must carry is which name the child
lives under: a swap candidate takes the alternate `_swap` name and keeps it after
cutover (`supervise_swap.rs:31-59`), so the module id alone does not say which
directory to remove at reap.

## The handoff record and environment

Two channels, split by secrecy:

- **Environment (not secret):** `SUBC_REEXEC_FDS` lists every inherited fd with
  its number and kind (`listener`, `module:<connection id>`,
  `stdout:<module id>`, `stderr:<module id>`), and `SUBC_REEXEC_HANDOFF` names
  the record file. Fd numbers are visible in `/proc` and `lsof` anyway; putting
  them here means the new image can find and close every inherited fd even when
  the record is unreadable.
- **Record file (secret):** a versioned JSON document in a 0600 file under the
  run directory (already 0700), created exclusively (`O_EXCL`, no symlink
  following) as a temp file and renamed into place, with a checksum. It holds
  the launch-nonce tables, which are connection-key grade, which is why they
  are not in the environment (readable through `ps eww` and `/proc`). Contents:
  schema version, old incarnation and build, the preflighted target's file
  identity, per-module supervisor state, per-connection registration state,
  HELLO_ACK facts, read-ahead bytes and allocator marks, per-child pump pending
  bytes and ring generations, global allocator marks, stderr ring snapshots.
  The fd numbers are repeated there and must match the environment.

Every fresh boot deletes stale handoff files in the run directory: a new image
that died before step 12 leaves nonces on disk.

**Version skew is the main risk.** Rules:
- The old image asks the target what it reads (`--reexec-preflight`) and
  refuses if its own schema, protocol version or HELLO_ACK facts are outside it
  (step 1).
- The new image reads its own schema version and the previous one. Anything
  else it refuses.

**Fallback.** A refusal after the exec (record unreadable or failing
validation, fd missing or of the wrong kind, schema unknown, `SIGCHLD`
ignored) falls back to today's behaviour, from inside the new image and before
its runtime starts anything:
- Take the fd list from `SUBC_REEXEC_FDS`, and close every inherited fd at or
  above 3 that the new image did not open itself. The environment list is the
  primary source; enumerating open descriptors (`/proc/self/fd`, `/dev/fd`)
  catches a corrupt list. Closing a module socket gives that module EOF, so
  subc modules run their own teardown.
- Take the pids from the **live-children record** (next section), which is a
  separate file from the handoff so it survives an unreadable handoff. Those
  processes are still this process's children, so the fallback waits for them
  by pid with their drain deadlines, signals stragglers, and reaps what it
  signals.
- Then boot fresh, including the orphan sweep (which by now finds nothing
  alive of its own).

A module whose part of the record fails (fd of the wrong kind, peer mismatch,
facts changed) falls back alone: its connection is closed and its process is
stopped and respawned by the normal restart path. So the worst case of a failed
in-place upgrade is today's normal case, never an orphaned fleet.

## Crash windows and orphans

- **Old image dies between step 2 and exec** (killed by an operator, OOM): the
  children see EOF on their module connection and exit, except
  `protocol: "none"` children and modules that ignore EOF (thalamus and
  condition-runner today). Since modules lead their own process groups
  (`supervise.rs:5631-5648`), those survive the daemon and become orphans.
  **This gap exists today** for any daemon crash, independent of this design.
- **New image dies after exec, before adoption finishes**: same shape; the
  inherited sockets close with the process, subc modules exit on EOF, the rest
  are orphaned.

So the first slice is an **orphan sweep** that also closes today's gap:

- The daemon keeps a **live-children record** in the run directory: one entry
  per roster entry (`child_roster.rs:32-42`: module id, pid, protocol, process
  start time) plus the executable identity and the cgroup name. It is a
  separate file from the handoff record and is rewritten atomically (temp file
  and rename) on every roster admit and release.
- At boot, before spawning anything, the daemon signals every recorded process
  that still matches on **pid, process start time and executable identity**
  (SIGTERM, then SIGKILL at a bound), so a crashed daemon's leftovers can never
  run beside fresh copies of the same modules.
- **The sweep skips every pid claimed by a valid handoff.** On reexec the live
  record lists exactly the children the new image is adopting, with matching
  start times; without the skip the sweep would kill them. The sweep therefore
  runs after step 12 has decided which pids are adopted, and only on the rest.
- **Executable identity.** The daemon records `spawned_file_identity` (device
  and inode of the spawned path, `provenance.rs:21-45`) at spawn
  (`supervise.rs:5666`). The sweep compares it with the identity of the image
  the live pid is running: on Linux, `stat` through `/proc/<pid>/exe`, which
  still resolves if the file was replaced since. With pid and start time this
  closes the remaining mis-kill case (a reused pid within one start-time tick).
  Identity check and signal are still two operations; on Linux a pidfd opened
  before the checks and signalled through (`pidfd_send_signal`) closes that
  race too.
- **macOS has neither half today.** `process_start_time` returns `None` off
  Linux (`provenance.rs:159-162`), and the macOS running-image probe compares
  only the spawned path's inode then and now, not the running process
  (`provenance.rs:100-109`). A pid alone is not safe to signal across a daemon
  crash (the orphan can be reaped by launchd and the pid reused). Until macOS
  has a per-pid start time and executable source, the sweep there logs matches
  and does not signal. Candidate sources are listed under "Unsettled".
- **Spawn-to-record window.** A child exists before the daemon can record it:
  its pid and start time are only known after `spawn` returns, and roster
  admission happens then (`supervise.rs:5650-5679`). A daemon crash inside that
  window leaves one unrecorded child per concurrently spawning module. A subc
  child in that window has not connected yet, so it fails to connect and exits;
  a `protocol: "none"` child can be orphaned. This is the residual gap, and it
  is small (the window is the time between `spawn` returning and a file
  rename).

## What each party sees

- **Modules (subc protocol):** route GOODBYEs for their provider routes and
  `route.closed` for routes they consumed, then new `route.bind`s. Same process,
  same memory, same connection, no HELLO, no HELLO_ACK, no `module.draining`.
  No module change is needed. A route GOODBYE lost under backpressure leaves
  the module holding that route until an orphan-route GOODBYE reaches it (see
  "Stated losses").
- **`protocol: "none"` modules (nats-server):** nothing at all.
- **Clients:** their connection closes after `route.closed` with reason
  `Restart`, they reconnect and reopen routes. Same as a cut today, but it takes
  as long as the drain plus the exec (sub-second when idle), not a fleet restart.
- **Spawn-event subscribers:** the daemon incarnation changes, so a held
  `supervisor.spawn_subscribe` stream ends and a resume cursor from the old
  incarnation is refused with `spawn_cursor_incarnation_mismatch`
  (`subc-control/src/lib.rs:190-205`). The subscriber re-reads
  `supervisor.spawn_snapshot`, which lists the adopted processes with their
  unchanged pids and `spawn_generation`. A subscriber that reconciles by
  (module, pid, spawn_generation) sees no exit and revokes nothing; one that
  treats an incarnation change as "every process restarted" would revoke
  everything, wrongly. There is no consumer of the stream in this tree today
  (`crates/ck-bus/src` has none), so nothing breaks now; the contract must say
  this before one is written, which is why the note moves ahead of adoption in
  the slices.
- **Terminal history:** no exit records for adopted modules. The journal gets
  `daemon_reexec_attempted` from the old image and `daemon_reexec_adopted` (or
  `daemon_reexec_fallback`) from the new one, or `daemon_reexec_failed` if the
  exec returned. They are new variants of the journal's marker enum
  (`terminal_journal.rs:23-38`), skipped by history reads like
  `daemon_shutdown` (`terminal_journal.rs:243-245`). An older daemon reading
  the journal after a downgrade counts them as skipped lines (`246`), which is
  harmless.
- **`ck module status`:** restart counts and `spawn_generation` unchanged;
  "started" times unchanged.

## TypeScript provider modules

The TS provider SDK reconnects after an unexpected drop, retrying transient
failures indefinitely (`clients/subc-client/src/provider.ts:1068-1104`,
`1106-1150`), and re-registers with the launch nonce from its environment
(`provider.ts:1303`). That suits an unsupervised plugin. This design assumes a
supervised TS provider's connection is carried like any other module's, so the
reconnect never triggers on a successful upgrade.

It does trigger in the fallback, and in today's cut: the daemon closes the
connection, the provider reconnects to the new daemon instead of exiting, and
presents a nonce the new daemon does not know. A reserved id is refused; a
non-reserved id can register as an unsupervised duplicate beside the fresh
process the supervisor spawns, until the fallback's straggler signal ends it.
Recommendation, outside this design's changes: when the provider runs under
supervision (it was given `SUBC_LAUNCH_NONCE`), it should exit on an
unexpected drop instead of reconnecting, as the Rust SDK does.

## Stated losses

Things the upgrade deliberately does not preserve:

- **Client routes** close with `Restart` and are reopened (non-goal above).
- **Bind-relay breaker state** (`forwarding.rs:507-511`) is not carried. It is
  a verdict about a process that survives, so this costs at most one extra
  full-budget `route.bind` wait per wedged module before the breaker trips again.
- **Late GOODBYEs.** A module GOODBYE that could not be delivered by the
  late-GOODBYE bound (step 6) is lost, and the module keeps that route until it
  sends a frame on it and the new image answers with an orphan-route GOODBYE
  (`router.rs:681-734`), or until its own idle reaper drops it. This is the
  existing accepted-lossy case (`forwarding.rs:95-122`); the carried epoch mark
  keeps it from aliasing a new route.
- **New subc ops or capabilities** in the target build are invisible to
  adopted modules until their next restart (step 1).
- **Late module-control replies** are dropped; **health-probe tombstones** are
  not carried, so a late health answer is not logged as proof of life.
- **The spawn-event ring** and the **in-memory terminal ring** start empty in
  the new incarnation. With a terminal journal configured
  (`bootstrap.rs:682-685`) the old incarnation's exits remain readable; without
  one (embedded and test daemons) they are gone.

## Operator surface

- New control op `supervisor.reexec` (operator-only, like `supervisor.restart`).
  It answers when admitted, after steps 1-2 pass and before step 3 begins, with
  "accepted" or the refusal (`reexec_busy`, preflight failure). It cannot
  answer with the outcome: the calling connection is a client and is closed in
  step 5. The outcome is in the daemon log, the journal marker, and
  `ck daemon`, which shows the last reexec attempt and its result (adopted,
  failed with error, fallback with the modules affected).
- `ck daemon upgrade` places nothing itself; it asks the running daemon to exec
  the binary at its own path, waits for the connection file to carry a new
  `daemon_id`, reconnects, and reports what `ck daemon` shows. If the
  `daemon_id` does not change within a bound, it reconnects to the old daemon
  and reports the failure from there. The placement script and `ck upgrade`
  use it when the running daemon advertises the op, and fall back to the
  service-manager restart (`scripts/fleet/cut-daemon.sh`) otherwise and on
  Windows.
- A daemon started by the service manager after a genuine stop still boots
  fresh, as today.

## Testing

- Fixture daemon with stub modules (Rust `serve`, the TS provider, a
  `protocol: "none"` sleeper): after `supervisor.reexec` onto a copy of the same
  binary, every module pid is unchanged, each subc module answers a request on a
  newly opened route without having re-sent HELLO and without receiving a
  second HELLO_ACK, the none module is still running, and no terminal exit is
  recorded.
- The stub's stderr pipe stays writable across the exec (the stub writes a line
  after the exec and it appears in `ck module stderr`). A line the stub writes
  half before and half after the exec appears once, whole.
- **Read-ahead and half frame:** the stub sends one complete frame and the
  first half of a second one just before the quiesce, and the rest after the
  exec. The new image reads both intact. A variant stalls mid-frame past the
  park bound and asserts the reexec aborts and resumes.
- **Late control reply:** a health probe is outstanding at the quiesce and the
  stub answers it after the exec; the new image drops it, and a health probe the
  new image sends afterwards is answered and matched correctly.
- **Late GOODBYE:** a module with a full egress queue at the drain; the late
  GOODBYE task is joined or aborted at its bound, and nothing is written to the
  module after the flush barrier.
- **Module as consumer:** a module holding a route to another module at the
  quiesce receives `route.closed` and can reopen it after the upgrade.
- **Allocator marks:** after the upgrade, the first route bound to an adopted
  module uses an epoch above any the old image issued on that channel.
- **Swap exclusion:** reexec during a swap candidate's warm-up is refused with
  `reexec_busy`; a swap requested while reexec holds the loops is refused; the
  same for restart.
- **Unregistered spawn:** reexec while a supervised child has spawned but not
  registered is refused.
- **Child exit during the quiesce:** a stub that exits after step 3 is reaped
  by the new image at adoption, recorded once, and restarted once by the
  normal policy; no `daemon_shutdown` disposition anywhere.
- **Failed exec after step 9:** the target is made non-executable after the
  preflight (a test hook between steps 9 and 11). The old image resumes: the
  modules keep their pids and connections and serve new routes, new clients
  connect, registration works (the drain marks are cleared), and the journal
  has `daemon_reexec_attempted` followed by `daemon_reexec_failed`.
- **Refusal paths:** target binary missing, target reporting an incompatible
  handoff version, protocol version, or storage descriptor: nothing changes, the
  old image keeps serving.
- **Post-exec fallback:** a handoff record the new image cannot read ends in the
  current full-restart behaviour, with every child stopped and fresh ones
  spawned, no inherited fd left open, and no orphan left.
- **CLOEXEC:** a module the new image spawns after an upgrade has no fd other
  than its stdio and its own connection.
- **Orphan sweep:** SIGKILL a fixture daemon, start a new one against the same
  run directory, and assert the old `protocol: "none"` child is gone before the
  new one is spawned (Linux). After a successful reexec, assert the sweep
  signalled nothing.
- Linux (CI) and macOS.

## Slices

1. **Orphan sweep**: live-children record (atomic, separate file) and boot-time
   sweep with pid, start time and executable identity; logs-only on macOS until
   it has an identity source. Useful on its own.
2. **Children and pipes across exec.** First, the spawn-stream contract note
   (adopted processes keep pid and `spawn_generation` across an incarnation
   change), since this slice is the first to adopt anything. Then the
   fd-inheritance crate (the unsafe-code decision), the handoff environment and
   record, preflight, exec, per-pid reapers, pumps with pending bytes, cgroup
   names, `protocol: "none"` modules and the fallback. No module connections
   yet: a subc module in this slice is handed over by closing its connection,
   which is today's behaviour for that module.
3a. **Quiesce and resume, without exec.** Supervise-loop holds and the
   refusal gate, the forwarding reexec gate, stopping and re-arming the accept
   loops, client close and join, module-consumer routes, late-GOODBYE tracking,
   RPC settling, sink seal and flush barrier, reader park with `fill_buf`, fd
   recovery, and the full resume path. Testable in-process: quiesce, then
   resume, and assert the fleet is exactly as before. This is also the
   exec-failure path of 3b.
3b. **Carrying module connections.** Record contents for connections, the
   adoption registration path with allocator marks, HELLO_ACK fact checks,
   read-ahead handoff, and the failed-exec test end to end.
4. **Operator surface**: `supervisor.reexec`, `ck daemon` reporting,
   `ck daemon upgrade`, placement and `ck upgrade` integration.

## Possible follow-up: keep routes alive too

With module connections and client connections both carried, and the
forwarding table serialized, even routes would survive. That is where the
remaining disruption (every caller reopening every route) would go away. It is
deliberately left out: the forwarding table is the most concurrency-critical
state in the daemon, and this design already removes the expensive part.

## Open questions, as of r2

1. **Quiescing a module connection** (r1 question 1). Answered: no, not just
   parking the reader. See steps 4-8.
2. **Swap, restart, drain during reexec** (r1 question 2). Answered: refuse,
   serialized through the supervise-loop hold (step 2).
3. **Reaping adopted children** (r1 question 3). Answered: per-pid wait is safe
   against tokio 1.52.3, with the conditions in "Adopting children". A tokio
   upgrade must re-check that its process driver still reaps only its own
   children.
4. **Spawn-stream readers** (r1 question 4). No reader exists in this tree; the
   contract note lands in slice 2, before any adoption ships.

## Unsettled

- **The unsafe exception.** Adopting inherited fd numbers needs
  `OwnedFd::from_raw_fd` (and closing unknown fds needs the same). Options: a
  one-function crate that allows unsafe, or an existing crate that wraps it
  (`listenfd`-style). Recommendation: the one-function crate, so the audited
  surface is ours and tiny.
- **macOS process identity.** The sweep needs a per-pid start time and running
  executable on macOS. Candidates: the `sysctl` `KERN_PROC_PID` entry through a
  safe wrapper crate, `libproc` (`proc_pidinfo`, `proc_pidpath`), or `ps -o
  lstart=,comm=` (one-second resolution, a subprocess per entry). Not chosen
  here.
- **Module-side liveness timers.** The quiesce is only invisible to modules
  that do not time out an idle daemon. The Rust SDK answers pings
  (`subc-client-rs/src/lib.rs:1039`) and a search found no daemon-liveness
  timer in it or in the TS provider, but hand-rolled modules were not checked.
  The quiesce budget (drain plus late-GOODBYE bound plus park) must stay below
  any such timer.
- **The `module.draining` wire contract.** Whether any module relies on
  receiving `module.draining` before every daemon incarnation change was not
  checked; this design stops sending it on reexec.
