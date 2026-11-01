//! `CellHost` contract tests (T4-CELL-1, #2675).
//!
//! - `cell_host_laws_hold_for_memory_cell`: the host-agnostic `CellHost` laws
//!   (`norn_testkit::cell_host`) against the in-memory reference host.
//! - Three mutant hosts prove the laws can fail: one leaks the enqueues of a
//!   refused commit, one ignores consumption, one drops ledger events.
//! - Fixtures: one commit is one transaction boundary for facts, ledger
//!   events, command enqueue, and timer enqueue; a refused commit applies
//!   none of them; `check_commit`'s refusal order; the wire shape.

use std::convert::Infallible;

use norn_ir::identifier_limits::{
    MAX_ACTION_CALL_ID_BYTES, MAX_OBJECT_KEY_BYTES, MAX_TOMBSTONE_KEY_BYTES,
};
use norn_kernel::cell_host::ObjectTypeName;
use norn_kernel::{
    CellCommit, CellHost, Command, CommitError, CommitModel, CommitReceipt, CommitVersions,
    CommitView, Consumed, DueTimer, Effect, FactExpectation, FactKey, FactMutation, FactState,
    FactVersion, FactWrite, LedgerSeq, LinkKey, LinkMutation, LinkReadExpectation, LinkState,
    LinkWrite, LogicalTime, Observed, OwnedRowsCommitProof, Performed, SerializationLane, Timer,
    TypeCatalog, WrittenLinkVersion, WrittenVersion, check_commit,
};
use norn_testkit::Settings;
use norn_testkit::cell_host::{
    CELL_HOST_LABELS, LAW_CHILD, LAW_LINK, LAW_STRAY, LAW_THING, LawCommand, LawEvent, LawInput,
    LedgerEntry, MemoryCell, SimulatedCell, check_cell_host, check_link_override, law_alias,
    law_command, law_key, law_link_key, law_object_types, law_observed, law_responder,
    law_timer_id, law_values, override_object_types, run_cell_host,
};

type LawCell = MemoryCell<LawEvent, LawCommand, LawInput>;

// ------------------------------------------------------------------ laws

#[test]
fn cell_host_laws_hold_for_memory_cell() {
    let coverage = check_cell_host::<LawCell>(&Settings::new(file!()).cases(128));
    coverage.assert_covered(CELL_HOST_LABELS);
}

/// A deliberately broken host, one bug per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bug {
    /// A refused commit still enqueues its effects.
    LeaksRefusedEnqueue,
    /// Commits never consume their delivery.
    IgnoresConsumption,
    /// Commits drop their ledger events.
    DropsEvents,
}

struct Mutant<const BUG: u8>(LawCell);

const fn bug(code: u8) -> Bug {
    match code {
        0 => Bug::LeaksRefusedEnqueue,
        1 => Bug::IgnoresConsumption,
        _ => Bug::DropsEvents,
    }
}

impl<const BUG: u8> CellHost for Mutant<BUG> {
    type Event = LawEvent;
    type Command = LawCommand;
    type Input = LawInput;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        self.0.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Infallible> {
        self.0.load_link(key)
    }

    fn commit(
        &mut self,
        mut commit: CellCommit<LawEvent, LawCommand, LawInput>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        match bug(BUG) {
            Bug::LeaksRefusedEnqueue => {
                let effects = commit.effects.clone();
                let result = self.0.commit(commit);
                if result.is_err() && !effects.is_empty() {
                    let leak = CellCommit {
                        effects,
                        ..CellCommit::empty()
                    };
                    let _leaked = self.0.commit(leak);
                }
                result
            }
            Bug::IgnoresConsumption => {
                commit.consumes = None;
                self.0.commit(commit)
            }
            Bug::DropsEvents => {
                commit.events.clear();
                self.0.commit(commit)
            }
        }
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        self.0.commit_owned_rows(commit, proof)
    }

