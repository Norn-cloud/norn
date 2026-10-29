//! Structural law admission and conservative whole-set impact closure. No proof claims.
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, EffectiveCatalog, Span,
};
use norn_expr::env::FieldSource;
use norn_expr::{Expression, Type, ZenTypeEnv, check_expression};
use norn_graph::{Budget, Direction, Graph, reachability};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, LawIR, LawModality, LawResponse, LawRoot,
    LawTiming, ObjectTypeName, ResourceLimitProfile,
};
use std::collections::{BTreeMap, BTreeSet};

/// Stable law diagnostic codes.
pub mod codes {
    /// Malformed closed `LawIR` document.
    pub const INVALID_DOCUMENT: &str = "law.validation.invalid_document";
    /// Unsupported temporal combination.
    pub const UNSUPPORTED_TEMPORAL: &str = "law.validation.unsupported_temporal";
    /// Non-Boolean, unresolved, or inadmissible ZEN predicate.
    pub const PREDICATE_TYPE: &str = "law.model.predicate_type";
    /// An ontology slot or its exact pinned dependency is absent.
    pub const UNKNOWN_BINDING: &str = "law.model.unknown_binding";
    /// A child declaration replaces rather than tightens an inherited law.
    pub const WEAKENS_INHERITED: &str = "law.model.weakens_inherited";
    /// Closure cannot be computed completely within the supplied budget.
    pub const CLOSURE_REFUSED: &str = "law.model.closure_refused";
}

