# subc lease contract — OMP-owned last-exit retirement

Status: v1 — implemented in `crates/subc-core/src/holder_monitor.rs` (PR #103),
verified by 6 unit tests + a two-holder forced-close regression.

Owner: subc. Applies only to Windows daemons started with `OMP_SUBC_OWNED=1`
by an OMP host. Standalone daemons, launchd/systemd services, and test daemons
are unaffected: the monitor does not spawn at all (see §6).

## 1. The problem this exists to solve

The OMP host starts the daemon on first use and expects it to retire when the
**last** OMP host exits. The extension's own retirement path handled graceful
`session_shutdown` but a force-killed host never ran it, so the daemon, its
modules, and their grandchildren leaked indefinitely. Retirement moved to the
daemon side, where it survives a host that dies without notice.

## 2. The files and their contract

All paths are in the daemon's run dir (`XDG_RUNTIME_DIR` for the OMP case):

| File | Writer | Reader | Purpose |
|---|---|---|---|
| `subc-lease-<pid>.json` | OMP host (before it starts/stops the daemon) | monitor | one per live holder; presence ⇒ a holder claims this daemon |
| `subc-retiring.lock` | monitor | monitor + every host | serializes retirement against registration |
| `subc-connection.json` | daemon | every consumer | discovery; removed on retirement so no consumer adopts a dead daemon |

Lease content (JSON; `Owner` in `holder_monitor.rs`):

```jsonc
{
  "pid": 1234,                     // the holder's pid, must be non-zero
  "token": "<uuid>",               // opaque; empty token is never dead
  "processIdentity": "2026-09-18T...Z",  // Win32_Process.CreationDate, UTC ISO
  "timestamp": 1780000000000       // informational; not part of liveness
}
```

The lock file is the same shape with a different semantic: `token` is the
**daemon's** token (`"<daemon-pid>-<daemon-identity>"`), and only the daemon
that wrote it may remove it.

`processIdentity` is the whole reason liveness is not just "is the pid alive".
Windows reuses pids. A lease for a dead holder whose pid gets recycled by an
unrelated process would otherwise look live forever. Identity comparison
distinguishes "the recorded process is still running" from "some process with
that number exists".

## 3. Liveness classification (fail-closed)

`process_state(pid)` probes via PowerShell `Get-CimInstance Win32_Process`:

- emits `GONE` (no such process) ⇒ **dead**
- emits a UTC ISO creation date ⇒ **live**, compared by `owner_gone` against
  the recorded identity; mismatch ⇒ dead (pid recycled), match ⇒ live
- timeout, non-zero exit, unparseable output, missing/empty identity, pid 0,
  empty token, or any probe error ⇒ **live** (fail-closed)

The fail-closed rule is the invariant that must survive any refactor. An
unprobeable holder is a live holder; a premature retirement of a live fleet is
the failure mode this contract exists to prevent, and every error path reads
that way on purpose.

Off-Windows the probe is `Unknown` by construction (see §6), so a Linux/macOS
OMP-owned daemon never retires — stay-up rather than retire-on-guess.

## 4. Retirement choreography (the load-bearing ordering)

```
probe unlocked            ── read lease fingerprints, classify each holder
   ↓ all gone?
take boundary lock        ── reaps a stranded lock inside take_lock
   ↓                      (read → probe → re-read → remove → create_new)
re-verify leases          ── unchanged since the probe? else abort
   ↓
operation lock            ── supervisor-level admission
   ↓
re-verify leases          ── unchanged since the boundary lock? else abort
   ↓
drain + retire trees      ── GOODBYE/drain FIRST, then taskkill /T /F
   ↓
remove connection file    ── discovery ends; consumers stop adopting
   ↓
exit                      ── both locks held until bootstrap is leaving
```

Three properties this ordering buys:

1. **Probes never hold the boundary lock.** Probing is one CIM query per lease
   (5s deadline each). Doing that under the lock would block every host's
   registration (`wx`, 250ms retry, 20s hard throw) for K×5s per tick on a
   K-holder fleet — a host failure on a healthy fleet under slow WMI.
2. **Two re-verifies close the registration race.** A holder that registers
   between the probe and either lock changes a lease's bytes; the fingerprint
   comparison aborts retirement. The comparison is exact bytes, so a lease
   rewritten with a fresh `timestamp` also aborts — conservative.
3. **Drain precedes the tree kill.** Modules hang graceful-stop work off the
   GOODBYE the drain delivers (broca seals its WAL, engram closes a capture).
   Killing the tree first makes that delivery a no-op against a dead process
   and can kill a module mid-write.

The monitor never kills a pid sourced from a lease. It kills only module
processes from `SupervisorHandle::list()`, via `taskkill /T /F` on the module
parent. Descendants that detach before the kill are the reason `/T` exists.

## 5. Stranded lock recovery

A daemon that dies mid-retirement leaves `subc-retiring.lock` behind, which
blocks every host forever. `take_lock` reaps it before retirement only:

1. read the lock; if absent, no work
2. probe the recorded owner; if **live and identity matches**, leave it alone
   (someone else is legitimately retiring)
3. re-read; if the bytes changed meanwhile, someone re-took it, leave it
4. remove, then `create_new` — the reaper is now the lock owner

Too-fresh locks (< 30s) are left alone, and any probe ambiguity fails closed
to "leave it". Reaping happens only when the monitor is about to retire anyway,
never on a routine tick, so a live fleet never sees its lock touched.

## 6. Platform and opt-in gating

The module is `#[cfg(windows)]`. `spawn_if_owned` additionally requires
`OMP_SUBC_OWNED=1` and `OMP_SUBC_STOP_ON_LAST_EXIT != "0"`. Absent env ⇒ zero
behavior change: standalone daemons, launchd/systemd services, test daemons,
and every non-Windows build compile the module out and behave exactly as before.

The extension's own stop path was **removed** in the same change. Two
retirement paths would race; the daemon monitor is now the single authority.
Hosts release their own lease on `session_shutdown` (unconditionally, last
holder only) and nothing else — pruning another host's lease is a liability,
since one host's shutdown could make the monitor retire a daemon a second live
host still needs.

## 7. Verification

- 6 unit tests (`holder_monitor::tests` + `loop_tests`): classification
  fail-closed; and the loop choreography with an injected probe — a live
  holder blocks, all-gone retires and removes the discovery file, a holder
  registering after the probe aborts, an unprobeable holder stays up, empty
  leases retire without deadlock.
- Two-holder regression (`subc-forced-close-check.mjs`): first-host kill
  preserves the second holder and the service tree; last-host force-kill
  retires daemon + module + grandchild; an abandoned registrar lock is
  recovered.
- Graceful sequence (`subc-real-lifecycle.mjs`): cold start → shared →
  first-exit preserve → last-exit retire → restart.

## 8. Honest limitations (v1)

- **Probe cost.** One PowerShell CIM query per lease per 2s tick. Under slow
  WMI this is real overhead; the fix is a native `OpenProcess` +
  `GetProcessTimes` probe via the `windows` crate (no new behavior, just a
  faster path to the same classification). Not a correctness issue; the
  boundary lock is never held across it (§4.1).
- **Fingerprint comparison is exact bytes.** A lease rewritten with a fresh
  `timestamp` aborts retirement that tick. No current writer does this; if
  spurious "leases changed during probe" warnings appear, a timestamp churn is
  the first suspect, not a real holder.
- **No cross-machine coordination.** The lease set is per-run-dir. Two daemons
  on the same machine with different run dirs are independent fleets and
  neither knows about the other's holders.
