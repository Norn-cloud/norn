//! Graph-load request validation and bounded relation-batch planning.

use norn_ir::{
    Diagnostic, DiagnosticRemediation, DiagnosticSeverity, GraphLoadEvidence, GraphLoadPlan,
    GraphLoadRequest, GraphReadShape, GraphRelationShape, RelationBatchPlan, ScopeRef,
};

/// Validate a [`GraphLoadRequest`] and compile its bounded relation-batch plan.
#[must_use]
pub fn plan_graph_load(request: &GraphLoadRequest) -> GraphLoadPlan {
    let mut diagnostics = Vec::new();

    validate_graph_scope(request, &mut diagnostics);
    validate_graph_shape(&request.shape, &mut diagnostics);

    let mut relation_batches = Vec::new();
    build_relation_batches(
        request,
        &request.shape.root_object_kind,
        &request.shape.root_ids,
        &request.shape.relations,
        1,
        &mut relation_batches,
        &mut diagnostics,
    );

    let depth = graph_max_depth(&request.shape.relations);
    let relation_shape_count = graph_relation_count(&request.shape.relations);
    let estimated_rows = estimate_graph_rows(
        u64::try_from(request.shape.root_ids.len()).unwrap_or(u64::MAX),
        &request.shape.relations,
    );

    validate_graph_budgets(
        request,
        depth,
        relation_shape_count,
        relation_batches.len(),
        estimated_rows,
        &mut diagnostics,
    );

    let evidence = GraphLoadEvidence {
        correlation_id: request.correlation_id.clone(),
        depth,
        relation_shape_count,
        relation_batch_count: u32::try_from(relation_batches.len()).unwrap_or(u32::MAX),
        estimated_rows,
        budget: request.budget.clone(),
    };

    GraphLoadPlan {
        request_id: request.request_id.clone(),
        correlation_id: request.correlation_id.clone(),
        root_identity_keys: request
            .shape
            .root_ids
            .iter()
            .map(|root_id| graph_identity_key(&request.shape.root_object_kind, root_id))
            .collect(),
        relation_batches,
        evidence,
        can_execute: diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != DiagnosticSeverity::Error),
        diagnostics,
    }
}

fn validate_graph_budgets(
    request: &GraphLoadRequest,
    depth: u32,
    relation_shape_count: u32,
    relation_batch_count: usize,
    estimated_rows: u64,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if u32::try_from(request.shape.root_ids.len())
        .ok()
        .is_none_or(|count| count > request.budget.max_root_objects)
    {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "root object count exceeds budget {}",
                request.budget.max_root_objects
            ),
            "shape.rootIds",
        ));
    }
    if depth > request.budget.max_depth {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.excessive_depth",
            format!(
                "graph depth {depth} exceeds budget {}",
                request.budget.max_depth
            ),
            "shape.relations",
        ));
    }
    if relation_shape_count > request.budget.max_relation_shapes {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "relation shape count {relation_shape_count} exceeds budget {}",
                request.budget.max_relation_shapes
            ),
            "shape.relations",
        ));
    }
    if u32::try_from(relation_batch_count)
        .ok()
        .is_none_or(|count| count > request.budget.max_relation_batches)
    {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "relation batch count {relation_batch_count} exceeds budget {}",
                request.budget.max_relation_batches
            ),
            "shape.relations",
        ));
    }
    if estimated_rows > request.budget.max_rows
        && request.budget.allow_higher_risk_policy != Some(true)
    {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            format!(
                "estimated row count {estimated_rows} exceeds budget {}",
                request.budget.max_rows
            ),
            "budget.maxRows",
        ));
    }
}
fn validate_graph_scope(request: &GraphLoadRequest, diagnostics: &mut Vec<Diagnostic>) {
    for key in &request.scope_required {
        if !graph_scope_has(&request.scope, key) {
            diagnostics.push(graph_diagnostic(
                "graph_load.validation.missing_scope",
                format!("required scope '{key}' is missing"),
                format!("scope.{key}"),
            ));
        }
    }
}