fn finding(
    code: &str,
    artifact: &ArtifactEnvelope,
    pointer: &str,
    message: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// Decode and locally check a law; used by the ordinary artifact checker/WASM ABI.
#[must_use]
pub fn check_law_local(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    match decode(artifact) {
        Ok(law) => local(&law, artifact),
        Err(error) => vec![finding(
            codes::INVALID_DOCUMENT,
            artifact,
            "/payload",
            error,
        )],
    }
}

fn decode(artifact: &ArtifactEnvelope) -> Result<LawIR, String> {
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .map_err(|error| error.to_string())
}

fn local(law: &LawIR, artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let root = match &law.scope.root {
        LawRoot::Company(id) | LawRoot::App(id) | LawRoot::Cell(id) | LawRoot::Entity(id) => id,
    };
    if [&law.id, &law.owner, &law.rationale, &law.subject, root]
        .iter()
        .any(|v| v.trim().is_empty())
        || law.scope.object_types.is_empty()
        || law
            .scope
            .object_types
            .iter()
            .chain(&law.scope.commitments)
            .any(|v| v.trim().is_empty())
    {
        out.push(finding(codes::INVALID_DOCUMENT, artifact, "/payload", "law identity, owner, rationale, subject, root and object types must be explicit and nonblank"));
    }
    if !matches!(law.modality, LawModality::Eventually { .. }) && law.timing.is_some() {
        out.push(finding(
            codes::UNSUPPORTED_TEMPORAL,
            artifact,
            "/payload/timing",
            "timing bounds are supported only on eventually; safety and strong-until are unbounded",
        ));
    }
    if let Some(LawTiming::Within { millis }) = &law.timing
        && millis
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .map(|value| value.to_string())
            .as_ref()
            != Some(millis)
    {
        out.push(finding(
            codes::INVALID_DOCUMENT,
            artifact,
            "/payload/timing/millis",
            "within requires positive canonical u64 milliseconds as decimal text",
        ));
    }
    if matches!(law.response, LawResponse::Invariant { .. })
        && (!matches!(law.modality, LawModality::Always { .. })
            || law.condition.is_some()
            || law.timing.is_some()
            || !law.scope.lifecycle_states.is_empty())
    {
        out.push(finding(codes::UNSUPPORTED_TEMPORAL, artifact, "/payload/response", "an invariant identity is an unconditional always law; filtering or negating it would change its meaning"));
    }
    if let LawRoot::Entity(name) = &law.scope.root
        && name != &law.subject
    {
        out.push(finding(
            codes::UNKNOWN_BINDING,
            artifact,
            "/payload/scope/root",
            "entity root must name the subject",
        ));
    }
    out
}

/// Whole-set structural checks, against the same effective entities as invariants/guards.
#[must_use]
pub fn check_law_set(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<Diagnostic> {
    let pool: Vec<_> = base.iter().chain(artifacts).cloned().collect();
    if !pool.iter().any(|a| a.kind() == ArtifactKind::Law) {
        return Vec::new();
    }
    let limits = ResourceLimitProfile::norn_limits_v1();
    let env = crate::derive_type_env(artifacts, base, &limits);
    let catalog = EffectiveCatalog::from_artifacts(&pool);
    let mut layer = Layer::default();
    for (index, a) in pool.iter().enumerate() {
        if let Decoded::Root(root) = decode_v2(a) {
            layer.push(index, *root);
        }
    }
    let context = LawContext {
        pool: &pool,
        catalog: &catalog,
        layer: &layer,
        env: env.as_ref().ok(),
    };
    // The whole-set inventory is identical for every law; compute it once.
    let closure_refused = pool
        .iter()
        .find(|a| a.kind() == ArtifactKind::Law)
        .is_some_and(|law| {
            conservative_closure(
                law,
                &pool,
                Budget {
                    max_nodes: 16_384,
                    max_edges: 131_072,
                    max_steps: 1_048_576,
                },
            )
            .is_err()
        });
    let mut out = Vec::new();
    let mut laws: BTreeMap<String, Vec<(&ArtifactEnvelope, LawIR)>> = BTreeMap::new();
    for artifact in &pool {
        if !matches!(artifact.payload, ArtifactPayload::Law(_)) {
            continue;
        }
        let Ok(law) = decode(artifact) else {
            out.extend(check_law_local(artifact));
            continue;
        };
        out.extend(local(&law, artifact));
        context.check_bound_law(&law, artifact, &mut out);
        for (parent, inherited) in laws.get(&law.id).into_iter().flatten() {
            if inherited != &law || crate::scope_conflict(&artifact.scope, &parent.scope).is_some()
            {
                out.push(finding(codes::WEAKENS_INHERITED, artifact, "/payload/id", "an existing law identity cannot be replaced; retain it and add a new conjunctive law identity"));
            }
        }
        if closure_refused {
            out.push(finding(
                codes::CLOSURE_REFUSED,
                artifact,
                "/dependencies",
                "whole-set closure contains unresolved dependencies or exceeds its graph budget",
            ));
        }
        laws.entry(law.id.clone())
            .or_default()
            .push((artifact, law));
    }
    out
}

struct LawContext<'a> {
    pool: &'a [ArtifactEnvelope],
    catalog: &'a EffectiveCatalog,
    layer: &'a Layer,
    env: Option<&'a ZenTypeEnv>,
}

impl LawContext<'_> {
    fn check_bound_law(&self, law: &LawIR, artifact: &ArtifactEnvelope, out: &mut Vec<Diagnostic>) {
        let subject = self
            .catalog
            .object_type_named(&law.subject)
            .and_then(Result::ok);
        let Some(subject) = subject else {
            out.push(finding(
                codes::UNKNOWN_BINDING,
                artifact,
                "/payload/subject",
                "law subject is not an admitted effective object type",
            ));
            return;
        };
        self.check_scope(law, artifact, subject, out);
        self.check_predicates(law, artifact, subject, out);
    }

    fn check_scope(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        subject: &crate::EffectiveObjectType,
        out: &mut Vec<Diagnostic>,
    ) {
        if !law.scope.object_types.iter().any(|name| {
            subject
                .lineage
                .iter()
                .any(|ancestor| ancestor.as_str() == name)
        }) {
            out.push(finding(
                codes::UNKNOWN_BINDING,
                artifact,
                "/payload/scope/objectTypes",
                "scope must include the subject or one of its supertypes",
            ));
        }
        for name in law
            .scope
            .object_types
            .iter()
            .chain(&law.scope.commitments)
            .chain(std::iter::once(&law.subject))
        {
            let bound = self.pool.iter().find(|a| {
                self.catalog
                    .object_type(&a.artifact_id)
                    .and_then(Result::ok)
                    .is_some_and(|t| t.name.as_str() == name)
            });
            if !bound.is_some_and(|target| pinned(artifact, target)) {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/scope",
                    format!(
                        "type {name:?} needs a required exact-version, visible entity dependency"
                    ),
                ));
            }
            if law.scope.commitments.contains(name)
                && self
                    .catalog
                    .object_type_named(name)
                    .and_then(Result::ok)
                    .is_none_or(|t| t.lifecycles.is_empty())
            {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/scope/commitments",
                    "Commitment scope needs a lifecycle-bearing registry type",
                ));
            }
        }
        for state in &law.scope.lifecycle_states {
            let machine = self
                .catalog
                .object_type_named(&state.object_type)
                .and_then(Result::ok)
                .and_then(|t| t.lifecycle(&state.property));
            if !law.scope.object_types.contains(&state.object_type)
                || state.states.is_empty()
                || !machine.is_some_and(|m| state.states.iter().all(|s| m.states.contains(s)))
            {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/scope/lifecycleStates",
                    "state selectors must bind declared lifecycle states of a scoped type",
                ));
            }
        }
    }

    fn check_predicates(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        subject: &crate::EffectiveObjectType,
        out: &mut Vec<Diagnostic>,
    ) {
        let bound_env = self.env.and_then(|env| {
            env.clone()
                .bind_self(&ObjectTypeName(law.subject.clone()))
                .ok()
        });
        if let Some(env) = &bound_env {
            if let Some(condition) = &law.condition {
                predicate(condition.as_str(), env, artifact, "/payload/condition", out);
            }
            if let LawModality::Until { predicate: stop } = &law.modality {
                predicate(
                    stop.as_str(),
                    env,
                    artifact,
                    "/payload/modality/predicate",
                    out,
                );
            }
            self.check_response(law, artifact, subject, env, out);
            if let Some(LawTiming::BeforeDeadline { property }) = &law.timing
                && !env
                    .record(&law.subject)
                    .and_then(|r| r.fields.get(property))
                    .is_some_and(|f| {
                        matches!(f.ty, Type::Timestamp { .. })
                            && matches!(f.source, FieldSource::Property { read_only: false })
                    })
            {
                out.push(finding(
                    codes::UNKNOWN_BINDING,
                    artifact,
                    "/payload/timing/property",
                    "deadline must bind a required stored timestamp property of the subject; computed fields are unsupported",
                ));
            }
        } else {
            out.push(finding(
                codes::PREDICATE_TYPE,
                artifact,
                "/payload",
                "law's effective ZEN environment cannot be derived",
            ));
        }
    }

    fn check_response(
        &self,
        law: &LawIR,
        artifact: &ArtifactEnvelope,
        subject: &crate::EffectiveObjectType,
        env: &ZenTypeEnv,
        out: &mut Vec<Diagnostic>,
    ) {
        match &law.response {
            LawResponse::Predicate { expression } => predicate(
                expression.as_str(),
                env,
                artifact,
                "/payload/response/expression",
                out,
            ),
            LawResponse::Invariant {
                object_type,
                invariant_id,
            } => {
                let invariant = self
                    .layer
                    .object_types
                    .iter()
                    .filter(|t| subject.lineage.contains(&t.name))
                    .flat_map(|t| &t.invariants)
                    .chain(
                        self.layer
                            .interfaces
                            .iter()
                            .filter(|i| subject.interfaces.contains(&i.name))
                            .flat_map(|i| &i.invariants),
                    )
                    .find(|invariant| invariant.id.as_str() == invariant_id);
                if object_type != &law.subject || invariant.is_none() {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "invariant identity is absent from the subject's effective contract",
                    ));
                } else if let Some(invariant) = invariant {
                    predicate(
                        invariant.expression.as_str(),
                        env,
                        artifact,
                        "/payload/response",
                        out,
                    );
                }
            }
            LawResponse::Action { artifact_id } => {
                if !self.pool.iter().any(|a| {
                    &a.artifact_id == artifact_id
                        && a.kind() == ArtifactKind::Action
                        && pinned(artifact, a)
                }) {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "action response needs an exact pinned ActionIR dependency",
                    ));
                }
            }
            LawResponse::Transition {
                object_type,
                property,
                transition_id,
            } => {
                if object_type != &law.subject
                    || !subject.lifecycle(property).is_some_and(|m| {
                        m.lifecycle
                            .transitions
                            .iter()
                            .any(|t| t.id.as_str() == transition_id)
                    })
                {
                    out.push(finding(
                        codes::UNKNOWN_BINDING,
                        artifact,
                        "/payload/response",
                        "transition response must bind a subject lifecycle transition",
                    ));
                }
            }
        }
    }
}

