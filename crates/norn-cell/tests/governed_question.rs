#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "governed read conformance"
)]
mod query_support;
use norn_cell::compute::{
    Engine, Freshness,
    governed::{GovernedRead, GovernedReadRefusal, bind_governed_read},
};
use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::action_commit::ActionReferenceReads;
use norn_kernel::cell_host::{BoundedFact, JsonObject};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, CellCommit, CellHost, CommitError, CommitReceipt, DecisionTurn,
    DueTimer, FactEvent, FactExpectation, FactKey, FactMutation, FactState, FactVersion, FactWrite,
    LinkKey, LinkState, LogicalTime, Observed, Performed, TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;
use std::collections::BTreeMap;
type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;
type Proposal = CellCommit<FactEvent, JsonObject, JsonObject>;

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn supports_reference_lineage(&self) -> bool {
        true
    }
}
struct Capture {
    cell: Cell,
    prepared: Option<(Proposal, ActionReferenceReads)>,
}
impl CellHost for Capture {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = std::io::Error;
    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        Ok(self.cell.load(key).expect("memory read"))
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        Ok(self.cell.load_link(key).expect("memory link"))
    }
    fn supports_reference_reads(&self) -> bool {
        true
    }
    fn load_reference(&self, key: &FactKey, max: u64) -> Result<Option<BoundedFact>, Self::Error> {
        Ok(self
            .cell
            .load_reference(key, max)
            .expect("memory reference"))
    }
    fn load_bounded(&self, key: &FactKey, max: u64) -> Result<Option<BoundedFact>, Self::Error> {
        Ok(self
            .cell
            .load_bounded(key, max)
            .expect("bounded memory read"))
    }
    fn commit(&mut self, _: Proposal) -> Result<CommitReceipt, CommitError<Self::Error>> {
        Err(CommitError::Host(std::io::Error::other(
            "capture requires the reference seam",
        )))
    }
    fn commit_action_references(
        &mut self,
        commit: Proposal,
        proof: ActionReferenceReads,
        _: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        self.prepared = Some((commit, proof));
        Err(CommitError::Host(std::io::Error::other(
            "captured before commit",
        )))
    }
    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, Self::Error> {
        Ok(None)
    }
    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, Self::Error> {
        Ok(None)
    }
    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, Self::Error> {
        Ok(None)
    }
    fn publish(&mut self, _: &CommitReceipt) -> Result<(), Self::Error> {
        Ok(())
    }
}
fn key(id: &str) -> FactKey {
    FactKey::new("example.Record", id)
}
fn put(cell: &mut Cell, id: &str, value: i64) {
    let mut commit = Proposal::empty();
    commit.writes.push(FactWrite {
        key: key(id),
        expected: cell.load(&key(id)).expect("version").version,
        mutation: FactMutation::Put {
            values: serde_json::from_value(json!({"key":id,"value":value})).expect("values"),
        },
    });
    cell.commit(commit).expect("seed/update");
}
fn prepared() -> (Cell, Proposal, ActionReferenceReads) {
    let types: Vec<ObjectTypeDef> =
        serde_json::from_value(json!([{"name":"example.Record","properties":[
            {"id":"key","type":{"kind":"text"}}, {"id":"value","type":{"kind":"integer"}}
        ]}]))
        .expect("types");
    let action: ActionRoot = serde_json::from_value(json!({
        "actionId":"example.record.update","scopeRequired":["orgId","environmentId"],"idempotent":true,"ledgered":true,
        "inputSchema":{"type":"object","additionalProperties":false,"properties":{"key":{"type":"string"},"value":{"type":"integer"}},"required":["key","value"]},
        "outputSchema":{"type":"object","properties":{},"additionalProperties":false},
        "slice":[{"name":"target","objectType":"example.Record","key":{"property":{"from":"input","property":"key"}}}],
        "writes":[{"entityKind":"example.Record","change":"updated"}],
        "effectProgram":{"kind":"upsert_input_object","objectKind":"example.Record","objectIdField":"key",
            "entityArtifactId":"example.Record.entity","entityArtifactVersion":"0.1.0","writeMode":{"kind":"upsert"}}
    })).expect("action");
    let mut cell = Cell::new(|command: &norn_kernel::Command<JsonObject>| command.payload.clone());
    put(&mut cell, "one", 1);
    put(&mut cell, "dep", 1);
    let mut capture = Capture {
        cell,
        prepared: None,
    };
    let call = ActionCall {
        action: &action,
        action_artifact_id: "example.record.update.action",
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: "test:operator".into(),
        input: serde_json::from_value(json!({"key":"one","value":2})).expect("input"),
        idempotency_key: "one-update".into(),
        object_type: None,
        turn: DecisionTurn {
            id: "test-turn".into(),
            now: LogicalTime::from_millis(1000),
            world: World::Main,
            consumes: None,
        },
    };
    let outcome = commit_action(&mut capture, &TypeCatalog::new(&types), &call, &Allow);
    assert!(outcome.is_err());
    let (proposal, proof) = capture
        .prepared
        .take()
        .expect("kernel reference proof captured");
    (capture.cell, proposal, proof)
}
#[test]
fn governed_point_read_uses_existing_commit_fence_and_rejects_a_racing_writer() {
    for race in [false, true] {
        let (mut cell, mut proposal, proof) = prepared();
        let guard = FactExpectation {
            key: key("dep"),
            expected: FactVersion(1),
        };
        let proof = bind_governed_read(proof, &mut proposal, GovernedRead::PointFacts(&[guard]))
            .expect("guard");
        assert!(proof.validates(&proposal));
        if race {
            put(&mut cell, "dep", 3);
        }
        let before = cell.ledger().len();
        let result = cell.commit_action_references(proposal, proof, None);
        if race {
            assert!(matches!(result, Err(CommitError::Conflict { .. })));
            assert_eq!(cell.ledger().len(), before);
        } else {
            result.expect("commit");
        }
        assert_eq!(
            cell.load(&key("one"))
                .expect("target")
                .values
                .expect("values")["value"],
            norn_ir::RuntimeValue::from(if race { 1i64 } else { 2i64 })
        );
    }
}
#[test]
fn conflicting_point_batch_leaves_the_whole_proposal_unchanged() {
    let (_, mut proposal, proof) = prepared();
    let before = proposal.clone();
    let guards = [
        FactExpectation {
            key: key("dep"),
            expected: FactVersion(1),
        },
        FactExpectation {
            key: key("dep"),
            expected: FactVersion(2),
        },
    ];
    assert!(matches!(
        bind_governed_read(proof, &mut proposal, GovernedRead::PointFacts(&guards)),
        Err(GovernedReadRefusal::ConflictingReadSet)
    ));
    assert_eq!(proposal, before);
}
#[test]
fn query_stale_and_even_exact_empty_root_reads_refuse_without_commit_or_proposal_changes() {
    let (store, plan, context) = query_support::fixture();
    let result = store
        .evaluate_question(&plan, context, |_| Ok(true))
        .expect("actual cell result");
    assert!(result.rows.is_empty());
    for engine in [Engine::Cell, Engine::Ivm] {
        for freshness in [
            Freshness::Exact,
            Freshness::Fresh,
            Freshness::Stale,
            Freshness::Incomplete,
        ] {
            let mut query = result.clone();
            query.engine = engine;
            query.freshness = freshness;
            let (cell, mut proposal, proof) = prepared();
            let before = proposal.clone();
            let head = cell.ledger().len();
            let refusal =
                bind_governed_read(proof, &mut proposal, GovernedRead::RootQuestion(&query))
                    .expect_err("root refusal");
            assert_eq!(refusal.code(), "core.compute.stale_input");
            assert_eq!(
                refusal,
                if engine == Engine::Cell && freshness == Freshness::Exact {
                    GovernedReadRefusal::UnfencedRoot
                } else {
                    GovernedReadRefusal::StaleInput
                }
            );
            assert_eq!(proposal, before);
            assert_eq!(cell.ledger().len(), head);
        }
    }
}
