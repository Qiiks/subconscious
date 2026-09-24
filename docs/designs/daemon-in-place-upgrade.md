# Upgrading the daemon without restarting the fleet

Status: design, for review before implementation.

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

Both `subc-daemon` and `subc-core` forbid unsafe code. Everything below is
reachable safely: `std::os::unix::process::CommandExt::exec` performs the
`execve` without forking, and `rustix` (already a dependency) clears
`FD_CLOEXEC` with `fcntl_setfd`.

## What has to cross the exec

| Thing | Why it must survive | How |
|---|---|---|
| Listening sockets | the port in the connection file stays valid; no bind race | inherit the fd |
| Each child's stdout and stderr pipe read ends | if the daemon closes them, the child's next write gets `SIGPIPE`/`EPIPE` and it dies | inherit the fds; the new image restarts the pumps on them |
| Each subc module's TCP connection to the daemon | Rust `serve()` returns on EOF and the module exits (`subc-client-rs` lib.rs `module_loop`: "Clean EOF: the daemon closed the connection. `return Ok(())`"), and hand-rolled modules do the same. Only the TypeScript provider reconnects. Dropping these connections restarts the modules, which is the thing we are removing | inherit the fds, quiesced at a frame boundary (below) |
| Per-module supervisor state | restart budget, `spawn_generation`, pid, spawn facts, launch nonces, last exit, enabled/state | serialized handoff record |
| Registration state of each module connection | manifest, negotiated wire version, readiness, capabilities, endpoint generation | serialized handoff record |
| Stderr ring contents | `ck module stderr` history of the running processes | serialized (bounded, small); the capture files on disk are unaffected |

Not carried: client connections (closed, see below), routes, pending
`route.bind` relays, in-flight requests (drained first, as today), the listener
accept backlog beyond what the kernel keeps.

Launch nonces: a nonce is minted per spawn and checked at HELLO against
`spawn_nonces` / `reserved_nonces` without being consumed
(`supervise.rs` `reserved_hello_rejection`). The new image needs the table so
reserved-id gates and consumer attestation keep working for the adopted
processes. It carries them in the handoff record, which is secret material and
is handled like the connection key (below).

## The sequence

Old image, on `supervisor.reexec` (operator control op):

1. **Preflight.** Resolve the target binary (the daemon's own configured path,
   which the placement has just replaced), check it exists, is executable, and
   answers `--handoff-version` with a version this image can write. Refuse
   otherwise, with nothing changed.
2. **Stop admitting.** Stop accepting new connections (keep the listener fd,
   stop the accept loop). Refuse new `route.open` with the retryable
   `module_reloading`. Stop spawning and restarting: a child that exits from here
   on is recorded and handed over as exited, not respawned (same gate as the
   shutdown flag from #121).
3. **Drain routes, not modules.** Run the existing drain for every endpoint with
   reason `Restart` and the usual bounded budget: `route.closing`, wait for
   in-flight requests, `route.closed`, then release every binding, sending the
   route GOODBYEs to modules so each module holds zero routes. Modules stay
   connected and registered.
4. **Close client connections.** Flush the `route.closed` pushes
   (`send_flushed`) and close every non-module connection. Clients reconnect with
   their normal backoff and will find the new image.
5. **Quiesce module connections at a frame boundary.** Park each module
   connection's reader after a complete frame (its task stops reading; bytes of a
   later frame stay in the kernel buffer), and flush its egress queue until
   empty. Daemon-originated control RPCs to modules (health probes,
   `catalog.update` replies) are completed or abandoned first; a reply that
   arrives after the exec for a correlation id the new image does not know is
   discarded by the normal "no pending request" path.
6. **Stop the stderr and stdout pumps** at a read boundary, keeping the pipe fds.
7. **Write the handoff record** (below) and clear `FD_CLOEXEC` on exactly the
   listed fds.
8. **Stamp the terminal journal** with a `daemon_reexec` marker (daemon
   incarnation and target build), then `exec`.
