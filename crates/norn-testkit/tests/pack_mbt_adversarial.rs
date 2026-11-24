//! Adversarial and direct-observer tests for `pack_mbt` and `ObservedSqliteCell` (#1656).
//!
//! Covers every negative and edge-case invariant:
//! - Multi-object commits, creations (not counted as transitions), and self-loop
//!   transitions (`from == to`) on [`ObservedSqliteCell`].
//! - Quarantined observed ingestion (`commit_observed`) recording violated laws
//!   as violations and never as holds.
//! - Typed `DecisionRefusal::ImmutableStateWrite` recording.
//! - Real SQLite `LAW-EXAMPLE-1` runtime parity failure (checker vs cell model
//!   disagreement).
//! - Fault-injected DST catching dropped mutations with a minimized trace.
//! - Virtual-time due timer delivery across clean reopen (`#176`).
//! - Static gap edge cases: dangling compensation, name-only reversal question,
//!   and dual-rule `SoD` bypass.

use std::path::PathBuf;
use std::sync::Arc;

use norn_cell::{
    CellError, DomainCoverageRecorder, ImmutableRefusalKey, Model, ObservedSqliteCell, SqliteCell,
    StoreOptions, TransitionKey,
};
use norn_ir::{
    ArtifactEnvelope, Decimal, DecisionOutput, FactChange, InstanceProvenance, MappingVersion,
    ObjectTypeName, PropertyId, RuntimeObject, RuntimeValue, SourceRecordRef, TransitionId,
    UtcTimestamp,
};
use norn_kernel::cell_host::{FactKey, FactMutation, FactState, JsonObject};
use norn_kernel::pack_mbt_support::LawExampleVerdict as Verdict;
use norn_kernel::snapshot_observe::{ObservedSnapshot, commit_observed_snapshot};
use norn_kernel::{
    CellCommit, CellHost, CommitError, CommitReceipt, DecisionTurn, DueTimer, FactEvent, LinkKey,
    LinkState, LogicalTime, ObservationEvent, Observed, Performed, TypeCatalog, World,
    commit_decision, load_slice,
};
use norn_testkit::pack_mbt::driver::run_pack_mbt;
use norn_testkit::pack_mbt::dst::{PackDstConfig, run_pack_dst};
use norn_testkit::pack_mbt::static_gaps::{GapSeverity, StructuralGap, analyze_static_gaps};
use norn_testkit::pack_mbt::{
    AdmittedPack, EvidenceLevel, LawExampleCase, PackHost, action_call_object_type,
};
use serde_json::json;
use tempfile::TempDir;

type FactSqliteCell = SqliteCell<FactEvent, JsonObject, JsonObject>;
type ObsSqliteCell = SqliteCell<ObservationEvent, JsonObject, JsonObject>;

fn runtime_obj(pairs: &[(&str, RuntimeValue)]) -> RuntimeObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn dec(text: &str) -> Result<RuntimeValue, String> {
    Decimal::parse_wire(text)
        .map(RuntimeValue::Number)
        .map_err(|err| format!("decimal {text}: {err}"))
}

fn self_loop_entity_envelope() -> Result<ArtifactEnvelope, String> {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": "obs.Ticket.entity", "version": "1.0.0",
        "kind": "entity", "namespace": "obs", "name": "Ticket", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "obs.Ticket",
                "identity": [{"name": "primary", "properties": ["ticket_id"]}],
                "properties": [
                    {"id": "ticket_id", "type": {"kind": "text"}},
                    {"id": "amount", "type": {"kind": "decimal", "scale": 2}},
                    {
                        "id": "status",
                        "default": "open",
                        "type": {
                            "kind": "enum",
                            "values": [
                                {"value": "open"},
                                {"value": "closed"}
                            ]
                        }
                    }
                ],
                "lifecycles": [{
                    "property": "status",
                    "initial": ["open"],
                    "terminal": ["closed"],
                    "immutable": ["closed"],
                    "transitions": [
                        {"id": "touch", "from": ["open"], "to": "open"},
                        {"id": "close", "from": ["open"], "to": "closed"}
                    ]
                }]
            }
        }
    }))
    .map_err(|err| format!("ticket entity: {err}"))
}

