//! PARAM-EXACT-1: project native numeric-setting facts without a float conversion.
//! This adapter accepts a captured live view; fact versions remain CAS identities.

use crate::{FactKey, FactState};
use norn_checker::parameter::{
    ParameterCandidate, ParameterContext, ParameterError, resolve_parameter,
};
use norn_ir::{
    DecimalLiteral, IrVersion01, RuntimeObject, RuntimeValue, ScopeRef, SettingReview,
    SettingReviewState, SettingRoot, SettingValue,
};

type Record = (FactKey, FactState);

fn text(values: &RuntimeObject, field: &str) -> Result<Option<String>, ParameterError> {
    match values.get(field) {
        Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        _ => Err(ParameterError::Invalid),
    }
}

fn project(record: &Record) -> Result<(SettingRoot, ScopeRef), ParameterError> {
    let values = record.1.values.as_ref().ok_or(ParameterError::Invalid)?;
    let required = |field| text(values, field)?.ok_or(ParameterError::Invalid);
    let Some(RuntimeValue::Number(value)) = values.get("value") else {
        return Err(ParameterError::Invalid);
    };
    let state = match required("reviewState")?.as_str() {
        "draft" => SettingReviewState::Draft,
        "proposed" => SettingReviewState::Proposed,
        "approved" => SettingReviewState::Approved,
        "active" => SettingReviewState::Active,
        "rejected" => SettingReviewState::Rejected,
        "superseded" => SettingReviewState::Superseded,
        _ => return Err(ParameterError::Invalid),
    };
    Ok((
        SettingRoot {
            setting_version: IrVersion01::V0_1,
            setting_key: required("settingKey")?,
            value: SettingValue::Number(DecimalLiteral(value.canonical())),
            effective_from: required("effectiveFrom")?,
            effective_to: text(values, "effectiveTo")?,
            supersedes: None,
            review: SettingReview {
                state,
                reviewed_by: None,
                reviewed_at: None,
                reason: None,
                additional: Default::default(),
            },
            scope_required: Vec::new(),
            extensions: None,
        },
        ScopeRef {
            org_id: text(values, "orgId")?,
            tenant_id: text(values, "tenantId")?,
            workspace_id: text(values, "workspaceId")?,
            environment_id: text(values, "environmentId")?,
            business_unit_id: text(values, "businessUnitId")?,
        },
    ))
}

/// Selects a setting from a captured, authorized live fact view.
/// The caller must fence every input fact and the complete source membership.
/// Historical queries use `resolve_parameter` with their own recorded revisions.
///
/// # Errors
/// Refuses non-current context, invalid native profiles, ambiguity or missing values.
pub fn resolve_current_parameter<'a>(
    records: &'a [Record],
    default: Option<&'a Record>,
    context: &ParameterContext<'_>,
) -> Result<&'a Record, ParameterError> {
    if context.revision != u64::MAX {
        return Err(ParameterError::Invalid);
    }
    let projected = records.iter().map(project).collect::<Result<Vec<_>, _>>()?;
    let candidate = |(setting, scope)| ParameterCandidate {
        setting,
        scope,
        revision: context.revision,
    };
    let candidates: Vec<_> = projected.iter().map(candidate).collect();
    let default_projection = default.map(project).transpose()?;
    let default_candidate = default_projection.as_ref().map(candidate);
    let selected = resolve_parameter(&candidates, context, default_candidate.as_ref())?;
    let index = candidates
        .iter()
        .position(|c| std::ptr::eq(c.setting, selected.setting));
    index
        .and_then(|index| records.get(index))
        .or(default)
        .ok_or(ParameterError::Missing)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "unit-test fixtures")]
mod tests {
    use super::*;

    fn record() -> Record {
        let values = RuntimeValue::from_json_text(r#"{"settingKey":"tax","value":0.1000000000000000000000000001,"effectiveFrom":"2026-01-01T00:00:00Z","effectiveTo":null,"reviewState":"approved","orgId":null,"tenantId":null,"workspaceId":null,"environmentId":null,"businessUnitId":null}"#).expect("exact").as_object().expect("object").clone();
        (
            FactKey::new("test.Parameter", "rate"),
            FactState {
                version: crate::FactVersion(7),
                values: Some(values),
            },
        )
    }

    #[test]
    fn native_parameter_projection_keeps_exact_values_and_rejects_incomplete_scope() {
        let record = record();
        let (setting, scope) = project(&record).expect("profile");
        assert_eq!(
            setting.value.runtime_value().expect("exact"),
            record
                .1
                .values
                .as_ref()
                .and_then(|v| v.get("value"))
                .expect("value")
                .clone()
        );
        let scopes = [scope.clone()];
        let context = ParameterContext {
            key: "tax",
            scope: &scope,
            precedence: &scopes,
            effective_at: "2026-06-30T00:00:00Z",
            revision: u64::MAX,
        };
        let records = [record];
        assert_eq!(
            resolve_current_parameter(&records, None, &context)
                .expect("selection")
                .0,
            records.first().expect("source").0
        );
        let mut invalid = records.first().expect("source").clone();
        invalid.1.values.as_mut().expect("values").remove("orgId");
        assert!(matches!(project(&invalid), Err(ParameterError::Invalid)));
    }
    proptest::proptest! {
        #[test]
        fn native_number_projection_is_exact(value in proptest::num::i64::ANY) {
            let mut source = record();
            let original = RuntimeValue::from(value);
            source.1.values.as_mut().expect("values").insert("value".into(), original.clone());
            let (setting, _) = project(&source).expect("numeric profile");
            proptest::prop_assert_eq!(setting.value.runtime_value().expect("exact"), original);
        }
    }
}