    fn perform(&mut self) -> Result<Option<Performed<LawInput>>, Infallible> {
        self.0.perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<LawInput>>, Infallible> {
        self.0.observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<LawInput>>, Infallible> {
        self.0.clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Infallible> {
        self.0.publish(receipt)
    }
}

impl<const BUG: u8> SimulatedCell for Mutant<BUG> {
    fn sim_new() -> Self {
        Self(LawCell::sim_new())
    }

    fn sim_advance_to(&mut self, now: LogicalTime) {
        self.0.sim_advance_to(now);
    }

    fn sim_inject(&mut self, observed: Observed<LawInput>) {
        self.0.sim_inject(observed);
    }

    fn sim_executions(&self) -> u64 {
        self.0.sim_executions()
    }

    fn sim_outbox(&self) -> Vec<Command<LawCommand>> {
        self.0.sim_outbox()
    }

    fn sim_timers(&self) -> Vec<DueTimer<LawInput>> {
        self.0.sim_timers()
    }

    fn sim_ledger(&self) -> Vec<LedgerEntry<LawEvent>> {
        self.0.sim_ledger()
    }
}

#[test]
fn cell_host_mutant_leaking_refused_enqueue_is_caught() {
    let failure = run_cell_host::<Mutant<0>>(&Settings::new(file!()).without_persistence())
        .expect_err("a host that enqueues effects of a refused commit must fail the laws");
    assert!(
        failure.reason.contains("refused commit changed the cell"),
        "{failure}"
    );
}

#[test]
fn cell_host_mutant_ignoring_consumption_is_caught() {
    let failure = run_cell_host::<Mutant<1>>(&Settings::new(file!()).without_persistence())
        .expect_err("a host that never consumes deliveries must fail the laws");
    assert!(!failure.transitions.is_empty(), "{failure}");
}

#[test]
fn cell_host_mutant_dropping_events_is_caught() {
    let failure = run_cell_host::<Mutant<2>>(&Settings::new(file!()).without_persistence())
        .expect_err("a host that drops ledger events must fail the laws");
    assert!(failure.reason.contains("does not refine"), "{failure}");
}

// ------------------------------------------------------------------ fixtures

fn put(n: u8, expected: u64, value: u8) -> FactWrite {
    FactWrite {
        key: law_key(n),
        expected: FactVersion(expected),
        mutation: FactMutation::Put {
            values: law_values(value),
        },
    }
}

fn schedule(n: u8, due: u64, payload: u8) -> Effect<LawCommand, LawInput> {
    Effect::Schedule(Timer {
        id: law_timer_id(n),
        due: LogicalTime::from_millis(due),
        payload: LawInput::Timer(payload),
    })
}

/// Everything one turn commits, as the host shows it afterwards.
fn visible(cell: &LawCell) -> (Result<Vec<FactState>, Infallible>, usize, usize, usize) {
    (
        (0..3).map(|n| cell.load(&law_key(n))).collect(),
        cell.ledger().len(),
        cell.outbox().len(),
        cell.timers().len(),
    )
}

#[test]
fn cell_host_commit_is_one_transaction_for_facts_events_and_enqueue() {
    let mut cell = LawCell::sim_new();

    // One commit: a fact write, two ledger events, one command, one timer.
    let receipt = cell
        .commit(CellCommit {
            writes: vec![put(0, 0, 7)],
            events: vec![1, 2],
            effects: vec![Effect::Perform(law_command(0, 0, 42)), schedule(0, 50, 9)],
            ..CellCommit::empty()
        })
        .unwrap();
    assert_eq!(
        receipt,
        CommitReceipt {
            seq: LedgerSeq(1),
            written: vec![WrittenVersion {
                key: law_key(0),
                version: FactVersion(1),
            }],
            linked: vec![],
        }
    );
    let entry = &cell.ledger()[0];
    assert_eq!(entry.events, vec![1, 2]);
    assert_eq!(entry.written, receipt.written);
    assert_eq!(cell.load(&law_key(0)).unwrap().values, Some(law_values(7)));
    assert_eq!(cell.outbox(), &[law_command(0, 0, 42)]);
    assert_eq!(cell.timers().len(), 1);

    // A stale commit carrying every kind of change applies none of them.
    let before = visible(&cell);
    let refused = cell.commit(CellCommit {
        writes: vec![put(1, 0, 1), put(0, 0, 8)],
        events: vec![3],
        effects: vec![Effect::Perform(law_command(1, 0, 1)), schedule(1, 0, 1)],
        ..CellCommit::empty()
    });
    assert_eq!(
        refused,
        Err(CommitError::Conflict {
            key: law_key(0),
            expected: FactVersion(0),
            current: FactVersion(1),
        })
    );
    assert_eq!(refused.unwrap_err().code(), "commit.conflict");
    assert_eq!(visible(&cell), before);

    // The command is performed once; its result is redelivered until a commit
    // consumes it, in the same transaction as that turn's own writes.
    let performed = cell.perform().unwrap().unwrap();
    assert_eq!(performed.input, LawInput::Result(42));
    assert_eq!(cell.perform().unwrap(), Some(performed.clone()));
    assert_eq!(cell.executions(), 1);
    cell.commit(CellCommit {
        writes: vec![put(0, 1, 8)],
        ..CellCommit::empty().consuming(performed.consumed())
    })
    .unwrap();
    assert_eq!(cell.perform().unwrap(), None);
    let replayed = cell.commit(CellCommit::empty().consuming(performed.consumed()));
    assert!(matches!(replayed, Err(CommitError::NotPending { .. })));

    // The timer is delivered only when due, and consumed exactly once.
    assert_eq!(cell.clock().unwrap(), None);
    cell.advance_to(LogicalTime::from_millis(50));
    let due = cell.clock().unwrap().unwrap();
    assert_eq!(due.timer.payload, LawInput::Timer(9));
    cell.commit(CellCommit::empty().consuming(due.consumed()))
        .unwrap();
    assert_eq!(cell.clock().unwrap(), None);
    assert_eq!(cell.ledger().len(), 3);
}

#[test]
fn cell_host_rescheduled_timer_fences_its_old_delivery() {
    let mut cell = LawCell::sim_new();
    cell.commit(CellCommit {
        effects: vec![schedule(0, 0, 1)],
        ..CellCommit::empty()
    })
    .unwrap();
    let delivered = cell.clock().unwrap().unwrap();
    // Rescheduled identically: same id, due time, and payload (ABA).
    cell.commit(CellCommit {
        effects: vec![schedule(0, 0, 1)],
        ..CellCommit::empty()
    })
    .unwrap();
    assert_eq!(cell.clock().unwrap().unwrap().timer, delivered.timer);
    assert_eq!(
        cell.commit(CellCommit::empty().consuming(delivered.consumed())),
        Err(CommitError::NotPending {
            consumed: delivered.consumed(),
        })
    );
    assert_eq!(cell.timers().len(), 1);
}

#[test]
fn cell_host_observation_ingested_once_and_consumed_once() {
    let mut cell = LawCell::sim_new();
    assert!(cell.inject(law_observed(0, 5)));
    assert!(!cell.inject(law_observed(0, 6)));
    let observed = cell.observe().unwrap().unwrap();
    assert_eq!(observed.input, LawInput::Observed(5));
    assert_eq!(observed.provenance.source, "law");
    cell.commit(CellCommit::empty().consuming(observed.consumed()))
        .unwrap();
    assert_eq!(cell.observe().unwrap(), None);
    assert!(!cell.inject(law_observed(0, 5)), "a consumed id stays seen");
}

/// Every delivery is stale, every object exists at version 1, and every link
/// is at version 1.
struct FixedView;

impl CommitView for FixedView {
    type Error = Infallible;

    fn pending(&self, _: &Consumed) -> Result<bool, Infallible> {
        Ok(false)
    }

    fn version(&self, _: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion(1))
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
        // Object 2 was deleted; the others are live `law.Thing`s.
        Ok((key.key != "k2").then(|| ObjectTypeName(LAW_THING.to_owned())))
    }

    fn link_version(&self, _: &LinkKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion(1))
    }

    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
        // Object 1 has the live link k1 -rel-> k1 (link 4).
        Ok(if *key == law_key(1) {
            vec![law_link_key(4)]
        } else {
            vec![]
        })
    }
}

