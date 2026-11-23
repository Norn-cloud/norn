use std::collections::{BTreeMap, BTreeSet};

use norn_checker::parameter::{
    ParameterCandidate, ParameterContext, ParameterError, resolve_parameter,
};
use norn_ir::{
    DecimalLiteral, IrVersion01, RuntimeValue, ScopeRef, SettingReview, SettingReviewState,
    SettingRoot, SettingValue, SliceDeclaration, SliceKey,
};

use crate::cell_host::{FactKey, FactVersion, JsonObject};
use crate::decision_commit::FactSlice;

use super::types::{ActionCall, ActionParameterPin, ActionRefusal};

struct ProjectedSetting<'a> {
    key: FactKey,
    version: FactVersion,
    values: &'a JsonObject,
    setting: SettingRoot,
    scope: ScopeRef,
}

pub(super) struct ParameterSliceInput<'a> {
    pub(super) call: &'a ActionCall<'a>,
    pub(super) effective_input: &'a JsonObject,
    pub(super) facts: &'a FactSlice,
    pub(super) entry_keys: &'a BTreeMap<String, Option<FactKey>>,
    pub(super) entry_list_keys: &'a BTreeMap<String, Vec<FactKey>>,
}

pub(super) struct ResolvedParameter {
    pub(super) key: FactKey,
    pub(super) values: JsonObject,
    pub(super) pin: ActionParameterPin,
}

pub(super) fn resolve_parameter_slice<X>(
    input: &ParameterSliceInput<'_>,
    entry: &SliceDeclaration,
) -> Result<ResolvedParameter, ActionRefusal<X>> {
    let SliceKey::Parameter {
        from,
        setting_key,
        effective_at,
        precedence,
        default_from,
    } = &entry.key
    else {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry.name.clone(),
            reason: "expected a parameter slice key".to_owned(),
        });
    };
    let refuse = |error: ParameterError, reason: String| ActionRefusal::ParameterRefused {
        entry: entry.name.clone(),
        error,
        reason,
    };
    if setting_key.trim().is_empty() {
        return Err(refuse(
            ParameterError::Invalid,
            "parameter settingKey must be non-empty".to_owned(),
        ));
    }
    if precedence.is_empty() {
        return Err(refuse(
            ParameterError::Invalid,
            "parameter precedence must declare at least one scope".to_owned(),
        ));
    }
    let Some(candidate_keys) = input.entry_list_keys.get(from) else {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry.name.clone(),
            reason: format!("parameter source entry {from:?} is not an earlier link-list slice"),
        });
    };
    let effective_at_value = effective_at_input(input.effective_input, effective_at.as_str())
        .map_err(|reason| refuse(ParameterError::Invalid, reason))?;
    let caller_scope = call_scope_ref(&input.call.scope).map_err(|reason| {
        refuse(
            ParameterError::Invalid,
            format!("invalid call scope: {reason}"),
        )
    })?;
    let (projected_candidates, projected_default, max_revision) =
        project_slice_candidates(input, candidate_keys, default_from.as_deref(), &entry.name)?;
    let selected = select_winning_setting(
        &projected_candidates,
        projected_default.as_ref(),
        &ParameterContext {
            key: setting_key.as_str(),
            scope: &caller_scope,
            precedence,
            effective_at: effective_at_value,
            revision: max_revision,
        },
        &entry.name,
    )?;
    let digest = parameter_pin_digest(selected.values).map_err(|reason| {
        refuse(
            ParameterError::Invalid,
            format!("selected setting could not be canonically digested: {reason}"),
        )
    })?;
    Ok(ResolvedParameter {
        key: selected.key.clone(),
        values: selected.values.clone(),
        pin: ActionParameterPin {
            entry: entry.name.clone(),
            setting_key: setting_key.clone(),
            effective_at: effective_at_value.to_owned(),
            setting: selected.key.clone(),
            version: selected.version.0,
            digest,
        },
    })
}

fn effective_at_input<'a>(effective_input: &'a JsonObject, field: &str) -> Result<&'a str, String> {
    match effective_input.get(field) {
        Some(RuntimeValue::String(timestamp)) if !timestamp.trim().is_empty() => {
            Ok(timestamp.as_str())
        }
        _ => Err(format!(
            "parameter effectiveAt input {field:?} must be a non-empty RFC 3339 timestamp"
        )),
    }
}

type ProjectedSlice<'a> = (Vec<ProjectedSetting<'a>>, Option<ProjectedSetting<'a>>, u64);

