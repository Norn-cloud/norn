//! The dev host's server policy: one local developer, named by the project
//! manifest, holding every grant on a loopback-only cell (DEV-LOOPBACK-1).
//!
//! The principal comes only from the session bearer token in
//! `.norn/dev.token`; any other caller is denied by default. This is server
//! policy, stated on every `ready` event, never client logic.
use crate::principal::PrincipalResolver;
use norn_api::authz::{
    AccessRequest, Authorizer, DenyAll, ReadCapability, ReadRequest, RefusalDetailRequest,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use norn_api::host::DevHost;
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
#[cfg(feature = "duckdb")]
use norn_cell::CellError;
#[cfg(feature = "duckdb")]
use norn_cell::compute::QueryResult;
use norn_cell::compute::QueryContext;
use norn_cell::{CellStore, InstalledDefinitionSet, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::{ArtifactEnvelope, LawRoot, MetricDefinition};
use norn_kernel::authz::{ActorKind, Delegation, PrincipalInput, ResourceInput};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};

/// Every grant to exactly one actor; default deny for everyone else.
pub(crate) struct LocalDeveloper {
    pub(crate) actor: String,
}

impl LocalDeveloper {
    fn is(&self, actor: &str) -> bool {
        actor == self.actor
    }
}

impl Authorizer for LocalDeveloper {
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if self.is(&request.access.principal.subject_id) {
            Ok(())
        } else {
            Err(norn_api::authz::read_denied())
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
            })
        }
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if self.is(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        self.is(request.actor)
    }
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if request.actor.is_some_and(|actor| self.is(actor)) {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
}

/// The session token is the only credential; it names the manifest's actor.
pub(crate) struct DevPrincipals {
    pub(crate) bearer: String,
    pub(crate) actor: String,
}

impl PrincipalResolver for DevPrincipals {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        let presented = authorization?.strip_prefix("Bearer ")?;
        // Equal length first, then every byte: no early exit on a prefix.
        (presented.len() == self.bearer.len()
            && presented
                .bytes()
                .zip(self.bearer.bytes())
                .fold(0_u8, |seen, (a, b)| seen | (a ^ b))
                == 0)
            .then(|| self.actor.clone())
    }
    fn end_request(&self) {}
}

/// Metric definitions derived from one installed definitions revision.
pub(crate) struct Derived {
    /// The receipt of the revision they were derived from.
    receipt: String,
    definitions: Vec<MetricDefinition>,
    catalog: EffectiveCatalog,
}

/// The dev host's question authority across revisions: the per-session
/// lineage handle key, and the metric space of the last revision asked about.
///
/// METRIC-DERIVE-1: the metrics are derived from the active revision's
/// admitted artifacts (sums of properties declared additive, declared ratios
/// and conserved ratios, each at a time grain), never declared by a request.
/// A new revision is derived again on its first question; nothing is cached
/// across revisions.
pub(crate) struct DevMetrics {
    pub(crate) handle_key: [u8; 32],
    pub(crate) scope: CellScope,
    cache: Mutex<Option<Arc<Derived>>>,
}

impl DevMetrics {
    pub(crate) fn new(handle_key: [u8; 32], scope: CellScope) -> Self {
        Self {
            handle_key,
            scope,
            cache: Mutex::new(None),
        }
    }

    /// The question runtime for `host`'s active revision.
    pub(crate) fn questions(
        &self,
        host: &DevHost<NativeEngine>,
    ) -> Result<DevQuestions<'_>, Refusal> {
        let active = host.active_definitions()?;
        let receipt = active
            .as_ref()
            .map_or_else(String::new, |set| set.receipt_sha256.clone());
        let mut cache = self.cache.lock().map_err(Refusal::host)?;
        let derived = match cache.as_ref() {
            Some(derived) if derived.receipt == receipt => Arc::clone(derived),
            Some(_) | None => {
                let derived = Arc::new(derive(&self.scope, receipt, active.as_ref())?);
                *cache = Some(Arc::clone(&derived));
                derived
            }
        };
        Ok(DevQuestions {
            metrics: self,
            derived,
        })
    }
}

