//! Core 08 (E2, #1945, `WORLD-FORK`): virtual scenario clock (effective vs.
//! recorded time), deterministic recorded entropy, fixture external-result
//! outcomes, nested `D_i -> S_i` isolation, and `FORK-NONINTERFERE`.

#![cfg(feature = "native")]

use super::query_support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_cell::compute::{ForkQuestionResult, Freshness, QueryContext, Watermark, compare_forks};
use norn_cell::{
    CellError, CellStore, ForkScenario, LedgerHead, MAIN_WORLD, Model, SqliteCell, StoreImage,
    WorldContext,
};
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::{QuestionPlan, admit};
use norn_ir::{
    ArtifactEnvelope, Decimal, MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue as V,
};
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, Consumed, Effect, Entropy, FactKey, FactMutation,
    FactVersion, FactWrite, FreshnessPolicy as EffectFreshness, Generation, IdempotencyKey,
    LogicalTime, ScenarioClock, Timer, TimerId, World, WorldId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type SimCell = SqliteCell<SimEvent, SimCommand, SimInput>;
type SimScenario = ForkScenario<SimEvent, SimCommand, SimInput>;

const SCOPE: &str = "org_demo/dev";
/// 2026-07-10T00:00:00Z in Unix milliseconds.
const EFFECTIVE_START: u64 = 1_783_641_600_000;
/// Recorded clock for the test harness (2026-10-25T00:00:00Z).
const CLOCK: u64 = 1_792_972_800_000;
const DAY_MS: u64 = 86_400_000;
const SEED_A: [u8; 32] = [0xA5; 32];
const SEED_B: [u8; 32] = [0x5A; 32];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum SimEvent {
    Seeded {
        key: String,
    },
    PaymentConfirmed {
        key: String,
        extra_grams: i64,
        entropy_tag: u8,
    },
    TimerFired {
        key: String,
        bonus_grams: i64,
        entropy_tag: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SimCommand {
    key: String,
    action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SimInput {
    key: String,
    extra_grams: i64,
}

fn sum_plan_and_model() -> Result<(Model, QuestionPlan), Box<dyn std::error::Error>> {
    let object: ObjectTypeDef = query_support::object_type();
    let ontology: Value =
        serde_json::from_str(include_str!("../../../fixtures/ir/metrics/ontology.json"))?;
    let mut artifact = ontology
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .ok_or("missing entity artifact in metrics fixture")?;
    let fields = artifact
        .as_object_mut()
        .ok_or("metrics fixture artifact must be an object")?;
    fields.insert("artifactId".to_owned(), json!("demo.Conversion.entity.v2"));
    fields.insert("name".to_owned(), json!(query_support::TYPE));
    fields.insert("namespace".to_owned(), json!("demo"));
    fields.insert(
        "provenance".to_owned(),
        json!({"source":"import","sourcePath":"synthetic:fork-e2-qualification"}),
    );
    fields.insert(
        "payload".to_owned(),
        json!({"entityVersion":"2.0","objectType":object}),
    );
    let artifact: ArtifactEnvelope = serde_json::from_value(artifact)?;
    let catalog =
        ScopedView::uniform(&[artifact], &std::collections::BTreeSet::new()).into_catalog();
    let model = Model::from_definitions(&[object], &[])?;

    let mut metric: MetricDefinition = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/net-sales.definition.json"
    ))?;
    metric.name = "demo.recovered_total".into();
    metric.label = "Recovered total".into();
    metric.type_params.expr = Some("recovered".into());
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
    question.name = "demo.recovered_weekly".into();
    question.object_set = norn_ir::ObjectTypeName(query_support::TYPE.into());
    question.comparison = None;
    question.metrics = serde_json::from_value(json!([{"metric": "demo.recovered_total"}]))?;
    question.dimensions =
        serde_json::from_value(json!([{"path": {"property": "occurred_at"}, "grain": "week"}]))?;
    question.filters = serde_json::from_value(
        json!([{"path": {"property": "metal"}, "operator": "eq", "value": "gold"}]),
    )?;
    question.time_window.start = norn_ir::UtcTimestamp("2026-07-06T00:00:00Z".into());
    question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
    question.freshness = norn_ir::metric::FreshnessPolicy::Live {
        max_age_ms: norn_ir::WholeNumber(5_000),
    };

    let plan = admit(&[metric], &question, &catalog, 100)
        .map_err(|err| format!("admit sum plan: {err:?}"))?;
    Ok((model, plan))
}

fn query_context_with_heartbeat(heartbeat_ms: u64) -> QueryContext {
    QueryContext {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        population_pin: "pop:e2-world-fork".to_owned(),
        authorization_epoch: 1,
        mapping_pin: "mapping:e2-v1".to_owned(),
        now_ms: heartbeat_ms,
        source_cursors: BTreeMap::from([(
            "erp".to_owned(),
            BTreeMap::from([("lots".to_owned(), "cursor/20".to_owned())]),
        )]),
        watermarks: BTreeMap::from([(
            "erp".to_owned(),
            BTreeMap::from([(
                "lots".to_owned(),
                Watermark {
                    observed: "cursor/20".to_owned(),
                    heartbeat_ms: Some(heartbeat_ms),
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
        ("metal".to_owned(), V::from("gold")),
    ]))
}

fn open_sim_cell(store: CellStore, dispatches: Arc<AtomicU64>) -> Result<SimCell, CellError> {
    let mut cell = SqliteCell::new(store, SCOPE, move |cmd: &Command<SimCommand>| {
        dispatches.fetch_add(1, Ordering::SeqCst);
        SimInput {
            key: cmd.payload.key.clone(),
            extra_grams: 12_000,
        }
    })?;
    cell.advance_to(LogicalTime::from_millis(CLOCK));
    Ok(cell)
}

fn seed_main_with_payment_and_timer(cell: &mut SimCell) -> TestResult {
    cell.commit(CellCommit {
        writes: vec![
            FactWrite {
                key: FactKey::new(query_support::TYPE, "lot-main"),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: conversion_values(100_000, 80_000, "2026-07-08T00:00:00Z")?,
                },
            },
            // Dated 2026-07-15: excluded when effective clock is 2026-07-10, included after +14d.
            FactWrite {
                key: FactKey::new(query_support::TYPE, "lot-future-effective"),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: conversion_values(40_000, 25_000, "2026-07-15T00:00:00Z")?,
                },
            },
        ],
        events: vec![SimEvent::Seeded {
            key: "lot-main".to_owned(),
        }],
        effects: vec![
            Effect::Perform(Command {
                id: CommandId("cmd-pay-1".to_owned()),
                generation: Generation(0),
                idempotency_key: IdempotencyKey("idem-pay-1".to_owned()),
                freshness: EffectFreshness::AppendOnly,
                deadline: None,
                payload: SimCommand {
                    key: "lot-main".to_owned(),
                    action: "payment.capture".to_owned(),
                },
            }),
            Effect::Schedule(Timer {
                id: TimerId("timer-restock-1".to_owned()),
                due: LogicalTime::from_millis(CLOCK + 2 * DAY_MS),
                payload: SimInput {
                    key: "lot-timer".to_owned(),
                    extra_grams: 20_000,
                },
            }),
        ],
        ..CellCommit::empty()
    })
    .map_err(|err| format!("seed main commit: {err:?}"))?;
    Ok(())
}

fn entropy_bonus_grams(entropy: Entropy) -> (u8, i64) {
    let tag = entropy.as_bytes().first().copied().unwrap_or(0);
    (tag, i64::from(tag).saturating_mul(100))
}

fn consume_simulated_payment(scenario: &mut SimScenario, entropy: Entropy) -> TestResult {
    let (tag, bonus) = entropy_bonus_grams(entropy);
    let simulated = scenario
        .apply_fixture_perform()?
        .ok_or("expected fixture outcome")?;
    let total_grams = simulated.input.extra_grams.saturating_add(bonus);
    scenario
        .commit(
            CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(query_support::TYPE, &simulated.input.key),
                    expected: FactVersion(0),
                    mutation: FactMutation::Put {
                        values: conversion_values(
                            50_000_i64.saturating_add(bonus),
                            total_grams,
                            "2026-07-09T00:00:00Z",
                        )?,
                    },
                }],
                events: vec![SimEvent::PaymentConfirmed {
                    key: simulated.input.key,
                    extra_grams: total_grams,
                    entropy_tag: tag,
                }],
                ..CellCommit::empty()
            }
            .consuming(Consumed::Result {
                command: simulated.command,
                generation: simulated.generation,
            }),
        )
        .map_err(|err| format!("consume simulated payment: {err:?}"))?;
    Ok(())
}

fn consume_due_timer(scenario: &mut SimScenario, write_key: &str, entropy: Entropy) -> TestResult {
    let (tag, _) = entropy_bonus_grams(entropy);
    let due = scenario.clock_due()?.ok_or("expected due timer")?;
    let consumed_timer = due.consumed();
    scenario
        .commit(
            CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(query_support::TYPE, write_key),
                    expected: FactVersion(0),
                    mutation: FactMutation::Put {
                        values: conversion_values(
                            30_000,
                            due.timer.payload.extra_grams,
                            "2026-07-14T00:00:00Z",
                        )?,
                    },
                }],
                events: vec![SimEvent::TimerFired {
                    key: due.timer.payload.key,
                    bonus_grams: due.timer.payload.extra_grams,
                    entropy_tag: tag,
                }],
                ..CellCommit::empty()
            }
            .consuming(consumed_timer),
        )
        .map_err(|err| format!("consume due timer: {err:?}"))?;
    Ok(())
}

