//! Singleton-role overapproximation of declared controls. All action orderings
//! are retained, including effects and guards we cannot concretely execute.
use crate::duties::{DutyEdge, DutyGraph, check_duties};
use crate::{Diagnostic, DiagnosticCode};
use norn_ir::access::{AccessCondition, PolicyRoot};
use norn_ir::{
    ActionRoot, ArtifactEnvelope, ArtifactPayload, ControlStatus, ControlVerdict,
    DiagnosticRemediation, DiagnosticSeverity, ObjectTypeDef, Reachability,
};
use std::collections::{BTreeMap, BTreeSet};

/// Compute controls for the complete candidate and installed artifact pool.
#[must_use]
pub fn check_controls(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> (Vec<ControlVerdict>, Vec<Diagnostic>) {
    let resolved = crate::artifact_set::resolve_artifact_set(artifacts, base).is_ok();
    let pool: Vec<_> = base.iter().chain(artifacts).collect();
    let objects: BTreeMap<_, _> = pool
        .iter()
        .filter_map(|a| {
            let ArtifactPayload::Entity(payload) = &a.payload else {
                return None;
            };
            let object = crate::entity_lowering::entity_object_type(payload)
                .ok()??
                .root
                .object_type;
            Some((object.name.clone(), (a.artifact_id.clone(), object)))
        })
        .collect();
    let actions: Option<Vec<ActionRoot>> = pool
        .iter()
        .filter_map(|a| {
            let ArtifactPayload::Action(payload) = &a.payload else {
                return None;
            };
            Some(
                serde_json::to_value(payload.to_json_value())
                    .and_then(serde_json::from_value)
                    .ok(),
            )
        })
        .collect();
    let policies: Option<Vec<PolicyRoot>> = pool
        .iter()
        .filter_map(|a| {
            let ArtifactPayload::Policy(payload) = &a.payload else {
                return None;
            };
            Some(
                serde_json::to_value(payload.to_json_value())
                    .and_then(serde_json::from_value)
                    .ok(),
            )
        })
        .collect();
    let mut verdicts = Vec::new();
    let mut findings = Vec::new();
    for (name, (artifact_id, object)) in &objects {
        let mut current = Some(object);
        let mut visited = BTreeSet::new();
        while let Some(owner) = current {
            if !visited.insert(owner.name.clone()) {
                break;
            }
            if let Some(controls) = &owner.controls {
                for path in &controls.sensitive_paths {
                    let valid = path.object_type == owner.name && !path.actions.is_empty();
                    let result = if valid && resolved {
                        actions
                            .as_ref()
                            .zip(policies.as_ref())
                            .and_then(|(actions, policies)| {
                                project(object, actions, policies).map(|(graph, concrete)| {
                                    evaluate_path(&graph, &path.actions, concrete)
                                })
                            })
                    } else {
                        None
                    };
                    let (verdict, reachability, trace) = result.unwrap_or((
                        ControlStatus::Unknown,
                        Reachability::Unknown,
                        Vec::new(),
                    ));
                    let refusal = if !valid {
                        Some("controls.validation.invalid_path")
                    } else if verdict == ControlStatus::Refuted {
                        Some("controls.validation.refuted")
                    } else if path.require_witness && reachability != Reachability::Witnessed {
                        Some("controls.validation.witness_required")
                    } else {
                        None
                    };
                    if let Some(code) = refusal {
                        findings.push(Diagnostic {
                            code: DiagnosticCode::new(code), severity: DiagnosticSeverity::Error,
                            message: format!("control on {} has safety {verdict:?} and availability {reachability:?}", name.as_str()),
                            span: None, artifact_id: Some(artifact_id.clone()),
                            remediation: Some(DiagnosticRemediation::FixDefinition),
                        });
                    }
                    verdicts.push(ControlVerdict {
                        object_type: name.clone(), actions: path.actions.clone(), verdict, reachability, trace,
                        explanation: "Singleton-role finite action-order overapproximation; data, guards and cross-object effects may erase concrete witnesses".into(),
                    });
                }
            }
            current = owner
                .extends
                .as_ref()
                .and_then(|parent| objects.get(parent).map(|(_, def)| def));
        }
    }
    (verdicts, findings)
}

fn project(
    object: &ObjectTypeDef,
    actions: &[ActionRoot],
    policies: &[PolicyRoot],
) -> Option<(DutyGraph, bool)> {
    let mut grants: BTreeMap<String, BTreeSet<String>> = actions
        .iter()
        .map(|a| (a.action_id.clone(), BTreeSet::new()))
        .collect();
    if grants.len() != actions.len() {
        return None;
    }
    let mut concrete = object.extends.is_none()
        && object.properties.is_empty()
        && object.links.is_empty()
        && object.invariants.is_empty()
        && object.lifecycles.is_empty()
        && object.implements.is_empty()
        && object.owned_collections.is_empty();
    for policy in policies {
        let PolicyRoot::Access(policy) = policy else {
            return None;
        };
        if !policy.generic_rules.is_empty() {
            return None;
        }
        for rule in &policy.rules {
            let mut roles: Option<BTreeSet<String>> = None;
            for condition in &rule.all_of {
                if let AccessCondition::Role(role) = condition {
                    let next: BTreeSet<_> = role.any_of.iter().cloned().collect();
                    roles = Some(roles.map_or_else(
                        || next.clone(),
                        |prior| prior.intersection(&next).cloned().collect(),
                    ));
                } else {
                    concrete = false;
                }
            }
            let roles = roles?;
            concrete &= rule.resource_type == object.name;
            for action in &rule.actions {
                if let Some(granted) = grants.get_mut(action) {
                    granted.extend(roles.clone());
                }
            }
        }
    }
    for action in actions {
        concrete &= action.effect_program.is_none()
            && action.writes.is_empty()
            && action.preconditions.is_empty()
            && action.slice.is_empty()
            && action.derive.is_empty()
            && action.commands.is_empty()
            && action.quantity_effect.is_none()
            && action.create_links.is_empty()
            && empty_schema(&action.input_schema)
            && empty_schema(&action.output_schema);
    }
    Some((
        DutyGraph {
            states: vec!["existing_object".into()],
            initial: BTreeSet::from([0]),
            edges: actions
                .iter()
                .map(|a| DutyEdge {
                    action: a.action_id.clone(),
                    from: 0,
                    to: 0,
                })
                .collect(),
            grants,
        },
        concrete,
    ))
}

fn empty_schema(schema: &norn_ir::rule::JsonObject) -> bool {
    serde_json::to_value(schema).ok()
        == Some(serde_json::json!({
            "type": "object", "properties": {}, "required": [], "additionalProperties": false
        }))
}

fn evaluate_path(
    graph: &DutyGraph,
    path: &[String],
    concrete: bool,
) -> (ControlStatus, Reachability, Vec<String>) {
    let check = check_duties(graph, path, 100_000);
    let (verdict, reachability) = crate::controls::evidence(&check, concrete);
    let trace = match check {
        crate::duties::DutyCheck::Counterexample { trace, .. } => trace
            .into_iter()
            .map(|step| format!("{}:{}", step.role, step.edge.action))
            .collect(),
        crate::duties::DutyCheck::Separated { .. }
        | crate::duties::DutyCheck::Unreachable
        | crate::duties::DutyCheck::Invalid
        | crate::duties::DutyCheck::Exhausted => Vec::new(),
    };
    (verdict, reachability, trace)
}
