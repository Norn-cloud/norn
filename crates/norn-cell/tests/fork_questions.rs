//! Core 08 (E1, #1944): question-in-fork evaluation under durable `World` pin,
//! host compare composition, read PDP / privacy suppression across world diffs,
//! and nonempty simulated outbox + `FORK-NONINTERFERE` qualification.

#![cfg(feature = "native")]

mod query_support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_cell::compute::ivm::IncrementalQuestion;
use norn_cell::compute::lineage::LineageDisclosure;
use norn_cell::compute::{
    ForkQuestionResult, QueryContext, QueryResult, Watermark, compare_forks, compare_main_and_fork,
    main_result_pin,
};
use norn_cell::{CellError, CellStore, KnowledgeCutoff, MAIN_WORLD, Model, SqliteCell};
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::{QuestionPlan, admit};
use norn_ir::{
    ArtifactEnvelope, Decimal, MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue as V,
};
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, Consumed, Effect, FactKey, FactMutation, FactVersion,
    FactWrite, FreshnessPolicy as EffectFreshness, Generation, IdempotencyKey, World, WorldId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type SimCell = SqliteCell<SimEvent, SimCommand, SimInput>;

const SCOPE: &str = "org_demo/dev";
const CLOCK: u64 = 1_792_972_800_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum SimEvent {
    Seeded { key: String },
    SupplierConfirmed { key: String, extra_grams: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SimCommand {
    key: String,
    action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SimInput {
    key: String,
    confirmed: bool,
    extra_grams: i64,
}

fn sum_plan_with_window(
    metric_name: &str,
    expr: &str,
    window_start: &str,
) -> Result<(Model, QuestionPlan), Box<dyn std::error::Error>> {
    let object: ObjectTypeDef = query_support::object_type();
    let ontology: Value =
        serde_json::from_str(include_str!("../../../fixtures/ir/metrics/ontology.json"))?;
    let mut artifact = ontology
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .ok_or("missing entity artifact in metrics fixture")?;
    artifact["artifactId"] = json!("demo.Conversion.entity.v2");
    artifact["name"] = json!(query_support::TYPE);
    artifact["namespace"] = json!("demo");
    artifact["provenance"] =
        json!({"source":"import","sourcePath":"synthetic:fork-e1-qualification"});
    artifact["payload"] = json!({"entityVersion":"2.0","objectType":object});
    let artifact: ArtifactEnvelope = serde_json::from_value(artifact)?;
    let catalog =
        ScopedView::uniform(&[artifact], &std::collections::BTreeSet::new()).into_catalog();
    let model = Model::from_definitions(&[object], &[])?;

    let mut metric: MetricDefinition = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/net-sales.definition.json"
    ))?;
    metric.name = metric_name.into();
    metric.label = format!("Sum of {expr}");
    metric.type_params.expr = Some(expr.into());
    if let Some(aggregate) = metric.type_params.metric_aggregation_params.as_mut() {
        aggregate.semantic_model = norn_ir::ObjectTypeName(query_support::TYPE.into());
        aggregate.agg_time_dimension = "occurred_at".into();
    }
    metric.norn.currency = None;
    metric.norn.default_filters.clear();
    metric.norn.time_dimension = Some(serde_json::from_value(json!({"property": "occurred_at"}))?);

    let mut question: QuestionIR = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))?;
    question.name = format!("{metric_name}_weekly");
    question.object_set = norn_ir::ObjectTypeName(query_support::TYPE.into());
    question.comparison = None;
    question.metrics = serde_json::from_value(json!([{"metric": metric_name}]))?;
    question.dimensions =
        serde_json::from_value(json!([{"path": {"property": "occurred_at"}, "grain": "week"}]))?;
    question.filters = serde_json::from_value(
        json!([{"path": {"property": "metal"}, "operator": "eq", "value": "gold"}]),
    )?;
    question.time_window.start = norn_ir::UtcTimestamp(window_start.into());
    question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
    question.freshness = norn_ir::metric::FreshnessPolicy::Live {
        max_age_ms: norn_ir::WholeNumber(5_000),
    };

    let plan = admit(&[metric], &question, &catalog, 100)
        .map_err(|err| format!("admit sum plan: {err:?}"))?;
    Ok((model, plan))
}