fn graph_scope_has(scope: &ScopeRef, key: &str) -> bool {
    match key {
        "orgId" | "org_id" => scope
            .org_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "tenantId" | "tenant_id" => scope
            .tenant_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "workspaceId" | "workspace_id" => scope
            .workspace_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "environmentId" | "environment_id" => scope
            .environment_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        "businessUnitId" | "business_unit_id" => scope
            .business_unit_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        _ => false,
    }
}

fn validate_graph_shape(shape: &GraphReadShape, diagnostics: &mut Vec<Diagnostic>) {
    if shape.root_object_kind.trim().is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.invalid_shape",
            "rootObjectKind must be present",
            "shape.rootObjectKind",
        ));
    }
    if shape.root_ids.is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.invalid_shape",
            "rootIds must not be empty",
            "shape.rootIds",
        ));
    }

    for (index, relation) in shape.relations.iter().enumerate() {
        validate_graph_relation_shape(relation, &format!("shape.relations[{index}]"), diagnostics);
    }
}

fn validate_graph_relation_shape(
    relation: &GraphRelationShape,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if relation.relation_kind.trim().is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.missing_relation_kind",
            "relationKind must be present",
            format!("{path}.relationKind"),
        ));
    }
    if relation.target_object_kind.trim().is_empty() {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.invalid_shape",
            "targetObjectKind must be present",
            format!("{path}.targetObjectKind"),
        ));
    }
    if relation.limit == 0 {
        diagnostics.push(graph_diagnostic(
            "graph_load.validation.over_budget",
            "relation limits must be positive integers",
            format!("{path}.limit"),
        ));
    }
    for (index, child) in relation.relations.iter().enumerate() {
        validate_graph_relation_shape(child, &format!("{path}.relations[{index}]"), diagnostics);
    }
}

fn build_relation_batches(
    request: &GraphLoadRequest,
    source_object_kind: &str,
    source_ids: &[String],
    relations: &[GraphRelationShape],
    depth: u32,
    batches: &mut Vec<RelationBatchPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for relation in relations {
        if source_ids.len() > 1 && !relation.relations.is_empty() && relation.limit > 500 {
            diagnostics.push(graph_diagnostic(
                "graph_load.validation.n_plus_one_risk",
                format!(
                    "relation batch '{}:{}' risks unbounded fan-out",
                    source_object_kind, relation.relation_kind
                ),
                "shape.relations",
            ));
        }

        batches.push(RelationBatchPlan {
            batch_id: format!("batch_{}", batches.len().saturating_add(1)),
            relation_kind: relation.relation_kind.clone(),
            direction: relation.direction.clone(),
            source_object_kind: source_object_kind.to_string(),
            target_object_kind: relation.target_object_kind.clone(),
            depth,
            limit: relation.limit,
            source_identity_keys: source_ids
                .iter()
                .map(|source_id| graph_identity_key(source_object_kind, source_id))
                .collect(),
            scope_pushdown: request.scope.clone(),
            include_relation_metadata: relation.include_relation_metadata.unwrap_or(false),
        });

        build_relation_batches(
            request,
            &relation.target_object_kind,
            &[],
            &relation.relations,
            depth.saturating_add(1),
            batches,
            diagnostics,
        );
    }
}

fn graph_max_depth(relations: &[GraphRelationShape]) -> u32 {
    relations
        .iter()
        .map(|relation| graph_max_depth(&relation.relations).saturating_add(1))
        .max()
        .unwrap_or(0)
}

fn graph_relation_count(relations: &[GraphRelationShape]) -> u32 {
    relations
        .iter()
        .map(|relation| graph_relation_count(&relation.relations).saturating_add(1))
        .fold(0_u32, u32::saturating_add)
}

fn estimate_graph_rows(root_count: u64, relations: &[GraphRelationShape]) -> u64 {
    let mut rows = root_count;
    for relation in relations {
        let direct_rows = root_count.max(1).saturating_mul(u64::from(relation.limit));
        rows = rows.saturating_add(
            direct_rows.saturating_add(estimate_graph_rows(direct_rows, &relation.relations)),
        );
    }
    rows
}

fn graph_identity_key(object_kind: &str, object_id: &str) -> String {
    format!("{object_kind}:{object_id}")
}

fn graph_diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id: None,
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
