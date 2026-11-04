//! Live host documents, denial ordering and generated-SDK correspondence.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "question route conformance"
)]
#[path = "../../norn-cell/tests/support/query_lineage.rs"]
mod lineage_support;
#[path = "../../norn-cell/tests/query_support/mod.rs"]
mod query_support;
use norn_api::{
    authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest},
    host::DevHost,
    question_routes::{
        QuestionAnswer, QuestionRuntime, QuestionSnapshot, TraceQuery, get_answer,
        get_numeric_trace,
    },
    refusal::{Refusal, RefusalCategory},
    scope::{CellScope, ScopeQuery},
};
use norn_cell::{
    CellStore, CommitBatch, KnowledgeCutoff, ObjectRecord,
    compute::{Freshness, QueryContext},
    engine::NativeEngine,
};
use norn_checker::{
    EffectiveCatalog,
    metric::compute::{QuestionPlan, admit},
};
use norn_ir::{ArtifactEnvelope, metric::FreshnessPolicy};
use norn_kernel::{
    LogicalTime,
    authz::{
        AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
    },
};
use query_support::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const QUESTION: &str = "yield";
const WEEK: &str = "\"2026-10-19\"";
#[derive(Clone, Copy)]
enum Mode {
    Live,
    Fresh,
    Stale,
    Incomplete,
    Historical,
    MissingCutoff,
    WrongCutoff,
}
struct Runtime {
    bad_row: bool,
    plan: QuestionPlan,
    context: QueryContext,
    cutoff: Option<KnowledgeCutoff>,
    resolved: Arc<AtomicUsize>,
}
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let actor = request.access.principal.subject_id.as_str();
        let allowed = match request.capability {
            ReadCapability::Population => {
                ["reader", "auditor", "partial"].contains(&actor)
                    && !(actor == "partial" && request.access.resource.id == "SYN-MELT-15-1")
            }
            ReadCapability::Values | ReadCapability::Sources => actor == "auditor",
        };
        if allowed {
            Ok(())
        } else {
            Err(Refusal::invalid("private policy diagnostic"))
        }
    }
}
fn physical() -> CellScope {
    CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    }
}
fn scope() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}
impl QuestionRuntime<NativeEngine> for Runtime {
    fn plan(&self, _: &str) -> Result<QuestionPlan, Refusal> {
        self.resolved.fetch_add(1, Ordering::SeqCst);
        Ok(self.plan.clone())
    }
    fn snapshot(
        &self,
        _: &CellStore,
        _: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        self.resolved.fetch_add(1, Ordering::SeqCst);
        let mut context = self.context.clone();
        context.population_pin = format!("fixture:{actor}");
        Ok(QuestionSnapshot {
            context,
            cutoff: self.cutoff,
        })
    }
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        question: &str,
        capability: ReadCapability,
        row: Option<&ObjectRecord>,
    ) -> Result<PdpRequest, Refusal> {
        let physical = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(PdpRequest {
            principal: PrincipalInput {
                subject_id: if self.bad_row && row.is_some() {
                    "different-principal".into()
                } else {
                    actor.into()
                },
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(physical.clone()),
            capability: format!("fixture.{capability:?}"),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: row.map_or(TYPE, |row| row.object_type.as_str()).into(),
                id: row.map_or(question, |row| row.id.as_str()).into(),
                scope: physical,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}
fn configured(mode: Mode) -> (DevHost<NativeEngine>, Runtime) {
    let (mut store, mut plan, mut context) = fixture();
    context.scope = physical().key();
    let initial = store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: context.scope.clone(),
            logical_time: CLOCK,
            writes: fixture_writes(),
            ..CommitBatch::default()
        })
        .expect("K1");
    let receipt = initial.receipt();
    let mut cutoff = None;
    match mode {
        Mode::Historical | Mode::MissingCutoff | Mode::WrongCutoff => {
            let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
                "../../../fixtures/ir/lineage/scrap-yield.artifacts.json"
            ))
            .expect("typed definitions");
            artifacts.push(
                serde_json::from_str(include_str!(
                    "../../../registry/core/artifacts/core.units.json"
                ))
                .expect("units"),
            );
            let catalog = EffectiveCatalog::from_artifacts(&artifacts);
            let mut question = plan.question().clone();
            question.freshness = FreshnessPolicy::AsOf {
                at: norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into()),
            };
            plan = admit(plan.definitions(), &question, &catalog, 100).expect("admitted as-of");
            if !matches!(mode, Mode::MissingCutoff) {
                cutoff = Some(KnowledgeCutoff {
                    recorded_at: LogicalTime::from_millis(receipt.body.logical_time),
                    frontier: receipt.body.commit_seq,
                });
            }
            if matches!(mode, Mode::WrongCutoff) {
                cutoff = cutoff.map(|point| KnowledgeCutoff {
                    recorded_at: LogicalTime::from_millis(CLOCK + 1),
                    ..point
                });
            }
            store
                .commit(&CommitBatch {
                    world: WORLD.into(),
                    scope: context.scope.clone(),
                    logical_time: CLOCK + 1000,
                    writes: vec![put(
                        "SYN-MELT-15-1",
                        75_000,
                        72_000,
                        "2026-10-23T12:01:00Z",
                        "gold",
                    )],
                    ..CommitBatch::default()
                })
                .expect("K2");
        }
        Mode::Fresh => context.now_ms += 1000,
        Mode::Stale => context.now_ms += 6000,
        Mode::Incomplete => {
            context
                .exclusions
                .insert("SYN-MELT-15-1".into(), "fixture:finding".into());
        }
        Mode::Live => {}
    }
    let host = DevHost::from_store(store, physical(), |_| BTreeMap::new())
        .expect("host")
        .with_authorizer(Policy);
    (
        host,
        Runtime {
            bad_row: false,
            plan,
            context,
            cutoff,
            resolved: Arc::new(AtomicUsize::new(0)),
        },
    )
}
fn trace_query(lineage: &str) -> TraceQuery {
    TraceQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
        lineage_ref: Some(lineage.into()),
        bucket: Some(WEEK.into()),
    }
}