/// Derive the metric space of an installed revision. A cell with nothing
/// installed has no metrics.
fn derive(
    scope: &CellScope,
    receipt: String,
    active: Option<&InstalledDefinitionSet>,
) -> Result<Derived, Refusal> {
    let Some(active) = active else {
        return Ok(Derived {
            receipt,
            definitions: Vec::new(),
            catalog: EffectiveCatalog::default(),
        });
    };
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(&active.canonical_set).map_err(Refusal::host)?;
    // The dev checker admits against an empty registry (`admit.rs`); derive
    // against the same, at the cell's own law scope.
    let space = derive_metric_space(
        &artifacts,
        &BTreeSet::new(),
        &LawRoot::Cell(scope.key()),
    )
    .map_err(|findings| {
        Refusal::host(format!(
            "the active revision's metric space does not derive: {} finding(s)",
            findings.len()
        ))
    })?;
    Ok(Derived {
        receipt,
        definitions: space
            .candidates()
            .iter()
            .map(|candidate| candidate.definition.clone())
            .collect(),
        catalog: space.catalog().clone(),
    })
}

/// Rows an ad-hoc question may read per evaluation on the dev cell.
pub(crate) const DEV_MAX_ROWS: u32 = 50_000;
/// Buckets an ad-hoc answer may hold on the dev cell.
pub(crate) const DEV_MAX_BUCKETS: usize = 5_000;

/// One request's view of [`DevMetrics`]: the active revision's metrics.
/// Questions are ad hoc (`:evaluate`, `:trace`); the dev cell declares no
/// named questions.
pub(crate) struct DevQuestions<'m> {
    metrics: &'m DevMetrics,
    derived: Arc<Derived>,
}

