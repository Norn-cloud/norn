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
use norn_cell::compute::QueryContext;
use norn_cell::engine::SqlEngine;
use norn_cell::{CellStore, InstalledDefinitionSet, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::{MetricSpace, derive_metric_space};
use norn_ir::{ArtifactEnvelope, LawRoot, MetricDefinition};
use norn_kernel::authz::{ActorKind, Binding, ResourceInput, UnboundProfile, VerifiedPrincipal};

/// Metric definitions derived from one installed definitions revision.
struct Derived {
    /// The receipt of the revision they were derived from.
    receipt: String,
    definitions: Vec<MetricDefinition>,
    catalog: EffectiveCatalog,
    /// The space itself, which admits every question (none when nothing is
    /// installed, so nothing is admitted).
    space: Option<MetricSpace>,
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
    handle_key: Vec<u8>,
    scope: CellScope,
    label: String,
    configured_connections: BTreeMap<String, String>,
    cache: Mutex<Option<Arc<Derived>>>,
}

impl DerivedQuestions {
    /// A runtime for the cell at `scope`, keyed by the host's lineage secret
    /// (at least 32 bytes, held by the host, never derived from cell data).
    #[must_use]
    pub fn new(handle_key: [u8; 32], scope: CellScope) -> Self {
        Self {
            handle_key: handle_key.to_vec(),
            scope,
            label: "dev".into(),
            configured_connections: BTreeMap::new(),
            cache: Mutex::new(None),
        }
    }

    /// [`Self::new`] keyed by a host secret of any length of at least 32
    /// bytes (a stage's `NORN_LINEAGE_KEY`); none for a shorter one.
    #[must_use]
    pub fn keyed(handle_key: &[u8], scope: CellScope) -> Option<Self> {
        (handle_key.len() >= 32).then(|| Self {
            handle_key: handle_key.to_vec(),
            scope,
            label: "dev".into(),
            configured_connections: BTreeMap::new(),
            cache: Mutex::new(None),
        })
    }

    /// Label the answer frontier's population and mapping pins with `label`
    /// (`dev` by default; a hosted cell says `tenant`). A label only.
    #[must_use]
    pub fn with_label(mut self, label: &str) -> Self {
        label.clone_into(&mut self.label);
        self
    }

    /// Register host-configured `(connection_id -> mapping_id)` bindings so
    /// every configured connector sharing a mapping is required even before
    /// its first sync writes a `source.Connection` record.
    #[must_use]
    pub fn with_configured_connections(mut self, connections: BTreeMap<String, String>) -> Self {
        self.configured_connections = connections;
        self
    }

    /// The question runtime for `host`'s active revision, using only the cell's
    /// durable clock and connector heartbeats (`now_ms == 0`).
    ///
    /// # Errors
    /// The active revision cannot be read, or its metric space does not derive.
    pub fn questions<E: SqlEngine>(
        &self,
        host: &DevHost<E>,
    ) -> Result<DerivedRuntime<'_>, Refusal> {
        self.questions_at(host, 0)
    }

    /// The question runtime for `host`'s active revision observed at wall-clock
    /// `now_ms` (Unix milliseconds; `0` falls back to durable store clocks),
    /// ensuring an idle cell with a paused connector ages from `Exact` to
    /// `Fresh` and `Stale` without requiring new commits.
    ///
    /// # Errors
    /// The active revision cannot be read, or its metric space does not derive.
    pub fn questions_at<E: SqlEngine>(
        &self,
        host: &DevHost<E>,
        now_ms: u64,
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
            request_now_ms: (now_ms > 0).then_some(now_ms),
            configured_override: BTreeMap::new(),
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
            space: None,
        });
    };
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(&active.canonical_set).map_err(Refusal::host)?;
    // The dev checker admits against an empty registry (`admit.rs`); derive
    // against the same, at the cell's own law scope.
    let view = ScopedView::bound(&scope.as_scope_ref(), &artifacts, &[], &BTreeSet::new());
    let space = derive_metric_space(&view, &LawRoot::Cell(scope.key())).map_err(|findings| {
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
        space: Some(space),
    })
}