#[test]
fn unauthorized_answers_and_traces_have_identical_403s_before_resolution() {
    let (host, runtime) = configured(Mode::Live);
    let baseline = get_answer(&host, &scope(), None, QUESTION, &runtime).expect_err("anonymous");
    let bytes = serde_json::to_vec(&baseline).expect("wire");
    for actor in [None, Some("unknown"), Some("metadata")] {
        for query in [
            scope(),
            ScopeQuery::default(),
            ScopeQuery {
                org_id: Some("foreign".into()),
                environment_id: Some("dev".into()),
            },
        ] {
            let denied =
                get_answer(&host, &query, actor, "guessed-question", &runtime).expect_err("denied");
            assert_eq!(denied.status, 403);
            assert_eq!(serde_json::to_vec(&denied).expect("wire"), bytes);
            let trace = TraceQuery {
                org_id: query.org_id,
                environment_id: query.environment_id,
                ..TraceQuery::default()
            };
            let denied = get_numeric_trace(&host, &trace, actor, QUESTION, &runtime)
                .expect_err("denied before missing trace coordinates");
            assert_eq!(denied.status, 403);
            assert_eq!(serde_json::to_vec(&denied).expect("wire"), bytes);
        }
    }
    assert_eq!(runtime.resolved.load(Ordering::SeqCst), 0);
    for query in [
        ScopeQuery::default(),
        ScopeQuery {
            org_id: Some("foreign".into()),
            environment_id: Some("dev".into()),
        },
    ] {
        assert_eq!(
            get_answer(&host, &query, Some("reader"), QUESTION, &runtime)
                .expect_err("scope")
                .status,
            403
        );
    }
    assert_eq!(runtime.resolved.load(Ordering::SeqCst), 0);
}

#[test]
fn live_answers_and_traces_apply_current_independent_population_and_disclosure_grants() {
    let (host, runtime) = configured(Mode::Live);
    let QuestionAnswer::Live(answer) =
        get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime).expect("live")
    else {
        panic!("live variant")
    };
    assert_eq!(answer.freshness, Freshness::Exact);
    assert_eq!(answer.rows[WEEK].value.expect("ratio").canonical(), "0.98");
    assert!(
        serde_json::to_string(&answer)
            .expect("exact JSON")
            .contains("\"value\":0.98")
    );
    let query = trace_query(&answer.lineage_ref);
    let restricted = get_numeric_trace(&host, &query, Some("reader"), QUESTION, &runtime)
        .expect("restricted trace");
    assert_eq!(restricted.contributions.len(), 4);
    assert!(restricted.contributions.iter().all(|c| c.restricted
        && c.values.is_none()
        && c.fact_digest.is_none()
        && matches!(
            c.source,
            norn_cell::compute::lineage::ContributionSource::Restricted
        )));
    let QuestionAnswer::Live(audited) =
        get_answer(&host, &scope(), Some("auditor"), QUESTION, &runtime).expect("audited")
    else {
        panic!("live")
    };
    let trace = get_numeric_trace(
        &host,
        &trace_query(&audited.lineage_ref),
        Some("auditor"),
        QUESTION,
        &runtime,
    )
    .expect("auditor");
    assert!(trace.contributions.iter().all(|c| !c.restricted
        && c.values.is_some()
        && c.fact_digest.is_some()
        && matches!(
            c.source,
            norn_cell::compute::lineage::ContributionSource::Unavailable
        )));
    let QuestionAnswer::Live(partial) =
        get_answer(&host, &scope(), Some("partial"), QUESTION, &runtime)
            .expect("visible population")
    else {
        panic!("live")
    };
    assert_eq!(partial.rows[WEEK].members.len(), 3);
    assert!(
        get_numeric_trace(&host, &query, Some("partial"), QUESTION, &runtime).is_err(),
        "a different population cannot reuse the old result pin"
    );
}

