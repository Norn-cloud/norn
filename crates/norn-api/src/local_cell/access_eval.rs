//! Fail-closed `AccessIR` policy snapshot and rule evaluator for [`super::LocalCell`].
//!
//! Never drops or ignores any `AccessRule` constraint: enforces `resource_type`,
//! mandatory and optional `scope` dimensions, `role` predicates, and literal
//! `resource` property predicates against kernel target values, while failing
//! closed on every predicate kind that `LocalCell` cannot verify (`rel`,
//! `context`, `marking`, `entitlement`, and `principalAttribute` operands).

use std::collections::BTreeMap;

use norn_ir::access::{
    AccessCondition, AccessLiteral, AccessRule, PolicyRoot, ResourceCompareOp, ResourceOperand,
    ScopeDimension,
};
use norn_ir::{ActionRoot, ArtifactKind, RuntimeObject, RuntimeValue};

use crate::kernel_set;
use crate::refusal::{Refusal, codes};
use crate::scope::CellScope;

use super::policy::LocalActor;

/// Evaluation stage distinguishing pre-storage host checks from kernel target callbacks.
pub(super) enum EvalStage<'a> {
    PreStorage {
        scope: &'a CellScope,
    },
    Kernel {
        cell_scope: &'a CellScope,
        request_scope: &'a BTreeMap<String, String>,
        target_type: &'a str,
        target_values: Option<&'a RuntimeObject>,
    },
}

/// Admitted `AccessIR` permit rules retained for governed actions in the active revision.
#[derive(Debug, Default)]
pub(super) struct PolicySnapshot {
    /// When `true`, policy refresh failed after a revision change and all actions fail closed.
    fail_closed: bool,
    /// For each action that declares an `access` policy, every `AccessRule` in
    /// the referenced `AccessIR` policy matching that action id. An empty `Vec`
    /// means the referenced policy was missing, non-`AccessIR`, or had no
    /// matching rule (fail closed).
    governed: BTreeMap<String, Vec<AccessRule>>,
}

impl PolicySnapshot {
    pub(super) fn fail_closed() -> Self {
        Self {
            fail_closed: true,
            governed: BTreeMap::new(),
        }
    }

    pub(super) fn from_canonical_set(canonical_set: &str) -> Result<Self, Refusal> {
        let artifacts = kernel_set::retained_artifacts(canonical_set).map_err(Refusal::host)?;
        let mut policies: BTreeMap<&str, Vec<AccessRule>> = BTreeMap::new();
        for artifact in &artifacts {
            if artifact.kind() != ArtifactKind::Policy {
                continue;
            }
            let decoded = serde_json::to_value(artifact.payload.to_json_value())
                .and_then(serde_json::from_value::<PolicyRoot>);
            if let Ok(PolicyRoot::Access(access)) = decoded {
                policies.insert(artifact.artifact_id.as_str(), access.rules);
            }
        }
        let mut governed = BTreeMap::new();
        for artifact in &artifacts {
            if artifact.kind() != ArtifactKind::Action {
                continue;
            }
            let action = serde_json::to_value(artifact.payload.to_json_value())
                .and_then(serde_json::from_value::<ActionRoot>)
                .map_err(Refusal::host)?;
            let Some(access_ref) = action.access else {
                continue;
            };
            let rules = policies.get(access_ref.artifact_id.as_str()).map_or_else(
                Vec::new,
                |policy_rules| {
                    policy_rules
                        .iter()
                        .filter(|rule| rule.actions.iter().any(|id| id == &action.action_id))
                        .cloned()
                        .collect()
                },
            );
            governed.insert(action.action_id, rules);
        }
        Ok(Self {
            fail_closed: false,
            governed,
        })
    }