/// The kernel's check against the law types and [`FixedView`].
fn fixed(commit: &CellCommit<(), (), ()>) -> Result<CommitVersions, CommitError<Infallible>> {
    let types = law_object_types();
    let catalog = TypeCatalog::new(&types);
    check_commit(commit, &CommitModel::new(&catalog), &FixedView)
}

#[test]
fn cell_host_bounds_write_identities_and_allows_maximum_admitted_lengths() {
    let max_key = CellCommit::<(), (), ()> {
        reads: vec![FactExpectation {
            key: FactKey::new(LAW_THING, "é".repeat(MAX_OBJECT_KEY_BYTES / 2)),
            expected: FactVersion(1),
        }],
        ..CellCommit::empty()
    };
    assert!(fixed(&max_key).is_ok());

    let over_limit_write = CellCommit::<(), (), ()> {
        writes: vec![FactWrite {
            key: FactKey::new(LAW_THING, "é".repeat(MAX_OBJECT_KEY_BYTES / 2 + 1)),
            expected: FactVersion(0),
            mutation: FactMutation::Delete,
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        fixed(&over_limit_write),
        Err(CommitError::IdentifierTooLong {
            field: "object key",
            actual_bytes,
            max_bytes: MAX_OBJECT_KEY_BYTES,
        }) if actual_bytes == MAX_OBJECT_KEY_BYTES + 2
    ));

    for (object_type, key_len) in [
        (norn_kernel::ACTION_CALL_TYPE, MAX_ACTION_CALL_ID_BYTES),
        (
            norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE,
            MAX_TOMBSTONE_KEY_BYTES,
        ),
        (
            norn_kernel::snapshot_observe::SOURCE_OWNERSHIP_TYPE,
            MAX_TOMBSTONE_KEY_BYTES,
        ),
    ] {
        let commit = CellCommit::<(), (), ()> {
            reads: vec![FactExpectation {
                key: FactKey::new(object_type, "k".repeat(key_len)),
                expected: FactVersion(1),
            }],
            ..CellCommit::empty()
        };
        assert!(norn_kernel::check_commit_identifiers::<(), (), (), Infallible>(&commit).is_ok());
    }

    let over_limit_source_ownership = CellCommit::<(), (), ()> {
        reads: vec![FactExpectation {
            key: FactKey::new(
                norn_kernel::snapshot_observe::SOURCE_OWNERSHIP_TYPE,
                "k".repeat(MAX_TOMBSTONE_KEY_BYTES + 1),
            ),
            expected: FactVersion(1),
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        norn_kernel::check_commit_identifiers::<(), (), (), Infallible>(
            &over_limit_source_ownership
        ),
        Err(CommitError::IdentifierTooLong {
            field: "object key",
            actual_bytes,
            max_bytes: MAX_TOMBSTONE_KEY_BYTES,
        }) if actual_bytes == MAX_TOMBSTONE_KEY_BYTES + 1
    ));
}

fn link(n: u8, expected: u64, add: bool) -> LinkWrite {
    LinkWrite {
        key: law_link_key(n),
        expected: FactVersion(expected),
        mutation: if add {
            LinkMutation::Add {
                values: norn_kernel::cell_host::JsonObject::new(),
            }
        } else {
            LinkMutation::Remove
        },
    }
}

#[test]
fn memory_cell_rejects_undeclared_empty_link_read() {
    let mut cell = LawCell::sim_new();
    let source = law_key(0);
    let error = cell
        .commit(CellCommit {
            link_reads: vec![LinkReadExpectation {
                source: source.clone(),
                link: LAW_STRAY.to_owned(),
                targets: Vec::new(),
            }],
            ..CellCommit::empty()
        })
        .expect_err("an empty read set must still name a declared link");

    assert_eq!(
        error,
        CommitError::InvalidLinkRead {
            source_key: source,
            link: LAW_STRAY.to_owned(),
        }
    );
}

#[test]
fn memory_cell_link_read_preserves_concrete_subtype_source_and_cas() {
    let mut cell = LawCell::sim_new();
    cell.commit(CellCommit {
        writes: vec![put(0, 0, 0), put(2, 0, 0)],
        links: vec![link(2, 0, true)],
        ..CellCommit::empty()
    })
    .expect("creates a child source and its declared relation");

    let source = law_key(2);
    let targets = cell
        .read_link_targets(&source, LAW_LINK, 4, 1024)
        .expect("reads link targets")
        .expect("the bounded link set is available");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].key.source, source);
    assert_eq!(targets[0].key.link, LAW_LINK);
    assert_eq!(targets[0].key.target, law_key(0));
    assert_eq!(targets[0].version, FactVersion(1));

    cell.commit(CellCommit {
        link_reads: vec![LinkReadExpectation {
            source,
            link: LAW_LINK.to_owned(),
            targets,
        }],
        ..CellCommit::empty()
    })
    .expect("the returned concrete source identity canonicalizes for CAS");
}

