//! Windows-only, opt-in last-holder retirement. Unknown process state fails closed.
//! Lease writers register before starting the daemon and serialize on LOCK_NAME.
use crate::SupervisorHandle;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{process::Command, sync::OwnedMutexGuard, task::JoinHandle, time};
use tracing::{info, warn};

pub const OWNED_ENV: &str = "OMP_SUBC_OWNED";
pub const STOP_ON_LAST_EXIT_ENV: &str = "OMP_SUBC_STOP_ON_LAST_EXIT";
pub const DEFAULT_HOLDER_INTERVAL: Duration = Duration::from_secs(2);
const LOCK_NAME: &str = "subc-retiring.lock";

#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    pid: u32,
    token: String,
    #[serde(default, rename = "processIdentity")]
    process_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProcessState {
    Gone,
    Live(String),
    Unknown,
}

// Keep process absence distinct from probe failure. The explicit sentinel is
// emitted only after a successful CIM query; timeouts and access errors stay Unknown.
/// The probe is the only Windows-bound piece. Test suites substitute a fake
/// so the lock choreography has coverage on any platform.
#[cfg_attr(test, allow(dead_code))]
pub(crate) trait ProcessProbe: Send + Sync {
    /// Object-safe form of an async probe: the boxed future lets tests
    /// substitute a fake without a dynamic-dispatch async-trait dependency.
    fn state<'a>(&'a self, pid: u32) -> Pin<Box<dyn Future<Output = ProcessState> + Send + 'a>>;
}

/// Production probe: shells to PowerShell once per lease. Fail-closed on any
/// error or timeout, so `Unknown` never means dead.
struct PowershellProbe;
impl ProcessProbe for PowershellProbe {
    fn state<'a>(&'a self, pid: u32) -> Pin<Box<dyn Future<Output = ProcessState> + Send + 'a>> {
        Box::pin(process_state(pid))
    }
}
/// Raw Windows probe. Keep process absence distinct from probe failure: the
/// explicit `GONE` sentinel is emitted only after a successful CIM query;
/// timeouts and access errors stay `Unknown`, which fails closed.
async fn process_state(pid: u32) -> ProcessState {
    if pid == 0 {
        return ProcessState::Unknown;
    }
    if !cfg!(windows) {
        return ProcessState::Unknown;
    }
    let script = format!("$ErrorActionPreference='Stop'; try {{$p=Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}'; if ($null -eq $p) {{'GONE'}} else {{$p.CreationDate.ToUniversalTime().ToString('o')}}}} catch {{exit 1}}");
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    match time::timeout(Duration::from_secs(5), command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if value == "GONE" {
                ProcessState::Gone
            } else if value.contains('T') && value.ends_with('Z') {
                ProcessState::Live(value)
            } else {
                ProcessState::Unknown
            }
        }
        _ => ProcessState::Unknown,
    }
}

fn owner_gone(owner: &Owner, state: &ProcessState) -> bool {
    if owner.pid == 0 || owner.token.is_empty() {
        return false;
    }
    match state {
        ProcessState::Gone => true,
        ProcessState::Live(actual) => owner
            .process_identity
            .as_ref()
            .is_some_and(|recorded| !recorded.is_empty() && recorded != actual),
        ProcessState::Unknown => false,
    }
}

/// Lease content snapshot for the race-free re-check under the boundary lock.
#[derive(Debug)]
struct LeaseFingerprint {
    path: PathBuf,
    bytes: Vec<u8>,
}