impl QuestionRuntime<NativeEngine> for DevQuestions<'_> {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(Refusal::new(
            http::StatusCode::NOT_FOUND,
            "question.not_found",
            "no such admitted question",
            "the dev cell declares no named questions; ask through /v1/questions:evaluate",
        ))
    }
    /// The dev cell's own frontier: its main world, the developer as the
    /// population, the active revision as the mapping pin, and the host
    /// clock. It tracks no external source, so there is no source frontier.
    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
        _plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Ok(QuestionSnapshot {
            context: QueryContext {
                world: norn_cell::MAIN_WORLD.to_owned(),
                scope: self.metrics.scope.key(),
                population_pin: format!("dev:{actor}"),
                authorization_epoch: 1,
                mapping_pin: format!("dev:{}", self.derived.receipt),
                now_ms: super::now_ms(),
                source_cursors: BTreeMap::new(),
                watermarks: BTreeMap::new(),
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
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        let key = crate::policy::scope_key(scope);
        Ok(norn_kernel::authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: crate::policy::capability_name(capability).into(),
            action_id: crate::policy::READ_ACTION.into(),
            resource: ResourceInput {
                object_type: row.map_or(question, |row| row.object_type.as_str()).into(),
                id: row.map_or(question, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
    fn collection_access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Ok(crate::policy::collection_access(
            scope,
            actor,
            capability,
            object_type,
        ))
    }
    fn min_disclosed_bucket(&self) -> usize {
        norn_api::question_routes::DEFAULT_MIN_DISCLOSED_BUCKET
    }
    fn handle_key(&self) -> &[u8] {
        &self.metrics.handle_key
    }
    /// With the `duckdb` feature, a plan DuckDB lowers (one keyed sum, or a
    /// ratio of two, over decimals of scale at most 6) is answered there;
    /// anything else falls back to the native exact engine. The answer's
    /// `engine` names which one answered.
    #[cfg(feature = "duckdb")]
    fn evaluate(
        &self,
        store: &CellStore<NativeEngine>,
        plan: &QuestionPlan,
        context: QueryContext,
        visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        match store.evaluate_question_duckdb(plan, context.clone(), &mut *visible) {
            Err(CellError::ComputeRefused(code))
                if code == norn_checker::metric::compute::UNSUPPORTED_ENGINE =>
            {
                store.evaluate_question(plan, context, visible)
            }
            answered => answered,
        }
    }
}

impl AdHocQuestions<NativeEngine> for DevQuestions<'_> {
    fn definitions(&self) -> &[MetricDefinition] {
        &self.derived.definitions
    }
    fn catalog(&self) -> &EffectiveCatalog {
        &self.derived.catalog
    }
    fn max_rows(&self) -> u32 {
        DEV_MAX_ROWS
    }
    fn max_buckets(&self) -> usize {
        DEV_MAX_BUCKETS
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dev-host question fixtures"
)]
mod tests {
    //! The dev host answers ad-hoc questions over the metrics its active
    //! revision derives, through the same dispatch the HTTP transport uses,
    //! and re-derives them when a new revision is installed.
    use super::*;
    use crate::server::{Request, handle_with};
    use norn_cell::engine::NativeEngine;
    use norn_cell::{CommitBatch, FactWriteRow, StoreOptions};
    use norn_ir::{Decimal, RuntimeValue};
    use serde_json::{Value, json};

    const ACTOR: &str = "dev@example";
    const BEARER: &str = "test-bearer-0123456789";
    const TYPE: &str = "shop.Sale";

    fn scope() -> CellScope {
        CellScope {
            org_id: "org_dev".into(),
            environment_id: "local".into(),
        }
    }

    /// A test project's one type. `amount` and `quantity` are flows, so they
    /// are declared additive; `price` is their declared ratio.
    fn artifacts(additive: bool) -> Vec<Value> {
        let flow = |scale: u64| {
            if additive {
                json!({"kind": "decimal", "scale": scale, "additivity": {"kind": "additive"}})
            } else {
                json!({"kind": "decimal", "scale": scale})
            }
        };
        let s = scope();
        vec![json!({
            "irVersion": "0.1", "artifactId": format!("{TYPE}.entity"), "kind": "entity",
            "namespace": "shop", "name": "Sale", "version": "0.1.0", "lifecycle": "active",
            "scope": {"orgId": s.org_id, "environmentId": s.environment_id},
            "dependencies": [], "provenance": {"source": "system", "sourcePath": "dev-host test"},
            "payload": {"entityVersion": "2.0", "objectType": {"name": TYPE, "properties": [
                {"id": "amount", "type": flow(2)},
                {"id": "quantity", "type": flow(3)},
                {"id": "region", "type": {"kind": "text"}},
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
            ]}}
        })]
    }

    fn sale(key: &str, amount: &str, quantity: &str, region: &str, at: &str) -> FactWriteRow {
        let decimal = |text: &str| RuntimeValue::from(Decimal::parse(text).unwrap());
        FactWriteRow::Put {
            object_type: TYPE.into(),
            key: key.into(),
            values: BTreeMap::from([
                ("amount".into(), decimal(amount)),
                ("quantity".into(), decimal(quantity)),
                ("region".into(), RuntimeValue::from(region)),
                ("occurred_at".into(), RuntimeValue::from(at)),
            ]),
        }
    }

    fn host(dir: &std::path::Path) -> DevHost<NativeEngine> {
        let admitted = super::super::admit::admit(&artifacts(true), 1_000).ok().expect("admitted");
        let engine = NativeEngine::open(&dir.join("cell.sqlite"), StoreOptions::default()).unwrap();
        let mut store =
            CellStore::provision_definitions(engine, admitted.model, &admitted.input, &scope().key())
                .unwrap();
        store
            .commit(&CommitBatch {
                world: norn_cell::MAIN_WORLD.into(),
                scope: scope().key(),
                logical_time: 2_000,
                writes: vec![
                    sale("s1", "10.00", "2.000", "north", "2026-11-02T10:00:00Z"),
                    sale("s2", "5.50", "1.000", "north", "2026-11-02T12:00:00Z"),
                    sale("s3", "7.25", "3.000", "south", "2026-11-03T09:00:00Z"),
                ],
                ..CommitBatch::default()
            })
            .unwrap();
        DevHost::from_store(store, scope(), |command| command.payload.clone())
            .expect("host")
            .with_authorizer(LocalDeveloper { actor: ACTOR.into() })
    }

    fn call(host: &DevHost<NativeEngine>, metrics: &DevMetrics, path: &str, body: &Value) -> (u16, Value) {
        let questions = metrics.questions(host).expect("runtime");
        let principals = DevPrincipals { bearer: BEARER.into(), actor: ACTOR.into() };
        let body = body.to_string();
        let authorization = format!("Bearer {BEARER}");
        let response = handle_with(
            host,
            &principals,
            Some(&questions),
            &Request {
                method: "POST",
                path,
                query: "orgId=org_dev&environmentId=local",
                authorization: Some(&authorization),
                now: 1_800_000_000,
                body: &body,
            },
        );
        (response.status, serde_json::from_str(&response.body).unwrap())
    }

    fn get(host: &DevHost<NativeEngine>, metrics: &DevMetrics, path: &str) -> (u16, Value) {
        let questions = metrics.questions(host).expect("runtime");
        let principals = DevPrincipals { bearer: BEARER.into(), actor: ACTOR.into() };
        let authorization = format!("Bearer {BEARER}");
        let response = handle_with(
            host,
            &principals,
            Some(&questions),
            &Request {
                method: "GET",
                path,
                query: "orgId=org_dev&environmentId=local",
                authorization: Some(&authorization),
                now: 1_800_000_000,
                body: "",
            },
        );
        (response.status, serde_json::from_str(&response.body).unwrap())
    }

    fn question(metric: &str) -> Value {
        json!({
            "irVersion": "0.1", "name": "dev.daily", "objectSet": TYPE,
            "aggregate": [{"metric": metric}],
            "groupBy": [{"path": {"property": "occurred_at"}, "grain": "day"}],
            "window": {"start": "2026-11-01T00:00:00Z", "end": "2026-11-08T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 60000},
            "calendar": "iso_utc"
        })
    }

    fn metric(metrics: &DevMetrics, host: &DevHost<NativeEngine>, property: &str, ratio: bool) -> String {
        let questions = metrics.questions(host).expect("runtime");
        questions
            .definitions()
            .iter()
            .find(|definition| {
                let is_ratio = definition.type_params.numerator.is_some();
                is_ratio == ratio
                    && definition.label.contains(property)
                    && definition
                        .type_params
                        .metric_aggregation_params
                        .as_ref()
                        .is_none_or(|params| params.agg_time_dimension == "occurred_at")
            })
            .map_or_else(
                || panic!("no derived {property} metric among {:?}", questions.definitions().iter().map(|d| &d.label).collect::<Vec<_>>()),
                |definition| definition.name.clone(),
            )
    }

    #[test]
    fn an_ad_hoc_question_is_answered_and_traced_over_the_derived_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        // The metrics a question may name are listed for the reader.
        let (status, listed) = get(&host, &metrics, "/v1/questions:metrics");
        assert_eq!(status, 200, "{listed}");
        let listed = listed["metrics"].as_array().unwrap();
        let entry = listed.iter().find(|metric| metric["name"] == amount.as_str()).unwrap();
        assert_eq!(entry["objectType"], TYPE);
        assert_eq!(entry["timeProperty"], "occurred_at");
        let (status, answer) = call(&host, &metrics, "/v1/questions:evaluate", &json!({"question": question(&amount)}));
        assert_eq!(status, 200, "{answer}");
        let rows = answer["rows"].as_object().unwrap();
        assert_eq!(rows.len(), 2, "{answer}");
        let totals: Vec<String> = rows.values().map(|row| row["totals"][0].as_str().unwrap_or_default().to_owned()).collect();
        assert!(totals.iter().any(|total| total.starts_with("15.5")), "{totals:?}");
        assert!(totals.iter().any(|total| total.starts_with("7.25")), "{totals:?}");
        let engine = answer["engine"].as_str().unwrap();
        if cfg!(feature = "duckdb") {
            assert_eq!(engine, "duckdb");
        } else {
            assert_eq!(engine, "cell");
        }
        // The trace of a bucket reaches the objects that contributed to it.
        let (bucket, _) = rows.iter().find(|(_, row)| row["totals"][0].as_str().unwrap_or_default().starts_with("15.5")).unwrap();
        let (status, trace) = call(
            &host,
            &metrics,
            "/v1/questions:trace",
            &json!({"question": question(&amount), "lineageRef": answer["lineageRef"], "bucket": bucket}),
        );
        assert_eq!(status, 200, "{trace}");
        let text = trace.to_string();
        assert!(text.contains("s1") && text.contains("s2") && !text.contains("\"s3\""), "{trace}");
    }

    #[test]
    fn every_derived_sum_is_answered_on_whichever_engine_lowers_it() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let questions = metrics.questions(&host).expect("runtime");
        assert!(!questions.definitions().is_empty());
        // Every derived sum answers, on whichever engine lowered it.
        for definition in questions.definitions().iter().filter(|d| d.type_params.numerator.is_none()) {
            let (status, answer) = call(&host, &metrics, "/v1/questions:evaluate", &json!({"question": question(&definition.name)}));
            assert_eq!(status, 200, "{} {answer}", definition.name);
            assert!(matches!(answer["engine"].as_str(), Some("cell" | "duckdb")));
        }
    }

    #[test]
    fn a_new_revision_is_derived_again_and_an_undeclared_measure_is_no_metric() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let amount = metric(&metrics, &host, "amount", false);
        let active = host.active_definitions().ok().flatten().expect("active");
        let admitted = super::super::admit::admit(&artifacts(false), 3_000).ok().expect("admitted");
        host.install_definitions(admitted.model, admitted.input, Some(active.version))
            .expect("installed");
        let questions = metrics.questions(&host).expect("runtime");
        assert!(questions.definitions().iter().all(|definition| definition.name != amount));
        let (status, refused) = call(&host, &metrics, "/v1/questions:evaluate", &json!({"question": question(&amount)}));
        assert_ne!(status, 200, "{refused}");
    }

    #[test]
    fn a_profile_counts_the_visible_rows_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let host = host(dir.path());
        let metrics = DevMetrics::new([7_u8; 32], scope());
        let (status, profile) = get(&host, &metrics, &format!("/v1/objects/{TYPE}:profile"));
        assert_eq!(status, 200, "{profile}");
        assert_eq!(profile["rows"], 3);
        assert_eq!(profile["withheldRows"], 0);
        assert_eq!(profile["approximate"], false);
        let property = |name: &str| {
            profile["properties"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["property"] == name)
                .unwrap()
                .clone()
        };
        let amount = property("amount");
        assert_eq!(amount["kind"], "numeric");
        assert_eq!(amount["count"], 3);
        assert_eq!(amount["nulls"], 0);
        assert_eq!(amount["distinct"], 3);
        assert_eq!(amount["min"].to_string(), "5.50");
        assert_eq!(amount["max"].to_string(), "10.00");
        let median = amount["quantiles"].as_array().unwrap().iter().find(|q| q["percent"] == 50).unwrap();
        assert_eq!(median["value"].to_string(), "7.25");
        let at = property("occurred_at");
        assert_eq!(at["kind"], "temporal");
        assert_eq!(at["histogram"]["kind"], "day");
        assert_eq!(at["histogram"]["bins"].as_array().unwrap().len(), 2);
        let region = property("region");
        assert_eq!((region["kind"].as_str(), region["distinct"].as_u64()), (Some("text"), Some(2)));
        let (status, _) = get(&host, &metrics, "/v1/objects/shop.Missing:profile");
        assert_ne!(status, 200);
    }
}