#[test]
fn cell_host_check_commit_link_refusal_order() {
    let commit = |writes: Vec<FactWrite>, links: Vec<LinkWrite>| CellCommit::<(), (), ()> {
        writes,
        links,
        ..CellCommit::empty()
    };
    // Duplicate links are a shape refusal, before compare-and-swap.
    assert_eq!(
        fixed(&commit(vec![], vec![link(0, 9, true), link(0, 9, true)])),
        Err(CommitError::DuplicateLink {
            key: Box::new(law_link_key(0)),
        })
    );
    // Object compare-and-swap before link compare-and-swap.
    assert!(matches!(
        fixed(&commit(vec![put(0, 9, 0)], vec![link(0, 9, true)])),
        Err(CommitError::Conflict { .. })
    ));
    assert_eq!(
        fixed(&commit(vec![], vec![link(0, 9, true)])),
        Err(CommitError::LinkConflict {
            key: Box::new(law_link_key(0)),
            expected: FactVersion(9),
            current: FactVersion(1),
        })
    );
    // Link 2 is k2 -rel-> k0, and k2 does not exist: the add would dangle,
    // unless this commit creates k2. A remove never dangles.
    assert_eq!(
        fixed(&commit(vec![], vec![link(2, 1, true)])),
        Err(CommitError::DanglingLink {
            key: Box::new(law_link_key(2)),
            missing: law_key(2),
        })
    );
    assert!(fixed(&commit(vec![], vec![link(2, 1, false)])).is_ok());
    assert_eq!(
        fixed(&commit(vec![put(2, 1, 0)], vec![link(2, 1, true)])),
        Ok(CommitVersions {
            written: vec![WrittenVersion {
                key: law_key(2),
                version: FactVersion(2),
            }],
            linked: vec![WrittenLinkVersion {
                key: law_link_key(2),
                version: FactVersion(2),
            }],
        })
    );
    // Deleting an endpoint in the same commit makes the add dangle.
    let delete = FactWrite {
        key: law_key(0),
        expected: FactVersion(1),
        mutation: FactMutation::Delete,
    };
    assert_eq!(
        fixed(&commit(vec![delete], vec![link(0, 1, true)])).map_err(|error| error.code()),
        Err("commit.dangling_link")
    );
    // Deleting an object with a live link dangles it, unless the same
    // commit removes the link.
    let delete_k1 = || FactWrite {
        key: law_key(1),
        expected: FactVersion(1),
        mutation: FactMutation::Delete,
    };
    assert_eq!(
        fixed(&commit(vec![delete_k1()], vec![])),
        Err(CommitError::DanglingLink {
            key: Box::new(law_link_key(4)),
            missing: law_key(1),
        })
    );
    assert!(fixed(&commit(vec![delete_k1()], vec![link(4, 1, false)])).is_ok());
    // #3018: an undeclared link is refused before anything else; so is a
    // second `owner` (to-one) from one source.
    assert_eq!(
        fixed(&commit(vec![], vec![link(18, 1, true)])).map_err(|error| error.code()),
        Err("commit.link_target")
    );
    // Links 9 and 12: k0 -owner-> k0 and k0 -owner-> k1.
    assert_eq!(
        fixed(&commit(vec![], vec![link(9, 1, true), link(12, 1, true)])),
        Err(CommitError::LinkCardinality {
            key: Box::new(law_link_key(9)),
            endpoint: law_key(0),
        })
    );
    // `law.Thing/k2` and `law.Child/k2` are one object.
    let alias = FactExpectation {
        key: law_alias(2),
        expected: FactVersion(1),
    };
    assert_eq!(
        fixed(&CellCommit {
            reads: vec![alias],
            writes: vec![put(2, 1, 0)],
            ..CellCommit::empty()
        }),
        Err(CommitError::DuplicateKey { key: law_key(2) })
    );
    // k1 is a live `law.Thing`: it is not written as a `law.Child`.
    let child = FactWrite {
        key: FactKey::new(LAW_CHILD, "k1"),
        expected: FactVersion(1),
        mutation: FactMutation::Put {
            values: law_values(1),
        },
    };
    assert_eq!(
        fixed(&commit(vec![child], vec![])).map_err(|error| error.code()),
        Err("commit.type_mismatch")
    );
}