fn project_slice_candidates<'a, X>(
    input: &ParameterSliceInput<'a>,
    candidate_keys: &[FactKey],
    default_from: Option<&str>,
    entry_name: &str,
) -> Result<ProjectedSlice<'a>, ActionRefusal<X>> {
    let mut projected_candidates = Vec::with_capacity(candidate_keys.len());
    let mut max_revision = 0_u64;
    for key in candidate_keys {
        let projected = load_projected_setting(input.facts, key, entry_name)?;
        max_revision = max_revision.max(projected.version.0);
        projected_candidates.push(projected);
    }
    let projected_default = match default_from {
        None => None,
        Some(default_name) => {
            let Some(maybe_key) = input.entry_keys.get(default_name) else {
                return Err(ActionRefusal::SliceUnavailable {
                    entry: entry_name.to_owned(),
                    reason: format!(
                        "parameter defaultFrom {default_name:?} is not an earlier single-object slice"
                    ),
                });
            };
            match maybe_key {
                None => None,
                Some(default_key) => {
                    let projected = load_projected_setting(input.facts, default_key, entry_name)?;
                    max_revision = max_revision.max(projected.version.0);
                    Some(projected)
                }
            }
        }
    };
    Ok((projected_candidates, projected_default, max_revision))
}

fn select_winning_setting<'a, X>(
    projected_candidates: &'a [ProjectedSetting<'a>],
    projected_default: Option<&'a ProjectedSetting<'a>>,
    context: &ParameterContext<'_>,
    entry_name: &str,
) -> Result<&'a ProjectedSetting<'a>, ActionRefusal<X>> {
    let candidates: Vec<ParameterCandidate<'_>> = projected_candidates
        .iter()
        .map(|candidate| ParameterCandidate {
            setting: &candidate.setting,
            scope: &candidate.scope,
            revision: candidate.version.0,
        })
        .collect();
    let default_candidate = projected_default.map(|candidate| ParameterCandidate {
        setting: &candidate.setting,
        scope: &candidate.scope,
        revision: candidate.version.0,
    });
    let winner =
        resolve_parameter(&candidates, context, default_candidate.as_ref()).map_err(|error| {
            let reason = match error {
                ParameterError::Ambiguous => format!(
                    "multiple active settings matched {:?} at equal precedence",
                    context.key
                ),
                ParameterError::Missing => format!(
                    "no active setting matched {:?} at {:?}",
                    context.key, context.effective_at
                ),
                ParameterError::Invalid => format!(
                    "invalid temporal window or scope context for {:?}",
                    context.key
                ),
            };
            ActionRefusal::ParameterRefused {
                entry: entry_name.to_owned(),
                error,
                reason,
            }
        })?;
    let selected = if default_candidate
        .as_ref()
        .is_some_and(|default| std::ptr::eq(winner, std::ptr::from_ref(default)))
    {
        projected_default
    } else {
        candidates
            .iter()
            .position(|candidate| std::ptr::eq(winner, std::ptr::from_ref(candidate)))
            .and_then(|index| projected_candidates.get(index))
    };
    selected.ok_or_else(|| ActionRefusal::SliceUnavailable {
        entry: entry_name.to_owned(),
        reason: "selected parameter candidate could not be traced back to the loaded slice"
            .to_owned(),
    })
}

fn load_projected_setting<'a, X>(
    facts: &'a FactSlice,
    key: &FactKey,
    entry_name: &str,
) -> Result<ProjectedSetting<'a>, ActionRefusal<X>> {
    let Some(state) = facts.get(key) else {
        return Err(ActionRefusal::SliceUnavailable {
            entry: entry_name.to_owned(),
            reason: format!("parameter candidate {key} is absent from the captured slice"),
        });
    };
    let Some(values) = state.values.as_ref() else {
        return Err(ActionRefusal::SliceObjectMissing {
            entry: entry_name.to_owned(),
        });
    };
    let (setting, scope) =
        project_setting_fact(values).map_err(|reason| ActionRefusal::ParameterRefused {
            entry: entry_name.to_owned(),
            error: ParameterError::Invalid,
            reason: format!("setting fact {key} is malformed: {reason}"),
        })?;
    Ok(ProjectedSetting {
        key: key.clone(),
        version: state.version,
        values,
        setting,
        scope,
    })
}

fn call_scope_ref(scope: &BTreeMap<String, String>) -> Result<ScopeRef, &'static str> {
    let mut out = ScopeRef::default();
    for (key, value) in scope {
        if value.trim().is_empty() {
            return Err("scope dimension value must not be blank");
        }
        match key.as_str() {
            "orgId" => out.org_id = Some(value.clone()),
            "tenantId" => out.tenant_id = Some(value.clone()),
            "workspaceId" => out.workspace_id = Some(value.clone()),
            "environmentId" => out.environment_id = Some(value.clone()),
            "businessUnitId" => out.business_unit_id = Some(value.clone()),
            _ => {}
        }
    }
    Ok(out)
}

