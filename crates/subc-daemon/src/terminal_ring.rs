use std::{collections::VecDeque, sync::Arc};

use crate::terminal_journal::{ring_history, TerminalJournal};

use subc_control::{TerminalDisposition, TerminalExitKind};

const DEFAULT_MAX_ENTRIES: usize = 32;

/// Fixed-size retention policy for one module's terminal exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRingConfig {
    max_entries: usize,
}

impl TerminalRingConfig {
    /// A zero-sized history cannot answer whether an exit was observed, so clamp it
    /// to one record instead of constructing a ring that lies by omission.
    pub const fn new(max_entries: usize) -> Self {
        Self {
            max_entries: if max_entries == 0 { 1 } else { max_entries },
        }
    }
}

impl Default for TerminalRingConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES)
    }
}

/// One observed child exit and the disposition chosen by its supervisor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalRecord {
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub at_ms: u64,
    pub disposition: TerminalDisposition,
    pub exit_kind: TerminalExitKind,
    /// Why the supervisor chose this disposition, when the disposition alone
    /// does not say. `failed` records the exhausted crash budget here, naming
    /// the limit AND the window it was counted over, because a module stopped
    /// by three crashes in ten minutes and one stopped by three crashes in a
    /// week are the same `failed` and call for different reactions.
    pub disposition_detail: Option<String>,
}

/// The retained terminal suffix for one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalHistorySnapshot {
    pub daemon_started_at_ms: u64,
    pub entries: Vec<TerminalRecord>,
    pub dropped: u64,
}

/// Bounded terminal-exit history for one supervised module.
///
/// This belongs to the module rather than an individual child so a replacement
/// process retains the exits that caused it to exist.
#[derive(Debug)]
pub struct TerminalRing {
    journal: Option<Arc<TerminalJournal>>,
    config: TerminalRingConfig,
    daemon_started_at_ms: u64,
    start_clock: Option<crate::clock::StartClock>,
    entries: VecDeque<TerminalRecord>,
    dropped: u64,
}

impl TerminalRing {
    pub fn new(config: TerminalRingConfig, daemon_started_at_ms: u64) -> Self {
        Self {
            journal: None,
            config,
            daemon_started_at_ms,
            start_clock: None,
            entries: VecDeque::new(),
            dropped: 0,
        }
    }

    pub(crate) fn with_start_clock(mut self, clock: crate::clock::StartClock) -> Self {
        self.start_clock = Some(clock);
        self
    }

    pub(crate) fn with_journal(mut self, journal: Option<Arc<TerminalJournal>>) -> Self {
        self.journal = journal;
        self
    }

    pub(crate) fn append_journal(&self, module_id: &str, entry: &TerminalRecord) {
        if let Some(journal) = &self.journal {
            journal.append(module_id, entry);
        }
    }

    pub(crate) fn durable_history(&self, module_id: &str) -> subc_control::TerminalHistory {
        match &self.journal {
            Some(journal) => journal.merge(module_id, self.snapshot()),
            None => ring_history(self.snapshot(), None),
        }
    }

    pub fn push(&mut self, entry: TerminalRecord) {
        self.entries.push_back(entry);
        while self.entries.len() > self.config.max_entries {
            self.entries.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    pub fn snapshot(&self) -> TerminalHistorySnapshot {
        TerminalHistorySnapshot {
            daemon_started_at_ms: self
                .start_clock
                .map_or(self.daemon_started_at_ms, |clock| clock.started_at_ms()),
            entries: self.entries.iter().cloned().collect(),
            dropped: self.dropped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalRecord, TerminalRing, TerminalRingConfig};
    use subc_control::{TerminalDisposition, TerminalExitKind};

    fn record(at_ms: u64) -> TerminalRecord {
        TerminalRecord {
            exit_code: Some(1),
            exit_signal: None,
            at_ms,
            disposition: TerminalDisposition::Restarting,
            exit_kind: TerminalExitKind::Crash,
            disposition_detail: None,
        }
    }

    #[test]
    fn the_ring_evicts_oldest_exits_and_counts_them() {
        let mut ring = TerminalRing::new(TerminalRingConfig::new(2), 10);
        ring.push(record(11));
        ring.push(record(12));
        ring.push(record(13));

        let snapshot = ring.snapshot();
        assert_eq!(snapshot.daemon_started_at_ms, 10);
        assert_eq!(snapshot.dropped, 1);
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.at_ms)
                .collect::<Vec<_>>(),
            vec![12, 13]
        );
    }

    #[test]
    fn an_incoherent_zero_capacity_keeps_one_terminal() {
        let mut ring = TerminalRing::new(TerminalRingConfig::new(0), 10);
        ring.push(record(11));
        ring.push(record(12));

        let snapshot = ring.snapshot();
        assert_eq!(snapshot.dropped, 1);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].at_ms, 12);
    }
}