fn positive_amount_law_envelope() -> Result<ArtifactEnvelope, String> {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": "obs.Ticket.positive_amount.law", "version": "1.0.0",
        "kind": "law", "namespace": "obs", "name": "Positive amount", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {
            "lawVersion": "0.1",
            "id": "obs.Ticket.positive_amount",
            "owner": "obs",
            "severity": "error",
            "rationale": "Open tickets carry a positive amount.",
            "provenance": {"source": "authoring_sdk"},
            "scope": {"root": {"kind": "entity", "id": "obs.Ticket"}, "objectTypes": ["obs.Ticket"]},
            "subject": "obs.Ticket",
            "condition": "self.status == 'open'",
            "modality": {"kind": "always"},
            "response": {"kind": "predicate", "expression": "self.amount > 0"}
        }
    }))
    .map_err(|err| format!("ticket law: {err}"))
}

fn seed_two_open_tickets(
    cell: &mut ObservedSqliteCell<FactEvent, JsonObject, JsonObject>,
    catalog: &TypeCatalog<'_>,
) -> Result<(FactKey, FactKey), String> {
    let k1 = FactKey::new("obs.Ticket", "t-1");
    let k2 = FactKey::new("obs.Ticket", "t-2");
    let slice = load_slice(cell, [k1.clone(), k2.clone()]).map_err(|e| format!("{e}"))?;
    let create_out = DecisionOutput {
        facts: vec![
            FactChange::Create {
                object_type: ObjectTypeName("obs.Ticket".to_owned()),
                key: "t-1".to_owned(),
                values: runtime_obj(&[
                    ("ticket_id", RuntimeValue::String("t-1".to_owned())),
                    ("amount", dec("10.00")?),
                    ("status", RuntimeValue::String("open".to_owned())),
                ]),
            },
            FactChange::Create {
                object_type: ObjectTypeName("obs.Ticket".to_owned()),
                key: "t-2".to_owned(),
                values: runtime_obj(&[
                    ("ticket_id", RuntimeValue::String("t-2".to_owned())),
                    ("amount", dec("20.00")?),
                    ("status", RuntimeValue::String("open".to_owned())),
                ]),
            },
        ],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn1 = DecisionTurn {
        id: "t-create".to_owned(),
        now: LogicalTime::EPOCH,
        world: World::Main,
        consumes: None,
    };
    commit_decision(cell, catalog, &slice, &turn1, &create_out)
        .map_err(|e| format!("create: {e}"))?;
    Ok((k1, k2))
}

fn commit_self_loop_and_close(
    cell: &mut ObservedSqliteCell<FactEvent, JsonObject, JsonObject>,
    catalog: &TypeCatalog<'_>,
    k1: &FactKey,
    k2: &FactKey,
) -> Result<(), String> {
    let slice2 = load_slice(cell, [k1.clone(), k2.clone()]).map_err(|e| format!("{e}"))?;
    let tr_out = DecisionOutput {
        facts: vec![
            FactChange::Transition {
                object_type: ObjectTypeName("obs.Ticket".to_owned()),
                key: "t-1".to_owned(),
                lifecycle: PropertyId("status".to_owned()),
                transition: TransitionId("touch".to_owned()),
            },
            FactChange::Transition {
                object_type: ObjectTypeName("obs.Ticket".to_owned()),
                key: "t-2".to_owned(),
                lifecycle: PropertyId("status".to_owned()),
                transition: TransitionId("close".to_owned()),
            },
        ],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn2 = DecisionTurn {
        id: "t-tr".to_owned(),
        now: LogicalTime::EPOCH,
        world: World::Main,
        consumes: None,
    };
    commit_decision(cell, catalog, &slice2, &turn2, &tr_out)
        .map_err(|e| format!("transitions: {e}"))?;
    Ok(())
}

#[test]
fn observed_sqlite_cell_handles_multi_object_self_loop_and_immutable_refusal() -> Result<(), String>
{
    let envelopes = vec![
        self_loop_entity_envelope()?,
        positive_amount_law_envelope()?,
    ];
    let pack = AdmittedPack::from_envelopes("obs_test", &envelopes)?;
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);
    let model = Model::from_definitions_with_interfaces(
        &pack.object_types,
        &pack.property_types,
        &pack.interfaces,
    )
    .map_err(|e| format!("model: {e}"))?
    .with_governing_laws(&pack.law_envelopes)
    .map_err(|e| format!("laws: {e}"))?;
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let mut cell = FactSqliteCell::open_in_memory(model, "org/dev", |cmd| cmd.payload.clone())
        .map_err(|e| format!("open: {e}"))?
        .into_observed(Arc::clone(&recorder));

    let (k1, k2) = seed_two_open_tickets(&mut cell, &catalog)?;
    assert!(
        recorder.snapshot().transitions.is_empty(),
        "creations must never be credited as lifecycle transitions"
    );
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("obs.Ticket.positive_amount"),
        Some(&2)
    );

    commit_self_loop_and_close(&mut cell, &catalog, &k1, &k2)?;
    let snap = recorder.snapshot();
    assert_eq!(
        snap.transitions.get(&TransitionKey {
            object_type: "obs.Ticket".to_owned(),
            lifecycle: "status".to_owned(),
            transition: "touch".to_owned(),
            from: "open".to_owned(),
            to: "open".to_owned(),
        }),
        Some(&1),
        "self-loop transition must be recorded"
    );
    assert_eq!(
        snap.transitions.get(&TransitionKey {
            object_type: "obs.Ticket".to_owned(),
            lifecycle: "status".to_owned(),
            transition: "close".to_owned(),
            from: "open".to_owned(),
            to: "closed".to_owned(),
        }),
        Some(&1),
    );
    assert_eq!(snap.law_holds.get("obs.Ticket.positive_amount"), Some(&3));

    let slice3 = load_slice(&cell, [k2]).map_err(|e| format!("{e}"))?;
    let bad_set = DecisionOutput {
        facts: vec![FactChange::Set {
            object_type: ObjectTypeName("obs.Ticket".to_owned()),
            key: "t-2".to_owned(),
            property: PropertyId("amount".to_owned()),
            value: dec("99.00")?,
        }],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn3 = DecisionTurn {
        id: "t-imm".to_owned(),
        now: LogicalTime::EPOCH,
        world: World::Main,
        consumes: None,
    };
    let Err(refusal) = commit_decision(&mut cell, &catalog, &slice3, &turn3, &bad_set) else {
        return Err("expected ImmutableStateWrite refusal".to_owned());
    };
    recorder.record_decision_refusal(&refusal);
    assert_eq!(
        recorder
            .snapshot()
            .immutable_refusals
            .get(&ImmutableRefusalKey {
                object_type: "obs.Ticket".to_owned(),
                lifecycle: "status".to_owned(),
                state: "closed".to_owned(),
            }),
        Some(&1)
    );
    Ok(())
}

#[test]
fn observed_sqlite_cell_records_quarantined_observation_violation_not_hold() -> Result<(), String> {
    let envelopes = vec![
        self_loop_entity_envelope()?,
        positive_amount_law_envelope()?,
    ];
    let pack = AdmittedPack::from_envelopes("obs_quarantine", &envelopes)?;
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);
    let model = Model::from_definitions_with_interfaces(
        &pack.object_types,
        &pack.property_types,
        &pack.interfaces,
    )
    .map_err(|e| format!("model: {e}"))?
    .with_governing_laws(&pack.law_envelopes)
    .map_err(|e| format!("laws: {e}"))?;
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let mut cell = ObsSqliteCell::open_in_memory(model, "org/dev", |cmd| cmd.payload.clone())
        .map_err(|e| format!("open: {e}"))?
        .into_observed(Arc::clone(&recorder));

    let key = FactKey::new("obs.Ticket", "t-bad");
    let slice = load_slice(&cell, [key.clone()]).map_err(|e| format!("{e}"))?;
    let values = runtime_obj(&[
        ("ticket_id", RuntimeValue::String("t-bad".to_owned())),
        ("amount", dec("0.00")?),
    ]);
    let provenance = InstanceProvenance {
        source: SourceRecordRef::new("Jira", "TICKET-1"),
        mapping_version: MappingVersion("jira.ticket@1".to_owned()),
        transform: None,
        observed_at: None,
        recorded_at: UtcTimestamp("2026-01-15T12:00:00Z".to_owned()),
        source_fields: Vec::new(),
        markings: Vec::new(),
        upstream: Vec::new(),
    };
    let turn = DecisionTurn {
        id: "obs-1".to_owned(),
        now: LogicalTime::EPOCH,
        world: World::Main,
        consumes: None,
    };
    commit_observed_snapshot(
        &mut cell,
        &catalog,
        &slice,
        &turn,
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .map_err(|e| format!("commit_observed_snapshot: {e}"))?;

    let snap = recorder.snapshot();
    assert_eq!(
        snap.law_violations.get("obs.Ticket.positive_amount"),
        Some(&1),
        "quarantined law violation must be recorded as a violation"
    );
    assert!(
        !snap.law_holds.contains_key("obs.Ticket.positive_amount"),
        "quarantined law violation must NEVER be credited as a hold"
    );
    Ok(())
}

struct BareHost {
    _dir: TempDir,
    db_path: PathBuf,
    model: Model,
    cell: Option<ObservedSqliteCell<FactEvent, JsonObject, JsonObject>>,
    recorder: Arc<DomainCoverageRecorder>,
    drop_set_mutations: bool,
    corrupt_linked_create: bool,
}

impl BareHost {
    fn open(
        pack: &AdmittedPack,
        attach_laws: bool,
        drop_set_mutations: bool,
    ) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
        let db_path = dir.path().join("adv.sqlite");
        let mut all_types = pack.object_types.clone();
        all_types.push(action_call_object_type());
        let base = Model::from_definitions_with_interfaces(
            &all_types,
            &pack.property_types,
            &pack.interfaces,
        )
        .map_err(|e| format!("{e}"))?;
        let model = if attach_laws {
            base.with_governing_laws(&pack.law_envelopes)
                .map_err(|e| format!("{e}"))?
        } else {
            base
        };
        let recorder = Arc::new(DomainCoverageRecorder::new());
        let inner = FactSqliteCell::open(
            &db_path,
            model.clone(),
            StoreOptions::default(),
            "org/dev",
            |cmd| cmd.payload.clone(),
        )
        .map_err(|e| format!("{e}"))?;
        Ok(Self {
            _dir: dir,
            db_path,
            model,
            cell: Some(inner.into_observed(Arc::clone(&recorder))),
            recorder,
            drop_set_mutations,
            corrupt_linked_create: false,
        })
    }

    fn with_corrupt_linked_create(mut self) -> Self {
        self.corrupt_linked_create = true;
        self
    }

    fn cell_ref(
        &self,
    ) -> Result<&ObservedSqliteCell<FactEvent, JsonObject, JsonObject>, CellError> {
        self.cell
            .as_ref()
            .ok_or_else(|| CellError::Engine("closed".to_owned()))
    }

    fn cell_mut(
        &mut self,
    ) -> Result<&mut ObservedSqliteCell<FactEvent, JsonObject, JsonObject>, CellError> {
        self.cell
            .as_mut()
            .ok_or_else(|| CellError::Engine("closed".to_owned()))
    }
}

