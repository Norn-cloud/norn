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
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, DecisionTurn,
    FactEvent, LogicalTime, TypeCatalog, World, commit_action,
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
    let interface: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.analytics.RestatementEvidence.interface.json"
    ))
    .expect("generated interface");
    let interface: norn_ir::InterfaceDef =
        serde_json::from_value(interface["payload"]["interface"].clone())
            .expect("policy interface");
    let model =
        Model::from_definitions_with_interfaces(&types, &[], &[interface]).expect("native model");
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
    stage_at(store, plan, context, context, point);
}
fn stage_at(
    store: &mut CellStore,
    plan: &QuestionPlan,
    build_context: &QueryContext,
    query_context: &QueryContext,
    point: KnowledgeCutoff,
) {
    let version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("slot version");
    let build = store
        .begin_restatement(
            ID,
            build_context.clone(),
            version,
            "late analytical evidence",
            &BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("private rebuild");
    let prepared = store
        .prepare_restatement(build, requests(plan, query_context, point), |_| Ok(true))
        .expect("delta correspondence");
    store
        .stage_restatement(prepared)
        .expect("complete candidate");
}

fn clock_test_correction(store: &mut CellStore, at: u64, recovered: i64) {
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.into(),
            scope: SCOPE.into(),
            logical_time: at,
            writes: vec![put(
                "SYN-MELT-15-1",
                75_000,
                recovered,
                "2026-10-23T12:01:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("source correction");
}

fn finish_resumed_clock_build(
    path: &Path,
    model: &Model,
    plan: &QuestionPlan,
    build_context: &QueryContext,
    query_context: &QueryContext,
    point: KnowledgeCutoff,
) -> CellStore {
    let mut store = open(path, model);
    let build = store
        .resume_restatement(ID, build_context.clone())
        .expect("recover build after reopen");
    let head = store.verified_head().expect("before preparation");
    let prepared = store
        .prepare_restatement(build, requests(plan, query_context, point), |_| Ok(true))
        .expect("clock-ahead correspondence");
    assert_eq!(store.verified_head().expect("read-only preparation"), head);
    let later = build_context
        .now_ms
        .checked_add(2)
        .expect("later source clock");
    clock_test_correction(&mut store, later, 69_000);
    store
        .stage_restatement(prepared)
        .expect("stage over concurrent source commits");
    let staged = store.verified_head().expect("staged receipt").commits;
    assert_eq!(
        store
            .read()
            .receipt_at(staged)
            .expect("receipt")
            .expect("staged")
            .body
            .logical_time,
        later
    );
    drop(store);
    activate(path, model, "clock/K2");
    open(path, model)
}

// RESTATE-HISTORY-1: query clocks can run ahead of committed receipt clocks.
fn clock_ahead_restatement_sequence(receipt_offset: u64, query_lead: u64) {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("clock.sqlite");
    let (mut store, plan, mut context, model) = setup(&path);
    let receipt_time = CLOCK
        .checked_add(receipt_offset)
        .expect("bounded receipt clock");
    let prior_clock = receipt_time
        .checked_add(query_lead)
        .expect("bounded query clock");
    let mut query = context.clone();
    progress(
        &mut query,
        prior_clock,
        store.verified_head().expect("initial frontier").commits,
    );
    let initial = cutoff(&store, &query);
    stage_at(&mut store, &plan, &context, &query, initial);
    drop(store);
    activate(&path, &model, "clock/K1");
    let mut store = open(&path, &model);
    let old = view(&store, &context);
    assert_eq!(week(&old).totals[0].canonical(), "294");
    let old_version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("old version");
    clock_test_correction(&mut store, receipt_time, 72_000);
    let frontier = store.verified_head().expect("K2 frontier").commits;
    assert!(
        store
            .read()
            .receipt_at(frontier)
            .expect("receipt")
            .expect("committed")
            .body
            .logical_time
            < prior_clock
    );
    progress(&mut context, receipt_time, frontier);
    query = context.clone();
    progress(
        &mut query,
        prior_clock.checked_add(1).expect("next query clock"),
        frontier,
    );
    let point = cutoff(&store, &query);
    let build = store
        .begin_restatement(
            ID,
            context.clone(),
            old_version,
            "clock-ahead correction",
            &BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("begin");
    // This later version is below the clock ceiling but outside the pinned frontier.
    clock_test_correction(
        &mut store,
        receipt_time.checked_add(1).expect("later source clock"),
        70_000,
    );
    let head = store.verified_head().expect("before refusal");
    let before = serde_json::to_vec(&view(&store, &context)).expect("old complete view");
    assert!(matches!(
        store.prepare_restatement(build, requests(&plan, &query, point), |_| Err(
            norn_cell::CellError::ComputeRefused("test.refusal")
        )),
        Err(norn_cell::CellError::ComputeRefused("test.refusal"))
    ));
    assert_eq!(store.verified_head().expect("no preparation writes"), head);
    assert_eq!(
        serde_json::to_vec(&view(&store, &context)).expect("unchanged view"),
        before
    );
    assert!(view(&store, &context).stale);
    drop(store);
    let store = finish_resumed_clock_build(&path, &model, &plan, &context, &query, point);
    assert_clock_slice(&store, &plan, &context, &query, point);
    let old_again = store
        .read_restatement_at_version(ID, old_version, &context)
        .expect("retained K1");
    assert_eq!(
        serde_json::to_vec(&old_again).expect("old K1"),
        serde_json::to_vec(&old).expect("original K1")
    );
}

fn assert_clock_slice(
    store: &CellStore,
    plan: &QuestionPlan,
    context: &QueryContext,
    query: &QueryContext,
    point: KnowledgeCutoff,
) {
    let current = view(store, context);
    assert!(!current.stale);
    assert_eq!(week(&current).totals[0].canonical(), "292.5");
    assert_eq!(week(&current).value.expect("ratio").canonical(), "0.975");
    assert_eq!(week(&current).members["SYN-MELT-15-1"].version, 2);
    assert_eq!(
        store
            .read()
            .version(MAIN_WORLD, TYPE, "SYN-MELT-15-1")
            .expect("later version"),
        4
    );
    assert_eq!(
        current.outputs["week"].knowledge_time_ms,
        point.recorded_at.as_millis()
    );
    assert_eq!(current.outputs["week"].result.known_as_of, point.frontier);
    assert_same_slice(store, plan, query, point);
}

#[test]
fn clock_ahead_cutoff_survives_lower_receipts_and_failed_preparation_restart() {
    clock_ahead_restatement_sequence(800, 200);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn generated_clock_leads_preserve_pinned_restatement_and_recovery(receipt_offset in 1u64..1000, query_lead in 1u64..1000) {
        clock_ahead_restatement_sequence(receipt_offset, query_lead);
    }
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
    governed_on(store, model, transition, key, allow)
}
fn governed_on(store: CellStore, model: &Model, transition: &str, key: &str, allow: bool) -> bool {
    let mut cell: SqliteCell<FactEvent, JsonObject, JsonObject> =
        SqliteCell::new(store, SCOPE, |c: &norn_kernel::Command<JsonObject>| {
            c.payload.clone()
        })
        .expect("native host");
    let action = action(transition);
    let defs: Vec<_> = model.object_types().collect();
    let catalog = TypeCatalog::new(defs).with_interfaces(model.interfaces());
    let invocation = ActionCall {
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
    commit_action(&mut cell, &catalog, &invocation, &Authority(allow)).is_ok()
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
    assert_eq!(
        old.outputs["loss"].result.rows["\"2026-10-19\""].totals[0].canonical(),
        "6"
    );
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
            &BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("begin");
    let stale = view(&store, &context);
    assert!(stale.stale);
    assert_eq!(stale.generation, old.generation);
    assert_eq!(week(&stale).totals[0].canonical(), "294");
    interrupt_staging(&mut store, build, &plan, &context, k2);
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
    assert!(governed(&path, &model, "approve", "approve/K2", true));
    let failing = open(&path, &model);
    failing.set_candidate_validator(Some(|_, _, _| {
        Err(norn_cell::CellError::Engine(
            "interrupted activation".into(),
        ))
    }));
    assert!(!governed_on(
        failing,
        &model,
        "activate",
        "interrupted",
        true
    ));
    let interrupted = open(&path, &model);
    assert_eq!(view(&interrupted, &context).generation, old.generation);
    assert!(view(&interrupted, &context).stale);
    drop(interrupted);
    activate(&path, &model, "K2");
    let store = open(&path, &model);
    assert_activated_showcase(&store, &plan, &context, &old, old_version, k2, &old_bytes);
}
fn interrupt_staging(
    store: &mut CellStore,
    build: norn_cell::compute::restatement::RestatementBuild,
    plan: &QuestionPlan,
    context: &QueryContext,
    k2: KnowledgeCutoff,
) {
    let prepared = store
        .prepare_restatement(build, requests(plan, context, k2), |_| Ok(true))
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
}
fn assert_activated_showcase(
    store: &CellStore,
    plan: &QuestionPlan,
    context: &QueryContext,
    old: &RestatementView,
    old_version: u64,
    k2: KnowledgeCutoff,
    old_bytes: &[u8],
) {
    let current = view(store, context);
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
    assert_same_slice(store, plan, context, k2);
    for (id, operand, expected) in [("week", 0, "-1.5"), ("week", 1, "0"), ("loss", 0, "1.5")] {
        let delta = &current.deltas[id];
        assert_eq!(
            signed_delta(
                &delta.retract["\"2026-10-19\""].totals[operand],
                &delta.replace["\"2026-10-19\""].totals[operand]
            )
            .canonical(),
            expected
        );
    }
    let delta = &current.deltas["week"];
    assert_eq!(delta.retract["\"2026-10-19\""].totals[0].canonical(), "294");
    assert_eq!(
        delta.replace["\"2026-10-19\""].totals[0].canonical(),
        "292.5"
    );
    let original = store
        .read_restatement_at_version(ID, old_version, context)
        .expect("reproducible original");
    assert_eq!(serde_json::to_vec(&original).expect("original"), old_bytes);
    let delta = &current.deltas["loss"];
    assert_eq!(delta.retract["\"2026-10-19\""].totals[0].canonical(), "6");
    assert_eq!(delta.replace["\"2026-10-19\""].totals[0].canonical(), "7.5");
    let mut revoked = context.clone();
    revoked.authorization_epoch = revoked
        .authorization_epoch
        .checked_add(1)
        .expect("bounded epoch");
    assert!(store.read_restatement(ID, &revoked).is_err());
}
#[test]
fn missing_incomplete_and_stale_candidates_never_replace_the_complete_output() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (mut store, plan, context, model) = setup(&path);
    let point = cutoff(&store, &context);
    stage(&mut store, &plan, &context, point);
    drop(store);
    activate(&path, &model, "initial");
    let mut store = open(&path, &model);
    let old = view(&store, &context).generation;
    let version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("version");
    assert!(
        store
            .begin_restatement(
                ID,
                context.clone(),
                version,
                "",
                &BTreeSet::from(["week".into(), "loss".into()])
            )
            .is_err()
    );
    assert!(
        store
            .begin_restatement(
                ID,
                context.clone(),
                version,
                "changed output membership",
                &BTreeSet::from(["week".into()])
            )
            .is_err()
    );
    let build = store
        .begin_restatement(
            ID,
            context.clone(),
            version,
            "review replacement",
            &BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("build");
    let mut missing = requests(&plan, &context, point);
    missing.pop();
    assert!(
        store
            .prepare_restatement(build, missing, |_| Ok(true))
            .is_err()
    );
    let build = store
        .resume_restatement(ID, context.clone())
        .expect("resume");
    let mut incomplete = requests(&plan, &context, point);
    incomplete[0]
        .context
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition")
        .complete = false;
    assert!(
        store
            .prepare_restatement(build, incomplete, |_| Ok(true))
            .is_err()
    );
    let first = store
        .prepare_restatement(
            store
                .resume_restatement(ID, context.clone())
                .expect("resume"),
            requests(&plan, &context, point),
            |_| Ok(true),
        )
        .expect("complete first");
    let late = store
        .prepare_restatement(
            store
                .resume_restatement(ID, context.clone())
                .expect("same incarnation"),
            requests(&plan, &context, point),
            |_| Ok(true),
        )
        .expect("complete late");
    store.stage_restatement(first).expect("stage first");
    assert!(store.stage_restatement(late).is_err());
    assert_eq!(view(&store, &context).generation, old);
    assert!(view(&store, &context).stale);
}
fn generated_restatement_sequence(changes: Vec<(i64, u8, bool, bool, bool)>) {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let (mut store, plan, mut context, model) = setup(&path);
    assert!(
        norn_checker::metric::compute::certificate::question(&plan)
            .algebraic
            .iter()
            .all(|result| *result
                == norn_checker::metric::compute::certificate::Algebraic::ProvedAlgebraic)
    );
    let point = cutoff(&store, &context);
    stage(&mut store, &plan, &context, point);
    drop(store);
    activate(&path, &model, "initial");
    let mut store = open(&path, &model);
    let original_version = store
        .read()
        .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
        .expect("version");
    let original = serde_json::to_vec(&view(&store, &context)).expect("original");
    for (index, (recovered, day, deleted, redeliver, restart)) in changes.into_iter().enumerate() {
        let at = CLOCK
            .checked_add(
                u64::try_from(index.checked_add(1).expect("bounded index")).expect("bounded"),
            )
            .expect("bounded clock");
        let batch = CommitBatch {
            world: MAIN_WORLD.into(),
            scope: SCOPE.into(),
            logical_time: at,
            idempotency_key: Some(format!("correction/{index}")),
            writes: vec![if deleted {
                norn_cell::FactWriteRow::Delete {
                    object_type: TYPE.into(),
                    key: "SYN-MELT-15-1".into(),
                }
            } else {
                put(
                    "SYN-MELT-15-1",
                    75_000,
                    recovered,
                    &format!("2026-10-{day:02}T12:01:00Z"),
                    "gold",
                )
            }],
            ..CommitBatch::default()
        };
        store.commit(&batch).expect("correction");
        let head = store.verified_head().expect("head");
        if redeliver {
            store.commit(&batch).expect("redelivery");
            assert_eq!(store.verified_head().expect("head"), head.clone());
        }
        progress(&mut context, at, head.commits);
        let point = cutoff(&store, &context);
        let old = view(&store, &context).generation;
        let version = store
            .read()
            .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
            .expect("version");
        let build = store
            .begin_restatement(
                ID,
                context.clone(),
                version,
                "generated late evidence",
                &BTreeSet::from(["week".into(), "loss".into()]),
            )
            .expect("begin");
        if restart {
            drop(build);
            drop(store);
            store = open(&path, &model);
            assert_eq!(view(&store, &context).generation, old.clone());
        }
        let build = store
            .resume_restatement(ID, context.clone())
            .expect("durable build");
        let prepared = store
            .prepare_restatement(build, requests(&plan, &context, point), |_| Ok(true))
            .expect("generated IVM equals recompute");
        store.stage_restatement(prepared).expect("stage");
        assert_eq!(view(&store, &context).generation, old);
        drop(store);
        activate(&path, &model, &format!("generation/{index}"));
        store = open(&path, &model);
        assert_same_slice(&store, &plan, &context, point);
        assert_eq!(
            serde_json::to_vec(
                &store
                    .read_restatement_at_version(ID, original_version, &context)
                    .expect("old generation")
            )
            .expect("original"),
            original.clone()
        );
    }
}
fn assert_same_slice(
    store: &CellStore,
    plan: &QuestionPlan,
    context: &QueryContext,
    point: KnowledgeCutoff,
) {
    let active = view(store, context);
    let exact = store
        .evaluate_question_known_at(plan, context.clone(), point, |_| Ok(true))
        .expect("same slice exact");
    for (id, answer) in &active.outputs {
        let expected = if id == "week" {
            exact.clone()
        } else {
            store
                .evaluate_question_known_at(&loss_plan(), context.clone(), point, |_| Ok(true))
                .expect("loss recompute")
        };
        assert_eq!(&answer.result, &expected.result);
    }
}
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn activated_deltas_equal_full_recompute_through_redelivery_and_restart(changes in proptest::collection::vec((0i64..75_000,14u8..28,proptest::bool::ANY,proptest::bool::ANY,proptest::bool::ANY),1..5)) {
        generated_restatement_sequence(changes);
    }
}

#[test]
fn declared_restatement_law_and_reviewer_actions_are_admitted_together() {
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = [
        include_str!("../../../registry/core/artifacts/core.analytics.Restatement.entity.json"),
        include_str!(
            "../../../registry/core/artifacts/core.analytics.RestatementEvidence.interface.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.analytics.restatement.approve.action.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.analytics.restatement.activate.action.json"
        ),
        include_str!(
            "../../../registry/core/artifacts/core.analytics.restatement.access.policy.json"
        ),
        include_str!("../../../registry/core/artifacts/core.analytics.restatement_policy.law.json"),
    ]
    .into_iter()
    .map(|bytes| serde_json::from_str(bytes).expect("typed compiler output"))
    .collect();
    let registry = BTreeSet::new();
    let context = norn_checker::EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let findings = norn_checker::check_artifact_set(&artifacts, &context);
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn prepared_candidates_stay_bound_to_their_native_host() {
    let dir = tempfile::tempdir().expect("directory");
    let (left, plan, context, _) = setup(&dir.path().join("left.sqlite"));
    let (right, _, _, _) = setup(&dir.path().join("right.sqlite"));
    let mut left: SqliteCell<FactEvent, JsonObject, JsonObject> =
        SqliteCell::new(left, SCOPE, |c: &norn_kernel::Command<JsonObject>| {
            c.payload.clone()
        })
        .expect("left host");
    let mut right: SqliteCell<FactEvent, JsonObject, JsonObject> =
        SqliteCell::new(right, SCOPE, |c: &norn_kernel::Command<JsonObject>| {
            c.payload.clone()
        })
        .expect("right host");
    let ids = BTreeSet::from(["week".into(), "loss".into()]);
    let build = left
        .begin_restatement(ID, context.clone(), 0, "native candidate", &ids)
        .expect("left build");
    let _right_build = right
        .begin_restatement(ID, context.clone(), 0, "native candidate", &ids)
        .expect("right build");
    let point = cutoff(left.store(), &context);
    assert!(
        right
            .store()
            .prepare_restatement(build, requests(&plan, &context, point), |_| panic!(
                "foreign build must refuse before reading"
            ))
            .is_err()
    );
    let build = left
        .store()
        .resume_restatement(ID, context.clone())
        .expect("resume origin");
    let prepared = left
        .store()
        .prepare_restatement(build, requests(&plan, &context, point), |_| Ok(true))
        .expect("complete origin");
    assert!(right.stage_restatement(prepared).is_err());
    let build = left
        .store()
        .resume_restatement(ID, context.clone())
        .expect("resume origin");
    let prepared = left
        .store()
        .prepare_restatement(build, requests(&plan, &context, point), |_| Ok(true))
        .expect("complete origin");
    left.stage_restatement(prepared)
        .expect("same host stages without reopen");
}

#[test]
fn candidate_budget_includes_the_complete_staged_slot() {
    let directory = tempfile::tempdir().expect("directory");
    let (mut store, plan, context, _) = setup(&directory.path().join("cell.sqlite"));
    let point = cutoff(&store, &context);
    let size = usize::try_from(
        norn_ir::ResourceLimitProfile::norn_limits_v1()
            .slice
            .max_canonical_bytes
            .checked_div(2)
            .expect("half")
            .checked_sub(8192)
            .expect("reserve"),
    )
    .expect("bounded text");
    let reason = "x".repeat(size);
    let build = store
        .begin_restatement(
            ID,
            context.clone(),
            0,
            &reason,
            &BTreeSet::from(["week".into(), "loss".into()]),
        )
        .expect("initial row fits");
    assert!(matches!(
        store.prepare_restatement(build, requests(&plan, &context, point), |_| Ok(true)),
        Err(norn_cell::CellError::ComputeRefused(
            "core.compute.bound_exceeded"
        ))
    ));
}

#[test]
fn resumed_slice_declarations_keep_the_same_admission_bound() {
    for declared in [
        BTreeSet::new(),
        BTreeSet::from([String::new()]),
        (0..17).map(|n| format!("slice-{n}")).collect(),
    ] {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, plan, context, _) = setup(&directory.path().join("cell.sqlite"));
        store
            .begin_restatement(
                ID,
                context.clone(),
                0,
                "resume boundary",
                &BTreeSet::from(["week".into(), "loss".into()]),
            )
            .expect("initial declaration");
        let mut values = store
            .read()
            .object(MAIN_WORLD, RESTATEMENT_TYPE, ID)
            .expect("slot")
            .expect("present")
            .values;
        values.insert(
            "slice_ids".into(),
            norn_ir::RuntimeValue::from(
                serde_json::to_string(&declared).expect("persisted declaration"),
            ),
        );
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: SCOPE.into(),
                logical_time: context.now_ms,
                writes: vec![norn_cell::FactWriteRow::Put {
                    object_type: RESTATEMENT_TYPE.into(),
                    key: ID.into(),
                    values,
                }],
                ..CommitBatch::default()
            })
            .expect("authenticated alternate writer");
        let point = cutoff(&store, &context);
        let build = store
            .resume_restatement(ID, context.clone())
            .expect("authenticated resume");
        let slices = declared
            .iter()
            .map(|id| RestatementSlice {
                id,
                plan: plan.clone(),
                context: context.clone(),
                cutoff: point,
            })
            .collect();
        assert!(matches!(
            store.prepare_restatement(build, slices, |_| panic!(
                "invalid persisted declaration must refuse before input reads"
            )),
            Err(norn_cell::CellError::ComputeRefused(
                "core.world.invalid_restatement"
            ))
        ));
    }
}

#[test]
fn approved_candidate_refuses_an_alternate_kernel_writer_before_activation() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (mut store, plan, context, model) = setup(&path);
    let point = cutoff(&store, &context);
    stage(&mut store, &plan, &context, point);
    drop(store);
    for state in ["ready", "approved"] {
        if state == "approved" {
            assert!(governed(&path, &model, "approve", "reviewed", true));
        }
        let store = open(&path, &model);
        let record = store
            .read()
            .object(MAIN_WORLD, RESTATEMENT_TYPE, ID)
            .expect("slot")
            .expect("present");
        let key = norn_kernel::FactKey::new(RESTATEMENT_TYPE, ID);
        let slice = norn_kernel::FactSlice::from([(
            key.clone(),
            norn_kernel::FactState {
                version: norn_kernel::FactVersion(record.version),
                values: Some(record.values),
            },
        )]);
        let definitions: Vec<_> = model.object_types().collect();
        let catalog = TypeCatalog::new(definitions).with_interfaces(model.interfaces());
        let output = norn_ir::DecisionOutput {
            facts: vec![norn_ir::FactChange::Set {
                object_type: norn_ir::ObjectTypeName(RESTATEMENT_TYPE.into()),
                key: ID.into(),
                property: norn_ir::PropertyId("candidate".into()),
                value: norn_ir::RuntimeValue::from("unreviewed generation"),
            }],
            commands: vec![],
            timers: vec![],
        };
        let turn = DecisionTurn {
            id: format!("alternate/{state}"),
            now: LogicalTime::from_millis(context.now_ms),
            world: World::Main,
            consumes: None,
        };
        assert!(matches!(
            norn_kernel::prepare_decision::<std::convert::Infallible>(
                &catalog, &slice, &turn, &output,
            ),
            Err(norn_kernel::DecisionRefusal::ImmutableStateWrite { .. })
        ));
        assert_eq!(
            store
                .read()
                .version(MAIN_WORLD, RESTATEMENT_TYPE, ID)
                .expect("unchanged"),
            slice.get(&key).expect("read").version.0
        );
    }
    assert!(governed(
        &path,
        &model,
        "activate",
        "reviewed-activation",
        true
    ));
}
