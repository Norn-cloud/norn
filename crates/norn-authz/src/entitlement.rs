//! Trusted matching for Core entitlement rows.
//!
//! Source rows are data, not policy. The matcher validates and compares each
//! row against one trusted principal/resource/scope snapshot, then returns only
//! the closed relationship kind that generic `AccessIR` rules understand.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::entitlement::{
    EntitlementCondition, EntitlementGrantee, EntitlementStatus, ObjectRef,
};

use crate::pdp::ConjunctOutcome;
use crate::request::{AccessRequest, AttrValue, EntitlementInput, PrincipalInput, ScopeKey};

/// Stable validation error for an imported entitlement row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntitlementValidationError {
    /// A required identity or provenance field is empty or wildcarded.
    ValueInvalid,
    /// A Norn object type or action id has invalid shape.
    IdentifierInvalid,
    /// The grantee reference does not agree with its closed kind.
    GranteeTypeInvalid,
    /// The action list is empty or duplicated.
    ActionsInvalid,
    /// An entitlement scope attribute is not an exact scalar binding.
    ScopeAttributeInvalid,
    /// The entitlement's physical scope is empty or wildcarded.
    ScopeInvalid,
    /// A relationship link id is invalid.
    LinkInvalid,
}

impl EntitlementValidationError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ValueInvalid => "authz.entitlement.value_invalid",
            Self::IdentifierInvalid => "authz.entitlement.identifier_invalid",
            Self::GranteeTypeInvalid => "authz.entitlement.grantee_type_invalid",
            Self::ActionsInvalid => "authz.entitlement.actions_invalid",
            Self::ScopeAttributeInvalid => "authz.entitlement.scope_attribute_invalid",
            Self::ScopeInvalid => "authz.entitlement.scope_invalid",
            Self::LinkInvalid => "authz.entitlement.link_invalid",
        }
    }
}

/// Validate the canonical row and its physical scope before admitting it to a
/// trusted entitlement snapshot.
///
/// # Errors
///
/// Returns a stable validation error for malformed identities, provenance,
/// actions, scope attributes, physical scope, or relationship references.
pub fn validate_entitlement(input: &EntitlementInput) -> Result<(), EntitlementValidationError> {
    let row = &input.entitlement;
    if !evidence_value(&row.id)
        || !evidence_value(&row.source.system)
        || !evidence_value(&row.source.record_id)
        || !evidence_value(&row.source.version)
    {
        return Err(EntitlementValidationError::ValueInvalid);
    }
    if !input.scope.is_exact() {
        return Err(EntitlementValidationError::ScopeInvalid);
    }
    if !valid_object_type(row.target_type.as_str()) {
        return Err(EntitlementValidationError::IdentifierInvalid);
    }
    if !valid_object_type(row.grantee.object_type().as_str()) || !exact(row.grantee.key()) {
        return Err(EntitlementValidationError::IdentifierInvalid);
    }
    match &row.grantee {
        EntitlementGrantee::Role { object_type, .. } if object_type.as_str() != "identity.Role" => {
            return Err(EntitlementValidationError::GranteeTypeInvalid);
        }
        EntitlementGrantee::Group { object_type, .. }
            if object_type.as_str() != "identity.Group" =>
        {
            return Err(EntitlementValidationError::GranteeTypeInvalid);
        }
        EntitlementGrantee::Role { .. }
        | EntitlementGrantee::Group { .. }
        | EntitlementGrantee::Principal { .. } => {}
    }
    let mut actions = BTreeSet::new();
    if row.actions.is_empty()
        || row
            .actions
            .iter()
            .any(|action| !valid_action_id(action) || !actions.insert(action))
    {
        return Err(EntitlementValidationError::ActionsInvalid);
    }
    for (name, value) in &row.scope_attributes {
        if !valid_member(name) || !exact(value) {
            return Err(EntitlementValidationError::ScopeAttributeInvalid);
        }
    }
    match &row.condition {
        EntitlementCondition::None => {}
        EntitlementCondition::OwnerOnly { link } => {
            if !valid_member(link.as_str()) {
                return Err(EntitlementValidationError::LinkInvalid);
            }
        }
        EntitlementCondition::LinkedRecord { link, target } => {
            if !valid_member(link.as_str()) {
                return Err(EntitlementValidationError::LinkInvalid);
            }
            if !valid_object_type(target.object_type.as_str()) || !exact(&target.key) {
                return Err(EntitlementValidationError::IdentifierInvalid);
            }
        }
    }
    Ok(())
}