fn run_scripted_scenario(
    main_store: &CellStore,
    plan: &QuestionPlan,
    world: &str,
    seed: [u8; 32],
) -> Result<
    (
        WorldContext,
        ForkQuestionResult,
        ForkQuestionResult,
        StoreImage,
    ),
    Box<dyn std::error::Error>,
> {
    let base = main_store.verified_head()?;
    let fork_dispatches = Arc::new(AtomicU64::new(0));
    let fork_cell = open_sim_cell(main_store.fork(world, &base)?, Arc::clone(&fork_dispatches))?;
    let params = BTreeMap::from([
        ("lead_time_days".to_owned(), V::from(14_i64)),
        ("demand_multiplier".to_owned(), V::from("1.30")),
    ]);
    let mut scenario = ForkScenario::open(
        fork_cell,
        params,
        ScenarioClock::new(
            LogicalTime::from_millis(EFFECTIVE_START),
            LogicalTime::from_millis(CLOCK),
        ),
        Entropy::from_bytes(seed),
    )?;
    scenario.register_fixture(
        "cmd-pay-1",
        0,
        &SimInput {
            key: "lot-pay-sim".to_owned(),
            extra_grams: 35_000,
        },
    )?;

    // At effective = 2026-07-10T00:00:00Z, lot-future-effective (2026-07-15) is excluded.
    let early =
        scenario.evaluate_question(plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?;
    assert_eq!(
        early.result.effective_as_of.as_str(),
        "2026-07-10T00:00:00Z"
    );
    assert_eq!(early.result.freshness, Freshness::Exact);
    assert!(!early.result.rows.contains_key("\"2026-07-13\""));

    // Advance ONLY effective clock +14 days (to 2026-07-24): recorded clock & due timers stay at CLOCK.
    let eff = scenario.advance_effective_by(14 * DAY_MS)?;
    assert_eq!(eff.effective.as_millis(), EFFECTIVE_START + 14 * DAY_MS);
    assert_eq!(eff.recorded.as_millis(), CLOCK);
    assert_eq!(scenario.clock_due()?, None);

    // Effective advance alone now includes lot-future-effective (2026-07-15) and changes pins.
    let obs_pay = scenario.observations("apply-payment-fixture")?;
    assert_eq!(obs_pay.clock, eff);
    assert_eq!(
        obs_pay.world,
        World::Fork {
            id: WorldId(world.to_owned()),
        }
    );
    consume_simulated_payment(&mut scenario, obs_pay.entropy)?;

    // Advance recorded virtual clock +2 days -> due timer fires; old CLOCK heartbeat is now Stale.
    scenario.advance_recorded_by(2 * DAY_MS)?;
    let obs_timer = scenario.observations("fire-restock-timer")?;
    consume_due_timer(&mut scenario, "lot-timer", obs_timer.entropy)?;

    assert_eq!(fork_dispatches.load(Ordering::SeqCst), 0);
    let stale_after =
        scenario.evaluate_question(plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?;
    assert_eq!(stale_after.result.freshness, Freshness::Stale);
    assert_eq!(
        stale_after.result.effective_as_of.as_str(),
        "2026-07-24T00:00:00Z"
    );
    assert_ne!(stale_after.base_query_pin, early.base_query_pin);
    assert_ne!(stale_after.fork_query_pin, early.fork_query_pin);
    assert_eq!(
        stale_after
            .result
            .rows
            .get("\"2026-07-13\"")
            .and_then(|r| r.value),
        Some(Decimal::parse("45.000000")?)
    );
    let image = scenario.store().image()?;
    Ok((scenario.context().clone(), early, stale_after, image))
}

#[test]
fn main_cell_refuses_fork_scenario_and_guards_clocks_scopes_and_fixtures() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model)?;
    let main_dispatches = Arc::new(AtomicU64::new(0));
    let mut main_cell = open_sim_cell(main_store, Arc::clone(&main_dispatches))?;
    seed_main_with_payment_and_timer(&mut main_cell)?;
    let base = main_cell.store().verified_head()?;

    let fork_dispatches = Arc::new(AtomicU64::new(0));
    let fork_cell = open_sim_cell(
        main_cell.store().fork("world-fork-check", &base)?,
        Arc::clone(&fork_dispatches),
    )?;
    let behind_cell = open_sim_cell(
        main_cell.store().fork("behind-clock-check", &base)?,
        Arc::new(AtomicU64::new(0)),
    )?;

    // Main never opens a simulated ForkScenario (FORK-SIMULATE-1).
    let main_open = ForkScenario::open(
        main_cell,
        BTreeMap::new(),
        ScenarioClock::synchronized(LogicalTime::from_millis(CLOCK)),
        Entropy::from_bytes(SEED_A),
    );
    assert!(matches!(
        main_open,
        Err(CellError::WorldSimulationForbidden)
    ));

    // Opening with recorded clock behind cell.now() refuses stale_input.
    let behind_open = ForkScenario::open(
        behind_cell,
        BTreeMap::new(),
        ScenarioClock::synchronized(LogicalTime::from_millis(CLOCK - 1)),
        Entropy::from_bytes(SEED_A),
    );
    assert!(matches!(
        behind_open,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));

    // WORLD-FORK: fork refuses real perform and refuses unfixtured apply_fixture_perform.
    let mut scenario = ForkScenario::open(
        fork_cell,
        BTreeMap::new(),
        ScenarioClock::synchronized(LogicalTime::from_millis(CLOCK)),
        Entropy::from_bytes(SEED_A),
    )?;
    assert_eq!(
        scenario.context().definition_pin,
        scenario.store().model().model_hash()
    );
    assert!(matches!(
        scenario.perform(),
        Err(CellError::WorldPerformForbidden)
    ));
    assert!(matches!(
        scenario.apply_fixture_perform(),
        Err(CellError::WorldPerformForbidden)
    ));
    assert_eq!(fork_dispatches.load(Ordering::SeqCst), 0);

    // QueryContext scope & world mismatches fail closed instead of being overwritten.
    let mut bad_scope = query_context_with_heartbeat(CLOCK);
    bad_scope.scope = "other_org/prod".to_owned();
    assert!(matches!(
        scenario.evaluate_question(&plan, bad_scope, |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    let mut bad_world = query_context_with_heartbeat(CLOCK);
    bad_world.world = "sibling-fork".to_owned();
    assert!(matches!(
        scenario.evaluate_question(&plan, bad_world, |_| Ok(true)),
        Err(CellError::ForkInvalidWorld)
    ));

    // Rewinding virtual effective or recorded clocks fails closed.
    assert!(matches!(
        scenario.advance_effective_to(LogicalTime::from_millis(CLOCK - 1)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert!(matches!(
        scenario.advance_recorded_to(LogicalTime::from_millis(CLOCK - 1)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    Ok(())
}

#[test]
fn deterministic_scenario_replay_produces_identical_entropy_images_and_metrics() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model)?;
    let mut main_cell = open_sim_cell(main_store, Arc::new(AtomicU64::new(0)))?;
    seed_main_with_payment_and_timer(&mut main_cell)?;

    let (ctx_1, early_1, after_1, img_1) =
        run_scripted_scenario(main_cell.store(), &plan, "replay-sim", SEED_A)?;
    let (ctx_2, early_2, after_2, img_2) =
        run_scripted_scenario(main_cell.store(), &plan, "replay-sim", SEED_A)?;
    assert_eq!(ctx_1, ctx_2);
    assert_eq!(ctx_1.recorded_entropy.len(), 2);
    assert_eq!(early_1, early_2);
    assert_eq!(after_1, after_2);
    assert_eq!(img_1, img_2);

    // Different seed changes entropy AND the entropy-driven turn's metric answer.
    let (ctx_b, _early_b, after_b, _img_b) =
        run_scripted_scenario(main_cell.store(), &plan, "replay-sim", SEED_B)?;
    assert_ne!(ctx_1.recorded_entropy, ctx_b.recorded_entropy);
    assert_ne!(
        after_1
            .result
            .rows
            .get("\"2026-07-06\"")
            .and_then(|r| r.value),
        after_b
            .result
            .rows
            .get("\"2026-07-06\"")
            .and_then(|r| r.value),
    );
    Ok(())
}

fn open_decision_scenario_d1(
    main_store: &CellStore,
    base: &LedgerHead,
) -> Result<(SimScenario, Arc<AtomicU64>), Box<dyn std::error::Error>> {
    let d1_dispatches = Arc::new(AtomicU64::new(0));
    let d1_cell = open_sim_cell(
        main_store.fork("decision-d1", base)?,
        Arc::clone(&d1_dispatches),
    )?;
    let mut d1 = ForkScenario::open(
        d1_cell,
        BTreeMap::from([("option".to_owned(), V::from("expedite-air"))]),
        ScenarioClock::synchronized(LogicalTime::from_millis(CLOCK)),
        Entropy::from_bytes(SEED_A),
    )?;
    d1.register_fixture(
        "cmd-pay-1",
        0,
        &SimInput {
            key: "lot-d1-po".to_owned(),
            extra_grams: 40_000,
        },
    )?;
    let obs = d1.observations("d1-payment")?;
    let (tag, _) = entropy_bonus_grams(obs.entropy);
    let sim_d1 = d1
        .apply_fixture_perform()?
        .ok_or("expected D1 fixture result")?;
    // Consume cmd-pay-1 and enqueue a follow-up supplier command for nested S_1 to simulate.
    d1.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(query_support::TYPE, &sim_d1.input.key),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: conversion_values(
                        50_000,
                        sim_d1.input.extra_grams,
                        "2026-07-09T00:00:00Z",
                    )?,
                },
            }],
            events: vec![SimEvent::PaymentConfirmed {
                key: sim_d1.input.key,
                extra_grams: sim_d1.input.extra_grams,
                entropy_tag: tag,
            }],
            effects: vec![Effect::Perform(Command {
                id: CommandId("cmd-carrier-s1".to_owned()),
                generation: Generation(0),
                idempotency_key: IdempotencyKey("idem-carrier-s1".to_owned()),
                freshness: EffectFreshness::AppendOnly,
                deadline: None,
                payload: SimCommand {
                    key: "lot-s1-carrier".to_owned(),
                    action: "carrier.confirm".to_owned(),
                },
            })],
            ..CellCommit::empty()
        }
        .consuming(Consumed::Result {
            command: sim_d1.command,
            generation: sim_d1.generation,
        }),
    )
    .map_err(|err| format!("D1 commit: {err:?}"))?;
    Ok((d1, d1_dispatches))
}

fn execute_main_perform_and_consume(cell: &mut SimCell) -> TestResult {
    let performed = cell.perform()?.ok_or("expected main perform")?;
    cell.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(query_support::TYPE, "lot-main-paid"),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: conversion_values(
                        20_000,
                        performed.input.extra_grams,
                        "2026-07-09T00:00:00Z",
                    )?,
                },
            }],
            events: vec![SimEvent::PaymentConfirmed {
                key: performed.input.key,
                extra_grams: performed.input.extra_grams,
                entropy_tag: 0,
            }],
            ..CellCommit::empty()
        }
        .consuming(Consumed::Result {
            command: performed.command,
            generation: performed.generation,
        }),
    )
    .map_err(|err| format!("main perform consume: {err:?}"))?;
    Ok(())
}