fn project_setting_fact(values: &JsonObject) -> Result<(SettingRoot, ScopeRef), &'static str> {
    let setting_key = required_non_blank_string(values, "setting_key")?;
    let decimal_literal = match values.get("value") {
        Some(RuntimeValue::Number(decimal)) => {
            let text = decimal.canonical();
            SettingValue::decimal(&text).map_err(|_error| "inexact numeric setting value")?;
            DecimalLiteral(text)
        }
        _ => return Err("value must be an exact decimal"),
    };
    let effective_from = required_non_blank_string(values, "effective_from")?;
    let effective_to = match values.get("effective_to") {
        None | Some(RuntimeValue::Null) => None,
        Some(RuntimeValue::String(text)) if !text.trim().is_empty() => Some(text.clone()),
        Some(_) => return Err("effective_to must be null or a non-empty timestamp string"),
    };
    let review_state = match values.get("review_state") {
        Some(RuntimeValue::String(state)) => parse_review_state(state)?,
        _ => return Err("review_state must be a valid setting review state string"),
    };
    let scope = ScopeRef {
        org_id: present_nullable_scope(values, "org_id")?,
        tenant_id: present_nullable_scope(values, "tenant_id")?,
        workspace_id: present_nullable_scope(values, "workspace_id")?,
        environment_id: present_nullable_scope(values, "environment_id")?,
        business_unit_id: present_nullable_scope(values, "business_unit_id")?,
    };
    let scope_required = match values.get("scope_required") {
        None | Some(RuntimeValue::Null) => Vec::new(),
        Some(RuntimeValue::Array(items)) => {
            let mut required = Vec::with_capacity(items.len());
            let mut seen = BTreeSet::new();
            for item in items {
                let RuntimeValue::String(dim) = item else {
                    return Err("scope_required entries must be strings");
                };
                if dim.trim().is_empty() || !seen.insert(dim.as_str()) {
                    return Err("scope_required entries must be distinct non-empty strings");
                }
                required.push(dim.clone());
            }
            required
        }
        Some(_) => return Err("scope_required must be an array of scope dimension strings"),
    };
    let setting = SettingRoot {
        setting_version: IrVersion01::V0_1,
        setting_key,
        value: SettingValue::Number(decimal_literal),
        effective_from,
        effective_to,
        supersedes: None,
        review: SettingReview {
            state: review_state,
            reviewed_by: None,
            reviewed_at: None,
            reason: None,
            additional: BTreeMap::new(),
        },
        scope_required,
        extensions: None,
    };
    Ok((setting, scope))
}

fn required_non_blank_string(values: &JsonObject, field: &str) -> Result<String, &'static str> {
    match values.get(field) {
        Some(RuntimeValue::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        _ => Err("required string field is missing or blank"),
    }
}

fn present_nullable_scope(
    values: &JsonObject,
    field: &str,
) -> Result<Option<String>, &'static str> {
    match values.get(field) {
        Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::String(text)) if !text.trim().is_empty() => Ok(Some(text.clone())),
        _ => Err("scope field must be present as null or a non-empty string"),
    }
}

fn parse_review_state(value: &str) -> Result<SettingReviewState, &'static str> {
    match value {
        "draft" => Ok(SettingReviewState::Draft),
        "proposed" => Ok(SettingReviewState::Proposed),
        "approved" => Ok(SettingReviewState::Approved),
        "active" => Ok(SettingReviewState::Active),
        "rejected" => Ok(SettingReviewState::Rejected),
        "superseded" => Ok(SettingReviewState::Superseded),
        _ => Err("unknown setting review_state"),
    }
}

pub(super) fn parameter_pin_digest(values: &JsonObject) -> Result<String, String> {
    let hex = norn_ir::canonical::domain_hash_hex(b"norn.parameter-pin.v1\0", values)
        .map_err(|error| error.to_string())?;
    Ok(format!("sha256:{hex}"))
}

pub(super) fn validate_parameter_pin(pin: &ActionParameterPin) -> bool {
    !pin.entry.trim().is_empty()
        && !pin.setting_key.trim().is_empty()
        && !pin.effective_at.trim().is_empty()
        && !pin.setting.object_type.as_str().trim().is_empty()
        && !pin.setting.key.trim().is_empty()
        && pin.version > 0
        && pin.digest.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}