#[test]
fn cell_host_link_overrides_bind_through_a_supertype_name() {
    check_link_override(&mut LawCell::new(law_responder).with_types(override_object_types()));
}

#[test]
fn cell_host_links_commit_with_the_turn_and_keep_their_version() {
    let mut cell = LawCell::sim_new();
    // Objects and the link between them in one commit.
    let receipt = cell
        .commit(CellCommit {
            writes: vec![put(0, 0, 1), put(1, 0, 2)],
            links: vec![link(3, 0, true)],
            events: vec![1],
            ..CellCommit::empty()
        })
        .unwrap();
    assert_eq!(
        receipt.linked,
        vec![WrittenLinkVersion {
            key: law_link_key(3),
            version: FactVersion(1),
        }]
    );
    assert_eq!(cell.ledger()[0].linked, receipt.linked);
    assert!(cell.load_link(&law_link_key(3)).unwrap().exists());
    // Removed, the link keeps its version; a stale re-add is refused and
    // changes nothing.
    cell.commit(CellCommit {
        links: vec![link(3, 1, false)],
        ..CellCommit::empty()
    })
    .unwrap();
    assert_eq!(
        cell.load_link(&law_link_key(3)).unwrap(),
        LinkState {
            version: FactVersion(2),
            values: None,
        }
    );
    let before = cell.ledger().len();
    assert!(matches!(
        cell.commit(CellCommit {
            links: vec![link(3, 0, true)],
            events: vec![9],
            ..CellCommit::empty()
        }),
        Err(CommitError::LinkConflict { .. })
    ));
    assert_eq!(cell.ledger().len(), before);
    assert!(!cell.load_link(&law_link_key(3)).unwrap().exists());
}

