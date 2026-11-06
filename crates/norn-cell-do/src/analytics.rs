//! The public scrap-yield analytics cell and its admitted question.
//!
//! Synthetic melt conversions (`fixtures/demo/scrap-yield-k1.csv`), measured in
//! grams, with the weekly gold yield question Studio's answers page shows. The
//! question is admitted by the shared metric checker at boot. The demo source
//! frontier is the recorded K1 snapshot: its cursor, watermark and observation
//! clock are fixed, so answers are exact and reproducible across restarts.

use std::collections::BTreeMap;

use norn_api::authz::ReadCapability;
use norn_api::question_routes::{QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::compute::{QueryContext, Watermark};
use norn_cell::engine::SqlEngine;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, Model, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::{QuestionPlan, admit};
use norn_ir::metric::{FreshnessPolicy, MetricAdditivity, MetricType, MetricTypeParams};
use norn_ir::{
    ArtifactEnvelope, Decimal, MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue,
};
use norn_kernel::authz::{AccessRequest, ActorKind, Delegation, PrincipalInput, ResourceInput};
use serde_json::{Value, json};

use crate::policy::{READ_ACTION, capability_name, scope_key};
use crate::seed::SeedError;

/// The conversion type the cell is opened with.
pub const TYPE: &str = "demo.Conversion";
/// The admitted weekly gold yield question.
pub const QUESTION: &str = "demo.yield_week";
const WORLD: &str = "synthetic-k1";
/// The recorded K1 observation clock (2026-10-23T00:00:00Z).
const CLOCK: u64 = 1_792_972_800_000;

fn fail(context: &str, error: impl std::fmt::Display) -> SeedError {
    SeedError(format!("{context}: {error}"))
}

/// The analytics cell's physical scope.
#[must_use]
pub fn analytics_scope() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "analytics".into(),
    }
}

fn object_type() -> Result<ObjectTypeDef, SeedError> {
    let mass = json!({"kind":"decimal","additivity":{"kind":"additive"},"scale":3,"unit":{"system":"ucum","code":"g"}});
    serde_json::from_value(json!({"name":TYPE,"properties":[
        {"id":"input","type":mass},
        {"id":"recovered","type":mass},
        {"id":"loss","type":mass},
        {"id":"occurred_at","type":{"kind":"timestamp","timezone":"utc"}},
        {"id":"metal","type":{"kind":"text"}}
    ]}))
    .map_err(|e| fail("object type", e))
}

/// The analytics cell: its store, opened (and loaded once) on `engine`, and
/// the admitted question plan.
pub struct Analytics<E: SqlEngine> {
    /// The opened store.
    pub store: CellStore<E>,
    /// The admitted question.
    pub plan: QuestionPlan,
}

/// Open the analytics cell, loading the K1 conversions on a fresh engine.
///
/// # Errors
/// The question no longer admits, or the engine holds a different cell.
pub fn provision<E: SqlEngine>(engine: E, scope: &CellScope) -> Result<Analytics<E>, SeedError> {
    let object = object_type()?;
    let model = Model::from_definitions(std::slice::from_ref(&object), &[])
        .map_err(|e| fail("model", e))?;
    let mut store = CellStore::from_engine(engine, model).map_err(|e| fail("open", e))?;
    // Loaded in one commit: an empty ledger means the load never happened.
    if store
        .verified_head()
        .map_err(|e| fail("ledger", e))?
        .commits
        == 0
    {
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: scope.key(),
                logical_time: CLOCK,
                writes: conversions()?,
                ..CommitBatch::default()
            })
            .map_err(|e| fail("K1 conversions", e))?;
    }
    Ok(Analytics {
        store,
        plan: plan(object)?,
    })
}

fn conversions() -> Result<Vec<FactWriteRow>, SeedError> {
    let grams = |mg: &str| {
        Decimal::parse(&format!("{mg}e-3"))
            .map(RuntimeValue::from)
            .map_err(|e| fail("mass", e))
    };
    include_str!("../../../fixtures/demo/scrap-yield-k1.csv")
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            let [_, _, key, metal, input, recovered, loss, _, at, _] = fields[..] else {
                return Err(SeedError(format!("malformed conversion row: {line}")));
            };
            Ok(FactWriteRow::Put {
                object_type: TYPE.into(),
                key: key.into(),
                values: BTreeMap::from([
                    ("input".into(), grams(input)?),
                    ("recovered".into(), grams(recovered)?),
                    ("loss".into(), grams(loss)?),
                    ("occurred_at".into(), RuntimeValue::from(at)),
                    ("metal".into(), RuntimeValue::from(metal)),
                ]),
            })
        })
        .collect()
}