impl CellHost for BareHost {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell_ref()?.load(key)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell_ref()?.load_link(key)
    }
    fn commit(
        &mut self,
        mut commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        if self.drop_set_mutations
            && commit
                .events
                .iter()
                .any(|ev| matches!(ev, FactEvent::Set { .. }))
        {
            // Fault injection: silently drop the FactWrite (and its aggregate
            // Modified commitment) while still appending the ledger event and
            // returning Ok(receipt), proving the DST oracle catches lost writes.
            commit.writes.clear();
            commit
                .events
                .retain(|ev| !matches!(ev, FactEvent::Modified { .. }));
        }
        if self.corrupt_linked_create && !commit.links.is_empty() {
            for write in &mut commit.writes {
                if write.key.object_type.as_str() == "lnk.Child"
                    && let FactMutation::Put { values } = &mut write.mutation
                {
                    values.insert(
                        "note".to_owned(),
                        RuntimeValue::String("CORRUPTED".to_owned()),
                    );
                }
            }
        }
        self.cell_mut().map_err(CommitError::Host)?.commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, CellError> {
        self.cell_mut()?.perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, CellError> {
        self.cell_mut()?.observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, CellError> {
        self.cell_mut()?.clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell_mut()?.publish(receipt)
    }
}

impl PackHost for BareHost {
    fn ledger_len(&self) -> usize {
        self.cell
            .as_ref()
            .and_then(|c| c.inner().store().read().receipts().ok())
            .map_or(0, |r| r.len())
    }
    fn logical_now(&self) -> LogicalTime {
        self.cell
            .as_ref()
            .map_or(LogicalTime::EPOCH, |c| c.inner().now())
    }
    fn advance_clock(&mut self, delta_millis: u64) {
        if let Some(cell) = self.cell.as_mut() {
            let next = cell.inner().now().as_millis().saturating_add(delta_millis);
            cell.inner_mut().advance_to(LogicalTime::from_millis(next));
        }
    }
    fn verify_ledger(&self) -> Result<(), String> {
        self.cell_ref()
            .map_err(|e| format!("{e}"))?
            .inner()
            .store()
            .read()
            .verify_ledger()
            .map(|_| ())
            .map_err(|e| format!("{e}"))
    }
    fn reopen(&mut self) -> Result<(), String> {
        let prev = self.logical_now();
        self.cell = None;
        let mut reopened = FactSqliteCell::open(
            &self.db_path,
            self.model.clone(),
            StoreOptions::default(),
            "org/dev",
            |cmd| cmd.payload.clone(),
        )
        .map_err(|e| format!("{e}"))?;
        reopened.advance_to(prev);
        reopened
            .store()
            .read()
            .verify_ledger()
            .map_err(|e| format!("{e}"))?;
        self.cell = Some(reopened.into_observed(Arc::clone(&self.recorder)));
        Ok(())
    }
    fn has_governing_law(&self, object_type: &str, artifact_id: &str) -> bool {
        self.cell.as_ref().is_some_and(|cell| {
            cell.inner()
                .store()
                .model()
                .governing_laws(object_type)
                .is_ok_and(|laws| laws.iter().any(|env| env.artifact_id == artifact_id))
        })
    }
}

#[test]
fn law_example_parity_catches_real_sqlite_runtime_disagreement() -> Result<(), String> {
    let envelopes = vec![
        self_loop_entity_envelope()?,
        positive_amount_law_envelope()?,
    ];
    let pack =
        AdmittedPack::from_envelopes("runtime_mismatch", &envelopes)?.with_law_examples(vec![
            LawExampleCase {
                name: "zero amount should violate in runtime too".to_owned(),
                law_id: "obs.Ticket.positive_amount".to_owned(),
                subject: runtime_obj(&[
                    ("ticket_id", RuntimeValue::String("T-BAD".to_owned())),
                    ("amount", dec("0.00")?),
                    ("status", RuntimeValue::String("open".to_owned())),
                ]),
                expected: Verdict::Violated,
            },
        ]);
    // Open SQLite WITHOUT attaching governing laws so checker returns Violated while runtime commits Ok!
    let err = run_pack_mbt(&pack, |_| BareHost::open(&pack, false, false));
    let msg = err
        .err()
        .ok_or_else(|| "expected runtime parity mismatch error".to_owned())?;
    assert!(
        msg.contains("parity mismatch"),
        "expected parity mismatch diagnostic, got: {msg}"
    );
    Ok(())
}

#[test]
fn dst_oracle_catches_fault_injected_dropped_mutation_and_shrinks() -> Result<(), String> {
    let envelopes = vec![
        self_loop_entity_envelope()?,
        positive_amount_law_envelope()?,
    ];
    let pack = AdmittedPack::from_envelopes("fault_dst", &envelopes)?;
    let err = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [0x11; 32],
            cases: 24,
            max_steps: 12,
            entities_per_type: 1,
        },
        |_| BareHost::open(&pack, true, true),
    );
    let msg = err
        .err()
        .ok_or_else(|| "expected fault-injected dropped mutation to fail DST".to_owned())?;
    assert!(
        (msg.contains("did not persist") || msg.contains("did not bump version"))
            && msg.contains("minimal sequence"),
        "expected minimized persistence/version failure, got: {msg}"
    );
    Ok(())
}