fn sum_plan_and_model() -> Result<(Model, QuestionPlan), Box<dyn std::error::Error>> {
    sum_plan_with_window("demo.recovered_total", "recovered", "2026-07-06T00:00:00Z")
}

fn main_context() -> QueryContext {
    QueryContext {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        population_pin: "pop:e1-fork-qual".to_owned(),
        authorization_epoch: 1,
        mapping_pin: "mapping:e1-v1".to_owned(),
        now_ms: CLOCK,
        source_cursors: BTreeMap::from([(
            "erp".to_owned(),
            BTreeMap::from([("lots".to_owned(), "cursor/10".to_owned())]),
        )]),
        watermarks: BTreeMap::from([(
            "erp".to_owned(),
            BTreeMap::from([(
                "lots".to_owned(),
                Watermark {
                    observed: "cursor/10".to_owned(),
                    heartbeat_ms: Some(CLOCK),
                    complete: true,
                },
            )]),
        )]),
    }
}

fn conversion_values(
    input: i64,
    recovered: i64,
    at: &str,
    metal: &str,
) -> Result<BTreeMap<String, V>, Box<dyn std::error::Error>> {
    let loss = input.saturating_sub(recovered);
    Ok(BTreeMap::from([
        (
            "input".to_owned(),
            V::from(Decimal::parse(&format!("{input}e-3"))?),
        ),
        (
            "recovered".to_owned(),
            V::from(Decimal::parse(&format!("{recovered}e-3"))?),
        ),
        (
            "loss".to_owned(),
            V::from(Decimal::parse(&format!("{loss}e-3"))?),
        ),
        ("occurred_at".to_owned(), V::from(at)),
        ("metal".to_owned(), V::from(metal)),
    ]))
}

fn open_sim_cell(store: CellStore, dispatches: Arc<AtomicU64>) -> Result<SimCell, CellError> {
    SqliteCell::new(store, SCOPE, move |cmd: &Command<SimCommand>| {
        dispatches.fetch_add(1, Ordering::SeqCst);
        SimInput {
            key: cmd.payload.key.clone(),
            confirmed: true,
            extra_grams: 10_000,
        }
    })
}

fn commit_conversion(
    cell: &mut SimCell,
    key: &str,
    input: i64,
    recovered: i64,
    at: &str,
) -> TestResult {
    let expected = FactVersion(cell.store().read().version(
        MAIN_WORLD,
        query_support::TYPE,
        key,
    )?);
    let values = conversion_values(input, recovered, at, "gold")?;
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(query_support::TYPE, key),
            expected,
            mutation: FactMutation::Put { values },
        }],
        events: vec![SimEvent::Seeded {
            key: key.to_owned(),
        }],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("commit conversion {key}: {err:?}"))?;
    Ok(())
}

fn seed_main_with_outbox_command(cell: &mut SimCell) -> TestResult {
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(query_support::TYPE, "lot-main"),
            expected: FactVersion(0),
            mutation: FactMutation::Put {
                values: conversion_values(100_000, 80_000, "2026-07-08T00:00:00Z", "gold")?,
            },
        }],
        events: vec![SimEvent::Seeded {
            key: "lot-main".to_owned(),
        }],
        effects: vec![Effect::Perform(Command {
            id: CommandId("cmd-po-1".to_owned()),
            generation: Generation(0),
            idempotency_key: IdempotencyKey("idem-po-1".to_owned()),
            freshness: EffectFreshness::AppendOnly,
            deadline: None,
            payload: SimCommand {
                key: "lot-main".to_owned(),
                action: "supply.po.dispatch".to_owned(),
            },
        })],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("main seed commit: {err:?}"))?;
    Ok(())
}