#[test]
fn cell_host_check_commit_refusal_order() {
    let commit =
        |reads: Vec<FactExpectation>, writes: Vec<FactWrite>, consumes| CellCommit::<(), (), ()> {
            consumes,
            reads,
            writes,
            ..CellCommit::empty()
        };
    let stale = FactExpectation {
        key: law_key(0),
        expected: FactVersion(9),
    };
    let missing = Some(Consumed::Observation {
        id: law_observed(0, 0).id,
    });
    let check = |commit: &CellCommit<(), (), ()>| fixed(commit).map(|versions| versions.written);

    // Shape before delivery before compare-and-swap.
    assert!(matches!(
        check(&commit(
            vec![stale.clone()],
            vec![put(0, 1, 0)],
            missing.clone()
        )),
        Err(CommitError::DuplicateKey { .. })
    ));
    assert!(matches!(
        check(&commit(vec![stale.clone()], vec![], missing)),
        Err(CommitError::NotPending { .. })
    ));
    // Reads are checked before writes.
    assert_eq!(
        check(&commit(vec![stale], vec![put(1, 0, 0)], None)),
        Err(CommitError::Conflict {
            key: law_key(0),
            expected: FactVersion(9),
            current: FactVersion(1),
        })
    );
    assert_eq!(
        check(&commit(vec![], vec![put(1, 1, 0)], None)),
        Ok(vec![WrittenVersion {
            key: law_key(1),
            version: FactVersion(2),
        }])
    );
    assert_eq!(
        check(&commit(vec![], vec![put(1, u64::MAX, 0)], None)).map_err(|error| error.code()),
        Err("commit.conflict")
    );
}

