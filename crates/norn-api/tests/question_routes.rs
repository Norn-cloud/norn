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
    CellStore, CommitBatch, FactWriteRow, KnowledgeCutoff, ObjectRecord,
    compute::{
        Freshness, QueryContext,
        lineage::{keyed_lineage_handle, keyed_receipt_handle},
    },
    engine::NativeEngine,
};
use norn_checker::metric::compute::QuestionPlan;
use norn_kernel::{
    LogicalTime,
    authz::{
        AccessRequest as PdpRequest, ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey,
    },
};
use query_support::*;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
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
    /// The cutoff is K2: one write, of SYN-MELT-15-1 (#1175).
    SingleWriteCutoff,
    /// WEEK keeps only SYN-MELT-15-1: its other members move a week earlier.
    Singleton,
}
struct Runtime {
    bad_row: bool,
    plan: QuestionPlan,
    context: QueryContext,
    cutoff: Option<KnowledgeCutoff>,
    /// The cutoff commit's raw receipt hash, when the fixture pins one.
    cutoff_receipt: Option<String>,
    resolved: Arc<AtomicUsize>,
    min_disclosed: usize,
}
/// The fixture cell's lineage handle key (D53).
const HANDLE_KEY: [u8; 32] = [7; 32];
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let actor = request.access.principal.subject_id.as_str();
        let allowed = match request.capability {
            ReadCapability::Population => {
                ["reader", "auditor", "partial", "mixed"].contains(&actor)
                    && !(actor == "partial" && request.access.resource.id == "SYN-MELT-15-1")
            }
            // `mixed` sees every member's values but one.
            ReadCapability::Values => {
                actor == "auditor"
                    || (actor == "mixed" && request.access.resource.id != "SYN-MELT-15-1")
            }
            ReadCapability::Sources => actor == "auditor",
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
    fn min_disclosed_bucket(&self) -> usize {
        self.min_disclosed
    }
    fn handle_key(&self) -> &[u8] {
        &HANDLE_KEY
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
    let mut cutoff_receipt = None;
    match mode {
        Mode::Historical | Mode::MissingCutoff | Mode::WrongCutoff | Mode::SingleWriteCutoff => {
            plan = asof_plan(&store, &plan, "2026-10-26T00:00:00Z");
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
            cutoff_receipt = Some(receipt.hash.clone());
            let corrected = store
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
            if matches!(mode, Mode::SingleWriteCutoff) {
                let single = corrected.receipt();
                cutoff = Some(KnowledgeCutoff {
                    recorded_at: LogicalTime::from_millis(single.body.logical_time),
                    frontier: single.body.commit_seq,
                });
                cutoff_receipt = Some(single.hash.clone());
            }
        }
        Mode::Fresh => context.now_ms = context.now_ms.checked_add(1000).expect("fresh clock"),
        Mode::Stale => context.now_ms = context.now_ms.checked_add(6000).expect("stale clock"),
        Mode::Incomplete => {
            // An incomplete source watermark, not a caller exclusion (DATA-QUARANTINE).
            for watermark in context
                .watermarks
                .values_mut()
                .flat_map(BTreeMap::values_mut)
            {
                watermark.complete = false;
            }
        }
        Mode::Singleton => {
            let moved = singleton_moves();
            assert_eq!(moved.len(), 3, "WEEK has three other members");
            store
                .commit(&CommitBatch {
                    world: WORLD.into(),
                    scope: context.scope.clone(),
                    logical_time: CLOCK + 1000,
                    writes: moved,
                    ..CommitBatch::default()
                })
                .expect("move");
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
            cutoff_receipt,
            resolved: Arc::new(AtomicUsize::new(0)),
            min_disclosed: norn_api::question_routes::DEFAULT_MIN_DISCLOSED_BUCKET,
        },
    )
}
/// WEEK's members but SYN-MELT-15-1, moved a week earlier.
fn singleton_moves() -> Vec<FactWriteRow> {
    include_str!("../../../fixtures/demo/scrap-yield-k1.csv")
        .lines()
        .skip(1)
        .map(|line| line.split(',').collect::<Vec<_>>())
        .filter(|fields| {
            fields[8].starts_with("2026-10-23")
                && fields[3] == "gold"
                && fields[2] != "SYN-MELT-15-1"
        })
        .map(|fields| {
            put(
                fields[2],
                fields[4].parse().expect("input"),
                fields[5].parse().expect("recovered"),
                "2026-10-14T12:00:00Z",
                fields[3],
            )
        })
        .collect()
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
    documents.push(resolved_trace_document());
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
    let typescript = sdk_fixture(&contract, documents);
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

type SdkDocument = (&'static str, &'static str, serde_json::Value);

fn resolved_trace_document() -> SdkDocument {
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
        cutoff_receipt: None,
        resolved: Arc::new(AtomicUsize::new(0)),
        min_disclosed: norn_api::question_routes::DEFAULT_MIN_DISCLOSED_BUCKET,
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
    (
        "resolved_trace",
        "NornNumericTrace",
        serde_json::to_value(trace).expect("engine source trace"),
    )
}

fn sdk_fixture(contract: &serde_json::Value, documents: Vec<SdkDocument>) -> String {
    let mut typescript = "// Generated from live question-route conformance (NORN_BLESS=1).\nimport type { Schemas } from \"../../src/index.ts\";\n".to_owned();
    for (name, root, document) in documents {
        let schema = serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","$ref":format!("#/components/schemas/{root}"),"components":contract["components"]});
        let validator = jsonschema::validator_for(&schema).expect("projected SDK schema");
        let errors: Vec<_> = validator
            .iter_errors(&document)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        if root == "NornNumericTrace" {
            assert!(document["contributions"][0]["source"].is_object());
            let mut extended = document.clone();
            extended["contributions"][0]["source"]["undisclosed"] = serde_json::json!(true);
            assert!(
                !validator.is_valid(&extended),
                "{name}: source must be closed"
            );
            if document["contributions"][0]["source"]["status"] != "resolved" {
                let mut leaked = document.clone();
                leaked["contributions"][0]["source"]["provenance"] = serde_json::json!({});
                assert!(
                    !validator.is_valid(&leaked),
                    "{name}: provenance must be absent"
                );
            }
        }
        writeln!(
            &mut typescript,
            "export const {name} = {} satisfies Schemas[\"{root}\"];",
            serde_json::to_string_pretty(&document).expect("exact fixture")
        )
        .expect("format fixture");
    }
    typescript
}

fn amounts_on_the_wire(document: &impl serde::Serialize) -> Vec<serde_json::Value> {
    let wire = serde_json::to_value(document).expect("wire");
    let mut found = Vec::new();
    let mut stack = vec![&wire];
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(amounts) = map.get("amounts") {
                    found.push(amounts.clone());
                }
                stack.extend(map.values());
            }
            serde_json::Value::Array(items) => stack.extend(items),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    found
}

/// DATA-VALUES-1: per-member amounts are row values. Population alone shows
/// who is counted and the bucket's aggregate, never a member's amounts, in the
/// answer or the trace; Values on every member shows them, and they sum to the
/// totals the answer already discloses.
#[test]
fn member_amounts_are_values_and_follow_the_values_grant() {
    let (host, runtime) = configured(Mode::Live);
    let live = |actor| match get_answer(&host, &scope(), Some(actor), QUESTION, &runtime)
        .expect("answer")
    {
        QuestionAnswer::Live(answer) => answer,
        QuestionAnswer::Historical(_) => panic!("live"),
    };
    let trace = |actor, answer: &norn_cell::compute::QueryResult| {
        get_numeric_trace(
            &host,
            &trace_query(&answer.lineage_ref),
            Some(actor),
            QUESTION,
            &runtime,
        )
        .expect("trace")
    };
    let (reader, auditor) = (live("reader"), live("auditor"));
    // The aggregate is the answer's grant and is the same for both.
    for (bucket, row) in &auditor.rows {
        assert_eq!(reader.rows[bucket].totals, row.totals, "{bucket}");
        assert_eq!(reader.rows[bucket].value, row.value, "{bucket}");
        assert_eq!(
            reader.rows[bucket].members.keys().collect::<Vec<_>>(),
            row.members.keys().collect::<Vec<_>>()
        );
    }
    // Population only: members are listed and restricted, with no amounts anywhere.
    let viewer_trace = trace("reader", &reader);
    assert!(
        reader
            .rows
            .values()
            .flat_map(|row| row.members.values())
            .all(|m| m.restricted && m.amounts.is_none())
    );
    assert!(
        viewer_trace
            .contributions
            .iter()
            .all(|c| c.restricted && c.amounts.is_none() && c.values.is_none())
    );
    for document in [
        amounts_on_the_wire(&reader),
        amounts_on_the_wire(&viewer_trace),
    ] {
        assert!(
            !document.is_empty() && document.iter().all(serde_json::Value::is_null),
            "{document:?}"
        );
    }
    // Values on every member: amounts are present and sum to the disclosed totals.
    let audited_trace = trace("auditor", &auditor);
    assert!(
        auditor
            .rows
            .values()
            .flat_map(|row| row.members.values())
            .all(|m| !m.restricted && m.amounts.is_some())
    );
    assert!(
        audited_trace
            .contributions
            .iter()
            .all(|c| !c.restricted && c.amounts.is_some())
    );
    // The trace's amounts are the answer's member amounts, member by member.
    for contribution in &audited_trace.contributions {
        assert_eq!(
            contribution.amounts,
            auditor.rows[WEEK].members[&contribution.fact.key].amounts
        );
    }
    for contribution in &viewer_trace.contributions {
        assert_eq!(
            contribution.amounts,
            reader.rows[WEEK].members[&contribution.fact.key].amounts
        );
    }
}

/// One restricted member withholds every amount in its bucket, so it does not
/// follow from the totals minus the others; buckets without one keep theirs.
#[test]
fn one_restricted_member_withholds_its_whole_bucket() {
    let (host, runtime) = configured(Mode::Live);
    let QuestionAnswer::Live(mixed) =
        get_answer(&host, &scope(), Some("mixed"), QUESTION, &runtime).expect("answer")
    else {
        panic!("live")
    };
    let (hidden, open): (Vec<_>, Vec<_>) = mixed
        .rows
        .values()
        .partition(|row| row.members.contains_key("SYN-MELT-15-1"));
    assert_eq!(hidden.len(), 1);
    let bucket = hidden[0];
    assert!(bucket.members.values().all(|m| m.amounts.is_none()));
    assert!(
        bucket
            .members
            .iter()
            .all(|(id, m)| m.restricted == (id == "SYN-MELT-15-1"))
    );
    assert!(
        open.iter()
            .flat_map(|row| row.members.values())
            .all(|m| !m.restricted && m.amounts.is_some())
    );
    let trace = get_numeric_trace(
        &host,
        &trace_query(&mixed.lineage_ref),
        Some("mixed"),
        QUESTION,
        &runtime,
    )
    .expect("trace");
    assert!(trace.contributions.iter().all(|c| c.amounts.is_none()));
    assert_eq!(
        trace.contributions.iter().filter(|c| c.restricted).count(),
        1
    );
    // In the trace's embedded answer too, only the hidden member's bucket is withheld.
    assert!(
        trace.result.rows[WEEK]
            .members
            .values()
            .all(|m| m.amounts.is_none())
    );
    assert!(
        trace
            .result
            .rows
            .iter()
            .filter(|(key, _)| key.as_str() != WEEK)
            .all(|(_, row)| row.members.values().all(|m| m.amounts.is_some()))
    );
}

fn value_tokens(document: &impl serde::Serialize) -> String {
    serde_json::to_string(document).expect("wire")
}

/// Whether `token` appears in `wire` as a whole JSON value.
fn carries(wire: &str, token: &str) -> bool {
    [":", "[", ","].iter().any(|before| {
        [",", "]", "}"]
            .iter()
            .any(|after| wire.contains(&format!("{before}{token}{after}")))
    })
}

/// D53 (1): in a bucket where the reader lacks Values on any member, no
/// value-bearing field of any member reaches the wire, in the answer or the
/// trace. A member's property cannot be reconstructed from the totals minus
/// the others, because no other member's property is there.
#[test]
fn a_mixed_bucket_discloses_no_member_value_from_which_the_hidden_one_follows() {
    let (host, runtime) = configured(Mode::Live);
    let live = |actor| match get_answer(&host, &scope(), Some(actor), QUESTION, &runtime)
        .expect("answer")
    {
        QuestionAnswer::Live(answer) => answer,
        QuestionAnswer::Historical(_) => panic!("live"),
    };
    let (mixed, auditor) = (live("mixed"), live("auditor"));
    let trace = get_numeric_trace(
        &host,
        &trace_query(&mixed.lineage_ref),
        Some("mixed"),
        QUESTION,
        &runtime,
    )
    .expect("trace");
    assert_eq!(trace.contributions.len(), 4);
    assert!(
        trace
            .contributions
            .iter()
            .all(|c| c.amounts.is_none() && c.values.is_none() && c.fact_digest.is_none())
    );
    assert_eq!(
        trace.contributions.iter().filter(|c| c.restricted).count(),
        1
    );
    // Not one member property of the bucket, visible or hidden, appears anywhere.
    let audited = get_numeric_trace(
        &host,
        &trace_query(&auditor.lineage_ref),
        Some("auditor"),
        QUESTION,
        &runtime,
    )
    .expect("auditor trace");
    let wire = format!(
        "{}{}",
        value_tokens(&mixed.rows[WEEK]),
        value_tokens(&trace)
    );
    for contribution in &audited.contributions {
        for (property, value) in contribution.values.as_ref().expect("auditor values") {
            let token = serde_json::to_string(value).expect("token");
            assert!(
                !carries(&wire, &token),
                "{}.{property} = {token} on the wire",
                contribution.fact.key
            );
        }
        let digest = contribution.fact_digest.as_deref().expect("auditor digest");
        assert!(
            !wire.contains(digest),
            "{} digest on the wire",
            contribution.fact.key
        );
    }
    // The aggregate stays: four members meet the default minimum of three.
    assert!(!mixed.rows[WEEK].suppressed);
    assert_eq!(mixed.rows[WEEK].totals, auditor.rows[WEEK].totals);
}

/// #1175: a historical answer names its cutoff commit. Its receipt hash commits
/// to that commit's values, so for a single write the reader lacks Values on it
/// would confirm a guess offline. Such a reader sees the keyed receipt handle,
/// and the raw hash appears nowhere in its document; `mixed` lacks Values on
/// that one row only. A reader holding Values over the whole commit keeps it.
#[test]
fn a_historical_cutoff_on_a_restricted_single_write_withholds_its_receipt_hash() {
    let (host, runtime) = configured(Mode::SingleWriteCutoff);
    let raw = runtime.cutoff_receipt.clone().expect("K2 receipt");
    let commit = |actor| {
        let QuestionAnswer::Historical(answer) =
            get_answer(&host, &scope(), Some(actor), QUESTION, &runtime).expect("historical")
        else {
            panic!("historical")
        };
        assert_eq!(answer.result.known_as_of, 2);
        let norn_kernel::World::Historical { commit } = &answer.world else {
            panic!("historical world")
        };
        (
            commit.0.clone(),
            serde_json::to_string(&answer).expect("document"),
        )
    };
    for actor in ["reader", "mixed"] {
        let (handle, document) = commit(actor);
        assert_eq!(handle, keyed_receipt_handle(&HANDLE_KEY, &raw), "{actor}");
        assert!(!document.contains(&raw), "{actor}: {document}");
    }
    assert_eq!(commit("auditor").0, raw);
}

/// D53 (2): to a reader lacking Values on any of its members, a bucket with
/// fewer members than the declared minimum discloses no aggregate: `value`
/// null, no totals, `suppressed`. A reader with every Values grant sees it, and
/// buckets at or above the minimum keep their aggregate.
#[test]
fn a_bucket_below_the_minimum_is_suppressed_for_a_reader_missing_values() {
    let (host, runtime) = configured(Mode::Singleton);
    let live = |actor| match get_answer(&host, &scope(), Some(actor), QUESTION, &runtime)
        .expect("answer")
    {
        QuestionAnswer::Live(answer) => answer,
        QuestionAnswer::Historical(_) => panic!("live"),
    };
    let auditor = live("auditor");
    assert_eq!(auditor.rows[WEEK].members.len(), 1);
    assert!(!auditor.rows[WEEK].suppressed && auditor.rows[WEEK].value.is_some());
    for actor in ["mixed", "reader"] {
        let answer = live(actor);
        let row = &answer.rows[WEEK];
        assert!(row.suppressed, "{actor}");
        assert!(row.value.is_none() && row.totals.is_empty(), "{actor}");
        assert!(row.members.values().all(|m| m.amounts.is_none()), "{actor}");
        let wire = serde_json::to_value(row).expect("wire");
        assert_eq!(wire["value"], serde_json::Value::Null);
        assert_eq!(wire["suppressed"], serde_json::Value::Bool(true));
        // The week the moved members joined has seven: its aggregate stays.
        for (bucket, other) in answer.rows.iter().filter(|(key, _)| key.as_str() != WEEK) {
            assert!(
                !other.suppressed && !other.totals.is_empty(),
                "{actor} {bucket}"
            );
        }
        let trace = get_numeric_trace(
            &host,
            &trace_query(&answer.lineage_ref),
            Some(actor),
            QUESTION,
            &runtime,
        )
        .expect("trace");
        assert!(trace.result.rows[WEEK].suppressed && trace.result.rows[WEEK].value.is_none());
        assert!(
            trace
                .contributions
                .iter()
                .all(|c| c.amounts.is_none() && c.values.is_none())
        );
    }
}

/// D53 (3): the public `lineageRef` is a keyed handle, never the content hash
/// of the rows. Recomputing the hash from restored (unredacted) rows does not
/// produce it, the internal pin does not open a trace, and only this cell's
/// key maps the handle back.
#[test]
fn the_public_lineage_handle_is_keyed_and_not_a_guessing_oracle() {
    let (host, runtime) = configured(Mode::Live);
    let QuestionAnswer::Live(answer) =
        get_answer(&host, &scope(), Some("reader"), QUESTION, &runtime).expect("answer")
    else {
        panic!("live")
    };
    // The internal pin, recomputed from the same rows restored unredacted: an
    // identically seeded store, the reader's context, every row visible.
    let (mut store, plan, _) = fixture();
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: physical().key(),
            logical_time: CLOCK,
            writes: fixture_writes(),
            ..CommitBatch::default()
        })
        .expect("K1");
    let snapshot = runtime.snapshot(&store, &plan, "reader").expect("snapshot");
    let internal = store
        .evaluate_question(&plan, snapshot.context, |_| Ok(true))
        .expect("unredacted")
        .lineage_ref;
    assert_ne!(
        answer.lineage_ref, internal,
        "the content hash is not published"
    );
    assert_eq!(
        answer.lineage_ref,
        keyed_lineage_handle(&HANDLE_KEY, &internal)
    );
    assert_ne!(
        answer.lineage_ref,
        keyed_lineage_handle(&[8; 32], &internal)
    );
    assert_eq!(answer.lineage_ref.len(), 64);
    // The internal pin (what a guesser would compute) does not open a trace.
    assert!(
        get_numeric_trace(
            &host,
            &trace_query(&internal),
            Some("reader"),
            QUESTION,
            &runtime
        )
        .is_err()
    );
    let trace = get_numeric_trace(
        &host,
        &trace_query(&answer.lineage_ref),
        Some("reader"),
        QUESTION,
        &runtime,
    )
    .expect("the handle opens its trace");
    assert_eq!(trace.result.lineage_ref, answer.lineage_ref);
}