#[test]
fn invalid_row_mapping_refuses_instead_of_publishing_a_complete_empty_answer() {
    let (host, mut runtime) = configured(Mode::Live);
    runtime.bad_row = true;
    let refused = get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime)
        .expect_err("unproven row mapping");
    assert_eq!(refused.code, "core.compute.stale_input");
}

#[test]
fn fresh_stale_and_incomplete_answers_keep_labels_but_nonexact_traces_refuse() {
    for (mode, freshness) in [
        (Mode::Fresh, Freshness::Fresh),
        (Mode::Stale, Freshness::Stale),
        (Mode::Incomplete, Freshness::Incomplete),
    ] {
        let (host, runtime) = configured(mode);
        let QuestionAnswer::Live(answer) =
            get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime)
                .expect("analytical answer")
        else {
            panic!("live")
        };
        assert_eq!(answer.freshness, freshness);
        let denied = get_numeric_trace(
            &host,
            &trace_query(&answer.lineage_ref),
            Some("reader"),
            QUESTION,
            &runtime,
        )
        .expect_err("exact trace required");
        assert_eq!(
            (denied.status, denied.category, denied.code.as_str()),
            (
                422,
                RefusalCategory::Precondition,
                "core.compute.stale_input"
            )
        );
    }
}

#[test]
fn asof_uses_authenticated_cutoff_and_preserves_the_old_answer_after_correction() {
    let (host, runtime) = configured(Mode::Historical);
    let QuestionAnswer::Historical(answer) =
        get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime).expect("historical")
    else {
        panic!("historical")
    };
    assert_eq!(answer.knowledge_time_ms, CLOCK);
    assert_eq!(answer.result.known_as_of, 1);
    assert_eq!(
        answer.result.rows[WEEK]
            .value
            .expect("K1 ratio")
            .canonical(),
        "0.98"
    );
    assert_eq!(
        get_numeric_trace(
            &host,
            &trace_query(&answer.result.lineage_ref),
            Some("reader"),
            QUESTION,
            &runtime
        )
        .expect_err("native historical trace unsupported")
        .code,
        "core.compute.unsupported_engine"
    );
    for (mode, code) in [
        (Mode::MissingCutoff, "core.compute.unsupported_engine"),
        (Mode::WrongCutoff, "core.compute.stale_input"),
    ] {
        let (host, runtime) = configured(mode);
        let denied = get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime)
            .expect_err("cutoff must match");
        assert_eq!((denied.status, denied.code.as_str()), (422, code));
    }
}

#[test]
fn compute_refusal_projection_keeps_codes_and_hides_other_diagnostics() {
    for code in [
        "core.compute.stale_input",
        "core.compute.unsupported_engine",
        "core.projection.lineage_incomplete",
    ] {
        let refusal = Refusal::from_compute(norn_cell::CellError::ComputeRefused(code));
        assert_eq!(
            (refusal.status, refusal.code.as_str(), refusal.category),
            (422, code, RefusalCategory::Precondition)
        );
        assert!(!refusal.retryable);
    }
    assert_eq!(
        Refusal::from_compute(norn_cell::CellError::TemporalLeakage).code,
        "core.compute.stale_input"
    );
    assert_eq!(
        Refusal::from_compute(norn_cell::CellError::ComputeRefused(
            "core.compute.bound_exceeded"
        ))
        .category,
        RefusalCategory::Limit
    );
    let hidden = Refusal::from_compute(norn_cell::CellError::Engine("private stored value".into()));
    assert_eq!(hidden.status, 500);
    assert!(
        !serde_json::to_string(&hidden)
            .expect("wire")
            .contains("private stored value")
    );
}

