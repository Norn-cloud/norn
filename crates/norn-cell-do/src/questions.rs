//! Ad-hoc questions over the metrics a cell's active definitions revision
//! derives (METRIC-DERIVE-1), for any host that serves [`crate::server`]: the
//! dev host and a hosted cell alike.
//!
//! The metrics are derived from the active revision's admitted artifacts
//! (sums of properties declared additive, declared ratios and conserved
//! ratios, each at a time grain), never declared by a request, and derived
//! again on the first question after a new revision. Access goes through the
//! host's own authorizer: this module maps a caller and a resource to the
//! kernel request and decides nothing. With the `duckdb` feature, a plan
//! `DuckDB` lowers is answered there and anything else falls back to the native
//! exact engine; the answer's `engine` names which one answered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use norn_api::authz::ReadCapability;
use norn_api::host::DevHost;
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
#[cfg(feature = "duckdb")]
use norn_cell::CellError;
use norn_cell::compute::QueryContext;
#[cfg(feature = "duckdb")]
use norn_cell::compute::QueryResult;
use norn_cell::engine::SqlEngine;
use norn_cell::{CellStore, InstalledDefinitionSet, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::{ArtifactEnvelope, LawRoot, MetricDefinition};
use norn_kernel::authz::{ActorKind, Delegation, PrincipalInput, ResourceInput};

/// Metric definitions derived from one installed definitions revision.
struct Derived {
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
pub struct DerivedQuestions {
    handle_key: [u8; 32],
    scope: CellScope,
    now: fn() -> u64,
    label: String,
    cache: Mutex<Option<Arc<Derived>>>,
}

impl DerivedQuestions {
    /// A runtime for the cell at `scope`, keyed by the host's lineage secret
    /// (at least 32 bytes, held by the host, never derived from cell data),
    /// reading the host's clock (milliseconds since the Unix epoch).
    #[must_use]
    pub fn new(handle_key: [u8; 32], scope: CellScope, now: fn() -> u64) -> Self {
        Self {
            handle_key,
            scope,
            now,
            label: "dev".into(),
            cache: Mutex::new(None),
        }
    }

    /// Label the answer frontier's population and mapping pins with `label`
    /// (`dev` by default; a hosted cell says `tenant`). A label only.
    #[must_use]
    pub fn with_label(mut self, label: &str) -> Self {
        label.clone_into(&mut self.label);
        self
    }

    /// The question runtime for `host`'s active revision, to pass to
    /// [`crate::server::handle_with`] for one request.
    ///
    /// # Errors
    /// The active revision cannot be read, or its metric space does not derive.
    pub fn questions<E: SqlEngine>(
        &self,
        host: &DevHost<E>,
    ) -> Result<DerivedRuntime<'_>, Refusal> {
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
        Ok(DerivedRuntime {
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
    let space = derive_metric_space(&artifacts, &BTreeSet::new(), &LawRoot::Cell(scope.key()))
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

/// Rows an ad-hoc question may read per evaluation.
pub const MAX_ROWS: u32 = 50_000;
/// Buckets an ad-hoc answer may hold.
pub const MAX_BUCKETS: usize = 5_000;

/// One request's view of [`DerivedQuestions`]: the active revision's metrics.
/// Questions are ad hoc (`:evaluate`, `:trace`); the cell declares no named
/// questions.
pub struct DerivedRuntime<'m> {
    metrics: &'m DerivedQuestions,
    derived: Arc<Derived>,
}

impl DerivedRuntime<'_> {
    /// The object type a read of `resource` is governed by: a derived sum's
    /// semantic model, a ratio's numerator's, and a type name itself.
    fn governed<'a>(&'a self, resource: &'a str) -> &'a str {
        let mut name = resource;
        // A ratio names its numerator; a derived ratio never nests deeper.
        for _ in 0..2 {
            let Some(metric) = self
                .derived
                .definitions
                .iter()
                .find(|definition| definition.name == name)
            else {
                return name;
            };
            if let Some(params) = &metric.type_params.metric_aggregation_params {
                return params.semantic_model.0.as_str();
            }
            match metric.type_params.numerator.as_deref() {
                Some(numerator) => name = numerator,
                None => return resource,
            }
        }
        resource
    }
}

impl<E: SqlEngine> QuestionRuntime<E> for DerivedRuntime<'_> {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(Refusal::new(
            http::StatusCode::NOT_FOUND,
            "question.not_found",
            "no such admitted question",
            "the cell declares no named questions; ask through /v1/questions:evaluate",
        ))
    }
    /// The cell's own frontier: its main world, the caller as the population,
    /// the active revision as the mapping pin, and the host clock. It tracks
    /// no external source, so there is no source frontier.
    fn snapshot(
        &self,
        _store: &CellStore<E>,
        _plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Ok(QuestionSnapshot {
            context: QueryContext {
                world: norn_cell::MAIN_WORLD.to_owned(),
                scope: self.metrics.scope.key(),
                population_pin: format!("{}:{actor}", self.metrics.label),
                authorization_epoch: 1,
                mapping_pin: format!("{}:{}", self.metrics.label, self.derived.receipt),
                now_ms: (self.metrics.now)(),
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
                object_type: row
                    .map_or_else(|| self.governed(question), |row| row.object_type.as_str())
                    .into(),
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
    /// With the `duckdb` feature, a plan `DuckDB` lowers (one keyed sum, or a
    /// ratio of two, over decimals of scale at most 6) is answered there;
    /// anything else falls back to the native exact engine. The answer's
    /// `engine` names which one answered.
    #[cfg(feature = "duckdb")]
    fn evaluate(
        &self,
        store: &CellStore<E>,
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

impl<E: SqlEngine> AdHocQuestions<E> for DerivedRuntime<'_> {
    fn definitions(&self) -> &[MetricDefinition] {
        &self.derived.definitions
    }
    fn catalog(&self) -> &EffectiveCatalog {
        &self.derived.catalog
    }
    fn max_rows(&self) -> u32 {
        MAX_ROWS
    }
    fn max_buckets(&self) -> usize {
        MAX_BUCKETS
    }
}