#[test]
fn cell_host_wire_shape_and_default_lane() {
    let commit: CellCommit<LawEvent, LawCommand, u8> = CellCommit {
        consumes: Some(Consumed::Timer {
            id: law_timer_id(0),
            incarnation: LedgerSeq(5),
        }),
        reads: vec![],
        writes: vec![FactWrite {
            key: FactKey::new("hr.Employee", "EMP-1"),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Delete,
        }],
        events: vec![1],
        effects: vec![Effect::CancelTimer(law_timer_id(0))],
        links: vec![],
        link_reads: Vec::new(),
    };
    let json = serde_json::to_value(&commit).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "consumes": { "timer": { "id": "t0", "incarnation": 5 } },
            "reads": [],
            "writes": [{
                "key": { "objectType": "hr.Employee", "key": "EMP-1" },
                "expected": 0,
                "mutation": "delete"
            }],
            "events": [1],
            "effects": [{ "cancel_timer": "t0" }]
        })
    );
    let back: CellCommit<LawEvent, LawCommand, u8> = serde_json::from_value(json).unwrap();
    assert_eq!(back, commit);

    let cell = LawCell::sim_new();
    assert_eq!(cell.serialize_by(&law_key(0)), SerializationLane::Cell);
}

#[test]
fn cell_host_reissued_command_never_executes_twice() {
    let mut cell = LawCell::sim_new();
    let enqueue = |generation| CellCommit {
        effects: vec![Effect::Perform(law_command(0, generation, 7))],
        ..CellCommit::empty()
    };
    cell.commit(enqueue(1)).unwrap();
    let performed = cell.perform().unwrap().unwrap();
    // Re-issued at the same generation while its result is pending: no-op.
    cell.commit(enqueue(1)).unwrap();
    assert_eq!(cell.perform().unwrap(), Some(performed.clone()));
    assert_eq!(cell.executions(), 1);
    cell.commit(CellCommit::empty().consuming(performed.consumed()))
        .unwrap();
    // Re-issued at the same or an older generation after completion: no-op.
    cell.commit(enqueue(1)).unwrap();
    cell.commit(enqueue(0)).unwrap();
    assert_eq!(cell.perform().unwrap(), None);
    // A newer generation executes again.
    cell.commit(enqueue(2)).unwrap();
    assert_eq!(cell.perform().unwrap().unwrap().generation.0, 2);
    assert_eq!(cell.executions(), 2);
}

/// Live subtype identities must retain owned-row protection through ancestor keys.
#[test]
fn raw_links_cannot_address_owned_children_through_ancestors() {
    struct OwnedView;
    impl CommitView for OwnedView {
        type Error = Infallible;
        fn pending(&self, _: &Consumed) -> Result<bool, Infallible> {
            Ok(true)
        }
        fn version(&self, _: &FactKey) -> Result<FactVersion, Infallible> {
            Ok(FactVersion(1))
        }
        fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
            Ok(Some(ObjectTypeName(
                if key.object_type.as_str() == "test.Base" {
                    "test.Child".to_owned()
                } else {
                    key.object_type.0.clone()
                },
            )))
        }
        fn link_version(&self, _: &LinkKey) -> Result<FactVersion, Infallible> {
            Ok(FactVersion(1))
        }
        fn links_of(&self, _: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
            panic!("protected links must be refused before adjacency reads")
        }
    }
    let types: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        {"name": "test.Base"},
        {"name": "test.Child", "extends": "test.Base"},
        {"name": "test.Parent", "links": [
            {"id": "rows", "target": "test.Child", "cardinality": {"to": "many", "from": "one"}}
        ], "ownedCollections": [
            {"id": "rows", "childType": "test.Child", "rowIdProperty": "id",
             "orderingKey": "idx", "maxRows": 2, "cascadeOnParentDelete": true}
        ]},
        {"name": "test.Other", "links": [
            {"id": "external", "target": "test.Base", "cardinality": {"to": "many", "from": "many"}}
        ]}
    ]))
    .unwrap();
    let catalog = TypeCatalog::new(&types);
    for mutation in [
        LinkMutation::Add {
            values: norn_kernel::cell_host::JsonObject::new(),
        },
        LinkMutation::Remove,
    ] {
        let commit = CellCommit::<(), (), ()> {
            links: vec![LinkWrite {
                key: LinkKey::new(
                    FactKey::new("test.Other", "other"),
                    "external",
                    FactKey::new("test.Base", "row"),
                ),
                expected: FactVersion(1),
                mutation,
            }],
            ..CellCommit::empty()
        };
        assert!(matches!(
            check_commit(&commit, &CommitModel::new(&catalog), &OwnedView),
            Err(CommitError::OwnedRowsProtected { .. })
        ));
    }
}

