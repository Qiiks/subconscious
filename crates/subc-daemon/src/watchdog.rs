use std::{
    collections::BTreeSet,
    fmt,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use subc_control::{ClientControlRequest, ClientControlResponse};
use subc_protocol::{ErrorBody, Flags, FrameType, Priority};
use subc_transport::{
    authenticate_client_with_role, connection_file, ConnectionFileError, ConnectionInfo,
    WATCHDOG_CLIENT_ROLE,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    task::JoinHandle,
    time::{self, Instant},
};
use tracing::{error, info, warn};

use crate::{read_frame, write_frame, Frame};

pub const DEFAULT_SELF_WATCHDOG_INTERVAL: Duration = Duration::from_secs(60);
const CLOCK_STEP_CHECK_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn spawn_clock_step_monitor() -> JoinHandle<()> {
    tokio::spawn(async {
        let mut detector = crate::clock::ClockStepDetector::new();
        // Establish the first offset at startup, before the first periodic tick.
        detector.check_now();
        let mut interval = time::interval(CLOCK_STEP_CHECK_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Some(step) = detector.check_now() {
                record_clock_step(&step, std::time::SystemTime::now());
            }
        }
    })
}
/// Log a wall-clock step where a reader will find it.
///
/// Day segments are named by the wall clock at write time, so a step that
/// crosses UTC midnight files the lines before it under one day and the lines
/// after it under another. The ordinary `warn!` lands in the corrected day's
/// segment; when the pre-step clock names a different segment, the same record
/// is also written there, stamped with the pre-step clock, so it sits beside
/// the lines that clock stamped. Without it, a reader opening that segment finds
/// the lines and not the warning, and a boot recorded under the wrong clock
/// reads as a real boot at the wrong time.
fn record_clock_step(step: &crate::clock::ClockStep, wall_now: std::time::SystemTime) {
    let direction = if step.delta_ms > 0 {
        "forward"
    } else {
        "backward"
    };
    let pre_step = pre_step_copy(step, wall_now);
    warn!(
        step_ms = step.delta_ms.abs(),
        direction,
        old_offset_ms = step.old_offset_ms,
        new_offset_ms = step.new_offset_ms,
        pre_step_segment = pre_step.as_ref().map(|(_, segment)| segment.as_str()),
        "wall clock stepped; timestamps around this point in the log may not be monotonic"
    );
    let Some((old_clock_now, _)) = pre_step else {
        return;
    };
    if let Some(logger) = cortexkit_log::installed() {
        logger.emit_at(
            old_clock_now,
            tracing::Level::WARN,
            "subc",
            "wall clock stepped; this copy is stamped with the pre-step clock so it sits beside the lines that clock filed here",
            &[
                ("step_ms".to_owned(), step.delta_ms.abs().to_string()),
                ("direction".to_owned(), direction.to_owned()),
                (
                    "corrected_segment".to_owned(),
                    cortexkit_log::segment_name("subc", wall_now),
                ),
            ],
        );
    }
}

/// The instant and segment for the pre-step copy of a step marker, or `None`
/// when the pre-step clock names the same day segment as the corrected one
/// (then the ordinary marker is already beside the lines, and a second copy
/// would only duplicate it).
fn pre_step_copy(
    step: &crate::clock::ClockStep,
    wall_now: std::time::SystemTime,
) -> Option<(std::time::SystemTime, String)> {
    let old_clock_now = step.old_clock_reading(wall_now);
    let pre_step_segment = cortexkit_log::segment_name("subc", old_clock_now);
    (pre_step_segment != cortexkit_log::segment_name("subc", wall_now))
        .then_some((old_clock_now, pre_step_segment))
}

pub const DEFAULT_SELF_WATCHDOG_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct DaemonSelfWatchdogConfig {
    interval: Duration,
    deadline: Duration,
}

impl Default for DaemonSelfWatchdogConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_SELF_WATCHDOG_INTERVAL,
            deadline: DEFAULT_SELF_WATCHDOG_DEADLINE,
        }
    }
}