/// Rows an ad-hoc question may scan per evaluation: the limit profile's
/// question bound, the most admission accepts.
pub const MAX_ROWS: u32 = norn_ir::QuestionLimits::DEFAULT.max_scanned_records;
/// Buckets an ad-hoc answer may hold.
pub const MAX_BUCKETS: usize = 5_000;

/// One request's view of [`DerivedQuestions`]: the active revision's metrics.
/// Questions are ad hoc (`:evaluate`, `:trace`); the cell declares no named
/// questions.
pub struct DerivedRuntime<'m> {
    metrics: &'m DerivedQuestions,
    derived: Arc<Derived>,
    request_now_ms: Option<u64>,
    configured_override: BTreeMap<String, String>,
}

impl DerivedRuntime<'_> {
    /// Merge per-request scoped `(connection_id -> mapping_id)` bindings (for
    /// example from `Connect::configured_mappings`) into this request's
    /// required-source check.
    #[must_use]
    pub fn with_configured_connections(mut self, connections: BTreeMap<String, String>) -> Self {
        self.configured_override.extend(connections);
        self
    }

    /// Merge optional per-request scoped `(connection_id -> mapping_id)` bindings.
    #[must_use]
    pub fn with_configured(mut self, connections: Option<BTreeMap<String, String>>) -> Self {
        if let Some(connections) = connections {
            self.configured_override.extend(connections);
        }
        self
    }

    fn effective_configured(&self) -> BTreeMap<String, String> {
        let mut combined = self.metrics.configured_connections.clone();
        combined.extend(
            self.configured_override
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        combined
    }
}

