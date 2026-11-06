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
use norn_cell::{CellStore, CommitBatch, FactWriteRow, ObjectRecord};
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

/// The core unit catalog the analytics masses are declared in.
const UNITS: &str = include_str!("../../../registry/core/artifacts/core.units.json");

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

/// The analytics object type.
///
/// # Errors
/// The declaration does not decode.
pub fn object_type() -> Result<ObjectTypeDef, SeedError> {
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
    /// The cell's declared metrics, which ad-hoc questions may name (F2b).
    pub definitions: Vec<MetricDefinition>,
    /// The catalog questions are admitted against.
    pub catalog: EffectiveCatalog,
}

/// Open the analytics cell. Its object type is admitted by the shared
/// checker and installed as the cell's first governed definitions version,
/// so retained definitions install through the governed path; then the K1
/// conversions are loaded once.
///
/// A cell opened before it had a definitions version (K1 loaded, no
/// version) upgrades on open: the same admitted set is installed as v1 in
/// one ledger commit, after its K1 rows, which are kept. Every later open is
/// a no-op.
///
/// # Errors
/// The definitions or the question no longer admit, or the engine holds a
/// different cell.
pub fn provision<E: SqlEngine>(engine: E, scope: &CellScope) -> Result<Analytics<E>, SeedError> {
    let object = object_type()?;
    let units: ArtifactEnvelope = serde_json::from_str(UNITS).map_err(|e| fail("core.units", e))?;
    // The unit catalog is the trusted base its masses are admitted against.
    // At the K1 clock, so an upgraded cell's install never precedes its rows.
    let mut store =
        crate::seed::open_governed(engine, scope, &[units, entity(&object, scope)?], CLOCK)?;
    // A fresh cell's first commit is v1 and its second the K1 load; an
    // upgraded cell's first was the K1 load. One commit means no K1 yet.
    if store
        .verified_head()
        .map_err(|e| fail("ledger", e))?
        .commits
        == 1
    {
        store
            .commit(&k1_batch(scope)?)
            .map_err(|e| fail("K1 conversions", e))?;
    }
    let (plan, definitions, catalog) = plan(&object)?;
    Ok(Analytics {
        store,
        plan,
        definitions,
        catalog,
    })
}

/// The one commit that loads the K1 conversions.
///
/// # Errors
/// The fixture rows do not parse.
pub fn k1_batch(scope: &CellScope) -> Result<CommitBatch, SeedError> {
    Ok(CommitBatch {
        world: WORLD.into(),
        scope: scope.key(),
        logical_time: CLOCK,
        writes: conversions()?,
        ..CommitBatch::default()
    })
}

