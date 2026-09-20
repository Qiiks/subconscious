use std::collections::BTreeSet;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Row {
    ModuleDeclaration,
    SupervisedServer,
    SupervisedServerArgv,
    GrantConformance,
    InstallBootstrap,
    Census,
    Revocation,
    SpawnStream,
    SpawnReconcile,
    ModuleHealth,
    Sentinel,
    DeadLetter,
    Membership,
    Leaf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Skipped {
        gate: &'static str,
        observation: String,
    },
}

#[derive(Debug, Clone)]
pub struct RowReport {
    row: Row,
    outcome: Outcome,
    served_by_harness_stub: bool,
    reached_stub_operation: Option<String>,
}

impl RowReport {
    pub fn passed(row: Row) -> Self {
        Self {
            row,
            outcome: Outcome::Passed,
            served_by_harness_stub: false,
            reached_stub_operation: None,
        }
    }

    pub fn skipped(row: Row, gate: &'static str, observation: impl Into<String>) -> Self {
        Self {
            row,
            outcome: Outcome::Skipped {
                gate,
                observation: observation.into(),
            },
            served_by_harness_stub: false,
            reached_stub_operation: None,
        }
    }

    pub fn served_by_harness_stub(mut self, operation: impl Into<String>) -> Self {
        self.served_by_harness_stub = true;
        self.reached_stub_operation = Some(operation.into());
        self
    }

    pub fn validate(&self, advertised_stub_ops: &BTreeSet<String>) -> Result<(), String> {
        if let Outcome::Skipped { gate, observation } = &self.outcome {
            if gate.trim().is_empty() || observation.trim().is_empty() {
                return Err("a skipped row must name its gate and observed condition".to_string());
            }
            if !allowed_gates(self.row).contains(gate) && !UNIVERSAL_GATES.contains(gate) {
                return Err(format!("row {:?} may not record gate {gate}", self.row));
            }
        }
        if let Some(operation) = &self.reached_stub_operation {
            if !self.served_by_harness_stub {
                return Err("a stub-served row must record served-by: harness-stub".to_string());
            }
            if !advertised_stub_ops.contains(operation) {
                return Err(format!(
                    "stub-served row reached unadvertised operation {operation}"
                ));
            }
        }
        Ok(())
    }
}

const UNIVERSAL_GATES: &[&str] = &[
    "naming-constructor-absent",
    "health-class-carrier-unpinned",
    "stub-reply-shape-unrecorded",
];

fn allowed_gates(row: Row) -> &'static [&'static str] {
    match row {
        Row::ModuleDeclaration => &[],
        Row::SupervisedServer | Row::SupervisedServerArgv => &["a1-signal-unix-only"],
        Row::GrantConformance => &["ckcred-mint-unlanded", "prefrontal-seat-unnamed"],
        Row::InstallBootstrap => &["ckcred-mint-unlanded", "ckcred-delete-unlanded"],
        Row::Census | Row::Revocation => &["ckcred-mint-unlanded", "ckcred-delete-unlanded"],
        Row::SpawnStream => &["spawn-stream-unlanded"],
        Row::SpawnReconcile => &[
            "spawn-stream-unlanded",
            "ckcred-mint-unlanded",
            "ckcred-delete-unlanded",
        ],
        Row::ModuleHealth => &["health-down-escalation-unpinned"],
        Row::Sentinel => &["ckcred-mint-unlanded", "health-down-escalation-unpinned"],
        Row::DeadLetter | Row::Leaf => &["ckcred-mint-unlanded"],
        Row::Membership => &["ckcred-mint-unlanded", "membership-contract-unpinned"],
    }
}