/// Stable, bounded per-row outcome included by the explain path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementRowEvaluation {
    /// Canonical `core.Entitlement` key.
    pub id: String,
    /// Provenance system.
    pub source_system: String,
    /// Source record id.
    pub source_record_id: String,
    /// Source record version.
    pub source_version: String,
    /// Imported status.
    pub status: EntitlementStatus,
    /// Result after comparing the row with this request.
    pub outcome: EntitlementRowOutcome,
}

/// Why one relevant row did or did not match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntitlementRowOutcome {
    /// An enforced row matched every predicate and can contribute a grant.
    EnforcedMatch,
    /// A matching shadow row is visible in explain but cannot grant in production.
    ShadowMatch,
    /// A candidate row did not match one of the request predicates.
    Mismatch(EntitlementDenialReason),
    /// The row unexpectedly failed validation and was excluded defensively.
    Invalid(EntitlementValidationError),
}

/// Stable reason that a related entitlement row did not match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntitlementDenialReason {
    /// The row targets another Norn type.
    TargetTypeMismatch,
    /// The row grants another action.
    ActionMismatch,
    /// A target scope attribute is absent or has another exact value.
    ScopeAttributeMismatch,
    /// The owner link does not resolve to the acting principal's represented identity.
    OwnerMismatch,
    /// The configured link does not resolve to the row's exact linked object.
    LinkedRecordMismatch,
}

impl EntitlementDenialReason {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TargetTypeMismatch => "authz.entitlement.target_type_mismatch",
            Self::ActionMismatch => "authz.entitlement.action_mismatch",
            Self::ScopeAttributeMismatch => "authz.entitlement.scope_attribute_mismatch",
            Self::OwnerMismatch => "authz.entitlement.owner_mismatch",
            Self::LinkedRecordMismatch => "authz.entitlement.linked_record_mismatch",
        }
    }
}

/// Exact provenance for one row that matched in either mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementMatch {
    /// Canonical `core.Entitlement` key.
    pub id: String,
    /// Provenance system.
    pub source_system: String,
    /// Source record id.
    pub source_record_id: String,
    /// Source record version.
    pub source_version: String,
}

/// Entitlement-specific result attached to every PDP explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementEvaluation {
    /// Whether an enforced row matched; `notEvaluated` when an outer gate stops access.
    pub outcome: ConjunctOutcome,
    /// Enforced rows that matched all predicates.
    pub enforced_matches: Vec<EntitlementMatch>,
    /// Matching shadow rows; they never grant in production.
    pub shadow_matches: Vec<EntitlementMatch>,
    /// Bounded diagnostics for rows related to this principal and physical scope.
    pub rows: Vec<EntitlementRowEvaluation>,
    /// Relevant row diagnostics omitted by the fixed output bound.
    pub omitted_rows: usize,
    /// Final PDP result after promoting matching shadow rows. Absent when none
    /// match; outer capability, delegation, and request gates still apply.
    pub would_allow_if_enforced: Option<bool>,
}

impl EntitlementEvaluation {
    pub(crate) fn not_evaluated() -> Self {
        Self {
            outcome: ConjunctOutcome::NotEvaluated,
            enforced_matches: Vec::new(),
            shadow_matches: Vec::new(),
            rows: Vec::new(),
            omitted_rows: 0,
            would_allow_if_enforced: None,
        }
    }
}

pub(crate) const MAX_EXPLAINED_ENTITLEMENTS: usize = 64;
const MAX_EVIDENCE_VALUE_BYTES: usize = 1024;

pub(crate) struct EntitlementMatches {
    pub(crate) evaluation: EntitlementEvaluation,
    pub(crate) enforced_kinds: BTreeSet<&'static str>,
    pub(crate) all_kinds: BTreeSet<&'static str>,
    pub(crate) has_relevant_shadow: bool,
}