/// Admit the weekly yield question: recovered over input, gold only, by week.
fn plan(object: ObjectTypeDef) -> Result<QuestionPlan, SeedError> {
    let mut artifact: Value = serde_json::from_str::<Vec<Value>>(include_str!(
        "../../../fixtures/ir/metrics/ontology.json"
    ))
    .map_err(|e| fail("ontology", e))?
    .into_iter()
    .next()
    .ok_or_else(|| SeedError("empty ontology".into()))?;
    artifact["artifactId"] = json!("demo.Conversion.entity.v2");
    artifact["name"] = json!(TYPE);
    artifact["namespace"] = json!("demo");
    artifact["provenance"] = json!({"source":"import","sourcePath":"synthetic:scrap-yield-k1.csv"});
    artifact["payload"] = json!({"entityVersion":"2.0","objectType":object});
    let artifact: ArtifactEnvelope =
        serde_json::from_value(artifact).map_err(|e| fail("entity", e))?;
    let catalog = EffectiveCatalog::from_artifacts(&[artifact]);
    let mut recovered: MetricDefinition = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/net-sales.definition.json"
    ))
    .map_err(|e| fail("metric", e))?;
    recovered.name = "demo.recovered".into();
    recovered.label = "Recovered".into();
    recovered.type_params.expr = Some("recovered".into());
    let aggregate = recovered
        .type_params
        .metric_aggregation_params
        .as_mut()
        .ok_or_else(|| SeedError("metric has no aggregation".into()))?;
    aggregate.semantic_model = norn_ir::ObjectTypeName(TYPE.into());
    aggregate.agg_time_dimension = "occurred_at".into();
    recovered.norn.currency = None;
    recovered.norn.default_filters.clear();
    recovered.norn.time_dimension = Some(
        serde_json::from_value(json!({"property":"occurred_at"})).map_err(|e| fail("time", e))?,
    );
    let mut input = recovered.clone();
    input.name = "demo.input".into();
    input.label = "Input".into();
    input.type_params.expr = Some("input".into());
    let mut ratio = recovered.clone();
    ratio.name = "demo.yield".into();
    ratio.label = "Yield".into();
    ratio.metric_type = MetricType::Ratio;
    ratio.type_params = MetricTypeParams {
        numerator: Some(recovered.name.clone()),
        denominator: Some(input.name.clone()),
        ..MetricTypeParams::default()
    };
    ratio.norn.additivity = MetricAdditivity::NonAdditive;
    let mut question: QuestionIR = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .map_err(|e| fail("question", e))?;
    question.name = QUESTION.into();
    question.object_set = norn_ir::ObjectTypeName(TYPE.into());
    question.comparison = None;
    let shape = |value: Value| serde_json::from_value(value).map_err(|e| fail("question shape", e));
    question.metrics = shape(json!([{"metric":"demo.yield"}]))?;
    question.dimensions = shape(json!([{"path":{"property":"occurred_at"},"grain":"week"}]))?;
    question.filters =
        shape(json!([{"path":{"property":"metal"},"operator":"eq","value":"gold"}]))?;
    question.time_window.start = norn_ir::UtcTimestamp("2026-07-06T00:00:00Z".into());
    question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
    question.freshness = FreshnessPolicy::Live {
        max_age_ms: norn_ir::WholeNumber(5_000),
    };
    admit(&[recovered, input, ratio], &question, &catalog, 100)
        .map_err(|e| fail("admission", format!("{e:?}")))
}

/// Server-owned resolution for the analytics cell's admitted question.
pub struct DemoQuestions {
    plan: QuestionPlan,
    scope: CellScope,
}

impl DemoQuestions {
    /// The runtime for one opened analytics cell.
    #[must_use]
    pub const fn new(plan: QuestionPlan, scope: CellScope) -> Self {
        Self { plan, scope }
    }
}

impl<E: SqlEngine> QuestionRuntime<E> for DemoQuestions {
    fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal> {
        if question == QUESTION {
            Ok(self.plan.clone())
        } else {
            Err(Refusal::new(
                http::StatusCode::NOT_FOUND,
                "question.not_found",
                "no such admitted question",
                "this cell admits only its declared questions",
            ))
        }
    }

    fn snapshot(
        &self,
        _: &CellStore<E>,
        _: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        let cursor = || BTreeMap::from([("measurements".to_owned(), "k1/80".to_owned())]);
        Ok(QuestionSnapshot {
            context: QueryContext {
                world: WORLD.into(),
                scope: self.scope.key(),
                population_pin: format!("demo-grants:{actor}"),
                authorization_epoch: 1,
                mapping_pin: "fixture:scrap-yield-k1".into(),
                now_ms: CLOCK,
                source_cursors: BTreeMap::from([("erp".into(), cursor())]),
                watermarks: BTreeMap::from([(
                    "erp".into(),
                    BTreeMap::from([(
                        "measurements".into(),
                        Watermark {
                            observed: "k1/80".into(),
                            heartbeat_ms: Some(CLOCK),
                            complete: true,
                        },
                    )]),
                )]),
            },
            cutoff: None,
        })
    }

    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        question: &str,
        capability: ReadCapability,
        row: Option<&ObjectRecord>,
    ) -> Result<AccessRequest, Refusal> {
        let key = scope_key(scope);
        Ok(AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: capability_name(capability).into(),
            action_id: READ_ACTION.into(),
            resource: ResourceInput {
                object_type: row.map_or(TYPE, |row| row.object_type.as_str()).into(),
                id: row.map_or(question, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}