fn pinned(owner: &ArtifactEnvelope, target: &ArtifactEnvelope) -> bool {
    crate::scope_conflict(&owner.scope, &target.scope).is_none()
        && owner.dependencies.iter().any(|d| {
            d.required
                && d.artifact_id == target.artifact_id
                && d.kind == target.kind()
                && d.version.as_ref() == Some(&target.version)
        })
}

fn predicate(
    source: &str,
    env: &ZenTypeEnv,
    artifact: &ArtifactEnvelope,
    pointer: &str,
    out: &mut Vec<Diagnostic>,
) {
    let result = Expression::compile(source)
        .map_err(|e| e.to_string())
        .and_then(|expr| {
            check_expression(&expr, env, &Type::Bool)
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        });
    if let Err(error) = result {
        out.push(finding(codes::PREDICATE_TYPE, artifact, pointer, error));
    }
}

/// Conservative invalidation inventory. Every artifact is retained: hidden/opaque writers
/// cannot disappear merely because their write set is not syntactically visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LawClosure {
    /// Exact artifact identities, including read-only base and laws.
    pub artifacts: BTreeSet<(String, String)>,
    /// Every action/decision/connector/mapping and effective transition.
    pub writers: BTreeSet<String>,
    /// Canonical semantic envelope digests; same-version content edits invalidate evidence too.
    pub content_digests: BTreeMap<String, String>,
    /// Runtime entrances requiring later verification contracts; never certified here.
    pub open_boundaries: BTreeSet<String>,
}