impl DaemonSelfWatchdogConfig {
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn deadline(&self) -> Duration {
        self.deadline
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogStage {
    Connect,
    Authenticate,
    Describe,
    ConnectionFile,
    Timeout,
}

impl WatchdogStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Authenticate => "authenticate",
            Self::Describe => "describe",
            Self::ConnectionFile => "connection_file",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchdogTickError {
    stage: WatchdogStage,
    message: String,
}

impl WatchdogTickError {
    fn new(stage: WatchdogStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }

    pub fn stage(&self) -> WatchdogStage {
        self.stage
    }
}

impl fmt::Display for WatchdogTickError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WatchdogTickError {}

#[derive(Debug, Clone)]
pub struct DaemonSelfWatchdog {
    live_connection_info: ConnectionInfo,
    connection_file_path: PathBuf,
    config: DaemonSelfWatchdogConfig,
}

impl DaemonSelfWatchdog {
    pub fn new(
        live_connection_info: ConnectionInfo,
        connection_file_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            live_connection_info,
            connection_file_path: connection_file_path.into(),
            config: DaemonSelfWatchdogConfig::default(),
        }
    }

    pub fn with_config(mut self, config: DaemonSelfWatchdogConfig) -> Self {
        self.config = config;
        self
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    pub async fn run_once(&self) -> Result<(), WatchdogTickError> {
        self.verify_loopback().await?;
        self.verify_connection_file()?;
        Ok(())
    }

    async fn run(self) {
        let mut consecutive_failures = 0u64;
        let mut tick_index = 0u64;
        loop {
            time::sleep_until(
                Instant::now()
                    + jittered_watchdog_delay(
                        &self.live_connection_info,
                        tick_index,
                        self.config.interval(),
                    ),
            )
            .await;
            tick_index = tick_index.wrapping_add(1);

            let result = time::timeout(self.config.deadline(), self.run_once()).await;
            match result {
                Ok(Ok(())) => {
                    if consecutive_failures > 0 {
                        info!(
                            connection_file = %self.connection_file_path.display(),
                            failure_streak = consecutive_failures,
                            "daemon self-watchdog recovered"
                        );
                        consecutive_failures = 0;
                    }
                }
                Ok(Err(err)) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    error!(
                        connection_file = %self.connection_file_path.display(),
                        stage = err.stage().as_str(),
                        consecutive_failures,
                        error = %err,
                        "daemon self-watchdog tick failed"
                    );
                }
                Err(_) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    error!(
                        connection_file = %self.connection_file_path.display(),
                        stage = WatchdogStage::Timeout.as_str(),
                        consecutive_failures,
                        deadline_ms = self.config.deadline().as_millis(),
                        "daemon self-watchdog tick failed"
                    );
                }
            }
        }
    }

