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
use norn_kernel::cell_host::{
    FactKey, FactMutation, FactState, FactVersion, FactWrite, JsonObject, LinkMutation, LinkWrite,
};
use norn_kernel::pack_mbt_support::{LawExampleVerdict as Verdict, LawIR, law_content_pin};
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
        let laws: &[ArtifactEnvelope] = if attach_laws {
            &pack.law_envelopes
        } else {
            &[]
        };
        Self::open_with_laws(pack, laws, drop_set_mutations)
    }

    fn open_with_laws(
        pack: &AdmittedPack,
        laws: &[ArtifactEnvelope],
        drop_set_mutations: bool,
    ) -> Result<Self, String> {
        Self::open_with_laws_and_recorder(
            pack,
            laws,
            drop_set_mutations,
            Arc::new(DomainCoverageRecorder::new()),
        )
    }

    fn open_with_laws_and_recorder(
        pack: &AdmittedPack,
        laws: &[ArtifactEnvelope],
        drop_set_mutations: bool,
        recorder: Arc<DomainCoverageRecorder>,
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
        let model = if laws.is_empty() {
            base
        } else {
            base.with_governing_laws(laws).map_err(|e| format!("{e}"))?
        };
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
    fn governing_law_pin(&self, object_type: &str, artifact_id: &str) -> Option<String> {
        let cell = self.cell.as_ref()?;
        let laws = cell
            .inner()
            .store()
            .model()
            .governing_laws(object_type)
            .ok()?;
        let env = laws.iter().find(|env| env.artifact_id == artifact_id)?;
        let text = serde_json::to_string(&env.payload.to_json_value()).ok()?;
        let law: LawIR = serde_json::from_str(&text).ok()?;
        law_content_pin(&law).ok()
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
        msg.contains("does not attach exact law pin"),
        "expected host missing-law pin diagnostic, got: {msg}"
    );
    Ok(())
}