    pub(super) fn authorize_action(
        &self,
        actor: &LocalActor,
        action_id: &str,
        stage: &EvalStage<'_>,
    ) -> Result<(), String> {
        if self.fail_closed {
            return Err(format!(
                "{}: local policy snapshot is fail-closed",
                codes::AUTHZ_DENIED
            ));
        }
        if !actor.is_valid() {
            return Err(codes::AUTHZ_DENIED.to_owned());
        }
        if let Some(rules) = self.governed.get(action_id) {
            if rules.is_empty() {
                return Err(format!(
                    "{}: no matching AccessIR permit rule for `{action_id}`",
                    codes::AUTHZ_DENIED
                ));
            }
            let mut last_reason = "no matching AccessIR rule satisfied";
            for rule in rules {
                match eval_rule(rule, actor, stage) {
                    Ok(()) => return Ok(()),
                    Err(reason) => last_reason = reason,
                }
            }
            return Err(format!("{}: {last_reason}", codes::AUTHZ_DENIED));
        }
        if let EvalStage::Kernel {
            cell_scope,
            request_scope,
            ..
        } = stage
        {
            kernel_scope_matches_cell(cell_scope, request_scope)
                .map_err(|reason| format!("{}: {reason}", codes::AUTHZ_DENIED))?;
        }
        if actor.has_any(&["member", "admin", "owner"]) {
            Ok(())
        } else {
            Err(codes::AUTHZ_DENIED.to_owned())
        }
    }
}

fn require_local_cell_dimensions(dimensions: &[ScopeDimension]) -> Result<(), &'static str> {
    for &dim in dimensions {
        match dim {
            ScopeDimension::Org | ScopeDimension::Environment => {}
            ScopeDimension::Tenant => {
                return Err("scope dimension `tenant` is not present in local cell scope");
            }
            ScopeDimension::Workspace => {
                return Err("scope dimension `workspace` is not present in local cell scope");
            }
            ScopeDimension::BusinessUnit => {
                return Err("scope dimension `businessUnit` is not present in local cell scope");
            }
        }
    }
    Ok(())
}

pub(super) fn kernel_scope_matches_cell(
    cell_scope: &CellScope,
    request_scope: &BTreeMap<String, String>,
) -> Result<(), &'static str> {
    if request_scope.keys().any(|key| {
        !matches!(
            key.as_str(),
            "orgId" | "tenantId" | "workspaceId" | "environmentId" | "businessUnitId"
        )
    }) {
        return Err("unsupported scope dimension in action request");
    }
    if cell_scope.org_id.trim().is_empty() || cell_scope.environment_id.trim().is_empty() {
        return Err("cell scope is incomplete");
    }
    if request_scope.get("orgId").map(String::as_str) != Some(cell_scope.org_id.as_str()) {
        return Err("request orgId does not match cell scope");
    }
    if request_scope.get("environmentId").map(String::as_str)
        != Some(cell_scope.environment_id.as_str())
    {
        return Err("request environmentId does not match cell scope");
    }
    Ok(())
}

fn scope_satisfies(
    dimensions: &[ScopeDimension],
    stage: &EvalStage<'_>,
) -> Result<(), &'static str> {
    if !dimensions.contains(&ScopeDimension::Org) {
        return Err("policy rule missing mandatory org scope dimension");
    }
    match stage {
        EvalStage::PreStorage { scope } => {
            if scope.org_id.trim().is_empty() || scope.environment_id.trim().is_empty() {
                return Err("request scope is incomplete");
            }
            require_local_cell_dimensions(dimensions)
        }
        EvalStage::Kernel {
            cell_scope,
            request_scope,
            ..
        } => {
            kernel_scope_matches_cell(cell_scope, request_scope)?;
            require_local_cell_dimensions(dimensions)
        }
    }
}

