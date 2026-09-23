//! Every supervised process this daemon has spawned and not yet reaped, and the
//! daemon's own end-of-life stop for them.
//!
//! Each supervised module runs in its own process group (see
//! `spawn_child_in_slot`). That keeps a service manager's process-group kill
//! from reaching modules before they see EOF on their control connection, but
//! it also means nothing outside this daemon will end a child that outlives it.
//! A `protocol: "none"` child (the NATS server) has no control connection and
//! never sees EOF at all; left alone it would survive the daemon as an orphan
//! and the next daemon would start a second one that fights it for its port.
//! So the daemon ends its own children on the announced-shutdown path, and this
//! roster is how it finds them: the child handles themselves are owned by the
//! per-module supervisor tasks, which keep reaping them while shutdown runs.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use subc_control::ModuleProtocol;

/// One live supervised process.
#[derive(Debug, Clone)]
pub(crate) struct RosterEntry {
    pub(crate) module_id: String,
    pub(crate) pid: u32,
    pub(crate) protocol: ModuleProtocol,
    /// Kernel start time where the platform exposes one, used to refuse a
    /// signal to a different process that has reused a reaped child's pid.
    pub(crate) start_time: Option<u64>,
}

#[derive(Debug, Default)]
struct RosterInner {
    next_key: AtomicU64,
    closed: AtomicBool,
    live: Mutex<HashMap<u64, RosterEntry>>,
}

/// Shared by every clone of one `Supervisor` and every module task it starts.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChildRoster {
    inner: Arc<RosterInner>,
}

/// Holds a child's roster entry. Dropped when the child is reaped, or when its
/// handle is dropped (which kills it), so the roster never outlives the pid.
#[derive(Debug)]
pub(crate) struct RosterGuard {
    inner: Arc<RosterInner>,
    key: u64,
}

impl Drop for RosterGuard {
    fn drop(&mut self) {
        lock(&self.inner.live).remove(&self.key);
    }
}

impl ChildRoster {
    /// True once daemon shutdown has begun. A spawn after this point would
    /// create a child the shutdown stop may already have finished looking for,
    /// so spawning refuses instead (the supervisor would otherwise restart each
    /// module as it exits on EOF).
    pub(crate) fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn admit(&self, entry: RosterEntry) -> RosterGuard {
        let key = self.inner.next_key.fetch_add(1, Ordering::Relaxed);
        lock(&self.inner.live).insert(key, entry);
        RosterGuard {
            inner: Arc::clone(&self.inner),
            key,
        }
    }

    pub(crate) fn live(&self) -> Vec<RosterEntry> {
        lock(&self.inner.live).values().cloned().collect()
    }

    #[cfg(unix)]
    fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(unix)]
pub(crate) use unix_shutdown::end_children_for_daemon_shutdown;

#[cfg(unix)]
mod unix_shutdown {
    use std::{future::Future, time::Duration};

    use rustix::process::{kill_process, Pid, Signal};
    use tokio::time::{sleep, Instant};
    use tracing::{debug, info, warn};

    use super::{ChildRoster, RosterEntry};
    use subc_control::ModuleProtocol;

    /// How long children get to exit on their own once their control
    /// connections are closed (EOF) and every `protocol: "none"` child has had
    /// SIGTERM. A module whose teardown hangs off EOF typically finishes in
    /// ~100 ms; one that needs longer must already survive being cut short
    /// (see docs/designs/daemon-shutdown-handler.md), and this wait sits after
    /// a 500 ms notice budget and a 2 s drain budget, so it is kept to one
    /// second rather than sized for the slowest module.
    const CHILD_EXIT_GRACE: Duration = Duration::from_millis(1000);
    /// After SIGTERM to every child still running, the time before SIGKILL.
    /// Long enough for a signal handler to write a last line and exit, short
    /// enough that the whole stop adds under two seconds.
    const CHILD_TERM_GRACE: Duration = Duration::from_millis(500);
    /// After SIGKILL, how long to wait for the supervisor tasks to reap. SIGKILL
    /// cannot be ignored, so this only covers scheduling; it bounds the wait
    /// even if a reap never lands.
    const CHILD_REAP_BOUND: Duration = Duration::from_millis(250);
    const POLL: Duration = Duration::from_millis(10);