#[test]
fn virtual_clock_delivers_due_timer_across_clean_reopen() -> Result<(), String> {
    let envelopes = vec![self_loop_entity_envelope()?];
    let pack = AdmittedPack::from_envelopes("timer_reopen", &envelopes)?;
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);
    let mut host = BareHost::open(&pack, false, false)?;

    let key = FactKey::new("obs.Ticket", "t-timer");
    let slice = load_slice(&host, [key]).map_err(|e| format!("{e}"))?;
    let output = DecisionOutput {
        facts: vec![FactChange::Create {
            object_type: ObjectTypeName("obs.Ticket".to_owned()),
            key: "t-timer".to_owned(),
            values: runtime_obj(&[
                ("ticket_id", RuntimeValue::String("t-timer".to_owned())),
                ("amount", dec("15.00")?),
                ("status", RuntimeValue::String("open".to_owned())),
            ]),
        }],
        links: Vec::new(),
        commands: Vec::new(),
        timers: vec![runtime_obj(&[
            ("timerId", RuntimeValue::String("escalate-1".to_owned())),
            ("afterMillis", RuntimeValue::from(5_000_i64)),
        ])],
    };
    let turn = DecisionTurn {
        id: "timer-create".to_owned(),
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    commit_decision(&mut host, &catalog, &slice, &turn, &output).map_err(|e| format!("{e}"))?;
    assert!(host.clock().map_err(|e| format!("{e}"))?.is_none());

    host.reopen()?;
    host.advance_clock(4_999);
    assert!(host.clock().map_err(|e| format!("{e}"))?.is_none());
    host.advance_clock(1);
    let due = host.clock().map_err(|e| format!("{e}"))?.ok_or_else(|| {
        "expected due timer after clean reopen and exact clock advance".to_owned()
    })?;
    assert_eq!(due.timer.id.0, "escalate-1");
    host.verify_ledger()
}