#[test]
fn ordinary_scalar_put_does_not_read_unrelated_adjacency() {
    struct NoAdjacency;
    impl CommitView for NoAdjacency {
        type Error = Infallible;
        fn pending(&self, consumed: &Consumed) -> Result<bool, Infallible> {
            FixedView.pending(consumed)
        }
        fn version(&self, key: &FactKey) -> Result<FactVersion, Infallible> {
            FixedView.version(key)
        }
        fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
            FixedView.concrete(key)
        }
        fn link_version(&self, key: &LinkKey) -> Result<FactVersion, Infallible> {
            FixedView.link_version(key)
        }
        fn links_of(&self, _: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
            panic!("ordinary scalar updates must not load adjacency")
        }
    }
    let types = law_object_types();
    let catalog = TypeCatalog::new(&types);
    // k1 has a live relation in FixedView; that neighborhood is irrelevant.
    let commit = CellCommit::<(), (), ()> {
        writes: vec![put(1, 1, 7)],
        ..CellCommit::empty()
    };
    assert!(check_commit(&commit, &CommitModel::new(&catalog), &NoAdjacency).is_ok());
}

#[test]
fn scalar_put_still_checks_required_inherited_owned_links() {
    struct ParentSubtypeView;
    impl CommitView for ParentSubtypeView {
        type Error = Infallible;
        fn pending(&self, consumed: &Consumed) -> Result<bool, Infallible> {
            FixedView.pending(consumed)
        }
        fn version(&self, key: &FactKey) -> Result<FactVersion, Infallible> {
            FixedView.version(key)
        }
        fn concrete(&self, _: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
            Ok(Some(ObjectTypeName("law.ParentSubtype".to_owned())))
        }
        fn link_version(&self, key: &LinkKey) -> Result<FactVersion, Infallible> {
            FixedView.link_version(key)
        }
        fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
            FixedView.links_of(key)
        }
        fn has_link_target(
            &self,
            source: &FactKey,
            link: &str,
            excluded: &std::collections::BTreeSet<FactKey>,
        ) -> Result<Option<bool>, Infallible> {
            Ok(Some(FixedView.links_of(source)?.iter().any(|key| {
                key.source == *source && key.link == link && !excluded.contains(&key.target)
            })))
        }
    }
    let mut types = law_object_types();
    let parent = types
        .iter_mut()
        .find(|ty| ty.name.as_str() == LAW_THING)
        .unwrap();
    parent.owned_collections.push(
        serde_json::from_value(serde_json::json!({
            "id": LAW_LINK, "childType": LAW_CHILD, "rowIdProperty": "id", "orderingKey": "idx",
            "maxRows": 2, "cascadeOnParentDelete": true
        }))
        .unwrap(),
    );
    parent
        .links
        .iter_mut()
        .find(|link| link.id.as_str() == LAW_LINK)
        .unwrap()
        .cardinality
        .required = true;
    types.push(
        serde_json::from_value(serde_json::json!({
            "name": "law.ParentSubtype", "extends": LAW_THING
        }))
        .unwrap(),
    );
    let catalog = TypeCatalog::new(&types);
    let commit = |key: &str| CellCommit::<(), (), ()> {
        writes: vec![FactWrite {
            key: FactKey::new("law.ParentSubtype", key),
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: law_values(7),
            },
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        check_commit(
            &commit("k0"),
            &CommitModel::new(&catalog),
            &ParentSubtypeView
        ),
        Err(CommitError::LinkRequired { .. })
    ));
    assert!(
        check_commit(
            &commit("k1"),
            &CommitModel::new(&catalog),
            &ParentSubtypeView
        )
        .is_ok()
    );
}
