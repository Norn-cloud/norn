//! DATA-ASOF / DATA-RESTATEMENT-DELTA / OUTPUT-ATOMIC conformance.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "native restatement scenarios"
)]
mod query_support;
use norn_cell::{
    CellStore, CommitBatch, CommitPoint, KnowledgeCutoff, MAIN_WORLD, Model, SqliteCell,
    StoreOptions,
    compute::{
        QueryContext,
        restatement::{RESTATEMENT_TYPE, RestatementSlice, RestatementView},
    },
};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, DecisionTurn,
    FactEvent, JsonObject, LogicalTime, TypeCatalog, World, commit_action,
};
use query_support::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
const SCOPE: &str = "org_demo/dev";
const ID: &str = "scrap-yield";
fn entity() -> ObjectTypeDef {
    let artifact: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.analytics.Restatement.entity.json"
    ))
    .expect("generated entity");
    serde_json::from_value(artifact["payload"]["objectType"].clone()).expect("typed concept")
}
fn action(transition: &str) -> ActionRoot {
    let bytes = match transition {
        "approve" => include_str!(
            "../../../registry/core/artifacts/core.analytics.restatement.approve.action.json"
        ),
        "activate" => include_str!(
            "../../../registry/core/artifacts/core.analytics.restatement.activate.action.json"
        ),
        _ => panic!("test transition"),
    };
    let artifact: Value = serde_json::from_str(bytes).expect("generated action");
    serde_json::from_value(artifact["payload"].clone()).expect("typed action")
}
fn setup(path: &Path) -> (CellStore, QuestionPlan, QueryContext, Model) {
    let (base, plan, mut context) = fixture();
    let mut types: Vec<_> = base.model().object_types().cloned().collect();
    types.push(entity());
    let properties: Vec<Value> = [
        "actionId",
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "actor",
        "turn",
        "transition",
        "from",
        "to",
        "rowOperations",
    ]
    .into_iter()
    .map(|id| {
        let kind = if [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ]
        .contains(&id)
        {
            "json-ext"
        } else {
            "text"
        };
        json!({"id":id,"type":{"kind":kind},"constraints":{"nullable":true}})
    })
    .collect();
    types.push(
        serde_json::from_value(json!({"name":ACTION_CALL_TYPE,"properties":properties}))
            .expect("action receipt schema"),
    );
    let model = Model::new(&norn_projections::SqliteDdlInput {
        object_types: &types,
        property_types: &[],
    })
    .expect("native model");
    let mut store =
        CellStore::open(path, model.clone(), StoreOptions::default()).expect("durable store");
    let writes = include_str!("../../../fixtures/demo/scrap-yield-k1.csv")
        .lines()
        .skip(1)
        .map(|row| {
            let f: Vec<_> = row.split(',').collect();
            put(
                f[2],
                f[4].parse().expect("input"),
                f[5].parse().expect("recovered"),
                f[8],
                f[3],
            )
        })
        .collect();
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: SCOPE.into(),
            logical_time: CLOCK,
            writes,
            ..CommitBatch::default()
        })
        .expect("K1 observed rows");
    context.world = MAIN_WORLD.into();
    context.scope = SCOPE.into();
    (store, plan, context, model)
}
fn cutoff(store: &CellStore, context: &QueryContext) -> KnowledgeCutoff {
    KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(context.now_ms),
        frontier: store.verified_head().expect("frontier").commits,
    }
}
fn progress(context: &mut QueryContext, at: u64, frontier: u64) {
    context.now_ms = at;
    let cursor = format!("correction/{frontier}");
    context
        .source_cursors
        .get_mut("erp")
        .expect("source")
        .insert("measurements".into(), cursor.clone());
    let w = context
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition");
    w.observed = cursor;
    w.heartbeat_ms = Some(at);
}
fn requests<'a>(
    plan: &'a QuestionPlan,
    context: &QueryContext,
    point: KnowledgeCutoff,
) -> Vec<RestatementSlice<'a>> {
    ["week", "loss"]
        .into_iter()
        .map(|id| RestatementSlice {
            id,
            plan: if id == "week" {
                plan.clone()
            } else {
                loss_plan()
            },
            context: context.clone(),
            cutoff: point,
        })
        .collect()
}
fn stage(
    store: &mut CellStore,
    plan: &QuestionPlan,
    context: &QueryContext,
    point: KnowledgeCutoff,
) {
    let version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("slot version");
    let build = store
        .begin_restatement(
            ID,
            context.clone(),
            version,
            "late analytical evidence",
            BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("private rebuild");
    let prepared = store
        .prepare_restatement(build, requests(plan, context, point), |_| Ok(true))
        .expect("delta correspondence");
    store
        .stage_restatement(prepared)
        .expect("complete candidate");
}
struct Authority(bool);
impl ActionAuthorizer for Authority {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.0 {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: "reviewer required".into(),
            })
        }
    }
}
fn governed(path: &Path, model: &Model, transition: &str, key: &str, allow: bool) -> bool {
    let store =
        CellStore::open(path, model.clone(), StoreOptions::default()).expect("action store");
    let mut cell: SqliteCell<FactEvent, JsonObject, JsonObject> =
        SqliteCell::new(store, SCOPE, |c| c.payload.clone()).expect("native host");
    let action = action(transition);
    let defs: Vec<_> = model.object_types().collect();
    let catalog = TypeCatalog::new(defs);
    let call = ActionCall {
        action: &action,
        action_artifact_id: &format!("core.analytics.restatement.{transition}.action"),
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: "synthetic:reviewer".into(),
        input: serde_json::from_value(json!({"id":ID})).expect("input"),
        idempotency_key: key.into(),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn/{key}"),
            now: LogicalTime::from_millis(cell.store().read().clock_floor().expect("clock")),
            world: World::Main,
            consumes: None,
        },
    };
    commit_action(&mut cell, &catalog, &call, &Authority(allow)).is_ok()
}
fn activate(path: &Path, model: &Model, key: &str) {
    assert!(governed(
        path,
        model,
        "approve",
        &format!("approve/{key}"),
        true
    ));
    assert!(governed(
        path,
        model,
        "activate",
        &format!("activate/{key}"),
        true
    ));
}
fn open(path: &Path, model: &Model) -> CellStore {
    CellStore::open(path, model.clone(), StoreOptions::default()).expect("reopen")
}
fn view(store: &CellStore, context: &QueryContext) -> RestatementView {
    store
        .read_restatement(ID, context)
        .expect("complete generation")
}
fn week(view: &RestatementView) -> &norn_cell::compute::QueryRow {
    &view.outputs["week"].result.rows["\"2026-10-19\""]
}
#[test]
fn governed_activation_switches_all_slices_and_preserves_old_answers() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let (mut store, plan, mut context, model) = setup(&path);
    let k1 = cutoff(&store, &context);
    stage(&mut store, &plan, &context, k1);
    assert!(store.read_restatement(ID, &context).is_err());
    drop(store);
    assert!(!governed(&path, &model, "activate", "premature", true));
    assert!(!governed(&path, &model, "approve", "denied", false));
    activate(&path, &model, "K1");
    let mut store = open(&path, &model);
    let old = view(&store, &context);
    let old_bytes = serde_json::to_vec(&old).expect("original answer");
    let old_version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("version");
    assert_eq!(week(&old).totals[0].canonical(), "294");
    assert_eq!(week(&old).value.expect("ratio").canonical(), "0.98");
    assert_eq!(old.label, "restated");
    assert!(!old.stale);
    let at = CLOCK + 86_400_000;
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: SCOPE.into(),
            logical_time: at,
            writes: vec![put(
                "SYN-MELT-15-1",
                75_000,
                72_000,
                "2026-10-23T12:01:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("late correction");
    let frontier = store.verified_head().expect("K2").commits;
    progress(&mut context, at, frontier);
    let k2 = cutoff(&store, &context);
    let version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("version");
    let build = store
        .begin_restatement(
            ID,
            context.clone(),
            version,
            "late correction",
            BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("begin");
    let stale = view(&store, &context);
    assert!(stale.stale);
    assert_eq!(stale.generation, old.generation);
    assert_eq!(week(&stale).totals[0].canonical(), "294");
    let prepared = store
        .prepare_restatement(build, requests(&plan, &context, k2), |_| Ok(true))
        .expect("delta equals full recompute");
    assert!(
        store
            .stage_restatement_probed(
                prepared,
                &mut |point| if point == CommitPoint::ReceiptWritten {
                    Err(norn_cell::CellError::Engine("interrupted rebuild".into()))
                } else {
                    Ok(())
                }
            )
            .is_err()
    );
    drop(store);
    let mut store = open(&path, &model);
    assert_eq!(view(&store, &context).generation, old.generation);
    assert!(view(&store, &context).stale);
    let resumed = store
        .resume_restatement(ID, context.clone())
        .expect("resume");
    let prepared = store
        .prepare_restatement(resumed, requests(&plan, &context, k2), |_| Ok(true))
        .expect("restart correspondence");
    store.stage_restatement(prepared).expect("ready");
    assert_eq!(view(&store, &context).generation, old.generation);
    drop(store);
    activate(&path, &model, "K2");
    let store = open(&path, &model);
    let current = view(&store, &context);
    assert!(!current.stale);
    assert_ne!(current.generation, old.generation);
    for (id, output) in &current.outputs {
        assert_eq!(
            output.result.rows["\"2026-10-19\""].totals[0].canonical(),
            if id == "week" { "292.5" } else { "7.5" }
        );
        assert_eq!(output.result.known_as_of, k2.frontier);
    }
    assert_eq!(week(&current).totals[1].canonical(), "300");
    assert_eq!(week(&current).value.expect("ratio").canonical(), "0.975");
    let delta = &current.deltas["week"];
    assert_eq!(delta.retract["\"2026-10-19\""].totals[0].canonical(), "294");
    assert_eq!(
        delta.replace["\"2026-10-19\""].totals[0].canonical(),
        "292.5"
    );
    let original = store
        .read_restatement_at_version(ID, old_version, &context)
        .expect("reproducible original");
    assert_eq!(serde_json::to_vec(&original).expect("original"), old_bytes);
    let mut revoked = context.clone();
    revoked.authorization_epoch += 1;
    assert!(store.read_restatement(ID, &revoked).is_err());
}
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn activated_deltas_equal_full_recompute_through_redelivery_and_restart(changes in proptest::collection::vec((0i64..75_000,14u8..28,proptest::bool::ANY,proptest::bool::ANY,proptest::bool::ANY),1..5)) {
        let dir=tempfile::tempdir().expect("directory");let path=dir.path().join("cell.sqlite");let (mut store,plan,mut context,model)=setup(&path);let point=cutoff(&store,&context);stage(&mut store,&plan,&context,point);drop(store);activate(&path,&model,"initial");
        let mut store=open(&path,&model);let original_version=store.read().version(MAIN_WORLD,RESTATEMENT_TYPE,ID).expect("version");let original=serde_json::to_vec(&view(&store,&context)).expect("original");
        for (index,(recovered,day,deleted,redeliver,restart)) in changes.into_iter().enumerate() {
            let at=CLOCK+u64::try_from(index+1).expect("bounded");let batch=CommitBatch{world:MAIN_WORLD.into(),scope:SCOPE.into(),logical_time:at,idempotency_key:Some(format!("correction/{index}")),writes:vec![if deleted {norn_cell::FactWriteRow::Delete{object_type:TYPE.into(),key:"SYN-MELT-15-1".into()}} else {put("SYN-MELT-15-1",75_000,recovered,&format!("2026-10-{day:02}T12:01:00Z"),"gold")}],..CommitBatch::default()};store.commit(&batch).expect("correction");let head=store.verified_head().expect("head");if redeliver {store.commit(&batch).expect("redelivery");proptest::prop_assert_eq!(store.verified_head().expect("head"),head.clone());}
            progress(&mut context,at,head.commits);let point=cutoff(&store,&context);let old=view(&store,&context).generation;
            let version=store.read().version(MAIN_WORLD,RESTATEMENT_TYPE,ID).expect("version");let build=store.begin_restatement(ID,context.clone(),version,"generated late evidence",BTreeSet::from(["week".into(),"loss".into()])).expect("begin");
            if restart {drop(build);drop(store);store=open(&path,&model);proptest::prop_assert_eq!(view(&store,&context).generation,old.clone());}
            let build=store.resume_restatement(ID,context.clone()).expect("durable build");let prepared=store.prepare_restatement(build,requests(&plan,&context,point),|_|Ok(true)).expect("generated IVM equals recompute");store.stage_restatement(prepared).expect("stage");proptest::prop_assert_eq!(view(&store,&context).generation,old);drop(store);activate(&path,&model,&format!("generation/{index}"));store=open(&path,&model);
            let active=view(&store,&context);let exact=store.evaluate_question_known_at(&plan,context.clone(),point,|_|Ok(true)).expect("same slice exact");for (id,answer) in &active.outputs {let expected=if id=="week" {exact.clone()} else {store.evaluate_question_known_at(&loss_plan(),context.clone(),point,|_|Ok(true)).expect("loss recompute")};proptest::prop_assert_eq!(&answer.result,&expected.result);}
            proptest::prop_assert_eq!(serde_json::to_vec(&store.read_restatement_at_version(ID,original_version,&context).expect("old generation")).expect("original"),original.clone());
        }
    }
}
