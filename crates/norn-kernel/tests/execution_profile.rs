#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "boundary scenario assertions"
)]

use norn_ir::ObjectTypeName;
use norn_kernel::execution_profile::{AtomicOwnership, supports_local};
use norn_kernel::{
    CellCommit, CellHost, CommitError, CommitModel, CommitView, Consumed, FactExpectation, FactKey,
    FactVersion, LinkKey, TypeCatalog, check_commit,
};
use norn_testkit::cell_host::{
    LawCommand, LawEvent, LawInput, MemoryCell, law_command, law_key, law_object_types,
    law_responder,
};
use norn_testkit::proptest::prelude::*;
use std::convert::Infallible;

struct View {
    ownership: AtomicOwnership,
    hidden: bool,
    pending_calls: std::cell::Cell<usize>,
}
impl CommitView for View {
    type Error = Infallible;
    fn atomic_ownership(&self, key: &FactKey) -> Result<AtomicOwnership, Infallible> {
        Ok(if key.key == "foreign" {
            self.ownership
        } else {
            AtomicOwnership::Local
        })
    }
    fn consumed_ownership(&self, _: &Consumed) -> Result<AtomicOwnership, Infallible> {
        Ok(self.ownership)
    }
    fn pending(&self, _: &Consumed) -> Result<bool, Infallible> {
        self.pending_calls.set(self.pending_calls.get() + 1);
        Ok(true)
    }
    fn version(&self, _: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion::ABSENT)
    }
    fn concrete(&self, _: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
        Ok(None)
    }
    fn link_version(&self, _: &LinkKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion::ABSENT)
    }
    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
        Ok(if self.hidden {
            vec![LinkKey::new(
                key.clone(),
                "next",
                FactKey::new(key.object_type.as_str(), "foreign"),
            )]
        } else {
            vec![]
        })
    }
}

#[test]
fn mixed_owner_immediate_read_closure_refuses_without_effects() {
    let types = law_object_types();
    let catalog = TypeCatalog::new(&types);
    let model = CommitModel::new(&catalog);
    let mut cell: MemoryCell<LawEvent, LawCommand, LawInput> =
        MemoryCell::new(law_responder).with_types(types.clone());
    let before = (
        cell.load(&law_key(1)).expect("fact"),
        cell.ledger().len(),
        cell.outbox().len(),
        cell.timers().len(),
    );
    let proposal = CellCommit {
        reads: vec![FactExpectation {
            key: FactKey::new(law_key(1).object_type.as_str(), "foreign"),
            expected: FactVersion::ABSENT,
        }],
        writes: vec![norn_kernel::FactWrite {
            key: law_key(1),
            expected: FactVersion::ABSENT,
            mutation: norn_kernel::FactMutation::Put {
                values: norn_testkit::cell_host::law_values(1),
            },
        }],
        events: vec![1],
        effects: vec![
            norn_kernel::Effect::Perform(law_command(1, 0, 1)),
            norn_kernel::Effect::Schedule(norn_kernel::Timer {
                id: norn_testkit::cell_host::law_timer_id(1),
                due: norn_kernel::LogicalTime::from_millis(1),
                payload: LawInput::Timer(1),
            }),
        ],
        ..CellCommit::empty()
    };
    let result = check_commit(
        &proposal,
        &model,
        &View {
            ownership: AtomicOwnership::Foreign,
            hidden: false,
            pending_calls: std::cell::Cell::default(),
        },
    );
    assert!(matches!(result, Err(CommitError::AtomicBoundary { .. })));
    if result.is_ok() {
        cell.commit(proposal.clone()).expect("admitted commit");
    }
    assert_eq!(
        (
            cell.load(&law_key(1)).expect("fact"),
            cell.ledger().len(),
            cell.outbox().len(),
            cell.timers().len(),
        ),
        before
    );
    check_commit(
        &proposal,
        &model,
        &View {
            ownership: AtomicOwnership::Local,
            hidden: false,
            pending_calls: std::cell::Cell::default(),
        },
    )
    .expect("co-located closure");
    cell.commit(proposal).expect("local commit");
    assert!(
        cell.load(&law_key(1))
            .expect("written fact")
            .values
            .is_some()
    );
    assert_eq!(cell.ledger().len(), 1);
    assert_eq!(cell.outbox().len(), 1);
    assert_eq!(cell.timers().len(), 1);
}