#[test]
fn static_gap_detector_catches_dangling_comp_heuristic_reversal_and_dual_rule_sod_bypass()
-> Result<(), String> {
    let raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "fin.Payout.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "fin", "name": "Payout", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "fin.Payout",
                    "immutable": true,
                    "properties": [
                        {"id": "payout_id", "type": {"kind": "text"}},
                        {"id": "requester", "type": {"kind": "text"}},
                        {"id": "region", "type": {"kind": "text"}}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "fin.payout.reverse.action", "version": "1.0.0",
            "kind": "action", "namespace": "fin", "name": "Reverse payout", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "actionId": "fin.payout.reverse",
                "scopeRequired": ["orgId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {"type": "object", "properties": {"payout_id": {"type": "string"}}, "required": ["payout_id"]},
                "outputSchema": {"type": "object", "properties": {"payout_id": {"type": "string"}}, "required": ["payout_id"]},
                "writes": [{"entityKind": "fin.Payout", "change": "created"}],
                "compensation": {"actionId": "fin.payout.nonexistent"}
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "fin.access.policy", "version": "1.0.0",
            "kind": "policy", "namespace": "fin", "name": "Dual rule bypass", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "accessVersion": "0.1",
                "policyId": "fin.access",
                "rules": [
                    {
                        "ruleId": "reverse_with_sod",
                        "actions": ["fin.payout.reverse"],
                        "resourceType": "fin.Payout",
                        "scope": {"dimensions": ["org"]},
                        "allOf": [
                            {"role": {"anyOf": ["treasury"]}},
                            {"resource": {"name": "requester", "op": "ne", "value": {"principalAttribute": "subject_id"}}}
                        ]
                    },
                    {
                        "ruleId": "admin_bypass_without_sod",
                        "actions": ["fin.payout.reverse"],
                        "resourceType": "fin.Payout",
                        "scope": {"dimensions": ["org"]},
                        "allOf": [
                            {"role": {"anyOf": ["admin"]}},
                            {"resource": {"name": "region", "op": "ne", "value": {"principalAttribute": "department"}}}
                        ]
                    }
                ]
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let pack = AdmittedPack::from_envelopes("fin_edge", &envelopes)?;
    let report = analyze_static_gaps(&pack);

    // Dangling compensation does NOT verify; name-only match becomes an explicit Question!
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::ImmutableFinancialUnverifiedReversal { object_type, .. }
            if object_type == "fin.Payout" && g.severity() == GapSeverity::Question
    )));
    // Second permit rule lacks requester != subject_id SoD -> still flagged as SoD Question!
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::HighImpactActionMissingSod { action_id, .. }
            if action_id == "fin.payout.reverse"
    )));
    Ok(())
}