    /// End every supervised child before the daemon exits.
    ///
    /// The caller has already closed every connection, so a subc module has its
    /// EOF and is running its own teardown. Order:
    ///
    /// 1. refuse further spawns, so a module exiting on EOF is not restarted;
    /// 2. SIGTERM every `protocol: "none"` child, which has no connection and so
    ///    no EOF, the same stop the supervisor sends it on restart;
    /// 3. wait up to [`CHILD_EXIT_GRACE`] for the roster to empty;
    /// 4. SIGTERM whatever is left, EOF modules included, and wait up to
    ///    [`CHILD_TERM_GRACE`];
    /// 5. SIGKILL whatever is left and wait up to [`CHILD_REAP_BOUND`].
    ///
    /// `already_escalated` or `escalate` resolving (a second SIGTERM to the
    /// daemon) skips the remaining graces and goes straight to step 5: the
    /// operator has said stop waiting, and leaving children behind would be
    /// the orphan this exists to prevent.
    pub(crate) async fn end_children_for_daemon_shutdown(
        roster: &ChildRoster,
        already_escalated: bool,
        escalate: impl Future<Output = ()>,
    ) {
        roster.close();
        tokio::pin!(escalate);
        let mut escalated = already_escalated;

        if !escalated {
            for entry in roster.live() {
                if entry.protocol == ModuleProtocol::None {
                    signal(&entry, Signal::TERM);
                }
            }
            escalated = !wait_for_empty(roster, CHILD_EXIT_GRACE, &mut escalate).await;
            if !escalated && !roster.live().is_empty() {
                for entry in roster.live() {
                    warn!(
                        module_id = %entry.module_id,
                        pid = entry.pid,
                        "supervised child still running after daemon shutdown grace; sending SIGTERM"
                    );
                    signal(&entry, Signal::TERM);
                }
                escalated = !wait_for_empty(roster, CHILD_TERM_GRACE, &mut escalate).await;
            }
        }
        if escalated {
            info!("second SIGTERM: killing remaining supervised children without further grace");
        }

        let remaining = roster.live();
        if remaining.is_empty() {
            return;
        }
        for entry in &remaining {
            warn!(
                module_id = %entry.module_id,
                pid = entry.pid,
                "supervised child did not exit during daemon shutdown; sending SIGKILL"
            );
            signal(entry, Signal::KILL);
        }
        let deadline = Instant::now() + CHILD_REAP_BOUND;
        while !roster.live().is_empty() && Instant::now() < deadline {
            sleep(POLL).await;
        }
    }

    /// Waits until the roster is empty or `budget` elapses. Returns false only
    /// when `escalate` resolved first.
    async fn wait_for_empty<F: Future<Output = ()>>(
        roster: &ChildRoster,
        budget: Duration,
        escalate: &mut std::pin::Pin<&mut F>,
    ) -> bool {
        let deadline = Instant::now() + budget;
        while !roster.live().is_empty() && Instant::now() < deadline {
            tokio::select! {
                biased;
                _ = escalate.as_mut() => return false,
                _ = sleep(POLL) => {}
            }
        }
        true
    }

    fn signal(entry: &RosterEntry, signal: Signal) {
        // A reaped child's pid can be reused. Where the kernel start time is
        // known, refuse to signal a process that is not the one spawned.
        if let Some(expected) = entry.start_time {
            if crate::provenance::process_start_time(entry.pid) != Some(expected) {
                debug!(
                    module_id = %entry.module_id,
                    pid = entry.pid,
                    "supervised child already gone; not signalling its pid"
                );
                return;
            }
        }
        let Some(pid) = i32::try_from(entry.pid).ok().and_then(Pid::from_raw) else {
            return;
        };
        if let Err(error) = kill_process(pid, signal) {
            debug!(
                module_id = %entry.module_id,
                pid = entry.pid,
                ?signal,
                %error,
                "signal to supervised child failed; it has most likely already exited"
            );
        }
    }
}
