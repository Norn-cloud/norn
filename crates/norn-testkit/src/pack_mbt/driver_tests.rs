//! Unit tests for `pack_mbt::driver` internal probes.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use norn_ir::ArtifactEnvelope;
use norn_kernel::cell_host::{
    CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactKey, FactMutation, FactState,
    FactVersion, FactWrite, JsonObject, JsonValue, LinkKey, LinkState, Observed, Performed,
};
use norn_kernel::{FactEvent, LogicalTime, TypeCatalog};
use serde_json::json;

use super::{LifecycleCtx, LifecycleStats, check_direct_lifecycle_write};
use crate::cell_host::MemoryCell;
use crate::pack_mbt::coverage::{DomainCoverage, distinct_lifecycle_state};
use crate::pack_mbt::{AdmittedPack, PackHost};

struct TestMemoryHost {
    cell: MemoryCell<FactEvent, JsonObject, JsonObject>,
    now: LogicalTime,
}

impl TestMemoryHost {
    fn open() -> Self {
        Self {
            cell: MemoryCell::new(|cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone()),
            now: LogicalTime::from_millis(1_000),
        }
    }
}

impl CellHost for TestMemoryHost {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Infallible> {
        self.cell.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        self.cell.commit(commit)
    }

    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, Infallible> {
        self.cell.perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, Infallible> {
        self.cell.observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, Infallible> {
        self.cell.clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Infallible> {
        self.cell.publish(receipt)
    }
}

impl PackHost for TestMemoryHost {
    fn ledger_len(&self) -> usize {
        self.cell.ledger().len()
    }

    fn logical_now(&self) -> LogicalTime {
        self.now
    }

    fn advance_clock(&mut self, delta_millis: u64) {
        self.now = LogicalTime::from_millis(self.now.as_millis().saturating_add(delta_millis));
    }

    fn verify_ledger(&self) -> Result<(), String> {
        Ok(())
    }

    fn reopen(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn check_direct_lifecycle_write_proposes_a_distinct_target_state() -> Result<(), String> {
    let envelope: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": "ex.Order.entity", "version": "1.0.0",
        "kind": "entity", "namespace": "ex", "name": "Order", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "ex.Order",
                "identity": [{"name": "primary", "properties": ["order_id"]}],
                "properties": [
                    {"id": "order_id", "type": {"kind": "text"}},
                    {
                        "id": "status",
                        "type": {"kind": "enum", "values": [{"value": "draft"}, {"value": "active"}]},
                        "default": "draft"
                    }
                ],
                "lifecycles": [{
                    "property": "status",
                    "initial": ["draft"],
                    "terminal": ["active"],
                    "transitions": [{"id": "activate", "from": ["draft"], "to": "active"}]
                }]
            }
        }
    }))
    .map_err(|err| format!("envelope: {err}"))?;
    let pack = AdmittedPack::from_envelopes("direct_write_mutant", &[envelope])?;
    let object_type = pack
        .object_types
        .first()
        .ok_or_else(|| "missing ex.Order".to_owned())?;
    let lifecycle = object_type
        .lifecycles
        .first()
        .ok_or_else(|| "missing status lifecycle".to_owned())?;

    // Mutant catalog with lifecycles stripped so `commit_decision` applies the
    // proposed `FactChange::Set`, letting us observe the exact state written.
    let mut stripped_type = object_type.clone();
    stripped_type.lifecycles.clear();
    let stripped_types = [stripped_type];
    let stripped_catalog = TypeCatalog::new(&stripped_types);

    let mut host = TestMemoryHost::open();
    let key = FactKey::new("ex.Order", "ord-1");
    let initial = JsonObject::from([
        ("order_id".to_owned(), JsonValue::String("ord-1".to_owned())),
        ("status".to_owned(), JsonValue::String("draft".to_owned())),
    ]);
    host.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values: initial },
        }],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("seed: {err:?}"))?;

    let coverage = Arc::new(Mutex::new(DomainCoverage::default()));
    let ctx = LifecycleCtx {
        pack: &pack,
        catalog: &stripped_catalog,
        object_type,
        lifecycle,
        coverage: &coverage,
    };
    let mut totals = LifecycleStats::default();
    let probe = check_direct_lifecycle_write(&mut host, &ctx, "draft", "ord-1", 1, &mut totals);
    assert!(
        probe.is_err(),
        "stripped catalog without lifecycle guard must fail the refusal check"
    );

    let after = host.load(&key).map_err(|err| format!("load: {err}"))?;
    let written_status = after
        .values
        .as_ref()
        .and_then(|vals| vals.get("status"))
        .and_then(JsonValue::as_str);
    assert_eq!(
        written_status,
        Some("active"),
        "direct lifecycle probe must propose a distinct state rather than no-op `draft`"
    );

    // Single-state fallback still picks a distinct synthetic state name.
    let mut single_state = lifecycle.clone();
    single_state.terminal.clear();
    single_state.transitions.clear();
    assert_eq!(
        distinct_lifecycle_state(&single_state, "draft"),
        "draft_illegal"
    );
    Ok(())
}