fn build_gap_pack() -> Result<AdmittedPack, String> {
    let raw_envelopes = vec![
        json!({
            "irVersion": "0.1", "artifactId": "gap.JournalEntry.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "gap", "name": "JournalEntry", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "gap.JournalEntry",
                    "immutable": true,
                    "properties": [
                        {"id": "entry_id", "type": {"kind": "text"}},
                        {"id": "orphan_flag", "type": {"kind": "boolean"}},
                        {
                            "id": "status",
                            "default": "open",
                            "type": {
                                "kind": "enum",
                                "values": [
                                    {"value": "open"},
                                    {"value": "stuck"},
                                    {"value": "ghost"},
                                    {"value": "closed"}
                                ]
                            }
                        }
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["closed"],
                        "immutable": ["closed"],
                        "transitions": [
                            {"id": "stall", "from": ["open"], "to": "stuck", "guards": ["self.orphan_flag"]},
                            {"id": "close", "from": ["open"], "to": "closed"}
                        ]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "gap.journal.post.action", "version": "1.0.0",
            "kind": "action", "namespace": "gap", "name": "Post journal", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "actionId": "gap.journal.post",
                "scopeRequired": ["orgId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"entry_id": {"type": "string"}},
                    "required": ["entry_id"]
                },
                "outputSchema": {"type": "object", "additionalProperties": false, "properties": {"entry_id": {"type": "string"}}, "required": ["entry_id"]},
                "effectProgram": {
                    "kind": "upsert_input_object",
                    "objectIdField": "entry_id",
                    "objectKind": "gap.JournalEntry",
                    "entityArtifactId": "gap.JournalEntry.entity",
                    "entityArtifactVersion": "1.0.0",
                    "writeMode": {"kind": "create_only"}
                },
                "writes": [{"entityKind": "gap.JournalEntry", "change": "created"}]
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw_envelopes
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|err| format!("envelope: {err}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("gap_pack", &envelopes)
}

#[test]
fn static_gap_detector_catches_every_structural_gap_class() -> Result<(), String> {
    let pack = build_gap_pack()?;
    let report = analyze_static_gaps(&pack);

    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::DeadEndNonTerminalState { state, .. } if state == "stuck"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::UnreachableState { state, .. } if state == "ghost"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::OrphanField { property, .. } if property == "orphan_flag"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::ImmutableFinancialMissingReversal { object_type, .. }
            if object_type == "gap.JournalEntry"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::HighImpactActionMissingSod { action_id, .. }
            if action_id == "gap.journal.post" && g.severity() == GapSeverity::Question
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::TransitionWithoutAction { transition_id, .. } if transition_id == "close"
    )));
    Ok(())
}

#[test]
fn law_example_parity_positive_case_fails_when_law_missing_from_host() -> Result<(), String> {
    let envelopes = vec![
        self_loop_entity_envelope()?,
        positive_amount_law_envelope()?,
    ];
    let pack =
        AdmittedPack::from_envelopes("positive_missing_law", &envelopes)?.with_law_examples(vec![
            LawExampleCase {
                name: "positive holds must fail when host lacks law".to_owned(),
                law_id: "obs.Ticket.positive_amount".to_owned(),
                subject: runtime_obj(&[
                    ("ticket_id", RuntimeValue::String("T-OK".to_owned())),
                    ("amount", dec("25.00")?),
                    ("status", RuntimeValue::String("open".to_owned())),
                ]),
                expected: Verdict::Holds,
            },
        ]);
    let err = run_pack_mbt(&pack, |_| BareHost::open(&pack, false, false));
    let msg = err
        .err()
        .ok_or_else(|| "expected positive-case parity failure when host lacks law".to_owned())?;
    assert!(
        msg.contains("does not attach law"),
        "expected host missing-law diagnostic, got: {msg}"
    );
    Ok(())
}

fn invariant_order_pack() -> Result<AdmittedPack, String> {
    let raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "obs.Order.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "obs", "name": "Order", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "obs.Order",
                    "properties": [
                        {"id": "order_id", "type": {"kind": "text"}},
                        {"id": "total", "type": {"kind": "decimal", "scale": 2}},
                        {"id": "status", "default": "open", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "closed"}]}}
                    ],
                    "invariants": [
                        {"id": "positive_total", "expression": "self.total > 0", "message": "total must be positive"}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "obs.Order.positive_total_law.law", "version": "1.0.0",
            "kind": "law", "namespace": "obs", "name": "Invariant-backed law", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "obs.Order.positive_total_law",
                "owner": "obs",
                "severity": "error",
                "rationale": "All orders satisfy positive_total.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "obs.Order"}, "objectTypes": ["obs.Order"]},
                "subject": "obs.Order",
                "modality": {"kind": "always"},
                "response": {"kind": "invariant", "objectType": "obs.Order", "invariantId": "positive_total"}
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("inv_law", &envelopes)
}

