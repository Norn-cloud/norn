//! Evidence projection for definition-owned finite duty controls.
use crate::duties::DutyCheck;
use norn_ir::entity_v2::controls::{ControlStatus, Reachability};

/// Project finite graph evidence without strengthening an abstraction.
/// `concrete` means every graph edge and grant has a concrete interpretation.
#[must_use]
pub fn evidence(check: &DutyCheck, concrete: bool) -> (ControlStatus, Reachability) {
    match check {
        DutyCheck::Separated { .. } => (ControlStatus::Proven, Reachability::Unknown),
        DutyCheck::Unreachable => (ControlStatus::Proven, Reachability::Unreachable),
        DutyCheck::Counterexample { trace, .. } if concrete && !trace.is_empty() => {
            (ControlStatus::Refuted, Reachability::Witnessed)
        }
        DutyCheck::Counterexample { .. } | DutyCheck::Invalid | DutyCheck::Exhausted => {
            (ControlStatus::Unknown, Reachability::Unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duties::{DutyEdge, DutyStep};

    #[test]
    fn incomplete_evidence_is_never_promoted() {
        for check in [DutyCheck::Invalid, DutyCheck::Exhausted] {
            for concrete in [false, true] {
                assert_eq!(evidence(&check, concrete), (ControlStatus::Unknown, Reachability::Unknown));
            }
        }
    }

    #[test]
    fn witnessed_requires_a_concrete_nonempty_trace() {
        for concrete in [false, true] {
            for present in [false, true] {
                let trace = if present {
                    vec![DutyStep {
                        edge: DutyEdge { action: "approve".into(), from: 0, to: 1 },
                        role: "reviewer".into(),
                    }]
                } else { vec![] };
                let check = DutyCheck::Counterexample {
                    role: "reviewer".into(), trace, sensitive_start: 0,
                };
                assert_eq!(evidence(&check, concrete), if concrete && present {
                    (ControlStatus::Refuted, Reachability::Witnessed)
                } else { (ControlStatus::Unknown, Reachability::Unknown) });
            }
        }
    }

    #[test]
    fn safe_overapproximation_does_not_claim_availability() {
        for concrete in [false, true] {
            assert_eq!(evidence(&DutyCheck::Separated { roles_checked: 2 }, concrete),
                (ControlStatus::Proven, Reachability::Unknown));
            assert_eq!(evidence(&DutyCheck::Unreachable, concrete),
                (ControlStatus::Proven, Reachability::Unreachable));
        }
    }
}