    async fn verify_loopback(&self) -> Result<(), WatchdogTickError> {
        let endpoint = self.live_connection_info.endpoints.first().ok_or_else(|| {
            WatchdogTickError::new(
                WatchdogStage::Describe,
                "live connection info has no endpoint",
            )
        })?;
        let ip = endpoint.host.parse::<IpAddr>().map_err(|err| {
            WatchdogTickError::new(
                WatchdogStage::Connect,
                format!(
                    "published endpoint host '{}' is not an IP: {err}",
                    endpoint.host
                ),
            )
        })?;
        let addr = SocketAddr::new(ip, endpoint.port);
        let mut stream = TcpStream::connect(addr).await.map_err(|err| {
            WatchdogTickError::new(WatchdogStage::Connect, format!("connect {addr}: {err}"))
        })?;

        authenticate_client_with_role(
            &mut stream,
            &self.live_connection_info,
            self.config.deadline(),
            WATCHDOG_CLIENT_ROLE,
        )
        .await
        .map_err(|err| {
            WatchdogTickError::new(
                WatchdogStage::Authenticate,
                format!("authenticate to {addr}: {err}"),
            )
        })?;

        let request = control_request_frame()?;
        write_frame(&mut stream, &request).await.map_err(|err| {
            WatchdogTickError::new(
                WatchdogStage::Describe,
                format!("write server.describe request to {addr}: {err}"),
            )
        })?;

        loop {
            let Some(reply) = read_frame(&mut stream).await.map_err(|err| {
                WatchdogTickError::new(
                    WatchdogStage::Describe,
                    format!("read server.describe reply from {addr}: {err}"),
                )
            })?
            else {
                return Err(WatchdogTickError::new(
                    WatchdogStage::Describe,
                    format!(
                        "daemon {addr} closed the connection before replying to server.describe"
                    ),
                ));
            };

            if reply.header.channel != 0 {
                continue;
            }
            match reply.header.ty {
                FrameType::Response => {
                    if reply.header.corr != request.header.corr {
                        return Err(WatchdogTickError::new(
                            WatchdogStage::Describe,
                            format!(
                                "server.describe reply correlation mismatch: expected {}, got {}",
                                request.header.corr, reply.header.corr
                            ),
                        ));
                    }
                    match serde_json::from_slice::<ClientControlResponse>(&reply.body) {
                        Ok(ClientControlResponse::ServerDescribe { .. }) => {
                            let _ = stream.shutdown().await;
                            return Ok(());
                        }
                        Ok(other) => {
                            return Err(WatchdogTickError::new(
                                WatchdogStage::Describe,
                                format!("unexpected server.describe reply: {other:?}"),
                            ));
                        }
                        Err(err) => {
                            return Err(WatchdogTickError::new(
                                WatchdogStage::Describe,
                                format!("decode server.describe reply: {err}"),
                            ));
                        }
                    }
                }
                FrameType::Error => {
                    return Err(WatchdogTickError::new(
                        WatchdogStage::Describe,
                        format!(
                            "server.describe rejected: {}",
                            decode_error_body(&reply.body)
                        ),
                    ));
                }
                _ => continue,
            }
        }
    }

    fn verify_connection_file(&self) -> Result<(), WatchdogTickError> {
        let file_info = connection_file::read_for_client(&self.connection_file_path)
            .map_err(|err| map_connection_file_error(&self.connection_file_path, err))?;

        let live_port = self
            .live_connection_info
            .endpoints
            .first()
            .map(|endpoint| endpoint.port)
            .ok_or_else(|| {
                WatchdogTickError::new(
                    WatchdogStage::ConnectionFile,
                    "live connection info has no endpoint",
                )
            })?;
        let file_ports = file_info
            .endpoints
            .iter()
            .map(|endpoint| endpoint.port)
            .collect::<BTreeSet<_>>();

        let mut divergences = Vec::new();
        if file_ports.len() != 1 || !file_ports.contains(&live_port) {
            divergences.push(format!(
                "port (live={live_port}, file={:?})",
                file_ports.into_iter().collect::<Vec<_>>()
            ));
        }
        if file_info.key != self.live_connection_info.key {
            divergences.push("key".to_owned());
        }
        if file_info.wire_version != self.live_connection_info.wire_version {
            divergences.push(format!(
                "wire_version (live={:?}, file={:?})",
                self.live_connection_info.wire_version, file_info.wire_version
            ));
        }
        if file_info.daemon_id != self.live_connection_info.daemon_id {
            divergences.push("daemon_id".to_owned());
        }

        if divergences.is_empty() {
            Ok(())
        } else {
            Err(WatchdogTickError::new(
                WatchdogStage::ConnectionFile,
                format!("connection file divergence: {}", divergences.join(", ")),
            ))
        }
    }
}

fn control_request_frame() -> Result<Frame, WatchdogTickError> {
    let body = serde_json::to_vec(&ClientControlRequest::ServerDescribe {}).map_err(|err| {
        WatchdogTickError::new(
            WatchdogStage::Describe,
            format!("encode server.describe request: {err}"),
        )
    })?;
    Frame::build(
        FrameType::Request,
        Flags::new(false, Priority::Interactive, false),
        0,
        0,
        1,
        body,
    )
    .map_err(|err| {
        WatchdogTickError::new(
            WatchdogStage::Describe,
            format!("build server.describe request frame: {err}"),
        )
    })
}

fn decode_error_body(body: &[u8]) -> String {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(error) => format!("{} — {}", error.code, error.message),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    }
}