/// The analytics object type as the entity artifact the checker admits.
fn entity(object: &ObjectTypeDef, scope: &CellScope) -> Result<ArtifactEnvelope, SeedError> {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": format!("{TYPE}.entity"),
        "kind": "entity",
        "namespace": "demo",
        "name": "Conversion",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": { "orgId": scope.org_id, "environmentId": scope.environment_id },
        // VAL-UNIT-1: grams resolve against an exact, required catalog pin.
        "dependencies": [{ "artifactId": "core.units", "kind": "entity", "required": true, "version": "1.0.0" }],
        "provenance": { "source": "system", "sourcePath": "crates/norn-cell-do/src/analytics.rs" },
        "payload": { "entityVersion": "2.0", "objectType": object },
    }))
    .map_err(|e| fail("entity artifact", e))
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
            let &[_, _, key, metal, input, recovered, loss, _, at, _] = fields.as_slice() else {
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
fn plan(
    object: &ObjectTypeDef,
) -> Result<(QuestionPlan, Vec<MetricDefinition>, EffectiveCatalog), SeedError> {
    let mut artifact: Value = serde_json::from_str::<Vec<Value>>(include_str!(
        "../../../fixtures/ir/metrics/ontology.json"
    ))
    .map_err(|e| fail("ontology", e))?
    .into_iter()
    .next()
    .ok_or_else(|| SeedError("empty ontology".into()))?;
    let fields = artifact
        .as_object_mut()
        .ok_or_else(|| SeedError("ontology entry is not an object".into()))?;
    for (key, value) in [
        ("artifactId", json!("demo.Conversion.entity.v2")),
        ("name", json!(TYPE)),
        ("namespace", json!("demo")),
        (
            "provenance",
            json!({"source":"import","sourcePath":"synthetic:scrap-yield-k1.csv"}),
        ),
        (
            "payload",
            json!({"entityVersion":"2.0","objectType":object}),
        ),
    ] {
        fields.insert(key.into(), value);
    }
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
    question.metrics = shape(json!([{"metric":"demo.yield"}]))?;
    question.dimensions = shape(json!([{"path":{"property":"occurred_at"},"grain":"week"}]))?;
    question.filters =
        shape(json!([{"path":{"property":"metal"},"operator":"eq","value":"gold"}]))?;
    question.time_window.start = norn_ir::UtcTimestamp("2026-07-06T00:00:00Z".into());
    question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
    question.freshness = FreshnessPolicy::Live {
        max_age_ms: norn_ir::WholeNumber(5_000),
    };
    let definitions = vec![recovered, input, ratio];
    let plan = admit(&definitions, &question, &catalog, 100)
        .map_err(|e| fail("admission", format!("{e:?}")))?;
    Ok((plan, definitions, catalog))
}

/// Server-owned resolution for the analytics cell's admitted question.
pub struct DemoQuestions {
    plan: QuestionPlan,
    scope: CellScope,
    handle_key: Vec<u8>,
    definitions: Vec<MetricDefinition>,
    catalog: EffectiveCatalog,
}

impl DemoQuestions {
    /// The runtime for one opened analytics cell. Until a lineage handle key
    /// is set ([`Self::with_handle_key`]) every answer and trace refuses.
    #[must_use]
    pub fn new(plan: QuestionPlan, scope: CellScope) -> Self {
        Self {
            plan,
            scope,
            handle_key: Vec::new(),
            definitions: Vec::new(),
            catalog: EffectiveCatalog::from_artifacts(&[]),
        }
    }

    /// F2b: answer ad-hoc questions over these declared metrics and catalog.
    /// Without them every ad-hoc question names an undeclared metric.
    #[must_use]
    pub fn with_metrics(
        mut self,
        definitions: Vec<MetricDefinition>,
        catalog: EffectiveCatalog,
    ) -> Self {
        self.definitions = definitions;
        self.catalog = catalog;
        self
    }

    /// D53: key this cell's lineage handles with the host's secret (the
    /// `NORN_LINEAGE_KEY` Worker secret, one per stage). The cell's own scope
    /// is mixed in, so two cells under one secret never share a handle.
    #[must_use]
    pub fn with_handle_key(mut self, secret: &[u8]) -> Self {
        self.handle_key = if secret.len() < 32 {
            Vec::new()
        } else {
            norn_cell::compute::lineage::keyed_lineage_handle(secret, &self.scope.key())
                .into_bytes()
        };
        self
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

    fn min_disclosed_bucket(&self) -> usize {
        norn_api::question_routes::DEFAULT_MIN_DISCLOSED_BUCKET
    }

    fn handle_key(&self) -> &[u8] {
        &self.handle_key
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

fn shape<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, SeedError> {
    serde_json::from_value(value).map_err(|e| fail("question shape", e))
}

/// Rows an ad-hoc question may read per evaluation (F2b).
pub const AD_HOC_MAX_ROWS: u32 = 1_000;
/// Buckets an ad-hoc answer may hold (F2b): two years of weeks.
pub const AD_HOC_MAX_BUCKETS: usize = 104;

impl<E: SqlEngine> norn_api::question_routes::AdHocQuestions<E> for DemoQuestions {
    fn definitions(&self) -> &[MetricDefinition] {
        &self.definitions
    }

    fn catalog(&self) -> &EffectiveCatalog {
        &self.catalog
    }

    fn max_rows(&self) -> u32 {
        AD_HOC_MAX_ROWS
    }

    fn max_buckets(&self) -> usize {
        AD_HOC_MAX_BUCKETS
    }
}