#[test]
fn law_example_parity_positive_case_fails_on_stale_law_revision() -> Result<(), String> {
    let envelopes = vec![
        self_loop_entity_envelope()?,
        positive_amount_law_envelope()?,
    ];
    let pack = AdmittedPack::from_envelopes("stale_law_rev", &envelopes)?.with_law_examples(vec![
        LawExampleCase {
            name: "positive holds must fail when host runs a stale revision under same id"
                .to_owned(),
            law_id: "obs.Ticket.positive_amount".to_owned(),
            subject: runtime_obj(&[
                ("ticket_id", RuntimeValue::String("T-STALE".to_owned())),
                ("amount", dec("25.00")?),
                ("status", RuntimeValue::String("open".to_owned())),
            ]),
            expected: Verdict::Holds,
        },
    ]);
    // Same artifactId and law.id, but stale condition (`status == 'closed'`) that makes
    // the `Holds` example `NotApplicable` on the host.
    let stale_law: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": "obs.Ticket.positive_amount.law", "version": "1.0.0",
        "kind": "law", "namespace": "obs", "name": "Positive amount (stale)", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {
            "lawVersion": "0.1",
            "id": "obs.Ticket.positive_amount",
            "owner": "obs",
            "severity": "error",
            "rationale": "Stale revision with different activation condition.",
            "provenance": {"source": "authoring_sdk"},
            "scope": {"root": {"kind": "entity", "id": "obs.Ticket"}, "objectTypes": ["obs.Ticket"]},
            "subject": "obs.Ticket",
            "condition": "self.status == 'closed'",
            "modality": {"kind": "always"},
            "response": {"kind": "predicate", "expression": "self.amount > 0"}
        }
    }))
    .map_err(|err| format!("stale law: {err}"))?;
    let host_laws = vec![stale_law];
    let err = run_pack_mbt(&pack, |_| {
        BareHost::open_with_laws(&pack, &host_laws, false)
    });
    let msg = err.err().ok_or_else(|| {
        "expected positive-case parity failure when host runs a stale law revision".to_owned()
    })?;
    assert!(
        msg.contains("does not attach exact law pin"),
        "expected exact law pin mismatch diagnostic, got: {msg}"
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

fn linked_and_invariant_pack() -> Result<AdmittedPack, String> {
    let raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "mix.Account.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "mix", "name": "Account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "mix.Account",
                    "properties": [
                        {"id": "account_id", "type": {"kind": "text"}},
                        {"id": "active", "type": {"kind": "boolean"}}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "mix.Charge.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "mix", "name": "Charge", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "mix.Charge",
                    "properties": [
                        {"id": "charge_id", "type": {"kind": "text"}},
                        {"id": "total", "type": {"kind": "decimal", "scale": 2}}
                    ],
                    "links": [{
                        "id": "account",
                        "target": "mix.Account",
                        "cardinality": {"from": "many", "to": "one", "required": true}
                    }],
                    "invariants": [
                        {"id": "positive_total", "expression": "self.total > 0", "message": "total must be positive"}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "mix.Charge.active_account.law", "version": "1.0.0",
            "kind": "law", "namespace": "mix", "name": "Linked active account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "mix.Charge.active_account",
                "owner": "mix",
                "severity": "error",
                "rationale": "Charges require an active linked account.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "mix.Charge"}, "objectTypes": ["mix.Charge"]},
                "subject": "mix.Charge",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.account.active == true"}
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "mix.Charge.positive_total.law", "version": "1.0.0",
            "kind": "law", "namespace": "mix", "name": "Invariant total positive", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "mix.Charge.positive_total_law",
                "owner": "mix",
                "severity": "error",
                "rationale": "All charges satisfy positive_total.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "mix.Charge"}, "objectTypes": ["mix.Charge"]},
                "subject": "mix.Charge",
                "modality": {"kind": "always"},
                "response": {"kind": "invariant", "objectType": "mix.Charge", "invariantId": "positive_total"}
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("linked_and_inv", &envelopes)
}

#[test]
fn observed_sqlite_cell_credits_invariant_law_alongside_linked_predicate_law() -> Result<(), String>
{
    let pack = linked_and_invariant_pack()?;
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

    let account_key = FactKey::new("mix.Account", "acct-1");
    let charge_key = FactKey::new("mix.Charge", "chg-1");
    let commit = CellCommit {
        writes: vec![
            FactWrite {
                key: account_key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: runtime_obj(&[
                        ("account_id", RuntimeValue::String("acct-1".to_owned())),
                        ("active", RuntimeValue::Bool(true)),
                    ]),
                },
            },
            FactWrite {
                key: charge_key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: runtime_obj(&[
                        ("charge_id", RuntimeValue::String("chg-1".to_owned())),
                        ("total", dec("42.00")?),
                    ]),
                },
            },
        ],
        links: vec![LinkWrite {
            key: LinkKey::new(charge_key, "account", account_key),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    };
    cell.commit(commit).map_err(|e| format!("commit: {e}"))?;

    let snap = recorder.snapshot();
    assert_eq!(
        snap.law_holds.get("mix.Charge.positive_total_law"),
        Some(&1),
        "unrelated invariant-backed law must still be credited alongside a linked predicate law"
    );
    assert_eq!(
        snap.law_holds.get("mix.Charge.active_account"),
        Some(&1),
        "linked predicate law with satisfied committed link must also be credited"
    );
    Ok(())
}

fn link_only_subtype_envelopes() -> Vec<serde_json::Value> {
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "lo.PremiumCharge.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "lo", "name": "PremiumCharge", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "lo.PremiumCharge",
                    "extends": "lo.Charge",
                    "properties": [{"id": "tier", "type": {"kind": "text"}}]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "lo.PremiumCharge.subtype_active.law", "version": "1.0.0",
            "kind": "law", "namespace": "lo", "name": "Subtype active account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "lo.PremiumCharge.subtype_active",
                "owner": "lo",
                "severity": "error",
                "rationale": "Premium charges linked to an account require it to be active.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "lo.PremiumCharge"}, "objectTypes": ["lo.PremiumCharge"]},
                "subject": "lo.PremiumCharge",
                "condition": "self.account != null",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.account.active == true"}
            }
        }),
    ]
}