fn simulate_and_consume_fork_outbox(
    d1_cell: &mut SimCell,
    fork_dispatches: &AtomicU64,
) -> TestResult {
    // WORLD-ISOLATE-1: fork perform refuses BEFORE any durable claim on nonempty outbox.
    let d1_image_pre_perform = d1_cell.store().image()?;
    assert!(matches!(
        d1_cell.perform(),
        Err(CellError::WorldPerformForbidden)
    ));
    assert_eq!(d1_cell.store().image()?, d1_image_pre_perform);
    assert_eq!(d1_cell.pending_commands()?.len(), 1);
    assert_eq!(fork_dispatches.load(Ordering::SeqCst), 0);

    // FORK-SIMULATE-1: record simulated fixture result and deliver through consume path.
    let simulated = d1_cell
        .simulate_perform(|cmd| SimInput {
            key: cmd.payload.key.clone(),
            confirmed: true,
            extra_grams: 45_000,
        })?
        .ok_or("expected simulated outbox result")?;
    assert_eq!(d1_cell.perform()?, Some(simulated.clone()));

    d1_cell
        .commit(
            CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(query_support::TYPE, "lot-simulated"),
                    expected: FactVersion(0),
                    mutation: FactMutation::Put {
                        values: conversion_values(
                            50_000,
                            simulated.input.extra_grams,
                            "2026-07-09T00:00:00Z",
                            "gold",
                        )?,
                    },
                }],
                events: vec![SimEvent::SupplierConfirmed {
                    key: simulated.input.key.clone(),
                    extra_grams: simulated.input.extra_grams,
                }],
                ..CellCommit::empty()
            }
            .consuming(Consumed::Result {
                command: simulated.command,
                generation: simulated.generation,
            }),
        )
        .map_err(|err| format!("fork consume simulated result: {err:?}"))?;

    assert_eq!(d1_cell.perform()?, None);
    assert_eq!(d1_cell.pending_commands()?.len(), 0);
    assert_eq!(fork_dispatches.load(Ordering::SeqCst), 0);
    Ok(())
}