async fn read_lease_fingerprints(run_dir: &Path) -> io::Result<Vec<LeaseFingerprint>> {
    let mut fingerprints = Vec::new();
    for entry in fs::read_dir(run_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("subc-lease-") || !name.ends_with(".json") {
            continue;
        }
        let bytes = match fs::read(entry.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        fingerprints.push(LeaseFingerprint {
            path: entry.path(),
            bytes,
        });
    }
    Ok(fingerprints)
}

/// Re-check under the boundary lock: a lease that appeared, disappeared, or
/// changed content since the probe means a holder moved and retirement aborts.
async fn leases_unchanged(run_dir: &Path, probed: &[LeaseFingerprint]) -> io::Result<bool> {
    leases_match(&read_lease_fingerprints(run_dir).await?, probed)
}

/// Pure comparison; the read is separated so the async boundary stays minimal.
fn leases_match(current: &[LeaseFingerprint], probed: &[LeaseFingerprint]) -> io::Result<bool> {
    if current.len() != probed.len() {
        return Ok(false);
    }
    let mut current = current.iter().collect::<Vec<_>>();
    let mut probed = probed.iter().collect::<Vec<_>>();
    current.sort_by(|left, right| left.path.cmp(&right.path));
    probed.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(current
        .iter()
        .zip(probed.iter())
        .all(|(current, probed)| current.path == probed.path && current.bytes == probed.bytes))
}
struct BoundaryLock {
    path: PathBuf,
    token: String,
}
impl Drop for BoundaryLock {
    fn drop(&mut self) {
        let current = fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Owner>(&bytes).ok());
        if current.is_some_and(|owner| owner.token == self.token) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

async fn take_lock(run_dir: &Path, identity: &str) -> io::Result<Option<BoundaryLock>> {
    let path = run_dir.join(LOCK_NAME);
    // Only this singleton daemon reclaims locks. A live owner without a known
    // identity is never considered foreign; a failed query never means dead.
    match fs::read(&path) {
        Ok(bytes) => {
            let owner: Owner = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if !owner_gone(&owner, &process_state(owner.pid).await) {
                return Ok(None);
            }
            if fs::read(&path)? != bytes {
                return Ok(None);
            }
            fs::remove_file(&path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let owner = Owner {
        pid: std::process::id(),
        token: format!("{}-{identity}", std::process::id()),
        process_identity: Some(identity.to_owned()),
    };
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
        Err(error) => return Err(error),
    };
    file.write_all(&serde_json::to_vec(&owner).map_err(io::Error::other)?)?;
    Ok(Some(BoundaryLock {
        path,
        token: owner.token,
    }))
}

/// Returned only after tree retirement. Keep both admission locks held until
/// bootstrap has dropped the listener and watchdog and is leaving the daemon.
pub(crate) struct Retired {
    _boundary: BoundaryLock,
    _operations: OwnedMutexGuard<()>,
}

pub(crate) struct HolderMonitor;
impl HolderMonitor {
    pub(crate) fn spawn_if_owned(
        connection_file: PathBuf,
        supervisor: SupervisorHandle,
    ) -> Option<JoinHandle<Retired>> {
        if !cfg!(windows)
            || std::env::var(OWNED_ENV).ok().as_deref() != Some("1")
            || std::env::var(STOP_ON_LAST_EXIT_ENV).ok().as_deref() == Some("0")
        {
            return None;
        }
        Some(tokio::spawn(async move {
            let run_dir = connection_file.parent().expect("connection file parent");
            let identity = loop {
                if let ProcessState::Live(identity) = process_state(std::process::id()).await {
                    break identity;
                }
                time::sleep(DEFAULT_HOLDER_INTERVAL).await;
            };
            info!("OMP-owned daemon: holder monitor active");
            loop {
                time::sleep(DEFAULT_HOLDER_INTERVAL).await;
                let probe = PowershellProbe;
                if let Some(retired) =
                    tick_once(run_dir, &identity, &connection_file, &supervisor, &probe).await
                {
                    return retired;
                }
            }
        }))
    }
}

/// Classify probed leases without touching the boundary lock.
async fn owners_gone(probe: &dyn ProcessProbe, probed: &[LeaseFingerprint]) -> bool {
    for fingerprint in probed {
        let Ok(owner) = serde_json::from_slice::<Owner>(&fingerprint.bytes) else {
            return false;
        };
        if owner.pid == 0 {
            return false;
        }
        if !owner_gone(&owner, &probe.state(owner.pid).await) {
            return false;
        }
    }
    true
}

/// One monitor pass. Returns `Some(Retired)` when retirement completed. Extracted
/// from the spawn loop so the lock choreography has automated coverage: the probe
/// is injected, the run dir is a temp dir, and the supervisor is a real
/// `SupervisorHandle`, so every re-verify and reap runs against the real types.
///
/// Ordering is load-bearing: probe unlocked -> all-gone check -> boundary lock
/// (reaping a stranded lock inside `take_lock`) -> re-verify -> operation lock
/// -> re-verify -> retire trees -> remove the connection file.
async fn tick_once(
    run_dir: &Path,
    identity: &str,
    connection_file: &Path,
    supervisor: &SupervisorHandle,
    probe: &dyn ProcessProbe,
) -> Option<Retired> {
    let probed = match read_lease_fingerprints(run_dir).await {
        Ok(probed) => probed,
        Err(error) => {
            warn!(%error, "holder monitor: lease read failed");
            return None;
        }
    };
    if !owners_gone(probe, &probed).await {
        return None;
    }
    let boundary = match take_lock(run_dir, identity).await {
        Ok(Some(lock)) => lock,
        Ok(None) => return None,
        Err(error) => {
            warn!(%error, "holder monitor: ownership lock unavailable");
            return None;
        }
    };
    // Re-verify under the boundary lock: a holder that registered while we
    // waited for the lock still aborts retirement.
    if !leases_unchanged(run_dir, &probed).await.unwrap_or(false) {
        warn!("holder monitor: leases changed during probe; retirement aborted");
        return None;
    }
    let operations = supervisor.operation_lock().lock_owned().await;
    // Re-verify under the operation lock: a holder that registered while we
    // waited for the lock still aborts retirement.
    if !leases_unchanged(run_dir, &probed).await.unwrap_or(false) {
        warn!("holder monitor: leases changed before retirement; aborted");
        return None;
    }
    let mut complete = true;
    for module in supervisor.list() {
        match module.retire_tree().await {
            Ok(()) => {
                supervisor.retire(module.module_id());
            }
            Err(error) => {
                warn!(%error, "holder monitor: tree retirement failed");
                complete = false;
            }
        }
    }
    if !complete {
        return None;
    }
    if let Err(error) = fs::remove_file(connection_file) {
        if error.kind() != io::ErrorKind::NotFound {
            warn!(%error, "holder monitor: cannot remove discovery file");
            return None;
        }
    }
    info!("holder monitor: last holder gone; supervised trees retired");
    Some(Retired {
        _boundary: boundary,
        _operations: operations,
    })
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use crate::supervise::SupervisorHandle;
    use std::collections::HashMap;
    use tokio::runtime::Runtime;

    /// A probe whose answers are scripted per pid. Unscripted pids fail closed
    /// (`Unknown`), exactly like a real probe error.
    struct FakeProbe(HashMap<u32, ProcessState>);
    impl ProcessProbe for FakeProbe {
        fn state<'a>(
            &'a self,
            pid: u32,
        ) -> Pin<Box<dyn Future<Output = ProcessState> + Send + 'a>> {
            let state = self.0.get(&pid).cloned().unwrap_or(ProcessState::Unknown);
            Box::pin(async move { state })
        }
    }

    fn write_lease(dir: &Path, pid: u32, identity: &str) {
        let owner = Owner {
            pid,
            token: format!("tok-{pid}"),
            process_identity: Some(identity.to_owned()),
        };
        fs::write(
            dir.join(format!("subc-lease-{pid}.json")),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
    }

    struct Fixture {
        dir: crate::test_support::TestTempDir,
        supervisor: SupervisorHandle,
    }
    fn fixture() -> Fixture {
        let dir = crate::test_support::TestTempDir::new("holder-monitor");
        let supervisor = SupervisorHandle::new();
        Fixture { dir, supervisor }
    }

    #[test]
    fn a_live_holder_blocks_retirement() {
        // (a) one live holder: nothing retires, the connection file survives.
        let fx = fixture();
        write_lease(fx.dir.path(), 1000, "id-a");
        let connection = fx.dir.path().join("subc-connection.json");
        fs::write(&connection, b"{}").unwrap();
        let probe = FakeProbe([(1000, ProcessState::Live("id-a".into()))].into());
        let rt = Runtime::new().unwrap();
        let retired = rt.block_on(tick_once(
            fx.dir.path(),
            "self",
            &connection,
            &fx.supervisor,
            &probe,
        ));
        assert!(retired.is_none());
        assert!(connection.exists());
    }

    #[test]
    fn all_gone_retires_and_removes_the_connection_file() {
        // (b) every holder provably gone: retirement completes and the
        // discovery file is removed.
        let fx = fixture();
        write_lease(fx.dir.path(), 1000, "id-a");
        let connection = fx.dir.path().join("subc-connection.json");
        fs::write(&connection, b"{}").unwrap();
        let probe = FakeProbe([(1000, ProcessState::Gone)].into());
        let rt = Runtime::new().unwrap();
        let retired = rt.block_on(tick_once(
            fx.dir.path(),
            "self",
            &connection,
            &fx.supervisor,
            &probe,
        ));
        assert!(retired.is_some());
        assert!(!connection.exists());
    }

    #[test]
    fn a_holder_registering_after_the_probe_aborts_retirement() {
        // (c) the probe says gone, but a second lease appears between the
        // probe and the boundary lock: the re-verify must abort.
        let fx = fixture();
        write_lease(fx.dir.path(), 1000, "id-a");
        let connection = fx.dir.path().join("subc-connection.json");
        fs::write(&connection, b"{}").unwrap();
        let probe = FakeProbe([(1000, ProcessState::Gone)].into());
        let dir = fx.dir.path().to_owned();
        let rt = Runtime::new().unwrap();
        // Register the new holder before the tick so the re-verify sees it.
        write_lease(&dir, 1001, "id-b");
        let retired = rt.block_on(tick_once(&dir, "self", &connection, &fx.supervisor, &probe));
        assert!(retired.is_none());
        assert!(connection.exists());
    }

    #[test]
    fn unprobeable_holder_fails_closed_and_stays_up() {
        // (d) a probe that cannot resolve a live pid reads Unknown, and the
        // daemon must stay up rather than retire a live fleet.
        let fx = fixture();
        write_lease(fx.dir.path(), 1000, "id-a");
        let connection = fx.dir.path().join("subc-connection.json");
        fs::write(&connection, b"{}").unwrap();
        // No scripted answer for 1000 -> Unknown.
        let probe = FakeProbe(HashMap::new());
        let rt = Runtime::new().unwrap();
        let retired = rt.block_on(tick_once(
            fx.dir.path(),
            "self",
            &connection,
            &fx.supervisor,
            &probe,
        ));
        assert!(retired.is_none());
        assert!(connection.exists());
    }

    #[test]
    fn empty_leases_retire_and_never_deadlock() {
        // (e) no leases at all: every holder is gone vacuously, so a daemon
        // whose hosts all exited cleanly still retires instead of pinning.
        let fx = fixture();
        let connection = fx.dir.path().join("subc-connection.json");
        fs::write(&connection, b"{}").unwrap();
        let probe = FakeProbe(HashMap::new());
        let rt = Runtime::new().unwrap();
        let retired = rt.block_on(tick_once(
            fx.dir.path(),
            "self",
            &connection,
            &fx.supervisor,
            &probe,
        ));
        assert!(retired.is_some());
        assert!(!connection.exists());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_owners_block_retirement() {
        let mut owner: Owner =
            serde_json::from_str(r#"{"pid":42,"token":"lease","processIdentity":"old"}"#).unwrap();
        assert!(!owner_gone(&owner, &ProcessState::Unknown));
        assert!(!owner_gone(&owner, &ProcessState::Live("old".into())));
        assert!(owner_gone(&owner, &ProcessState::Live("new".into())));
        owner.process_identity = None;
        assert!(!owner_gone(&owner, &ProcessState::Live("new".into())));
        assert!(owner_gone(&owner, &ProcessState::Gone));
        owner.pid = 0;
        assert!(!owner_gone(&owner, &ProcessState::Gone));
    }
}