/// A host whose handle key is shorter than 32 bytes refuses answers and traces.
#[test]
fn a_short_handle_key_refuses() {
    struct Short(Runtime);
    impl QuestionRuntime<NativeEngine> for Short {
        fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal> {
            self.0.plan(question)
        }
        fn snapshot(
            &self,
            store: &CellStore,
            plan: &QuestionPlan,
            actor: &str,
        ) -> Result<QuestionSnapshot, Refusal> {
            self.0.snapshot(store, plan, actor)
        }
        fn access(
            &self,
            scope: &CellScope,
            actor: &str,
            question: &str,
            capability: ReadCapability,
            row: Option<&ObjectRecord>,
        ) -> Result<PdpRequest, Refusal> {
            self.0.access(scope, actor, question, capability, row)
        }
        fn min_disclosed_bucket(&self) -> usize {
            3
        }
        fn handle_key(&self) -> &[u8] {
            &HANDLE_KEY[..31]
        }
    }
    let (host, runtime) = configured(Mode::Live);
    let short = Short(runtime);
    assert_eq!(
        get_answer(&host, &scope(), Some("auditor"), QUESTION, &short)
            .expect_err("short key")
            .status,
        500
    );
    assert_eq!(
        get_numeric_trace(
            &host,
            &trace_query(&"0".repeat(64)),
            Some("auditor"),
            QUESTION,
            &short
        )
        .expect_err("short key")
        .status,
        500
    );
}