/// Compute bounded whole-set invalidation via the shared graph reachability library.
/// The pool must be the complete effective set, not merely declared law dependencies.
/// # Errors
/// Refuses duplicate/missing nodes, absent required dependencies or exhausted budgets.
pub fn conservative_closure(
    law: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    budget: Budget,
) -> Result<LawClosure, norn_graph::Error<String>> {
    if pool.len() > budget.max_nodes {
        return Err(norn_graph::Error::BudgetExceeded(
            norn_graph::BudgetExceeded {
                resource: norn_graph::Limit::Nodes,
                limit: budget.max_nodes,
            },
        ));
    }
    let nodes: Vec<_> = pool.iter().map(|a| a.artifact_id.clone()).collect();
    let by_id: BTreeMap<_, _> = pool.iter().map(|a| (a.artifact_id.as_str(), a)).collect();
    let mut edges = Vec::new();
    for a in pool {
        // Whole-set invalidation is deliberately broader than field-level influence.
        if edges.len() >= budget.max_edges {
            return Err(norn_graph::Error::BudgetExceeded(
                norn_graph::BudgetExceeded {
                    resource: norn_graph::Limit::Edges,
                    limit: budget.max_edges,
                },
            ));
        }
        edges.push((law.artifact_id.clone(), a.artifact_id.clone()));
        for d in &a.dependencies {
            if let Some(target) = by_id.get(d.artifact_id.as_str()) {
                if d.version.as_ref() != Some(&target.version) || d.kind != target.kind() {
                    return Err(norn_graph::Error::UnknownNode(d.artifact_id.clone()));
                }
                if edges.len() >= budget.max_edges {
                    return Err(norn_graph::Error::BudgetExceeded(
                        norn_graph::BudgetExceeded {
                            resource: norn_graph::Limit::Edges,
                            limit: budget.max_edges,
                        },
                    ));
                }
                edges.push((a.artifact_id.clone(), target.artifact_id.clone()));
            } else if d.required {
                return Err(norn_graph::Error::UnknownNode(d.artifact_id.clone()));
            }
        }
    }
    let reached = reachability(
        &Graph { nodes, edges },
        std::slice::from_ref(&law.artifact_id),
        Direction::Downstream,
        budget,
    )?;
    let content_digests = pool
        .iter()
        .map(|a| {
            crate::receipt::ArtifactRef::of(a)
                .map(|identity| (identity.artifact_id, identity.sha256))
                .map_err(|_hash_error| norn_graph::Error::Invariant)
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let artifacts = pool
        .iter()
        .filter(|a| reached.contains(&a.artifact_id))
        .map(|a| (a.artifact_id.clone(), a.version.clone()))
        .collect();
    let writers = writer_inventory(pool);
    Ok(LawClosure {
        artifacts,
        writers,
        content_digests,
        open_boundaries: [
            "observe",
            "reconciliation",
            "migration",
            "bootstrap",
            "restore",
            "opaque_effects",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect(),
    })
}

fn writer_inventory(pool: &[ArtifactEnvelope]) -> BTreeSet<String> {
    let mut writers: BTreeSet<_> = pool
        .iter()
        .filter(|a| {
            matches!(
                a.kind(),
                ArtifactKind::Action
                    | ArtifactKind::Rule
                    | ArtifactKind::Connector
                    | ArtifactKind::Mapping
            )
        })
        .map(|a| a.artifact_id.clone())
        .collect();
    let catalog = EffectiveCatalog::from_artifacts(pool);
    for artifact in pool {
        if let Some(object_type) = catalog
            .object_type(&artifact.artifact_id)
            .and_then(Result::ok)
        {
            for lifecycle in &object_type.lifecycles {
                for transition in &lifecycle.lifecycle.transitions {
                    writers.insert(format!(
                        "{}:{}:{}",
                        object_type.name.as_str(),
                        lifecycle.lifecycle.property.as_str(),
                        transition.id.as_str()
                    ));
                }
            }
        }
    }
    writers
}
