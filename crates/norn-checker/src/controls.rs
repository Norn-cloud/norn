//! Finite model evidence; candidate traces carry no runtime availability claim.
use crate::duties::DutyCheck;
use norn_ir::entity_v2::controls::{ControlStatus, Reachability};

/// Project model safety evidence. Only kernel replay can establish availability.
#[must_use]
pub fn evidence(check: &DutyCheck) -> (ControlStatus, Reachability) {
    let verdict = match check {
        DutyCheck::Separated { .. } | DutyCheck::Unreachable => ControlStatus::Proven,
        DutyCheck::Counterexample { trace, .. } if !trace.is_empty() => ControlStatus::Refuted,
        DutyCheck::Counterexample { .. } | DutyCheck::Invalid | DutyCheck::Exhausted => {
            ControlStatus::Unknown
        }
    };
    (verdict, Reachability::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duties::{DutyEdge, DutyStep};

    #[test]
    fn incomplete_evidence_is_never_promoted() {
        for check in [DutyCheck::Invalid, DutyCheck::Exhausted] {
            assert_eq!(
                evidence(&check),
                (ControlStatus::Unknown, Reachability::Unknown)
            );
        }
    }

    #[test]
    fn model_counterexamples_are_candidates_only() {
        for present in [false, true] {
            let trace = if present {
                vec![DutyStep {
                    edge: DutyEdge {
                        action: "approve".into(),
                        from: 0,
                        to: 1,
                    },
                    role: "reviewer".into(),
                }]
            } else {
                vec![]
            };
            let check = DutyCheck::Counterexample {
                role: "reviewer".into(),
                trace,
                sensitive_start: 0,
            };
            assert_eq!(
                evidence(&check),
                (
                    if present {
                        ControlStatus::Refuted
                    } else {
                        ControlStatus::Unknown
                    },
                    Reachability::Unknown
                )
            );
        }
    }

    #[test]
    fn safe_overapproximation_does_not_claim_availability() {
        for check in [
            DutyCheck::Separated { roles_checked: 2 },
            DutyCheck::Unreachable,
        ] {
            assert_eq!(
                evidence(&check),
                (ControlStatus::Proven, Reachability::Unknown)
            );
        }
    }
}