9. If `exec` returns, it failed: log it, restore `FD_CLOEXEC`, un-park the
   readers and pumps, restart the accept loop and spawning, and answer the
   control op with the error. Clients that were closed simply reconnect.

New image, at startup, if the handoff variable is set:

10. Read and validate the handoff record (version, checksum, owner-only mode,
    the fds it names are open and of the right kind). Delete the file.
11. Adopt the listener fds and publish a new connection file (new key, new
    `daemon_id`), exactly as a fresh boot does; module connections are already
    authenticated and do not need the key.
12. Rebuild the supervisor entries from the record, with each child marked
    alive at its pid. The pids cannot have been reused: an unreaped child keeps
    its pid until its parent reaps it, and the parent is this process.
13. Start one reaper per adopted child. Tokio's `Child` handle did not survive,
    so the new image waits by pid (`rustix::process::waitpid(Some(pid))` on a
    blocking thread; never `waitpid(-1)`, which would steal the exits of
    children the new image spawns itself). Exits are handled by the same
    `on_child_exit` path as any child.
14. Re-register each module connection from the record (registry entry and
    forwarding endpoint with zero routes, readiness and capabilities as
    declared), then start its reader. Run the capability census once.
15. Restart the pumps on the inherited pipes, and restore the stderr rings.
16. Resume normal service. `route.open` works again as soon as step 14 is done
    for the target.

## The handoff record

A versioned JSON document in a 0600 file under the run directory (already
0700), path passed in `SUBC_REEXEC_HANDOFF`, deleted by the new image on read.
It holds the connection-key-grade secrets (launch nonces), which is why it is a
file with the same permissions as the connection file and not an environment
variable (environment is readable via `ps eww`/`/proc`). Contents: schema
version, old daemon incarnation and build, per-module supervisor state, per-
module-connection registration state and fd number, listener fd numbers, pipe
fd numbers per child, stderr ring snapshots.

**Version skew is the main risk.** The new image must read the old image's
record. Rules:
- The old image asks the target for the range it reads
  (`--handoff-version`) and refuses to exec if its own version is outside it.
- The new image reads its own version and the previous one. Anything else it
  refuses.
- A refusal after exec (record unreadable, fd missing, version unknown) falls
  back to today's behaviour: close every inherited module connection (EOF, so
  the modules run their own teardown), wait for the adopted children with their
  drain deadlines, signal stragglers, then boot fresh. This is exactly the
  current cut, reached from inside the new image. So the worst case of a failed
  in-place upgrade is today's normal case, never an orphaned fleet.

## Crash windows and orphans

- **Old image dies between step 2 and exec** (killed by an operator, OOM): the
  children see EOF on their module connection and exit, except
  `protocol: "none"` children and modules that ignore EOF (thalamus and
  condition-runner today). Since modules lead their own process groups, those
  survive the daemon and become orphans. **This gap exists today** for any
  daemon crash, independent of this design.
- **New image dies after exec, before adoption finishes**: same shape; the
  inherited sockets close with the process, subc modules exit on EOF, the rest
  are orphaned.

So this design needs, as its first slice, an **orphan sweep** that also closes
today's gap: the daemon keeps a small live-children record (pid, process start
time, module id) in the run directory, updated on spawn and reap. At boot, before
spawning anything, it signals every recorded process whose pid and start time
still match (SIGTERM, then SIGKILL at a bound), so a crashed daemon's leftovers
can never run beside the new daemon's fresh copies of the same modules.

## What each party sees

- **Modules (subc protocol):** nothing but route GOODBYEs for their routes (they
  hold zero routes afterwards) and then new `route.bind`s. Same process, same
  memory, same connection, no HELLO. No module change is needed.
- **`protocol: "none"` modules (nats-server):** nothing at all.
- **Clients:** their connection closes after `route.closed` with reason
  `Restart`, they reconnect and reopen routes. Same as a cut today, but it takes
  as long as the drain plus the exec (sub-second when idle), not a fleet restart.