fn eval_resource_literal(
    actual: &RuntimeValue,
    op: ResourceCompareOp,
    expected: &AccessLiteral,
) -> Result<(), &'static str> {
    let matched = match expected {
        AccessLiteral::Bool(exp) => {
            let Some(act) = actual.as_bool() else {
                return Err("resource property type mismatch: expected boolean");
            };
            match op {
                ResourceCompareOp::Eq => act == *exp,
                ResourceCompareOp::Ne => act != *exp,
                ResourceCompareOp::Lt
                | ResourceCompareOp::Le
                | ResourceCompareOp::Gt
                | ResourceCompareOp::Ge
                | ResourceCompareOp::In => {
                    return Err("unsupported comparison operator for boolean resource property");
                }
            }
        }
        AccessLiteral::Long(exp) => {
            let Some(act) = actual.as_i64() else {
                return Err("resource property type mismatch: expected integer");
            };
            match op {
                ResourceCompareOp::Eq => act == *exp,
                ResourceCompareOp::Ne => act != *exp,
                ResourceCompareOp::Lt => act < *exp,
                ResourceCompareOp::Le => act <= *exp,
                ResourceCompareOp::Gt => act > *exp,
                ResourceCompareOp::Ge => act >= *exp,
                ResourceCompareOp::In => {
                    return Err("`in` resource operator requires principalAttribute set");
                }
            }
        }
        AccessLiteral::String(exp) => {
            let Some(act) = actual.as_str() else {
                return Err("resource property type mismatch: expected string");
            };
            match op {
                ResourceCompareOp::Eq => act == exp.as_str(),
                ResourceCompareOp::Ne => act != exp.as_str(),
                ResourceCompareOp::Lt
                | ResourceCompareOp::Le
                | ResourceCompareOp::Gt
                | ResourceCompareOp::Ge
                | ResourceCompareOp::In => {
                    return Err("unsupported comparison operator for string resource property");
                }
            }
        }
    };
    if matched {
        Ok(())
    } else {
        Err("resource predicate not satisfied")
    }
}

fn eval_condition(
    cond: &AccessCondition,
    actor: &LocalActor,
    stage: &EvalStage<'_>,
) -> Result<(), &'static str> {
    match cond {
        AccessCondition::Role(role) => {
            if !role.any_of.is_empty() && actor.has_any_owned(&role.any_of) {
                Ok(())
            } else {
                Err("role predicate not satisfied")
            }
        }
        AccessCondition::Resource(res) => match &res.value {
            ResourceOperand::PrincipalAttribute(_) => {
                Err("principalAttribute resource predicate cannot be verified in local cell")
            }
            ResourceOperand::Literal(lit) => {
                if res.op == ResourceCompareOp::In {
                    return Err("`in` resource operator requires principalAttribute set");
                }
                match stage {
                    EvalStage::PreStorage { .. } => Ok(()),
                    EvalStage::Kernel { target_values, .. } => {
                        let Some(values) = target_values else {
                            return Err("resource predicate requires target values");
                        };
                        let Some(actual) = values.get(&res.name) else {
                            return Err("resource property missing on target");
                        };
                        eval_resource_literal(actual, res.op, lit)
                    }
                }
            }
        },
        AccessCondition::Rel(_) => Err("relation predicate cannot be verified in local cell"),
        AccessCondition::Context(_) => Err("context predicate cannot be verified in local cell"),
        AccessCondition::Marking(_) => Err("marking predicate cannot be verified in local cell"),
        AccessCondition::Entitlement(_) => {
            Err("entitlement predicate cannot be verified in local cell")
        }
    }
}

fn eval_rule(
    rule: &AccessRule,
    actor: &LocalActor,
    stage: &EvalStage<'_>,
) -> Result<(), &'static str> {
    scope_satisfies(&rule.scope.dimensions, stage)?;
    if let EvalStage::Kernel { target_type, .. } = stage
        && rule.resource_type.as_str() != *target_type
    {
        return Err("policy rule resourceType does not match target");
    }
    for cond in &rule.all_of {
        eval_condition(cond, actor, stage)?;
    }
    Ok(())
}