pub(crate) fn evaluate_entitlements(
    request: &AccessRequest,
    scope: &ScopeKey,
    inputs: &[EntitlementInput],
    explain_shadow: bool,
) -> EntitlementMatches {
    let mut rows = Vec::new();
    let mut enforced_matches = Vec::new();
    let mut shadow_matches = Vec::new();
    let mut enforced_kinds = BTreeSet::new();
    let mut all_kinds = BTreeSet::new();
    let mut has_relevant_shadow = false;
    let mut total_rows = 0_usize;

    let resource_type = request.resource.object_type.as_str();
    let ordered = inputs
        .iter()
        .filter(|input| input.entitlement.target_type.as_str() == resource_type)
        .chain(
            inputs
                .iter()
                .filter(|input| input.entitlement.target_type.as_str() != resource_type),
        );
    for input in ordered {
        let row = &input.entitlement;
        if row.status == EntitlementStatus::Shadow && !explain_shadow {
            continue;
        }
        if !grantee_is_candidate(&row.grantee, &request.principal) || &input.scope != scope {
            continue;
        }
        let outcome = match validate_entitlement(input) {
            Err(error) => EntitlementRowOutcome::Invalid(error),
            Ok(()) => match match_matches(request, input) {
                Err(reason) => EntitlementRowOutcome::Mismatch(reason),
                Ok(()) => match row.status {
                    EntitlementStatus::Enforced => {
                        let kind = condition_kind(&row.condition);
                        enforced_kinds.insert(kind);
                        all_kinds.insert(kind);
                        keep_bounded_match(&mut enforced_matches, match_evidence(input));
                        EntitlementRowOutcome::EnforcedMatch
                    }
                    EntitlementStatus::Shadow => {
                        has_relevant_shadow = true;
                        let kind = condition_kind(&row.condition);
                        all_kinds.insert(kind);
                        keep_bounded_match(&mut shadow_matches, match_evidence(input));
                        EntitlementRowOutcome::ShadowMatch
                    }
                },
            },
        };
        total_rows = total_rows.saturating_add(1);
        keep_bounded_row(
            &mut rows,
            row.target_type.as_str() == resource_type,
            EntitlementRowEvaluation {
                id: bounded_text(&row.id),
                source_system: bounded_text(&row.source.system),
                source_record_id: bounded_text(&row.source.record_id),
                source_version: bounded_text(&row.source.version),
                status: row.status,
                outcome,
            },
        );
    }

    let omitted_rows = total_rows.saturating_sub(rows.len());
    let outcome = if enforced_matches.is_empty() {
        ConjunctOutcome::Deny
    } else {
        ConjunctOutcome::Allow
    };
    EntitlementMatches {
        evaluation: EntitlementEvaluation {
            outcome,
            enforced_matches,
            shadow_matches,
            rows: rows.into_iter().map(|(_, row)| row).collect(),
            omitted_rows,
            would_allow_if_enforced: None,
        },
        enforced_kinds,
        all_kinds,
        has_relevant_shadow,
    }
}

fn match_order(left: &EntitlementMatch, right: &EntitlementMatch) -> std::cmp::Ordering {
    (
        &left.id,
        &left.source_system,
        &left.source_record_id,
        &left.source_version,
    )
        .cmp(&(
            &right.id,
            &right.source_system,
            &right.source_record_id,
            &right.source_version,
        ))
}

fn row_order(
    left: &EntitlementRowEvaluation,
    right: &EntitlementRowEvaluation,
) -> std::cmp::Ordering {
    (
        &left.id,
        &left.source_system,
        &left.source_record_id,
        &left.source_version,
    )
        .cmp(&(
            &right.id,
            &right.source_system,
            &right.source_record_id,
            &right.source_version,
        ))
        .then_with(|| status_key(left.status).cmp(status_key(right.status)))
        .then_with(|| outcome_key(&left.outcome).cmp(&outcome_key(&right.outcome)))
}

fn status_key(status: EntitlementStatus) -> &'static str {
    match status {
        EntitlementStatus::Enforced => "enforced",
        EntitlementStatus::Shadow => "shadow",
    }
}

fn outcome_key(outcome: &EntitlementRowOutcome) -> (&'static str, &'static str) {
    match outcome {
        EntitlementRowOutcome::EnforcedMatch => ("enforced_match", ""),
        EntitlementRowOutcome::ShadowMatch => ("shadow_match", ""),
        EntitlementRowOutcome::Mismatch(reason) => ("mismatch", reason.code()),
        EntitlementRowOutcome::Invalid(error) => ("invalid", error.code()),
    }
}

fn keep_bounded_row(
    rows: &mut Vec<(bool, EntitlementRowEvaluation)>,
    target_type_matches: bool,
    row: EntitlementRowEvaluation,
) {
    rows.push((target_type_matches, row));
    rows.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| row_order(&left.1, &right.1))
    });
    rows.truncate(MAX_EXPLAINED_ENTITLEMENTS);
}

fn keep_bounded_match(matches: &mut Vec<EntitlementMatch>, evidence: EntitlementMatch) {
    matches.push(evidence);
    matches.sort_by(match_order);
    matches.truncate(MAX_EXPLAINED_ENTITLEMENTS);
}

fn match_evidence(input: &EntitlementInput) -> EntitlementMatch {
    EntitlementMatch {
        id: bounded_text(&input.entitlement.id),
        source_system: bounded_text(&input.entitlement.source.system),
        source_record_id: bounded_text(&input.entitlement.source.record_id),
        source_version: bounded_text(&input.entitlement.source.version),
    }
}