fn link_only_recheck_pack() -> Result<AdmittedPack, String> {
    let mut raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "lo.Account.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "lo", "name": "Account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "lo.Account",
                    "properties": [
                        {"id": "account_id", "type": {"kind": "text"}},
                        {"id": "active", "type": {"kind": "boolean"}}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "lo.Charge.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "lo", "name": "Charge", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "lo.Charge",
                    "properties": [
                        {"id": "charge_id", "type": {"kind": "text"}},
                        {"id": "total", "type": {"kind": "decimal", "scale": 2}}
                    ],
                    "links": [
                        {
                            "id": "account",
                            "target": "lo.Account",
                            "cardinality": {"from": "many", "to": "one", "required": false}
                        },
                        {
                            "id": "memo_ref",
                            "target": "lo.Account",
                            "cardinality": {"from": "many", "to": "one", "required": false}
                        }
                    ],
                    "invariants": [
                        {"id": "positive_total", "expression": "self.total > 0", "message": "total must be positive"}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "lo.Charge.active_account.law", "version": "1.0.0",
            "kind": "law", "namespace": "lo", "name": "Linked active account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "lo.Charge.active_account",
                "owner": "lo",
                "severity": "error",
                "rationale": "When linked to an account, that account must be active.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "lo.Charge"}, "objectTypes": ["lo.Charge"]},
                "subject": "lo.Charge",
                "condition": "self.account != null",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.account.active == true"}
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "lo.Charge.positive_total.law", "version": "1.0.0",
            "kind": "law", "namespace": "lo", "name": "Invariant total positive", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "lo.Charge.positive_total_law",
                "owner": "lo",
                "severity": "error",
                "rationale": "All charges satisfy positive_total.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "lo.Charge"}, "objectTypes": ["lo.Charge"]},
                "subject": "lo.Charge",
                "modality": {"kind": "always"},
                "response": {"kind": "invariant", "objectType": "lo.Charge", "invariantId": "positive_total"}
            }
        }),
    ];
    raw.extend(link_only_subtype_envelopes());
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("link_only_recheck", &envelopes)
}

#[test]
fn observed_sqlite_cell_records_hold_on_link_only_commit() -> Result<(), String> {
    let pack = link_only_recheck_pack()?;
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

    let account_key = FactKey::new("lo.Account", "acct-1");
    let charge_key = FactKey::new("lo.Charge", "chg-1");
    cell.commit(CellCommit {
        writes: vec![
            FactWrite {
                key: account_key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: runtime_obj(&[
                        ("account_id", RuntimeValue::String("acct-1".to_owned())),
                        ("active", RuntimeValue::Bool(true)),
                    ]),
                },
            },
            FactWrite {
                key: charge_key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: runtime_obj(&[
                        ("charge_id", RuntimeValue::String("chg-1".to_owned())),
                        ("total", dec("15.00")?),
                    ]),
                },
            },
        ],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("seed commit: {e}"))?;
    // Before linking, `self.account != null` is NotApplicable, so active_account has 0 holds.
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("lo.Charge.active_account"),
        None
    );
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("lo.Charge.positive_total_law"),
        Some(&1)
    );

    // Link-only commit (`account`, zero fact writes): activates & satisfies lo.Charge.active_account.
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(charge_key.clone(), "account", account_key.clone()),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("link-only commit: {e}"))?;
    let snap = recorder.snapshot();
    assert_eq!(
        snap.law_holds.get("lo.Charge.active_account"),
        Some(&1),
        "link-only commit activating a linked law must credit law_holds"
    );
    assert_eq!(
        snap.law_holds.get("lo.Charge.positive_total_law"),
        Some(&1),
        "link-only commit must not double-credit unrelated non-linked laws"
    );

    // Unrelated link-only commit (`memo_ref`) while `active_account` IS active must leave hold count at 1.
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(charge_key, "memo_ref", account_key),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("unrelated link commit: {e}"))?;
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("lo.Charge.active_account"),
        Some(&1),
        "unrelated edge must not increment a law reading only `account`"
    );
    Ok(())
}

#[test]
fn observed_sqlite_cell_records_subtype_hold_on_ancestor_addressed_link() -> Result<(), String> {
    let pack = link_only_recheck_pack()?;
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

    let account_key = FactKey::new("lo.Account", "acct-sub");
    let prem_key = FactKey::new("lo.PremiumCharge", "prem-1");
    cell.commit(CellCommit {
        writes: vec![
            FactWrite {
                key: account_key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: runtime_obj(&[
                        ("account_id", RuntimeValue::String("acct-sub".to_owned())),
                        ("active", RuntimeValue::Bool(true)),
                    ]),
                },
            },
            FactWrite {
                key: prem_key,
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: runtime_obj(&[
                        ("charge_id", RuntimeValue::String("prem-1".to_owned())),
                        ("total", dec("25.00")?),
                        ("tier", RuntimeValue::String("gold".to_owned())),
                    ]),
                },
            },
        ],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("seed subtype commit: {e}"))?;
    assert_eq!(
        recorder
            .snapshot()
            .law_holds
            .get("lo.PremiumCharge.subtype_active"),
        None
    );

    // Address the subtype via its parent type (`lo.Charge`) on the link write.
    let ancestor_key = FactKey::new("lo.Charge", "prem-1");
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(ancestor_key, "account", account_key),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .map_err(|e| format!("ancestor-addressed link commit: {e}"))?;
    let snap = recorder.snapshot();
    assert_eq!(
        snap.law_holds.get("lo.PremiumCharge.subtype_active"),
        Some(&1),
        "ancestor-addressed link commit must resolve concrete subtype and credit subtype law hold"
    );
    assert_eq!(
        snap.law_holds.get("lo.Charge.active_account"),
        Some(&1),
        "inherited parent linked law must also be credited on the subtype"
    );
    Ok(())
}