#[test]
fn observed_sqlite_cell_credits_invariant_backed_law_holds() -> Result<(), String> {
    let pack = invariant_order_pack()?;
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);
    let model = Model::from_definitions_with_interfaces(
        &pack.object_types,
        &pack.property_types,
        &pack.interfaces,
    )
    .map_err(|e| format!("model: {e}"))?
    .with_governing_laws(&pack.law_envelopes)
    .map_err(|e| format!("laws: {e}"))?;
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let mut cell = FactSqliteCell::open_in_memory(model, "org/dev", |cmd| cmd.payload.clone())
        .map_err(|e| format!("open: {e}"))?
        .into_observed(Arc::clone(&recorder));

    let k_open = FactKey::new("obs.Order", "o-1");
    let slice1 = load_slice(&cell, [k_open]).map_err(|e| format!("{e}"))?;
    let out1 = DecisionOutput {
        facts: vec![FactChange::Create {
            object_type: ObjectTypeName("obs.Order".to_owned()),
            key: "o-1".to_owned(),
            values: runtime_obj(&[
                ("order_id", RuntimeValue::String("o-1".to_owned())),
                ("total", dec("10.00")?),
                ("status", RuntimeValue::String("open".to_owned())),
            ]),
        }],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn1 = DecisionTurn {
        id: "inv-1".to_owned(),
        now: LogicalTime::EPOCH,
        world: World::Main,
        consumes: None,
    };
    commit_decision(&mut cell, &catalog, &slice1, &turn1, &out1).map_err(|e| format!("{e}"))?;
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("obs.Order.positive_total_law"),
        Some(&1),
        "active satisfied invariant-backed law must credit law_holds"
    );

    let k_bad = FactKey::new("obs.Order", "o-2");
    let slice2 = load_slice(&cell, [k_bad]).map_err(|e| format!("{e}"))?;
    let out2 = DecisionOutput {
        facts: vec![FactChange::Create {
            object_type: ObjectTypeName("obs.Order".to_owned()),
            key: "o-2".to_owned(),
            values: runtime_obj(&[
                ("order_id", RuntimeValue::String("o-2".to_owned())),
                ("total", dec("0.00")?),
                ("status", RuntimeValue::String("open".to_owned())),
            ]),
        }],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn2 = DecisionTurn {
        id: "inv-2".to_owned(),
        now: LogicalTime::EPOCH,
        world: World::Main,
        consumes: None,
    };
    let bad_res = commit_decision(&mut cell, &catalog, &slice2, &turn2, &out2);
    assert!(bad_res.is_err(), "violating invariant-backed law must fail");
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("obs.Order.positive_total_law"),
        Some(&1),
        "violated invariant-backed law must not increment law_holds"
    );
    Ok(())
}

