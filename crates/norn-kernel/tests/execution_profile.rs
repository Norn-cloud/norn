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
    fn pending(&self, _: &Consumed) -> Result<bool, Infallible> {
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
    );
    let proposal = CellCommit {
        reads: vec![FactExpectation {
            key: FactKey::new(law_key(1).object_type.as_str(), "foreign"),
            expected: FactVersion::ABSENT,
        }],
        effects: vec![norn_kernel::Effect::Perform(law_command(1, 0, 1))],
        ..CellCommit::empty()
    };
    let result = check_commit(
        &proposal,
        &model,
        &View {
            ownership: AtomicOwnership::Foreign,
            hidden: false,
        },
    );
    assert!(matches!(result, Err(CommitError::AtomicBoundary { .. })));
    if result.is_ok() {
        cell.commit(proposal).expect("admitted commit");
    }
    assert_eq!(
        (
            cell.load(&law_key(1)).expect("fact"),
            cell.ledger().len(),
            cell.outbox().len()
        ),
        before
    );
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
                hidden: false
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
                hidden: false
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
                hidden: true
            }
        ),
        Err(CommitError::AtomicBoundary { .. })
    ));
}