fn inherited_immutability_raw_envelopes() -> Vec<serde_json::Value> {
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "inh.ImmParent.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "inh", "name": "ImmParent", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "inh.ImmParent",
                    "immutable": true,
                    "properties": [
                        {"id": "imm_id", "type": {"kind": "text"}},
                        {"id": "memo", "type": {"kind": "text"}},
                        {"id": "status", "default": "open", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "closed"}]}}
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["closed"],
                        "transitions": [{"id": "close", "from": ["open"], "to": "closed"}]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "inh.ImmChild.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "inh", "name": "ImmChild", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "inh.ImmChild",
                    "extends": "inh.ImmParent",
                    "properties": [{"id": "tag", "type": {"kind": "text"}}]
                }
            }
        }),
    ]
}

fn inherited_links_and_immutability_pack() -> Result<AdmittedPack, String> {
    let mut raw = vec![
        json!({
            "irVersion": "0.1", "artifactId": "inh.Account.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "inh", "name": "Account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "inh.Account",
                    "properties": [
                        {"id": "account_id", "type": {"kind": "text"}},
                        {"id": "active", "type": {"kind": "boolean"}}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "inh.BaseOrder.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "inh", "name": "BaseOrder", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "inh.BaseOrder",
                    "properties": [
                        {"id": "order_id", "type": {"kind": "text"}},
                        {"id": "audit_note", "type": {"kind": "text"}},
                        {"id": "status", "default": "open", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "done"}]}}
                    ],
                    "links": [{
                        "id": "account",
                        "target": "inh.Account",
                        "cardinality": {"from": "many", "to": "one", "required": true}
                    }],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["done"],
                        "transitions": [{"id": "complete", "from": ["open"], "to": "done"}]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "inh.SpecialOrder.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "inh", "name": "SpecialOrder", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "inh.SpecialOrder",
                    "extends": "inh.BaseOrder",
                    "properties": [
                        {"id": "priority", "type": {"kind": "text"}}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "inh.BaseOrder.audit_note.law", "version": "1.0.0",
            "kind": "law", "namespace": "inh", "name": "Base order audit note", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "inh.BaseOrder.audit_note",
                "owner": "inh",
                "severity": "error",
                "rationale": "Base orders and subtypes must carry a non-empty audit note.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "inh.BaseOrder"}, "objectTypes": ["inh.BaseOrder"]},
                "subject": "inh.BaseOrder",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.audit_note != ''"}
            }
        }),
    ];
    raw.extend(inherited_immutability_raw_envelopes());
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("inh_pack", &envelopes)
}

#[test]
fn inherited_required_links_laws_and_immutability_pass_mbt_dst_and_gaps() -> Result<(), String> {
    let pack = inherited_links_and_immutability_pack()?;
    let mbt = run_pack_mbt(&pack, |_| BareHost::open(&pack, true, false))?;
    assert!(mbt.valid_transitions_run >= 2);
    let dst = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [0x33; 32],
            cases: 12,
            max_steps: 10,
            entities_per_type: 1,
        },
        |_| BareHost::open(&pack, true, false),
    )?;
    assert_eq!(dst.cases_run, 12);

    let gaps = analyze_static_gaps(&pack);
    assert!(
        gaps.defects().iter().any(|g| matches!(
            g,
            StructuralGap::OrphanField { object_type, property, .. }
                if object_type == "inh.SpecialOrder" && property == "audit_note"
        )),
        "inherited parent-type law reading audit_note must report OrphanField on subtype:\n{gaps}"
    );
    Ok(())
}

