//! Scoped parameter selection; values are borrowed without numeric conversion.

use crate::artifact_set::scope_conflict;
pub use crate::parameter_window::{ParameterError, ParameterWindow, unique_active};
use norn_expr::temporal::timestamp_ordering_key;
use norn_ir::{ScopeRef, SettingReviewState, SettingRoot};

/// One setting in the caller's recorded snapshot; identity/pins stay with its owner.
#[derive(Debug)]
pub struct ParameterCandidate<'a> {
    /// Existing typed setting, including its exact value and review.
    pub setting: &'a SettingRoot,
    /// Explicit declaration scope.
    pub scope: &'a ScopeRef,
    /// Committed revision at which this version was recorded.
    pub revision: u64,
}

/// Explicit temporal and scope basis; authorization is checked by the caller.
pub struct ParameterContext<'a> {
    /// Stable parameter key.
    pub key: &'a str,
    /// Authorized caller scope, including required dimensions.
    pub scope: &'a ScopeRef,
    /// Highest precedence first; no inferred scope ranking.
    pub precedence: &'a [ScopeRef],
    /// Declared RFC 3339 effective instant.
    pub effective_at: &'a str,
    /// Recorded snapshot frontier.
    pub revision: u64,
}

fn window(
    candidate: &ParameterCandidate<'_>,
    context: &ParameterContext<'_>,
) -> Result<ParameterWindow, ParameterError> {
    for required in &candidate.setting.scope_required {
        let value = match required.as_str() {
            "orgId" => &context.scope.org_id,
            "tenantId" => &context.scope.tenant_id,
            "workspaceId" => &context.scope.workspace_id,
            "environmentId" => &context.scope.environment_id,
            "businessUnitId" => &context.scope.business_unit_id,
            _ => return Err(ParameterError::Invalid),
        };
        if value.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err(ParameterError::Invalid);
        }
    }
    let parse = |s: &str| timestamp_ordering_key(s).map_err(|_error| ParameterError::Invalid);
    Ok(ParameterWindow {
        from: parse(&candidate.setting.effective_from)?,
        to: candidate
            .setting
            .effective_to
            .as_deref()
            .map(parse)
            .transpose()?,
        revision: candidate.revision,
    })
}

fn eligible(candidate: &ParameterCandidate<'_>, context: &ParameterContext<'_>) -> bool {
    candidate.revision <= context.revision
        && candidate.setting.setting_key == context.key
        && matches!(
            candidate.setting.review.state,
            SettingReviewState::Approved | SettingReviewState::Active
        )
}

/// PARAM-BOUND-1: fall through empty scopes; use only an explicit active default.
/// PARAM-EXACT-1: any equal-precedence active overlap refuses, including shadowed scopes.
///
/// # Errors
/// Refuses ambiguity, invalid/inaccessible context, or a missing value without a default.
pub fn resolve_parameter<'a>(
    candidates: &'a [ParameterCandidate<'a>],
    context: &ParameterContext<'_>,
    default: Option<&'a ParameterCandidate<'a>>,
) -> Result<&'a ParameterCandidate<'a>, ParameterError> {
    let at =
        timestamp_ordering_key(context.effective_at).map_err(|_error| ParameterError::Invalid)?;
    let mut winner = None;
    for (rank, scope) in context.precedence.iter().enumerate() {
        if scope_conflict(context.scope, scope).is_some()
            || context
                .precedence
                .get(..rank)
                .is_some_and(|seen| seen.contains(scope))
        {
            return Err(ParameterError::Invalid);
        }
        let group: Vec<_> = candidates
            .iter()
            .filter(|c| c.scope == scope && eligible(c, context))
            .collect();
        let windows = group
            .iter()
            .map(|c| window(c, context))
            .collect::<Result<Vec<_>, _>>()?;
        let selected = unique_active(&windows, at, context.revision)?;
        if winner.is_none() {
            winner = selected.and_then(|index| group.get(index).copied());
        }
    }
    if let Some(winner) = winner {
        return Ok(winner);
    }
    if let Some(default) = default.filter(|c| eligible(c, context)) {
        if scope_conflict(context.scope, default.scope).is_some() {
            return Err(ParameterError::Invalid);
        }
        if unique_active(&[window(default, context)?], at, context.revision)?.is_some() {
            return Ok(default);
        }
    }
    Err(ParameterError::Missing)
}