#[test]
fn guard_fallback_succeeds_after_law_blocked_prefix_walk() -> Result<(), String> {
    let raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "fb.Job.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "fb", "name": "Job", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "fb.Job",
                    "properties": [
                        {"id": "job_id", "type": {"kind": "text"}},
                        {"id": "ready", "type": {"kind": "boolean"}},
                        {"id": "status", "default": "draft", "type": {"kind": "enum", "values": [
                            {"value": "draft"}, {"value": "mid"}, {"value": "staged"}, {"value": "done"}
                        ]}}
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["draft"],
                        "terminal": ["done"],
                        "transitions": [
                            {"id": "to_mid", "from": ["draft"], "to": "mid"},
                            {"id": "to_staged", "from": ["mid"], "to": "staged"},
                            {"id": "finish", "from": ["staged"], "to": "done", "guards": ["self.ready"]}
                        ]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "fb.Job.mid_ready.law", "version": "1.0.0",
            "kind": "law", "namespace": "fb", "name": "Mid requires ready", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "fb.Job.mid_ready",
                "owner": "fb",
                "severity": "error",
                "rationale": "mid requires ready=true (so direct walk with ready=false fails on to_mid after seeding draft).",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "fb.Job"}, "objectTypes": ["fb.Job"]},
                "subject": "fb.Job",
                "condition": "self.status == 'mid'",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.ready"}
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let pack = AdmittedPack::from_envelopes("guard_fallback", &envelopes)?;
    let report = run_pack_mbt(&pack, |_| BareHost::open(&pack, true, false))?;
    assert_eq!(report.uncovered_guard_refusals, Vec::<String>::new());
    assert!(report.evidence.contains(&(
        "lifecycle_walks_and_refusals",
        EvidenceLevel::ExhaustiveFinite
    )));
    Ok(())
}

#[test]
fn dst_oracle_catches_fault_injected_corrupted_linked_create() -> Result<(), String> {
    let raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "lnk.Parent.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "lnk", "name": "Parent", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "lnk.Parent",
                    "properties": [{"id": "parent_id", "type": {"kind": "text"}}]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "lnk.Child.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "lnk", "name": "Child", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "lnk.Child",
                    "properties": [
                        {"id": "child_id", "type": {"kind": "text"}},
                        {"id": "note", "type": {"kind": "text"}},
                        {"id": "status", "default": "open", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "closed"}]}}
                    ],
                    "links": [{
                        "id": "parent",
                        "target": "lnk.Parent",
                        "cardinality": {"from": "many", "to": "one", "required": true}
                    }],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["closed"],
                        "transitions": [{"id": "close", "from": ["open"], "to": "closed"}]
                    }]
                }
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let pack = AdmittedPack::from_envelopes("linked_fault_dst", &envelopes)?;
    let err = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [0x22; 32],
            cases: 16,
            max_steps: 8,
            entities_per_type: 1,
        },
        |_| Ok(BareHost::open(&pack, false, false)?.with_corrupt_linked_create()),
    );
    let msg = err
        .err()
        .ok_or_else(|| "expected DST to catch corrupted linked create".to_owned())?;
    assert!(
        msg.contains("oracle divergence")
            && msg.contains("CORRUPTED")
            && msg.contains("minimal sequence"),
        "expected minimized linked-create corruption diagnostic, got: {msg}"
    );
    Ok(())
}