- **Spawn-event subscribers (ck-bus):** the daemon incarnation changes, so a
  held `supervisor.spawn_subscribe` stream ends and a resume cursor from the old
  incarnation is refused with `spawn_cursor_incarnation_mismatch`, as designed.
  The subscriber re-reads `supervisor.spawn_snapshot`, which lists the adopted
  processes with their unchanged pids and `spawn_generation`. A subscriber that
  reconciles by (module, pid, spawn_generation) sees no exit and revokes
  nothing. This needs to be stated in the spawn-stream contract before ck-bus
  consumes it.
- **Terminal history:** no exit records for the modules. One `daemon_reexec`
  marker in the journal.
- **`ck module status`:** restart counts and `spawn_generation` unchanged;
  "started" times unchanged.

## Operator surface

- New control op `supervisor.reexec` (operator-only, like `supervisor.restart`),
  answered at initiation like `supervisor.restart`, with the result visible in
  the daemon log and `ck daemon`.
- `ck daemon upgrade` places nothing itself; it asks the running daemon to exec
  the binary at its own path. The placement script and `ck upgrade` use it when
  the running daemon advertises the op, and fall back to the service-manager
  restart (`scripts/fleet/cut-daemon.sh`) otherwise and on Windows.
- A daemon started by the service manager after a genuine stop still boots
  fresh, as today.

## Testing

- Fixture daemon with stub modules (Rust `serve`, the TS provider, a
  `protocol: "none"` sleeper): after `supervisor.reexec` onto a copy of the same
  binary, every module pid is unchanged, each subc module answers a request on a
  newly opened route without having re-sent HELLO, the none module is still
  running, and no terminal exit is recorded.
- The stub's stderr pipe stays writable across the exec (the stub writes a line
  after the exec and it appears in `ck module stderr`).
- A frame the module sends while the daemon is quiesced is read by the new
  image intact.
- A child that exits during the quiesce is recorded once, not respawned by
  either image, and not adopted.
- Refusal paths: target binary missing, and target reporting an incompatible
  handoff version: nothing changes, the old image keeps serving.
- Post-exec fallback: a handoff record the new image cannot read ends in the
  current full-restart behaviour, with every child stopped and fresh ones
  spawned, and no orphan left.
- Orphan sweep: SIGKILL a fixture daemon, start a new one against the same run
  directory, and assert the old `protocol: "none"` child is gone before the new
  one is spawned.
- Linux (CI) and macOS.

## Slices

1. Orphan sweep (live-children record and boot-time sweep). Useful on its own.
2. Handoff record, fd inheritance and exec for children and pipes, with
   `protocol: "none"` modules and the fallback path. No module connections yet:
   a subc module in this slice is handed over by closing its connection, which
   is today's behaviour for that module.
3. Carrying module connections: quiesce, record, re-register, resume.
4. `supervisor.reexec`, `ck daemon upgrade`, placement and `ck upgrade`
   integration, and the spawn-stream contract note for ck-bus.

## Possible follow-up: keep routes alive too

With module connections and client connections both carried, and the
forwarding table serialized, even routes would survive. That is where the
remaining disruption (every caller reopening every route) would go away. It is
deliberately left out: the forwarding table is the most concurrency-critical
state in the daemon, and this design already removes the expensive part.

## Open questions for review

1. Is quiescing a module connection at a frame boundary as simple as parking its
   reader task between frames, or are there frames in flight inside the daemon
   (router, pending relay tables) that make "zero routes, no pending control
   RPCs" hard to reach?
2. Should the old image refuse to exec while any module is mid-swap, mid-restart
   or draining, or can those states be carried?
3. Is one blocking thread per adopted child acceptable (19 on this host), or
   should Linux use `pidfd` and macOS `kqueue` `EVFILT_PROC` through a safe
   wrapper?
4. Does the spawn-stream contract change above (same pids and generations, new
   incarnation) break any reader that assumes an incarnation change means every
   process restarted?