fn qualify_nested_scenario_fork(
    d1_cell: &SimCell,
    d1_answer: &ForkQuestionResult,
    main_horizon: u64,
    plan: &QuestionPlan,
    ctx: &QueryContext,
) -> TestResult {
    let d1_head = d1_cell.store().verified_head()?;
    let d1_image_before_s1 = d1_cell.store().image()?;
    let s1_store = d1_cell.store().fork("scenario-s1", &d1_head)?;
    assert_eq!(
        s1_store.fork_pin().map(|p| p.main_horizon),
        Some(main_horizon)
    );
    assert_eq!(
        s1_store.fork_pin().map(|p| p.parent.as_str()),
        Some("decision-d1")
    );
    let mut s1_cell = open_sim_cell(s1_store, Arc::new(AtomicU64::new(0)))?;
    commit_conversion(
        &mut s1_cell,
        "lot-s1",
        30_000,
        15_000,
        "2026-07-10T00:00:00Z",
    )?;
    let s1_answer = s1_cell
        .store()
        .evaluate_question_in_fork(plan, ctx.clone(), |_| Ok(true))?;
    assert_ne!(s1_answer.fork_query_pin, d1_answer.fork_query_pin);
    assert_ne!(s1_answer.result.lineage_ref, d1_answer.result.lineage_ref);
    assert!(matches!(
        s1_cell.store().transparency_checkpoint(),
        Err(CellError::ForkEvidenceForbidden)
    ));
    let d1_vs_s1 = compare_forks(d1_answer, &s1_answer)?;
    assert_eq!(
        d1_vs_s1
            .deltas
            .get("\"2026-07-06\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("15.000000")?)
    );
    assert_eq!(d1_cell.store().image()?, d1_image_before_s1);
    Ok(())
}

fn assert_fork_world_pin_isolated(
    main_all: &QueryResult,
    fork_cell: &SimCell,
    fork_all: &ForkQuestionResult,
) -> TestResult {
    assert_eq!(
        fork_all.world,
        World::Fork {
            id: WorldId("port-strike".to_owned())
        }
    );
    assert!(fork_all.simulated);
    assert_eq!(fork_all.base_query_pin, main_all.query_pin);
    assert_eq!(fork_all.result.context.world, "port-strike");
    assert_eq!(Some(&fork_all.pin), fork_cell.store().fork_pin());
    assert_ne!(fork_all.fork_query_pin, main_all.query_pin);
    assert_ne!(fork_all.result.query_pin, main_all.query_pin);
    assert_ne!(fork_all.result.lineage_ref, main_all.lineage_ref);
    assert_ne!(fork_all.result_pin, main_result_pin(main_all)?);
    Ok(())
}

#[test]
fn fork_question_evaluates_under_world_pin_isolates_pins_and_enforces_pdp() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model)?;
    let mut main_cell = open_sim_cell(main_store, Arc::new(AtomicU64::new(0)))?;
    commit_conversion(
        &mut main_cell,
        "lot-1",
        100_000,
        80_000,
        "2026-07-08T00:00:00Z",
    )?;
    commit_conversion(
        &mut main_cell,
        "lot-2",
        50_000,
        40_000,
        "2026-07-09T00:00:00Z",
    )?;
    commit_conversion(
        &mut main_cell,
        "lot-secret",
        30_000,
        25_000,
        "2026-07-10T00:00:00Z",
    )?;

    let ctx = main_context();
    // Main store refuses fork-simulated question evaluation (FORK-SIMULATE-1).
    assert!(matches!(
        main_cell
            .store()
            .evaluate_question_in_fork(&plan, ctx.clone(), |_| Ok(true)),
        Err(CellError::WorldSimulationForbidden)
    ));

    let main_all = main_cell
        .store()
        .evaluate_question(&plan, ctx.clone(), |_| Ok(true))?;
    let base = main_cell.store().verified_head()?;
    let fork_store = main_cell.store().fork("port-strike", &base)?;
    let mut fork_cell = open_sim_cell(fork_store, Arc::new(AtomicU64::new(0)))?;
    commit_conversion(
        &mut fork_cell,
        "lot-4",
        60_000,
        50_000,
        "2026-07-11T00:00:00Z",
    )?;

    // WORLD-ISOLATE-1: forked store refuses unwrapped `evaluate_question` even with world="main".
    assert!(matches!(
        fork_cell
            .store()
            .evaluate_question(&plan, ctx.clone(), |_| Ok(true)),
        Err(CellError::ForkInvalidWorld)
    ));

    // Unrelated world in QueryContext fails closed.
    let mut wrong_ctx = ctx.clone();
    wrong_ctx.world = "unrelated-world".to_owned();
    assert!(matches!(
        fork_cell
            .store()
            .evaluate_question_in_fork(&plan, wrong_ctx, |_| Ok(true)),
        Err(CellError::ForkInvalidWorld)
    ));

    // Evaluating in the fork binds World::Fork, simulated: true, and isolated pins.
    let mut fork_ctx = ctx.clone();
    fork_ctx.world = "port-strike".to_owned();
    let fork_all = fork_cell
        .store()
        .evaluate_question_in_fork(&plan, fork_ctx.clone(), |_| Ok(true))?;
    assert_fork_world_pin_isolated(&main_all, &fork_cell, &fork_all)?;

    // Read PDP visibility enforces identical row filtering inside the fork.
    let main_filtered = main_cell
        .store()
        .evaluate_question(&plan, ctx, |rec| Ok(rec.id != "lot-secret"))?;
    let fork_filtered = fork_cell
        .store()
        .evaluate_question_in_fork(&plan, fork_ctx, |rec| Ok(rec.id != "lot-secret"))?;
    let week_key = "\"2026-07-06\"";
    assert_eq!(
        main_filtered.rows.get(week_key).and_then(|r| r.value),
        Some(Decimal::parse("120.000000")?)
    );
    assert_eq!(
        fork_filtered
            .result
            .rows
            .get(week_key)
            .and_then(|r| r.value),
        Some(Decimal::parse("170.000000")?)
    );
    assert!(
        !fork_filtered
            .result
            .rows
            .get(week_key)
            .is_some_and(|r| r.members.contains_key("lot-secret"))
    );

    // PRIVACY-1 / DATA-VALUES-1: withheld small buckets suppress deltas across compare.
    let mut withheld_fork = fork_filtered;
    if let Some(row) = withheld_fork.result.rows.get_mut(week_key) {
        row.withhold(|id| id == "lot-4", 5);
        assert!(row.suppressed);
    }
    let suppressed_diff = compare_main_and_fork(&main_filtered, &withheld_fork)?;
    let delta = suppressed_diff
        .deltas
        .get(week_key)
        .ok_or("missing week bucket")?;
    assert!(delta.suppressed);
    assert_eq!(delta.baseline_value, None);
    assert_eq!(delta.candidate_value, None);
    assert_eq!(delta.delta_value, None);

    Ok(())
}