fn consume_s1_carrier_fixture(s1: &mut SimScenario) -> TestResult {
    s1.register_fixture(
        "cmd-carrier-s1",
        0,
        &SimInput {
            key: "lot-s1-carrier".to_owned(),
            extra_grams: 15_000,
        },
    )?;
    let obs_carrier = s1.observations("s1-carrier-fixture")?;
    let (carrier_tag, _) = entropy_bonus_grams(obs_carrier.entropy);
    let sim_carrier = s1
        .apply_fixture_perform()?
        .ok_or("expected S1 carrier fixture")?;
    s1.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(query_support::TYPE, &sim_carrier.input.key),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: conversion_values(
                        20_000,
                        sim_carrier.input.extra_grams,
                        "2026-07-09T00:00:00Z",
                    )?,
                },
            }],
            events: vec![SimEvent::PaymentConfirmed {
                key: sim_carrier.input.key,
                extra_grams: sim_carrier.input.extra_grams,
                entropy_tag: carrier_tag,
            }],
            ..CellCommit::empty()
        }
        .consuming(Consumed::Result {
            command: sim_carrier.command,
            generation: sim_carrier.generation,
        }),
    )
    .map_err(|err| format!("S1 carrier commit: {err:?}"))?;
    Ok(())
}

fn qualify_nested_stress_scenario(
    d1: &SimScenario,
    d1_answer: &ForkQuestionResult,
    main_horizon: u64,
    plan: &QuestionPlan,
) -> TestResult {
    let s1_dispatches = Arc::new(AtomicU64::new(0));
    let s1_counter = Arc::clone(&s1_dispatches);
    let mut s1 = d1.fork_nested(
        "scenario-s1",
        None,
        Some(Entropy::from_bytes(SEED_B)),
        move |cmd| {
            s1_counter.fetch_add(1, Ordering::SeqCst);
            SimInput {
                key: cmd.payload.key.clone(),
                extra_grams: 0,
            }
        },
    )?;
    assert_eq!(s1.context().pin.parent, "decision-d1");
    assert_eq!(s1.context().pin.main_horizon, main_horizon);
    assert!(matches!(
        s1.store().transparency_checkpoint(),
        Err(CellError::ForkEvidenceForbidden)
    ));
    // Nested S_1 inherits pending cmd-carrier-s1 and refuses real perform or unfixtured apply.
    assert!(matches!(
        s1.perform(),
        Err(CellError::WorldPerformForbidden)
    ));
    assert!(matches!(
        s1.apply_fixture_perform(),
        Err(CellError::WorldPerformForbidden)
    ));

    consume_s1_carrier_fixture(&mut s1)?;
    s1.advance_recorded_by(2 * DAY_MS)?;
    let obs_timer = s1.observations("s1-stress-timer")?;
    consume_due_timer(&mut s1, "lot-s1-stress", obs_timer.entropy)?;

    // Evaluate S_1 with refreshed heartbeat vs stale heartbeat.
    let s1_stale = s1.evaluate_question(plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?;
    assert_eq!(s1_stale.result.freshness, Freshness::Stale);
    let s1_answer = s1.evaluate_question(
        plan,
        query_context_with_heartbeat(CLOCK + 2 * DAY_MS),
        |_| Ok(true),
    )?;
    assert_eq!(s1_answer.result.freshness, Freshness::Exact);
    let d1_vs_s1 = compare_forks(d1_answer, &s1_answer)?;
    assert_eq!(
        d1_vs_s1
            .deltas
            .get("\"2026-07-06\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("15.000000")?)
    );
    assert_eq!(
        d1_vs_s1
            .deltas
            .get("\"2026-07-13\"")
            .and_then(|d| d.delta_value),
        Some(Decimal::parse("20.000000")?)
    );
    assert_eq!(s1_dispatches.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn nested_scenario_s1_isolates_virtual_clock_and_fixtures_from_d1_and_main() -> TestResult {
    let (model, plan) = sum_plan_and_model()?;
    let main_store = CellStore::open_in_memory(model.clone())?;
    let main_dispatches = Arc::new(AtomicU64::new(0));
    let mut main_cell = open_sim_cell(main_store, Arc::clone(&main_dispatches))?;
    seed_main_with_payment_and_timer(&mut main_cell)?;

    // Build an identical reference main cell (trace `t \ F`) to verify FORK-NONINTERFERE.
    let ref_dispatches = Arc::new(AtomicU64::new(0));
    let mut ref_main = open_sim_cell(
        CellStore::open_in_memory(model)?,
        Arc::clone(&ref_dispatches),
    )?;
    seed_main_with_payment_and_timer(&mut ref_main)?;

    let main_base = main_cell.store().verified_head()?;
    let (d1, d1_dispatches) = open_decision_scenario_d1(main_cell.store(), &main_base)?;
    let d1_image_before_s1 = d1.store().image()?;
    let d1_ctx_before_s1 = d1.context().clone();
    let d1_answer =
        d1.evaluate_question(&plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?;

    // Interleave real main perform + commit while forks exist (FORK-NONINTERFERE).
    execute_main_perform_and_consume(&mut main_cell)?;
    execute_main_perform_and_consume(&mut ref_main)?;

    qualify_nested_stress_scenario(&d1, &d1_answer, main_base.commits, &plan)?;
    assert_eq!(d1_dispatches.load(Ordering::SeqCst), 0);

    // D_1 is completely uncontaminated by S_1 (still has pending cmd-carrier-s1 and unchanged image).
    assert_eq!(d1.pending_commands()?.len(), 1);
    assert_eq!(d1.store().image()?, d1_image_before_s1);
    assert_eq!(d1.context(), &d1_ctx_before_s1);
    assert_eq!(
        d1.evaluate_question(&plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?,
        d1_answer
    );

    // FORK-NONINTERFERE: main matches reference trace `t \ F` across ledger, outbox, and metrics.
    assert_eq!(main_dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(ref_dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(main_cell.pending_commands()?.len(), 0);
    assert_eq!(
        main_cell.store().verified_head()?.commits,
        ref_main.store().verified_head()?.commits
    );
    assert_eq!(
        main_cell
            .store()
            .evaluate_question(&plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?
            .rows,
        ref_main
            .store()
            .evaluate_question(&plan, query_context_with_heartbeat(CLOCK), |_| Ok(true))?
            .rows
    );
    main_cell.store().read().verify_ledger()?;
    Ok(())
}