fn map_connection_file_error(path: &Path, err: ConnectionFileError) -> WatchdogTickError {
    let message = match err {
        ConnectionFileError::Io { op, source, .. } => {
            format!("connection file {} {}: {}", path.display(), op, source)
        }
        ConnectionFileError::JsonRead { source, .. } => {
            format!(
                "connection file {} parse failed: {}",
                path.display(),
                source
            )
        }
        ConnectionFileError::UnsupportedSchema { schema, supported } => format!(
            "connection file {} schema mismatch: file={}, supported={}",
            path.display(),
            schema,
            supported
        ),
        ConnectionFileError::WireVersionMismatch { file, supported } => format!(
            "connection file {} wire version mismatch: file={}, supported={}; the binary must be upgraded",
            path.display(),
            file,
            supported
        ),
        ConnectionFileError::Invalid { reason } => {
            format!("connection file {} invalid: {}", path.display(), reason)
        }
        ConnectionFileError::KeyTooShort { len, min } => format!(
            "connection file {} key is too short: len={}, min={}",
            path.display(),
            len,
            min
        ),
        ConnectionFileError::InsecurePermissions { mode, .. } => format!(
            "connection file {} permissions are not owner-only: mode={mode:#o}",
            path.display()
        ),
        other => format!("connection file {} error: {other}", path.display()),
    };
    WatchdogTickError::new(WatchdogStage::ConnectionFile, message)
}

fn jittered_watchdog_delay(
    live_connection_info: &ConnectionInfo,
    tick_index: u64,
    interval: Duration,
) -> Duration {
    if interval.is_zero() {
        return Duration::ZERO;
    }
    let interval_ms = interval.as_millis() as u64;
    if interval_ms == 0 {
        return interval;
    }

    let jitter_span = (interval_ms / 10).max(1);
    let hash = live_connection_info.daemon_id.iter().fold(
        tick_index.wrapping_mul(0x9E37_79B9_7F4A_7C15),
        |acc, byte| {
            acc.wrapping_mul(1099511628211)
                .wrapping_add(u64::from(*byte))
        },
    );
    let offset = (hash % (jitter_span.saturating_mul(2).saturating_add(1))) as i128
        - i128::from(jitter_span);
    let jittered_ms = (i128::from(interval_ms) + offset).max(0) as u64;
    Duration::from_millis(jittered_ms)
}

impl fmt::Display for DaemonSelfWatchdogConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "interval={:?}, deadline={:?}",
            self.interval(),
            self.deadline()
        )
    }
}

impl fmt::Display for WatchdogStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod clock_step_tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::pre_step_copy;
    use crate::clock::ClockStep;

    fn at(ms: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    /// The boot this was found on: an RTC holding local time (UTC+2) booted at
    /// 22:14Z on 09-20, so the clock read 09-21 00:14 until NTP stepped it back
    /// two hours at 22:15:17Z. Lines before the step went to the 09-21 segment;
    /// the marker, written after, would land in 09-20's.
    #[test]
    fn a_step_across_utc_midnight_places_a_copy_in_the_pre_step_segment() {
        let corrected = at(1_789_942_517_000); // 2026-09-20T22:15:17Z
        let step = ClockStep {
            delta_ms: -7_200_000,
            old_offset_ms: 0,
            new_offset_ms: -7_200_000,
        };
        let (instant, segment) = pre_step_copy(&step, corrected)
            .expect("a midnight-crossing step needs a pre-step copy");
        assert_eq!(segment, "subc.2026-09-21.log");
        assert_eq!(instant, at(1_789_949_717_000)); // 2026-09-21T00:15:17Z
        assert_eq!(
            cortexkit_log::segment_name("subc", corrected),
            "subc.2026-09-20.log",
            "the ordinary marker lands in the other day's segment"
        );
    }

    #[test]
    fn a_step_inside_one_utc_day_writes_no_second_copy() {
        let corrected = at(1_789_905_600_000); // 2026-09-20T12:00:00Z
        let step = ClockStep {
            delta_ms: 9_000,
            old_offset_ms: 0,
            new_offset_ms: 9_000,
        };
        assert_eq!(pre_step_copy(&step, corrected), None);
    }
}