fn shadow_link_dst_entities() -> Vec<serde_json::Value> {
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "shd.Account.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "shd", "name": "Account", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "shd.Account",
                    "properties": [
                        {"id": "account_id", "type": {"kind": "text"}},
                        {"id": "tier", "type": {"kind": "text"}}
                    ]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "shd.Order.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "shd", "name": "Order", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "shd.Order",
                    "properties": [
                        {"id": "order_id", "type": {"kind": "text"}},
                        {"id": "note", "type": {"kind": "text"}},
                        {"id": "status", "default": "open", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "done"}]}}
                    ],
                    "links": [{
                        "id": "account",
                        "target": "shd.Account",
                        "cardinality": {"from": "many", "to": "one", "required": true}
                    }],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["done"],
                        "transitions": [{"id": "complete", "from": ["open"], "to": "done"}]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "shd.Locked.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "shd", "name": "Locked", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "shd.Locked",
                    "immutable": true,
                    "properties": [
                        {"id": "locked_id", "type": {"kind": "text"}},
                        {"id": "ready", "type": {"kind": "boolean"}},
                        {"id": "memo", "type": {"kind": "text"}},
                        {"id": "status", "default": "open", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "closed"}]}}
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["closed"],
                        "immutable": ["open", "closed"],
                        "transitions": [
                            {"id": "reopen", "from": ["closed"], "to": "open"}
                        ]
                    }]
                }
            }
        }),
    ]
}

fn shadow_link_dst_pack() -> Result<AdmittedPack, String> {
    let mut raw = shadow_link_dst_entities();
    raw.extend(vec![
        json!({
            "irVersion": "0.1", "artifactId": "shd.Order.has_account_id.law", "version": "1.0.0",
            "kind": "law", "namespace": "shd", "name": "Open order has linked account id", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "shd.Order.has_account_id",
                "owner": "shd",
                "severity": "error",
                "rationale": "Open orders must link to an account with a non-empty account_id.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "shd.Order"}, "objectTypes": ["shd.Order"]},
                "subject": "shd.Order",
                "condition": "self.status == 'open'",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.account.account_id != ''"}
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "shd.Order.enterprise_done.law", "version": "1.0.0",
            "kind": "law", "namespace": "shd", "name": "Done order requires enterprise tier", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "shd.Order.enterprise_done",
                "owner": "shd",
                "severity": "error",
                "rationale": "Completed orders require an enterprise-tier linked account.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "shd.Order"}, "objectTypes": ["shd.Order"]},
                "subject": "shd.Order",
                "condition": "self.status == 'done'",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.account.tier == 'enterprise'"}
            }
        }),
    ]);
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|e| format!("{e}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("shadow_link_dst", &envelopes)
}

#[test]
fn dst_exercises_shadow_link_resolution_and_object_immutability_precedence() -> Result<(), String> {
    let pack = shadow_link_dst_pack()?;
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let dst = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [0x44; 32],
            cases: 24,
            max_steps: 18,
            entities_per_type: 1,
        },
        |_| {
            BareHost::open_with_laws_and_recorder(
                &pack,
                &pack.law_envelopes,
                false,
                Arc::clone(&recorder),
            )
        },
    )?;
    assert_eq!(dst.cases_run, 24);
    assert!(dst.steps_run >= 24);
    let snap = recorder.snapshot();
    assert!(
        snap.law_holds
            .get("shd.Order.has_account_id")
            .copied()
            .unwrap_or(0)
            >= 1,
        "DST must exercise holding linked create via shadow links: {snap:?}"
    );
    assert!(
        snap.law_violations
            .get("shd.Order.enterprise_done")
            .copied()
            .unwrap_or(0)
            >= 1,
        "DST must exercise refused `complete` transition via shadow links: {snap:?}"
    );
    assert!(
        dst.immutable_object_transition_refusals >= 1,
        "DST must exercise wrong-source `reopen` refusal on object-immutable `shd.Locked`"
    );
    assert!(
        dst.immutable_object_mutation_refusals >= 1,
        "DST must exercise mutation refusal when both object and lifecycle immutability apply"
    );
    assert!(
        dst.coverage
            .disallowed_transitions
            .keys()
            .all(|k| k.object_type != "shd.Locked"),
        "wrong-source `reopen` on object-immutable `shd.Locked` must not classify as NotFromState"
    );
    Ok(())
}