#[test]
fn unknown_inherited_dependency_is_not_supported() {
    let types = law_object_types();
    let catalog = TypeCatalog::new(&types);
    let proposal: CellCommit<(), (), ()> = CellCommit {
        reads: vec![FactExpectation {
            key: FactKey::new(law_key(1).object_type.as_str(), "foreign"),
            expected: FactVersion::ABSENT,
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        check_commit(
            &proposal,
            &CommitModel::new(&catalog),
            &View {
                ownership: AtomicOwnership::Unknown,
                hidden: false,
                pending_calls: std::cell::Cell::default(),
            }
        ),
        Err(CommitError::AtomicBoundary { .. })
    ));
    assert!(
        check_commit(
            &proposal,
            &CommitModel::new(&catalog),
            &View {
                ownership: AtomicOwnership::Local,
                hidden: false,
                pending_calls: std::cell::Cell::default(),
            }
        )
        .is_ok()
    );
}

proptest! {
    #[test]
    fn unresolved_ownership_never_gains_support(foreign in any::<bool>()) {
        let ownership = if foreign { AtomicOwnership::Foreign } else { AtomicOwnership::Unknown };
        prop_assert!(!supports_local(ownership));
        prop_assert!(supports_local(AtomicOwnership::Local));
    }
}

#[test]
fn discovered_graph_dependency_cannot_escape_the_profile() {
    let types = law_object_types();
    let catalog = TypeCatalog::new(&types);
    let proposal: CellCommit<(), (), ()> = CellCommit {
        writes: vec![norn_kernel::FactWrite {
            key: law_key(1),
            expected: FactVersion::ABSENT,
            mutation: norn_kernel::FactMutation::Delete,
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        check_commit(
            &proposal,
            &CommitModel::new(&catalog),
            &View {
                ownership: AtomicOwnership::Foreign,
                hidden: true,
                pending_calls: std::cell::Cell::default(),
            }
        ),
        Err(CommitError::AtomicBoundary { .. })
    ));
}

#[test]
fn boundary_code_survives_refusal_adapters() {
    let commit: CommitError<Infallible> = CommitError::AtomicBoundary { key: law_key(1) };
    let decision: norn_kernel::DecisionRefusal<Infallible> =
        norn_kernel::DecisionRefusal::AtomicBoundary { key: law_key(1) };
    assert_eq!(commit.code(), "core.txn.atomic_boundary");
    assert_eq!(decision.code(), commit.code());
    assert_eq!(decision.category(), commit.category());
}

#[test]
fn foreign_and_unknown_delivery_consumption_refuses_before_pending_or_effects() {
    let types = law_object_types();
    let catalog = TypeCatalog::new(&types);
    let model = CommitModel::new(&catalog);
    let deliveries = [
        Consumed::Timer {
            id: norn_kernel::TimerId("timer".into()),
            incarnation: norn_kernel::LedgerSeq(1),
        },
        Consumed::Result {
            command: norn_kernel::CommandId("command".into()),
            generation: norn_kernel::Generation(1),
        },
        Consumed::Observation {
            id: norn_kernel::ObservationId("observation".into()),
        },
    ];
    for consumed in deliveries {
        let proposal: CellCommit<LawEvent, LawCommand, LawInput> = CellCommit {
            consumes: Some(consumed.clone()),
            writes: vec![norn_kernel::FactWrite {
                key: law_key(1),
                expected: FactVersion::ABSENT,
                mutation: norn_kernel::FactMutation::Put {
                    values: norn_testkit::cell_host::law_values(1),
                },
            }],
            events: vec![1],
            effects: vec![
                norn_kernel::Effect::Perform(law_command(1, 0, 1)),
                norn_kernel::Effect::Schedule(norn_kernel::Timer {
                    id: norn_testkit::cell_host::law_timer_id(1),
                    due: norn_kernel::LogicalTime::from_millis(1),
                    payload: LawInput::Timer(1),
                }),
            ],
            ..CellCommit::empty()
        };
        for ownership in [AtomicOwnership::Foreign, AtomicOwnership::Unknown] {
            let view = View {
                ownership,
                hidden: false,
                pending_calls: std::cell::Cell::default(),
            };
            let mut cell: MemoryCell<LawEvent, LawCommand, LawInput> =
                MemoryCell::new(law_responder).with_types(types.clone());
            let before = (
                cell.load(&law_key(1)).expect("fact"),
                cell.ledger().len(),
                cell.outbox().len(),
                cell.timers().len(),
            );
            let result = check_commit(&proposal, &model, &view);
            assert!(
                matches!(&result, Err(CommitError::AtomicDeliveryBoundary { consumed: found }) if *found == consumed)
            );
            assert_eq!(view.pending_calls.get(), 0);
            assert_eq!(
                result.as_ref().expect_err("foreign delivery").code(),
                "core.txn.atomic_boundary"
            );
            if result.is_ok() {
                cell.commit(proposal.clone()).expect("admitted commit");
            }
            assert_eq!(
                (
                    cell.load(&law_key(1)).expect("fact"),
                    cell.ledger().len(),
                    cell.outbox().len(),
                    cell.timers().len()
                ),
                before
            );
        }
        let local = View {
            ownership: AtomicOwnership::Local,
            hidden: false,
            pending_calls: std::cell::Cell::default(),
        };
        check_commit(&proposal, &model, &local).expect("co-located delivery");
        assert_eq!(local.pending_calls.get(), 1);
    }
}