/// The object type a read of `resource` is governed by: a derived sum's
/// semantic model, and a type name itself. A ratio is governed by its
/// operands' type only when the numerator and the denominator are sums over
/// the same type; otherwise it names no type and every read of it is refused.
/// Admission authorizes each operand against its own type as well, so a
/// caller who cannot read the denominator's type never gets the ratio.
pub(crate) fn governed<'a>(definitions: &'a [MetricDefinition], resource: &'a str) -> &'a str {
    let find = |name: &str| {
        definitions
            .iter()
            .find(|definition| definition.name == name)
    };
    let model = |name: &str| {
        find(name)?
            .type_params
            .metric_aggregation_params
            .as_ref()
            .map(|params| params.semantic_model.0.as_str())
    };
    let Some(metric) = find(resource) else {
        return resource;
    };
    if let Some(model) = model(resource) {
        return model;
    }
    let operand = |name: Option<&'a String>| name.and_then(|name| model(name));
    match (
        operand(metric.type_params.numerator.as_ref()),
        operand(metric.type_params.denominator.as_ref()),
    ) {
        (Some(numerator), Some(denominator)) if numerator == denominator => numerator,
        _ => resource,
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
    /// the active revision as the mapping pin, the cell's persisted clock
    /// advanced by the trusted request clock (`request_now_ms`), and real
    /// connector cursors/watermarks/heartbeats for every external source
    /// feeding the question's types (QUERY-FRESH-1).
    fn snapshot(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        let clock_floor = store.read().clock_floor().map_err(Refusal::from_compute)?;
        let configured = self.effective_configured();
        let freshness = store
            .source_freshness_snapshot_with_configured(norn_cell::MAIN_WORLD, plan, &configured)
            .map_err(Refusal::from_compute)?;
        // Native-only questions have no connector heartbeats to age against, so
        // keep their `now_ms` anchored to the durable store clock (preserving
        // unpinned `:trace` stability across wall-clock ticks). Source-backed
        // questions advance with the trusted request clock so paused connectors
        // age from `Exact` to `Fresh` and `Stale` even in an idle cell.
        let now_ms = if freshness.source_cursors.is_empty() && freshness.watermarks.is_empty() {
            clock_floor
        } else {
            clock_floor
                .max(freshness.max_heartbeat_ms)
                .max(self.request_now_ms.unwrap_or(0))
        };
        Ok(QuestionSnapshot {
            context: QueryContext {
                world: norn_cell::MAIN_WORLD.to_owned(),
                scope: self.metrics.scope.key(),
                population_pin: format!("{}:{actor}", self.metrics.label),
                authorization_epoch: 1,
                mapping_pin: format!("{}:{}", self.metrics.label, self.derived.receipt),
                now_ms,
                source_cursors: freshness.source_cursors,
                watermarks: freshness.watermarks,
            },
            cutoff: None,
        })
    }
    fn snapshot_at(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        actor: &str,
        answered: Option<(u64, u64)>,
    ) -> Result<QuestionSnapshot, Refusal> {
        let Some((frontier, now)) = answered else {
            return self.snapshot(store, plan, actor);
        };
        // Pinned `:trace` reconstructs strictly against the historical
        // definitions and `source.Connection` reports in force at `frontier`
        // (never current live `NORN_SOURCES`), so adding a new connection
        // later cannot invalidate an earlier exact answer without a ledger change.
        let freshness = store
            .source_freshness_snapshot_at(
                norn_cell::MAIN_WORLD,
                plan,
                frontier,
                now,
                &BTreeMap::new(),
            )
            .map_err(Refusal::from_compute)?;
        Ok(QuestionSnapshot {
            context: QueryContext {
                world: norn_cell::MAIN_WORLD.to_owned(),
                scope: self.metrics.scope.key(),
                population_pin: format!("{}:{actor}", self.metrics.label),
                authorization_epoch: 1,
                mapping_pin: format!("{}:{}", self.metrics.label, self.derived.receipt),
                now_ms: now,
                source_cursors: freshness.source_cursors,
                watermarks: freshness.watermarks,
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
        Ok(norn_kernel::authz::AccessRequest::new(
            &VerifiedPrincipal::new(actor, ActorKind::User, None, vec![], BTreeMap::new()),
            Some(key.clone()),
            crate::policy::capability_name(capability),
            crate::policy::READ_ACTION,
            ResourceInput {
                object_type: row
                    .map_or_else(
                        || governed(&self.derived.definitions, question),
                        |row| row.object_type.as_str(),
                    )
                    .into(),
                id: row.map_or(question, |row| row.id.as_str()).into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedRead),
        ))
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
    fn as_ad_hoc(&self) -> Option<&dyn AdHocQuestions<E>> {
        Some(self)
    }
}

impl<E: SqlEngine> AdHocQuestions<E> for DerivedRuntime<'_> {
    fn definitions(&self) -> &[MetricDefinition] {
        &self.derived.definitions
    }
    fn unit_partition(&self, metric: &str) -> Vec<String> {
        let Some(space) = &self.derived.space else {
            return Vec::new();
        };
        space
            .candidates()
            .iter()
            .find(|candidate| candidate.definition.name == metric)
            .map_or_default(|candidate| {
                candidate
                    .unit_partition
                    .iter()
                    .map(|path| {
                        path.links
                            .iter()
                            .map(|link| link.0.as_str())
                            .chain(std::iter::once(path.property.0.as_str()))
                            .collect::<Vec<_>>()
                            .join(".")
                    })
                    .collect()
            })
    }
    fn catalog(&self) -> &EffectiveCatalog {
        &self.derived.catalog
    }
    fn admit(
        &self,
        question: &norn_ir::QuestionIR,
        max_rows: u32,
    ) -> Result<QuestionPlan, Vec<norn_checker::Diagnostic>> {
        // One admission seam: the derived space's own rules apply to every
        // question over its metrics.
        match &self.derived.space {
            Some(space) => space.admit(question, max_rows),
            None => norn_checker::metric::compute::admit(
                &self.derived.definitions,
                question,
                &self.derived.catalog,
                max_rows,
            ),
        }
    }
    fn max_rows(&self) -> u32 {
        MAX_ROWS
    }
    fn max_buckets(&self) -> usize {
        MAX_BUCKETS
    }
}