fn match_matches(
    request: &AccessRequest,
    input: &EntitlementInput,
) -> Result<(), EntitlementDenialReason> {
    let row = &input.entitlement;
    if row.target_type.as_str() != request.resource.object_type.as_str() {
        return Err(EntitlementDenialReason::TargetTypeMismatch);
    }
    if !row
        .actions
        .iter()
        .any(|action| action == &request.action_id)
    {
        return Err(EntitlementDenialReason::ActionMismatch);
    }
    for (name, value) in &row.scope_attributes {
        if request.resource.attributes.get(name) != Some(&AttrValue::String(value.clone())) {
            return Err(EntitlementDenialReason::ScopeAttributeMismatch);
        }
    }
    match &row.condition {
        EntitlementCondition::None => Ok(()),
        EntitlementCondition::OwnerOnly { link } => {
            let Some(principal) = request
                .principal
                .attributes
                .get("represents")
                .and_then(object_ref)
            else {
                return Err(EntitlementDenialReason::OwnerMismatch);
            };
            if resource_link_matches(&request.resource.attributes, link.as_str(), &principal) {
                Ok(())
            } else {
                Err(EntitlementDenialReason::OwnerMismatch)
            }
        }
        EntitlementCondition::LinkedRecord { link, target } => {
            if resource_link_matches(&request.resource.attributes, link.as_str(), target) {
                Ok(())
            } else {
                Err(EntitlementDenialReason::LinkedRecordMismatch)
            }
        }
    }
}

fn resource_link_matches(
    attributes: &BTreeMap<String, AttrValue>,
    link: &str,
    expected: &ObjectRef,
) -> bool {
    match attributes.get(link) {
        Some(AttrValue::Record(_)) => {
            attributes.get(link).and_then(object_ref).as_ref() == Some(expected)
        }
        Some(AttrValue::Set(values)) => values
            .iter()
            .filter_map(object_ref)
            .any(|reference| &reference == expected),
        _ => false,
    }
}

fn object_ref(value: &AttrValue) -> Option<ObjectRef> {
    let AttrValue::Record(fields) = value else {
        return None;
    };
    let (Some(AttrValue::String(object_type)), Some(AttrValue::String(key))) =
        (fields.get("type"), fields.get("key"))
    else {
        return None;
    };
    (fields.len() == 2).then(|| ObjectRef {
        object_type: norn_ir::ObjectTypeName(object_type.clone()),
        key: key.clone(),
    })
}

fn grantee_is_candidate(grantee: &EntitlementGrantee, principal: &PrincipalInput) -> bool {
    match grantee {
        EntitlementGrantee::Role { key, .. } => {
            contains_string_set(principal.attributes.get("entitlement_roles"), key)
        }
        EntitlementGrantee::Group { key, .. } => {
            contains_string_set(principal.attributes.get("entitlement_groups"), key)
        }
        EntitlementGrantee::Principal { object_type, key } => principal
            .attributes
            .get("represents")
            .and_then(object_ref)
            .is_some_and(|represents| {
                &represents.object_type == object_type && represents.key.as_str() == key.as_str()
            }),
    }
}

fn contains_string_set(value: Option<&AttrValue>, expected: &str) -> bool {
    match value {
        Some(AttrValue::Set(values)) => values
            .iter()
            .any(|value| matches!(value, AttrValue::String(value) if value == expected)),
        _ => false,
    }
}

fn condition_kind(condition: &EntitlementCondition) -> &'static str {
    match condition {
        EntitlementCondition::None => "none",
        EntitlementCondition::OwnerOnly { .. } => "ownerOnly",
        EntitlementCondition::LinkedRecord { .. } => "linkedRecord",
    }
}

fn exact(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "*"
}

fn evidence_value(value: &str) -> bool {
    exact(value) && value.len() <= MAX_EVIDENCE_VALUE_BYTES
}

fn bounded_text(value: &str) -> String {
    if value.len() <= MAX_EVIDENCE_VALUE_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_EVIDENCE_VALUE_BYTES.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", value.get(..end).unwrap_or_default())
}

fn valid_object_type(value: &str) -> bool {
    let Some((namespace, local)) = value.rsplit_once('.') else {
        return false;
    };
    !namespace.is_empty()
        && namespace.split('.').all(valid_namespace)
        && local.chars().next().is_some_and(char::is_uppercase)
        && local
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

fn valid_namespace(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn valid_member(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn valid_action_id(value: &str) -> bool {
    exact(value)
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
        })
}