#[test]
fn host_compare_composition_returns_frontiers_pins_and_exact_deltas() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model)?;
    let mut main_cell = open_sim_cell(main_store, Arc::new(AtomicU64::new(0)))?;
    commit_conversion(
        &mut main_cell,
        "w1-a",
        100_000,
        80_000,
        "2026-07-08T00:00:00Z",
    )?;
    commit_conversion(
        &mut main_cell,
        "w2-a",
        50_000,
        40_000,
        "2026-07-15T00:00:00Z",
    )?;

    let ctx = main_context();
    let main_ans = main_cell
        .store()
        .evaluate_question(&plan, ctx.clone(), |_| Ok(true))?;
    let base = main_cell.store().verified_head()?;

    // Fork A: updates week 1 (+25), leaves week 2 unchanged, adds week 3 (+30).
    let mut fork_a = open_sim_cell(
        main_cell.store().fork("option-a", &base)?,
        Arc::new(AtomicU64::new(0)),
    )?;
    commit_conversion(&mut fork_a, "w1-b", 40_000, 25_000, "2026-07-09T00:00:00Z")?;
    commit_conversion(&mut fork_a, "w3-a", 40_000, 30_000, "2026-07-22T00:00:00Z")?;
    let ans_a = fork_a
        .store()
        .evaluate_question_in_fork(&plan, ctx.clone(), |_| Ok(true))?;

    // Fork B: lowers week 1 (-10 via overwrite of w1-a from 80 -> 70), leaves week 2 unchanged.
    let mut fork_b = open_sim_cell(
        main_cell.store().fork("option-b", &base)?,
        Arc::new(AtomicU64::new(0)),
    )?;
    commit_conversion(&mut fork_b, "w1-a", 100_000, 70_000, "2026-07-08T00:00:00Z")?;
    let ans_b = fork_b
        .store()
        .evaluate_question_in_fork(&plan, ctx, |_| Ok(true))?;

    let main_vs_a = compare_main_and_fork(&main_ans, &ans_a)?;
    assert_eq!(main_vs_a.baseline.world, World::Main);
    assert!(!main_vs_a.baseline.simulated);
    assert_eq!(main_vs_a.baseline.revision, base);
    assert_eq!(main_vs_a.baseline.result_pin, main_result_pin(&main_ans)?);
    assert_eq!(
        main_vs_a.candidate.world,
        World::Fork {
            id: WorldId("option-a".to_owned())
        }
    );
    assert!(main_vs_a.candidate.simulated);
    assert_eq!(main_vs_a.candidate.result_pin, ans_a.result_pin);
    assert_eq!(
        main_vs_a.candidate.source_cursors,
        ans_a.result.context.source_cursors
    );
    assert_eq!(
        main_vs_a
            .deltas
            .get("\"2026-07-06\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("25.000000")?)
    );
    assert_eq!(
        main_vs_a
            .deltas
            .get("\"2026-07-13\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("0.000000")?)
    );
    assert_eq!(
        main_vs_a
            .deltas
            .get("\"2026-07-20\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("30.000000")?)
    );

    // Compare sibling decision forks A vs B: week 1 is 70 - 105 = -35; week 3 removed (-30).
    let a_vs_b = compare_forks(&ans_a, &ans_b)?;
    assert!(a_vs_b.baseline.simulated && a_vs_b.candidate.simulated);
    assert_ne!(a_vs_b.baseline.query_pin, a_vs_b.candidate.query_pin);
    assert_eq!(
        a_vs_b
            .deltas
            .get("\"2026-07-06\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("-35.000000")?)
    );
    assert_eq!(
        a_vs_b
            .deltas
            .get("\"2026-07-20\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("-30.000000")?)
    );

    Ok(())
}

#[test]
fn host_compare_rejects_incompatible_questions_scopes_and_forged_fork_pins() -> TestResult {
    let (model, recovered_plan) = sum_plan_and_model()?;
    let (_, input_plan) =
        sum_plan_with_window("demo.input_total", "input", "2026-07-06T00:00:00Z")?;
    let (_, later_window_plan) =
        sum_plan_with_window("demo.recovered_total", "recovered", "2026-07-13T00:00:00Z")?;

    let main_store = CellStore::open_in_memory(model)?;
    let mut main_cell = open_sim_cell(main_store, Arc::new(AtomicU64::new(0)))?;
    commit_conversion(
        &mut main_cell,
        "w1-a",
        100_000,
        80_000,
        "2026-07-08T00:00:00Z",
    )?;
    commit_conversion(
        &mut main_cell,
        "w2-a",
        50_000,
        40_000,
        "2026-07-15T00:00:00Z",
    )?;

    let ctx = main_context();
    let main_recovered =
        main_cell
            .store()
            .evaluate_question(&recovered_plan, ctx.clone(), |_| Ok(true))?;
    let base = main_cell.store().verified_head()?;
    let fork_store = main_cell.store().fork("option-check", &base)?;

    // Different metric over same grain, scope, and effective window must fail closed.
    let fork_input =
        fork_store.evaluate_question_in_fork(&input_plan, ctx.clone(), |_| Ok(true))?;
    assert!(matches!(
        compare_main_and_fork(&main_recovered, &fork_input),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));

    // Different time_window.start with identical grain, scope, and effective_as_of must fail closed.
    let fork_later_window =
        fork_store.evaluate_question_in_fork(&later_window_plan, ctx.clone(), |_| Ok(true))?;
    assert!(matches!(
        compare_main_and_fork(&main_recovered, &fork_later_window),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));

    // Mismatched scope fails closed.
    let fork_recovered =
        fork_store.evaluate_question_in_fork(&recovered_plan, ctx, |_| Ok(true))?;
    let mut bad_scope = fork_recovered.clone();
    bad_scope.result.context.scope = "other_org/prod".to_owned();
    assert!(matches!(
        compare_forks(&fork_recovered, &bad_scope),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));

    // Forged base_query_pin (mismatching fork_query_pin) fails closed.
    let mut forged = fork_input;
    main_recovered
        .query_pin
        .clone_into(&mut forged.base_query_pin);
    assert!(matches!(
        compare_main_and_fork(&main_recovered, &forged),
        Err(CellError::ForkInvalidWorld)
    ));

    // Tampered result_pin fails closed.
    let mut tampered_pin = fork_recovered.clone();
    tampered_pin.result_pin = "forged-result-pin".to_owned();
    assert!(matches!(
        compare_main_and_fork(&main_recovered, &tampered_pin),
        Err(CellError::ForkInvalidWorld)
    ));
    assert!(matches!(
        compare_forks(&fork_recovered, &tampered_pin),
        Err(CellError::ForkInvalidWorld)
    ));

    Ok(())
}

#[test]
fn main_only_compute_boundaries_refuse_forked_stores() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model)?;
    let mut main_cell = open_sim_cell(main_store, Arc::new(AtomicU64::new(0)))?;
    commit_conversion(
        &mut main_cell,
        "lot-1",
        100_000,
        80_000,
        "2026-07-08T00:00:00Z",
    )?;
    let ctx = main_context();
    let main_ans = main_cell
        .store()
        .evaluate_question(&plan, ctx.clone(), |_| Ok(true))?;
    let base = main_cell.store().verified_head()?;
    let fork_store = main_cell.store().fork("boundary-check", &base)?;

    let cutoff = KnowledgeCutoff {
        recorded_at: norn_kernel::LogicalTime::from_millis(CLOCK),
        frontier: base.commits,
    };
    assert!(matches!(
        fork_store.evaluate_question_known_at(&plan, ctx.clone(), cutoff, |_| Ok(true)),
        Err(CellError::ForkInvalidWorld)
    ));
    assert!(matches!(
        IncrementalQuestion::rebuild(&fork_store, &plan, ctx.clone(), |_| Ok(true)),
        Err(CellError::ForkInvalidWorld)
    ));
    let mut main_ivm =
        IncrementalQuestion::rebuild(main_cell.store(), &plan, ctx.clone(), |_| Ok(true))?;
    assert!(matches!(
        main_ivm.advance(&fork_store, ctx.clone(), |_| Ok(true)),
        Err(CellError::ForkInvalidWorld)
    ));
    let mut allow_values = |_: &norn_cell::ObjectRecord| Ok(true);
    let mut allow_source = |_: &norn_cell::ObjectRecord| Ok(true);
    assert!(matches!(
        fork_store.trace_question(
            &plan,
            ctx,
            &main_ans.lineage_ref,
            "\"2026-07-06\"",
            |_| Ok(true),
            LineageDisclosure {
                values: &mut allow_values,
                source: &mut allow_source,
            },
        ),
        Err(CellError::ForkInvalidWorld)
    ));

    Ok(())
}

#[test]
fn nonempty_simulated_outbox_fork_isolation_and_noninterference() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model)?;
    let main_dispatches = Arc::new(AtomicU64::new(0));
    let mut main_cell = open_sim_cell(main_store, Arc::clone(&main_dispatches))?;
    seed_main_with_outbox_command(&mut main_cell)?;

    let ctx = main_context();
    let main_image_before = main_cell.store().image()?;
    let main_head_before = main_cell.store().verified_head()?;
    let main_answer_before = main_cell
        .store()
        .evaluate_question(&plan, ctx.clone(), |_| Ok(true))?;
    assert_eq!(main_cell.pending_commands()?.len(), 1);

    // Main never records simulated results (FORK-SIMULATE-1).
    assert!(matches!(
        main_cell.simulate_perform(|cmd| SimInput {
            key: cmd.payload.key.clone(),
            confirmed: true,
            extra_grams: 0,
        }),
        Err(CellError::WorldSimulationForbidden)
    ));

    // Fork decision world D_1 at main's verified head (inherits nonempty outbox).
    let fork_dispatches = Arc::new(AtomicU64::new(0));
    let d1_store = main_cell.store().fork("decision-d1", &main_head_before)?;
    let mut d1_cell = open_sim_cell(d1_store, Arc::clone(&fork_dispatches))?;
    simulate_and_consume_fork_outbox(&mut d1_cell, &fork_dispatches)?;

    let d1_answer = d1_cell
        .store()
        .evaluate_question_in_fork(&plan, ctx.clone(), |_| Ok(true))?;
    let diff = compare_main_and_fork(&main_answer_before, &d1_answer)?;
    assert_eq!(
        diff.deltas
            .get("\"2026-07-06\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("45.000000")?)
    );

    qualify_nested_scenario_fork(&d1_cell, &d1_answer, main_head_before.commits, &plan, &ctx)?;

    // FORK-NONINTERFERE: main's store image, head, outbox, metrics, and dispatch count are unchanged.
    assert_eq!(main_cell.store().image()?, main_image_before);
    assert_eq!(main_cell.store().verified_head()?, main_head_before);
    assert_eq!(main_cell.pending_commands()?.len(), 1);
    assert_eq!(main_dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(
        main_cell
            .store()
            .evaluate_question(&plan, ctx, |_| Ok(true))?,
        main_answer_before
    );
    main_cell.store().read().verify_ledger()?;

    Ok(())
}