#[test]
fn live_route_documents_conform_to_the_generated_sdk() {
    let contract: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../sdks/openapi/norn-api.openapi.json"
        ))
        .expect("generated contract"),
    )
    .expect("OpenAPI");
    let mut documents = Vec::new();
    for (name, mode, schema) in [
        ("live", Mode::Live, "NornQueryResult"),
        ("fresh", Mode::Fresh, "NornQueryResult"),
        ("stale", Mode::Stale, "NornQueryResult"),
        ("incomplete", Mode::Incomplete, "NornQueryResult"),
        (
            "historical",
            Mode::Historical,
            "NornHistoricalQuestionResult",
        ),
    ] {
        let (host, runtime) = configured(mode);
        let answer =
            get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime).expect("live document");
        documents.push((
            name,
            schema,
            serde_json::to_value(answer).expect("engine JSON"),
        ));
    }
    for (name, actor) in [
        ("restricted_trace", "reader"),
        ("unavailable_trace", "auditor"),
    ] {
        let (host, runtime) = configured(Mode::Live);
        let QuestionAnswer::Live(answer) =
            get_answer(&host, &scope(), Some(actor), QUESTION, &runtime).expect("answer")
        else {
            panic!("live")
        };
        let trace = get_numeric_trace(
            &host,
            &trace_query(&answer.lineage_ref),
            Some(actor),
            QUESTION,
            &runtime,
        )
        .expect("trace");
        documents.push((
            name,
            "NornNumericTrace",
            serde_json::to_value(trace).expect("trace JSON"),
        ));
    }
    let (mapped, plan, context, _, directory) = lineage_support::loaded();
    let model = mapped.store().model().clone();
    drop(mapped);
    let store = CellStore::open(
        &directory.path().join("cell.sqlite"),
        model,
        norn_cell::StoreOptions::default(),
    )
    .expect("reopen mapped cell");
    let host = DevHost::from_store(store, physical(), |_| BTreeMap::new())
        .expect("mapped host")
        .with_authorizer(Policy);
    let runtime = Runtime {
        bad_row: false,
        plan,
        context,
        cutoff: None,
        resolved: Arc::new(AtomicUsize::new(0)),
    };
    let QuestionAnswer::Live(answer) =
        get_answer(&host, &scope(), Some("auditor"), QUESTION, &runtime).expect("mapped answer")
    else {
        panic!("live")
    };
    let trace = get_numeric_trace(
        &host,
        &trace_query(&answer.lineage_ref),
        Some("auditor"),
        QUESTION,
        &runtime,
    )
    .expect("resolved source trace");
    assert_eq!(trace.bucket, lineage_support::WEEK);
    assert!(trace.contributions.iter().all(|c| matches!(
        c.source,
        norn_cell::compute::lineage::ContributionSource::Resolved { .. }
    )));
    documents.push((
        "resolved_trace",
        "NornNumericTrace",
        serde_json::to_value(trace).expect("engine source trace"),
    ));
    let (host, runtime) = configured(Mode::Stale);
    let QuestionAnswer::Live(answer) =
        get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime).expect("stale answer")
    else {
        panic!("live")
    };
    let refused = get_numeric_trace(
        &host,
        &trace_query(&answer.lineage_ref),
        Some("reader"),
        QUESTION,
        &runtime,
    )
    .expect_err("nonexact trace refusal");
    documents.push((
        "nonexact_trace",
        "Refusal",
        serde_json::to_value(refused).expect("typed refusal"),
    ));
    let (host, runtime) = configured(Mode::Live);
    documents.push((
        "denial",
        "Refusal",
        serde_json::to_value(
            get_answer(&host, &scope(), None, QUESTION, &runtime).expect_err("denied"),
        )
        .expect("refusal JSON"),
    ));
    let mut typescript = "// Generated from live question-route conformance (NORN_BLESS=1).\nimport type { Schemas } from \"../../src/index.ts\";\n".to_owned();
    for (name, root, document) in documents {
        let schema = serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","$ref":format!("#/components/schemas/{root}"),"components":contract["components"]});
        let validator = jsonschema::validator_for(&schema).expect("projected SDK schema");
        let errors: Vec<_> = validator
            .iter_errors(&document)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        typescript.push_str(&format!(
            "export const {name} = {} satisfies Schemas[\"{root}\"];\n",
            serde_json::to_string_pretty(&document).expect("exact fixture")
        ));
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/norn-sdk/test/fixtures/host-answers.ts"
    );
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::write(path, &typescript).expect("bless native documents");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("SDK fixture"),
        typescript,
        "regenerate from live routes with NORN_BLESS=1 and typecheck @norn/sdk"
    );
}
