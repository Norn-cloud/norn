//! Checks for the typed payload roots (T1-IR-1c #2508, T1-IR-1d #2509,
//! T1-IR-1e #2820).
//!
//! The envelope payload roots in `norn-ir` decode leniently, so a malformed
//! member is reported here as a path-based diagnostic rather than failing the
//! artifact decode. Members added by the typed roots are decoded into their
//! `norn-ir` types ([`norn_ir::ActionRoot`] members, [`norn_ir::JdmGraph`]);
//! the members that predate them keep their existing JSON-walking checks in
//! [`crate::envelope`] unchanged.
//!
//! # `RuleIR` v2 / Norn profile hook (T3-DEC-1)
//!
//! [`validate_rule_ir_v2_payload`] is the entry point for JDM decision graphs:
//! it checks the typed root members and runs the Norn-profile validator
//! ([`crate::jdm_profile`], T3-DEC-1) over the graph and the decision
//! contract.
//!
//! # Typed-root agreement (T1-IR-1d)
//!
//! `check_typed_root` decodes a payload that its kind validator accepted
//! into the strict typed root — Access ([`norn_ir::PolicyRoot`]), Widget,
//! Connector, Customization, View ([`norn_ir::ViewIrPayload`]), Setting
//! ([`norn_ir::SettingRoot`]), `BusinessPack`
//! ([`norn_ir::BusinessPackRoot`], composition and ontology forms), Entity
//! ([`norn_ir::EntityRootV1`] or [`norn_ir::EntityRootV2`], selected by
//! `entityVersion`), and Relation ([`norn_ir::RelationRoot`]) — and
//! reports any disagreement at the member or array element that fails. The
//! typed roots mirror the validators' accepted shape, so this never fires for
//! an accepted payload unless the two drift apart.
//!
//! # `EntityIR` v2 entity payloads (T1-IR-1b)
//!
//! An entity payload with `entityVersion` is the v2 form: exactly one
//! definition — an [`norn_ir::ObjectTypeDef`] under `objectType`, an
//! [`norn_ir::InterfaceDef`] under `interface`, an
//! [`norn_ir::ObjectTypeExtension`] under `objectTypeExtension`, a
//! [`norn_ir::PropertyTypeDef`] catalog under `propertyTypes`, or a marking
//! catalog under `markings` (#2844).
//! `validate_entity_ir_v2_payload` is its per-artifact check; the v1 form
//! keeps its validator unchanged. Cross-definition rules are the artifact-set
//! check ([`crate::check_artifact_set_entity_model`]).
//! `relation` has no v2 form (a v2 relation is a link on its source type), so
//! a relation payload with `entityVersion` is
//! [`codes::RELATION_V2_IS_LINK`].
//!
//! # Formerly generic members (T1-IR-2, #2430)
//!
//! Every payload-root member now has a real type. The members that were any
//! JSON until T1-IR-2 are checked by their kind validators under the kind's
//! existing `*.validation.invalid_payload` code: entity `overlaySources`
//! (customization ids), rule `sideEffects` (only `false`) and
//! `requiredServices` (only `[]`, in both rule forms), the obligations-form
//! policy `appliesTo`/`reviewRequiredWhen`/`approvalRoles` (their
//! approval-form types, optional), derived `scopeRequired` (scope
//! dimensions, optional), and ontology-mapping `ratifiedBy`/`ratifiedAt`
//! (optional off `exact_match`). A setting `value` is a
//! [`norn_ir::SettingValue`] tagged by `valueType`; its diagnostics are
//! unchanged. The envelope `kind` is the payload root's tag
//! ([`norn_ir::ArtifactEnvelope::kind`]), so [`codes::KIND_MISMATCH`] is
//! retired.
//!
//! # Retired kinds
//!
//! [`decode_artifact_json`] decodes an envelope from JSON and turns a retired
//! kind (`process`) or an unknown kind into a located diagnostic instead of a
//! bare serde error.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::env::INPUT_RECORD;
use norn_expr::{
    EnvOptions, Expression, InferredType, RootCostBounds, Type, ZenTypeEnv,
    check_expression_with_root_bounds,
};
use norn_ir::{
    ActionCommand, ActionCommandOperation, ActionInputDerivation, ActionRoot, ArtifactDependency,
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, Diagnostic, DiagnosticRemediation,
    DiagnosticSeverity, JsonValue, RULE_IR_V2_VERSION, ResourceLimitProfile, SliceDeclaration,
    SliceKey, ZenExpression,
};

use crate::envelope::{
    invalid_diagnostic, is_access_policy_payload, is_modern_policy_payload,
    reject_rule_side_effect_fields, require_literal_field, require_non_empty_field,
    validate_json_object_field, validate_non_empty_string_array_field,
};

/// Stable diagnostic codes emitted by the typed-root checks.
pub mod codes {
    /// Outbound command expressions require an object input schema from which
    /// the checker can derive a complete expression environment.
    pub const ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE: &str =
        "action.validation.command_environment_underivable";
    /// The Norn-profile node rejections, defined with the rest of the profile
    /// in [`crate::jdm_profile::codes`].
    pub use crate::jdm_profile::codes::{JDM_CUSTOM_NODE_REJECTED, JDM_FUNCTION_NODE_REJECTED};
    /// A retired artifact kind (`process`): Run and Task are `EntityIR`
    /// types and `RuleIR` owns decision graphs (T1-IR-1d).
    pub const RETIRED_KIND: &str = "ir.validation.retired_kind";
    /// An artifact kind outside the closed kind set.
    pub const UNKNOWN_KIND: &str = "ir.validation.unknown_kind";
    /// An envelope that does not decode.
    pub const INVALID_ENVELOPE: &str = "ir.validation.invalid_envelope";
    /// Retired (T1-IR-2, #2430): an envelope's kind is the tag of its payload
    /// root, so it can no longer disagree with it and nothing emits this code.
    /// It stays registered because codes are never reused.
    pub const KIND_MISMATCH: &str = "ir.validation.kind_mismatch";
    /// A `relation` payload in a v2 form: `EntityIR` v2 relations are links on
    /// the source object type, not relation artifacts (T1-IR-1b).
    pub const RELATION_V2_IS_LINK: &str = "relation.validation.v2_relation_is_link";
}

/// Retired artifact kinds and why each is rejected.
const RETIRED_KINDS: &[(&str, &str)] = &[(
    "process",
    "artifact kind 'process' is retired: model Run and Task as EntityIR types and decision graphs as RuleIR",
)];

const ACTION_CODE: &str = "action.validation.invalid_payload";
const ENTITY_CODE: &str = "entity.validation.invalid_payload";
const RULE_CODE: &str = "rule.validation.invalid_payload";

#[derive(Default)]
struct ActionDerivationCheck {
    cost: u64,
    /// Whether the last successfully checked derivation for a field may
    /// produce `null`/an absent value. A missing entry is not proven safe.
    final_may_be_null: BTreeMap<String, bool>,
}

/// Decode one JSON value into a `norn-ir` type, reporting a decode failure as
/// a diagnostic at `path`.
macro_rules! decode_typed {
    ($ty:ty, $value:expr, $path:expr, $code:expr, $diagnostics:expr, $artifact_id:expr) => {{
        let path: String = $path.into();
        match serde_json::to_value($value).and_then(serde_json::from_value::<$ty>) {
            Ok(decoded) => Some(decoded),
            Err(error) => {
                $diagnostics.push(invalid_diagnostic(
                    $code,
                    format!("{path} is invalid: {error}"),
                    path,
                    $artifact_id,
                ));
                None
            }
        }
    }};
}

/// Decode an optional payload member into a `norn-ir` type.
macro_rules! decode_member {
    ($ty:ty, $payload:expr, $field:literal, $code:expr, $diagnostics:expr, $artifact_id:expr) => {
        match $payload.get($field) {
            Some(value) => decode_typed!(
                $ty,
                value,
                concat!("payload.", $field),
                $code,
                $diagnostics,
                $artifact_id
            ),
            None => None,
        }
    };
}

/// Check the typed-root `ActionIR` members (title, description, ledger
/// category, preconditions, commands, write set, compensation, access).
fn validate_action_transaction_members(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if payload.contains_key("createLinks") {
        let _ = decode_member!(
            Vec<norn_ir::ActionCreateLink>,
            payload,
            "createLinks",
            ACTION_CODE,
            diagnostics,
            artifact_id
        );
    }
    if payload.contains_key("quantityEffect") {
        let _ = decode_member!(
            norn_ir::ActionQuantityEffect,
            payload,
            "quantityEffect",
            ACTION_CODE,
            diagnostics,
            artifact_id
        );
    }
}

pub(crate) fn validate_action_typed_members(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    for field in ["title", "description", "ledgerCategory"] {
        if let Some(value) = payload.get(field) {
            let path = format!("payload.{field}");
            match value {
                JsonValue::String(text) if !text.trim().is_empty() => {}
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::String(_)
                | JsonValue::Array(_)
                | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    format!("{path} must be a non-empty string"),
                    path,
                    artifact_id,
                )),
            }
        }
    }

    if let Some(preconditions) = decode_member!(
        Vec<norn_ir::ZenExpression>,
        payload,
        "preconditions",
        ACTION_CODE,
        diagnostics,
        artifact_id
    ) {
        for (index, precondition) in preconditions.iter().enumerate() {
            if precondition.as_str().trim().is_empty() {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    "precondition must be a non-empty ZEN Expression",
                    format!("payload.preconditions[{index}]"),
                    artifact_id,
                ));
            }
        }
    }

    let derivations = validate_action_derivations(payload, diagnostics, artifact_id);
    validate_action_slice(payload, &derivations, diagnostics, artifact_id);
    let precondition_cost = validate_action_preconditions(payload, diagnostics, artifact_id);
    let turn_fuel = ResourceLimitProfile::norn_limits_v1().turn.fuel_units;
    if derivations.cost.saturating_add(precondition_cost) > turn_fuel {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "the aggregate static cost of action derivations and input preconditions exceeds the turn fuel cap of {turn_fuel}"
            ),
            "payload.preconditions",
            artifact_id,
        ));
    }

    validate_action_transaction_members(payload, diagnostics, artifact_id);
    validate_action_commands(payload, diagnostics, artifact_id);

    validate_action_write_declarations(payload, diagnostics, artifact_id);

    if let Some(compensation) = decode_member!(
        norn_ir::ActionCompensation,
        payload,
        "compensation",
        ACTION_CODE,
        diagnostics,
        artifact_id
    ) && compensation.action_id.trim().is_empty()
    {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "actionId must be a non-empty string",
            "payload.compensation.actionId",
            artifact_id,
        ));
    }

    if let Some(access) = decode_member!(
        norn_ir::AccessRef,
        payload,
        "access",
        ACTION_CODE,
        diagnostics,
        artifact_id
    ) && access.artifact_id.trim().is_empty()
    {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "artifactId must be a non-empty string",
            "payload.access.artifactId",
            artifact_id,
        ));
    }
}

fn validate_action_write_declarations(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if let Some(writes) = decode_member!(
        Vec<norn_ir::ActionWrite>,
        payload,
        "writes",
        ACTION_CODE,
        diagnostics,
        artifact_id
    ) {
        let mut seen = std::collections::BTreeSet::new();
        for (index, write) in writes.iter().enumerate() {
            if write.entity_kind.trim().is_empty() {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    "entityKind must be a non-empty string",
                    format!("payload.writes[{index}].entityKind"),
                    artifact_id,
                ));
            }
            if !seen.insert((write.entity_kind.as_str(), write.change)) {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    "duplicate write-set entry",
                    format!("payload.writes[{index}]"),
                    artifact_id,
                ));
            }
        }
    }
}

/// Validate an action's bounded input- and link-keyed read slice.
fn validate_action_slice(
    payload: &BTreeMap<String, JsonValue>,
    derivations: &ActionDerivationCheck,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(slice) = decode_member!(
        Vec<SliceDeclaration>,
        payload,
        "slice",
        ACTION_CODE,
        diagnostics,
        artifact_id
    ) else {
        return;
    };
    let limits = ResourceLimitProfile::norn_limits_v1().slice;
    if slice.len() > usize::try_from(limits.max_records).unwrap_or(usize::MAX) {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "slice has {} entries, above the profile cap of {}",
                slice.len(),
                limits.max_records
            ),
            "payload.slice",
            artifact_id,
        ));
    }
    let mut names = BTreeSet::new();
    let mut link_depths = BTreeMap::<String, u32>::new();
    for (index, entry) in slice.iter().enumerate() {
        let path = format!("payload.slice[{index}]");
        if !is_action_member_name(&entry.name) || !names.insert(entry.name.as_str()) {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!("slice name {:?} is empty or repeated", entry.name),
                format!("{path}.name"),
                artifact_id,
            ));
        }
        let source_depth = match &entry.key {
            SliceKey::TriggerObject
            | SliceKey::TriggerBefore
            | SliceKey::TriggerAfter
            | SliceKey::TriggerChanges => {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    "actions select slice objects from input properties or earlier slice entries; rule trigger keys are not available",
                    format!("{path}.key"),
                    artifact_id,
                ));
                0
            }
            SliceKey::Property { from, .. } if from == "input" => {
                validate_action_input_slice_key(
                    payload,
                    derivations,
                    entry,
                    &path,
                    diagnostics,
                    artifact_id,
                );
                0
            }
            SliceKey::Property { from, .. } | SliceKey::Link { from, .. } => {
                action_prior_slice_depth(
                    &slice,
                    index,
                    from,
                    &link_depths,
                    diagnostics,
                    artifact_id,
                )
            }
        };
        link_depths.insert(entry.name.clone(), source_depth);
        if source_depth > limits.max_relation_depth {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "slice entry {:?} has link depth {source_depth}, above the profile cap of {}",
                    entry.name, limits.max_relation_depth
                ),
                format!("{path}.key"),
                artifact_id,
            ));
        }
        if entry.optional && !matches!(&entry.key, SliceKey::Property { .. }) {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                "only a property-keyed slice entry can be optional",
                format!("{path}.optional"),
                artifact_id,
            ));
        }
    }
}

fn action_prior_slice_depth(
    slice: &[SliceDeclaration],
    index: usize,
    from: &str,
    link_depths: &BTreeMap<String, u32>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> u32 {
    let path = format!("payload.slice[{index}]");
    match slice
        .iter()
        .take(index)
        .find(|earlier| earlier.name == from)
    {
        None => {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!("slice key names {from:?}, which is not an earlier slice entry or input"),
                format!("{path}.key"),
                artifact_id,
            ));
            0
        }
        Some(earlier) if earlier.is_list() => {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!("slice key names {from:?}, a list entry; keys and links must come from one object"),
                format!("{path}.key"),
                artifact_id,
            ));
            0
        }
        Some(earlier) => {
            let depth = *link_depths.get(&earlier.name).unwrap_or(&0);
            depth.saturating_add(1)
        }
    }
}

fn validate_action_input_slice_key(
    payload: &BTreeMap<String, JsonValue>,
    derivations: &ActionDerivationCheck,
    entry: &SliceDeclaration,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let SliceKey::Property { property, .. } = &entry.key else {
        return;
    };
    let property = property.as_str();
    match action_input_field_type(payload.get("inputSchema"), property) {
        None => diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!("input property {property:?} is not declared in inputSchema"),
            format!("{path}.key.property"),
            artifact_id,
        )),
        Some(ty) if !is_string_key_type(&ty) => diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!("input property {property:?} must have a string or enum type"),
            format!("{path}.key.property"),
            artifact_id,
        )),
        Some(_) => {}
    }
    if !entry.optional && !action_input_key_is_guaranteed(payload, property, derivations) {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "input property {property:?} may be absent; mark the slice entry optional or require the final input value"
            ),
            format!("{path}.key.property"),
            artifact_id,
        ));
    }
}

/// Validate ordered derivations against the final input schema, then type
/// each expression in the environment visible at that step.
fn validate_action_derivations(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> ActionDerivationCheck {
    let Some(derive) = decode_member!(
        Vec<ActionInputDerivation>,
        payload,
        "derive",
        ACTION_CODE,
        diagnostics,
        artifact_id
    ) else {
        return ActionDerivationCheck::default();
    };
    let max_derivations =
        usize::try_from(ResourceLimitProfile::norn_limits_v1().envelope.max_commands)
            .unwrap_or(usize::MAX);
    if derive.len() > max_derivations {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "derive has {} steps, above the profile cap of {max_derivations}",
                derive.len()
            ),
            "payload.derive",
            artifact_id,
        ));
    }
    if derive.is_empty() {
        return ActionDerivationCheck::default();
    }
    let Some(input_schema) = payload.get("inputSchema") else {
        return ActionDerivationCheck::default();
    };
    let Some(schema) = input_schema.as_object() else {
        return ActionDerivationCheck::default();
    };
    let Some(properties) = schema.get("properties").and_then(JsonValue::as_object) else {
        return ActionDerivationCheck::default();
    };
    let (optional_targets, mut required_targets) =
        validate_action_derivation_declarations(&derive, properties, diagnostics, artifact_id);
    // Conflicting repeated declarations already have a finding. Type-check
    // them conservatively as possibly absent so a malformed payload cannot
    // gain admission by whichever declaration happened to be last.
    required_targets.retain(|field| !optional_targets.contains(field));
    validate_action_derivation_steps(
        &derive,
        input_schema,
        &optional_targets,
        &required_targets,
        diagnostics,
        artifact_id,
    )
}

fn validate_action_derivation_declarations(
    derive: &[ActionInputDerivation],
    properties: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut input_requirements = BTreeMap::<String, bool>::new();
    let mut optional_targets = BTreeSet::new();
    let mut required_targets = BTreeSet::new();
    for (index, derivation) in derive.iter().enumerate() {
        let path = format!("payload.derive[{index}]");
        if derivation.field.trim().is_empty() {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                "derivation field must be a non-empty input property name",
                format!("{path}.field"),
                artifact_id,
            ));
        }
        if !properties.contains_key(&derivation.field) {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "derivation target {:?} is not declared in inputSchema",
                    derivation.field
                ),
                format!("{path}.field"),
                artifact_id,
            ));
        }
        if derivation.expression.as_str().trim().is_empty() {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                "derivation expression must be a non-empty ZEN Expression",
                format!("{path}.expression"),
                artifact_id,
            ));
        }
        if let Some(previous) =
            input_requirements.insert(derivation.field.clone(), derivation.input_required)
            && previous != derivation.input_required
        {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "inputRequired must be consistent across derivations of {:?}",
                    derivation.field
                ),
                format!("{path}.inputRequired"),
                artifact_id,
            ));
        }
        if derivation.input_required {
            required_targets.insert(derivation.field.clone());
        } else {
            optional_targets.insert(derivation.field.clone());
        }
    }
    (optional_targets, required_targets)
}

fn validate_action_derivation_steps(
    derive: &[ActionInputDerivation],
    input_schema: &JsonValue,
    optional_targets: &BTreeSet<String>,
    required_targets: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> ActionDerivationCheck {
    let mut fields = action_input_fields(input_schema, optional_targets, required_targets);
    let mut result = ActionDerivationCheck::default();
    for (index, derivation) in derive.iter().enumerate() {
        if action_input_field_type(Some(input_schema), &derivation.field).is_none()
            || derivation.expression.as_str().trim().is_empty()
        {
            continue;
        }
        let Some(expected) = action_derivation_target_type(Some(input_schema), &derivation.field)
        else {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "derivation target {:?} has an opaque JSON shape that cannot be type-checked",
                    derivation.field
                ),
                format!("payload.derive[{index}].field"),
                artifact_id,
            ));
            continue;
        };
        let Some(environment) = fields
            .clone()
            .and_then(|fields| ZenTypeEnv::input_only(fields, EnvOptions::default()).ok())
        else {
            diagnostics.push(invalid_diagnostic(
                codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
                "derivation expressions require an object input schema with a derivable expression environment",
                "payload.inputSchema",
                artifact_id,
            ));
            return result;
        };
        let path = format!("payload.derive[{index}].expression");
        let expression = match Expression::compile(derivation.expression.as_str()) {
            Ok(expression) => expression,
            Err(error) => {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    format!(
                        "derivation expression {:?} does not compile: {error}",
                        derivation.expression.as_str()
                    ),
                    path,
                    artifact_id,
                ));
                continue;
            }
        };
        match check_expression_with_root_bounds(
            &expression,
            &environment,
            &expected,
            &action_input_expression_limits(),
            &action_expression_root_bounds(&environment),
        ) {
            Ok(checked) => {
                result.cost = result.cost.saturating_add(checked.cost().units());
                result.final_may_be_null.insert(
                    derivation.field.clone(),
                    matches!(
                        checked.inferred(),
                        InferredType::Null | InferredType::Optional(_)
                    ),
                );
                if let Some(fields) = fields.as_mut() {
                    fields.insert(
                        derivation.field.clone(),
                        refined_derivation_type(expected, checked.inferred()),
                    );
                }
            }
            Err(findings) => {
                for finding in findings {
                    diagnostics.push(invalid_diagnostic(
                        ACTION_CODE,
                        format!(
                            "derivation expression {:?} is invalid: {} ({})",
                            derivation.expression.as_str(),
                            finding.message,
                            finding.code
                        ),
                        path.clone(),
                        artifact_id,
                    ));
                }
            }
        }
    }
    result
}

/// Type-check input-only preconditions per artifact. Preconditions that read a
/// declared slice are checked later, once the entity set resolves its records.
fn validate_action_preconditions(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> u64 {
    let Some(preconditions) = payload.get("preconditions") else {
        return 0;
    };
    let Some(preconditions) = preconditions.as_array() else {
        return 0;
    };
    if preconditions.is_empty() {
        return 0;
    }
    let max_preconditions =
        usize::try_from(ResourceLimitProfile::norn_limits_v1().envelope.max_events)
            .unwrap_or(usize::MAX);
    if preconditions.len() > max_preconditions {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "preconditions has {} expressions, above the profile cap of {max_preconditions}",
                preconditions.len()
            ),
            "payload.preconditions",
            artifact_id,
        ));
    }
    if payload
        .get("slice")
        .and_then(JsonValue::as_array)
        .is_none_or(<[JsonValue]>::is_empty)
    {
        return check_action_input_preconditions(payload, preconditions, diagnostics, artifact_id);
    }
    0
}

fn check_action_input_preconditions(
    payload: &BTreeMap<String, JsonValue>,
    preconditions: &[JsonValue],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> u64 {
    let Some(environment) = action_input_environment(payload, true) else {
        diagnostics.push(invalid_diagnostic(
            codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
            "preconditions require an object input schema with a derivable expression environment",
            "payload.inputSchema",
            artifact_id,
        ));
        return 0;
    };
    let mut aggregate_cost = 0_u64;
    for (index, value) in preconditions.iter().enumerate() {
        let Some(source) = value.as_string() else {
            continue;
        };
        if let Some(cost) = check_action_expression_with_cost(
            &ZenExpression(source.to_owned()),
            &Type::Bool,
            &environment,
            ActionExpressionSite {
                path: &format!("payload.preconditions[{index}]"),
                label: "precondition",
                limits: &action_input_expression_limits(),
            },
            diagnostics,
            artifact_id,
        ) {
            aggregate_cost = aggregate_cost.saturating_add(cost);
        }
    }
    aggregate_cost
}

/// Type-check action preconditions over the resolved slice object records.
/// Entity type resolution belongs to the artifact-set checker; this helper
/// only assembles the closed ZEN environment and checks boolean expressions.
pub(crate) fn check_action_slice_preconditions(
    root: &ActionRoot,
    records: BTreeMap<String, BTreeMap<String, Type>>,
    slice_fields: BTreeMap<String, Type>,
    artifact_id: Option<&str>,
) -> Vec<Diagnostic> {
    if root.preconditions.is_empty() {
        return Vec::new();
    }
    let schema = JsonValue::Object(root.input_schema.clone());
    if root
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_none()
    {
        return vec![invalid_diagnostic(
            codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
            "preconditions require an object input schema with a derivable expression environment",
            "payload.inputSchema",
            artifact_id,
        )];
    }
    let Some(environment) = action_slice_environment(root, &schema, records, slice_fields) else {
        return vec![invalid_diagnostic(
            codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
            "preconditions require every declared slice object to have a resolved, closed property record",
            "payload.slice",
            artifact_id,
        )];
    };
    let mut diagnostics = Vec::new();
    let mut aggregate_cost = action_derivation_static_cost(root).unwrap_or_default();
    for (index, expression) in root.preconditions.iter().enumerate() {
        if let Some(cost) = check_action_expression_with_cost(
            expression,
            &Type::Bool,
            &environment,
            ActionExpressionSite {
                path: &format!("payload.preconditions[{index}]"),
                label: "precondition",
                limits: &ResourceLimitProfile::norn_limits_v1(),
            },
            &mut diagnostics,
            artifact_id,
        ) {
            aggregate_cost = aggregate_cost.saturating_add(cost);
        }
    }
    let turn_fuel = ResourceLimitProfile::norn_limits_v1().turn.fuel_units;
    if aggregate_cost > turn_fuel {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "the aggregate static cost of action derivations and slice preconditions is {aggregate_cost}, above the turn fuel cap of {turn_fuel}"
            ),
            "payload.preconditions",
            artifact_id,
        ));
    }
    diagnostics
}

fn action_derivation_static_cost(root: &ActionRoot) -> Option<u64> {
    action_post_derivation_fields(&JsonValue::Object(root.input_schema.clone()), &root.derive)
        .map(|(_, cost)| cost)
}

/// Input-only action expressions have the receipt bound as well as the slice
/// bound: the kernel checks raw/effective input before each preparation step.
/// Keep the profile's fuel ceilings; only specialize the enforced byte bound.
fn action_input_expression_limits() -> ResourceLimitProfile {
    let mut limits = ResourceLimitProfile::norn_limits_v1();
    limits.slice.max_canonical_bytes = limits
        .slice
        .max_canonical_bytes
        .min(limits.envelope.max_output_bytes);
    limits
}

/// The kernel independently bounds effective input and the resolved slice.
/// Flat aliases duplicate input positions, so conservatively give each alias
/// its own input-sized pool instead of pretending its bytes are disjoint from
/// `input`. Input record lists have only the byte cap, not the slice record cap.
pub(crate) fn action_expression_root_bounds(
    environment: &ZenTypeEnv,
) -> BTreeMap<String, RootCostBounds> {
    let limits = ResourceLimitProfile::norn_limits_v1();
    environment
        .variables()
        .keys()
        .map(|root| {
            let bound = if root == "slice" {
                RootCostBounds {
                    max_canonical_bytes: limits.slice.max_canonical_bytes,
                    max_records: u64::from(limits.slice.max_records),
                }
            } else {
                RootCostBounds {
                    max_canonical_bytes: limits.envelope.max_output_bytes,
                    max_records: u64::MAX,
                }
            };
            (root.clone(), bound)
        })
        .collect()
}

fn refined_derivation_type(expected: Type, inferred: &InferredType) -> Type {
    if matches!(inferred, InferredType::Null | InferredType::Optional(_)) {
        expected.optional()
    } else if let Type::Optional(inner) = expected {
        *inner
    } else {
        expected
    }
}

fn action_post_derivation_fields(
    schema: &JsonValue,
    derive: &[ActionInputDerivation],
) -> Option<(BTreeMap<String, Type>, u64)> {
    let optional: BTreeSet<_> = derive
        .iter()
        .filter(|step| !step.input_required)
        .map(|step| step.field.clone())
        .collect();
    let required: BTreeSet<_> = derive
        .iter()
        .filter(|step| step.input_required && !optional.contains(&step.field))
        .map(|step| step.field.clone())
        .collect();
    let mut fields = action_input_fields(schema, &optional, &required)?;
    let mut cost = 0_u64;
    for step in derive {
        let environment = ZenTypeEnv::input_only(fields.clone(), EnvOptions::default()).ok()?;
        let expected = action_input_field_type(Some(schema), &step.field)?;
        let expression = Expression::compile(step.expression.as_str()).ok()?;
        let checked = check_expression_with_root_bounds(
            &expression,
            &environment,
            &expected,
            &action_input_expression_limits(),
            &action_expression_root_bounds(&environment),
        )
        .ok()?;
        cost = cost.saturating_add(checked.cost().units());
        fields.insert(
            step.field.clone(),
            refined_derivation_type(expected, checked.inferred()),
        );
    }
    Some((fields, cost))
}

fn action_slice_environment(
    root: &ActionRoot,
    input_schema: &JsonValue,
    mut records: BTreeMap<String, BTreeMap<String, Type>>,
    slice_fields: BTreeMap<String, Type>,
) -> Option<ZenTypeEnv> {
    let (fields, _) = action_post_derivation_fields(input_schema, &root.derive)?;
    let slice_record = "norn:ActionSlice".to_owned();
    records.insert(slice_record.clone(), slice_fields);
    let mut environment = ZenTypeEnv::input_only_with_records(
        records,
        fields,
        EnvOptions::from_limit_profile(&ResourceLimitProfile::norn_limits_v1()),
    )
    .ok()?;
    environment = environment
        .bind_operand("slice", Type::Record(slice_record))
        .ok()?;
    bind_flat_input_aliases(environment)
}

/// Validate declared outbound commands and type-check their expressions
/// against the action's closed input schema.
fn validate_action_commands(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(value) = payload.get("commands") else {
        return;
    };
    let Some(commands) = value.as_array() else {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "commands must be an array",
            "payload.commands",
            artifact_id,
        ));
        return;
    };
    if commands.is_empty() {
        return;
    }
    let environment = action_input_environment(payload, false);
    if environment.is_none()
        && !diagnostics.iter().any(|diagnostic| {
            diagnostic.code == codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE
                && diagnostic.path.as_deref() == Some("payload.inputSchema")
        })
    {
        diagnostics.push(invalid_diagnostic(
            codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
            "outbound command expressions require an object input schema with a derivable expression environment",
            "payload.inputSchema",
            artifact_id,
        ));
    }

    for (index, value) in commands.iter().enumerate() {
        if let Some(command) = decode_action_command(value, index, diagnostics, artifact_id) {
            validate_action_command(
                &command,
                index,
                environment.as_ref(),
                diagnostics,
                artifact_id,
            );
        }
    }
}

/// Decode one command, reporting strict-shape, operation, and unknown-member
/// failures at the command's array entry.
fn decode_action_command(
    value: &JsonValue,
    index: usize,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<ActionCommand> {
    let path = format!("payload.commands[{index}]");
    let Some(command) = value.as_object() else {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!("{path} is invalid: command must be a JSON object"),
            path,
            artifact_id,
        ));
        return None;
    };
    if !matches!(
        command.get("operation").and_then(JsonValue::as_string),
        Some("submit" | "create" | "update" | "cancel")
    ) {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            format!(
                "{path} is invalid: operation must be one of the strings 'submit', 'create', 'update', or 'cancel'"
            ),
            path,
            artifact_id,
        ));
        return None;
    }
    match serde_json::to_value(value).and_then(serde_json::from_value::<ActionCommand>) {
        Ok(command) => Some(command),
        Err(error) => {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!("{path} is invalid: {error}"),
                path,
                artifact_id,
            ));
            None
        }
    }
}

/// Always check a decoded command's declaration; check its expressions when
/// the input environment is available. An unavailable environment is already
/// reported as an admission error by the caller.
fn validate_action_command(
    command: &ActionCommand,
    index: usize,
    environment: Option<&ZenTypeEnv>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let path = format!("payload.commands[{index}]");
    if command.connector.trim().is_empty() {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "connector must be a non-empty string",
            format!("{path}.connector"),
            artifact_id,
        ));
    }
    if command.doctype.trim().is_empty() {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "doctype must be a non-empty string",
            format!("{path}.doctype"),
            artifact_id,
        ));
    }
    if command.name.is_none()
        && matches!(
            command.operation,
            ActionCommandOperation::Submit
                | ActionCommandOperation::Update
                | ActionCommandOperation::Cancel
        )
    {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "name is required for submit, update, and cancel commands",
            format!("{path}.name"),
            artifact_id,
        ));
    }
    if let Some(name) = &command.name
        && let Some(environment) = environment
    {
        check_action_command_expression(
            name,
            &Type::text(),
            environment,
            &format!("{path}.name"),
            "name",
            diagnostics,
            artifact_id,
        );
    }
    if let Some(version) = &command.expected_modified {
        if command.operation != ActionCommandOperation::Update {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                "expectedModified is supported only for update commands",
                format!("{path}.expectedModified"),
                artifact_id,
            ));
        }
        if let Some(environment) = environment {
            check_action_command_expression(
                version,
                &Type::text(),
                environment,
                &format!("{path}.expectedModified"),
                "source version",
                diagnostics,
                artifact_id,
            );
        }
    }
    validate_action_child_merges(command, &path, environment, diagnostics, artifact_id);
    for (field, expression) in &command.fields {
        if field.trim().is_empty() {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                "field names must be non-empty strings",
                format!("{path}.fields"),
                artifact_id,
            ));
        }
        if let Some(environment) = environment {
            check_action_command_expression(
                expression,
                &Type::Json,
                environment,
                &format!("{path}.fields.{field}"),
                "field value",
                diagnostics,
                artifact_id,
            );
        }
    }
}

fn validate_action_child_merges(
    command: &ActionCommand,
    path: &str,
    environment: Option<&ZenTypeEnv>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if !command.child_merges.is_empty()
        && (command.operation != ActionCommandOperation::Update
            || command.expected_modified.is_none())
    {
        diagnostics.push(invalid_diagnostic(
            ACTION_CODE,
            "child merges require an update with expectedModified",
            format!("{path}.childMerges"),
            artifact_id,
        ));
    }
    let mut tables = BTreeSet::new();
    for (merge_index, merge) in command.child_merges.iter().enumerate() {
        let merge_path = format!("{path}.childMerges[{merge_index}]");
        if merge.field.trim().is_empty()
            || merge.ownership_field.trim().is_empty()
            || merge.row_key_field.trim().is_empty()
            || !tables.insert(&merge.field)
            || command.fields.contains_key(&merge.field)
        {
            diagnostics.push(invalid_diagnostic(ACTION_CODE, "merges require distinct nonempty table and discriminator fields and cannot replace a fields entry", &merge_path, artifact_id));
        }
        if let Some(environment) = environment {
            check_action_command_expression(
                &merge.ownership_value,
                &Type::text(),
                environment,
                &format!("{merge_path}.ownershipValue"),
                "row owner",
                diagnostics,
                artifact_id,
            );
            check_action_command_expression(
                &merge.rows,
                &Type::Json.list(),
                environment,
                &format!("{merge_path}.rows"),
                "child rows",
                diagnostics,
                artifact_id,
            );
        }
    }
}

/// Construct the post-derivation input environment used by action expressions.
/// Derived targets exist after the ordered derivation pipeline even when the
/// caller schema does not require the final field.
fn action_input_environment(
    payload: &BTreeMap<String, JsonValue>,
    flat_aliases: bool,
) -> Option<ZenTypeEnv> {
    let schema = payload.get("inputSchema")?;
    let derivations: Vec<ActionInputDerivation> = match payload.get("derive") {
        Some(value) => serde_json::from_value(serde_json::to_value(value).ok()?).ok()?,
        None => Vec::new(),
    };
    let (fields, _) = action_post_derivation_fields(schema, &derivations)?;
    let environment = ZenTypeEnv::input_only(fields, EnvOptions::default()).ok()?;
    if flat_aliases {
        bind_flat_input_aliases(environment)
    } else {
        Some(environment)
    }
}

fn bind_flat_input_aliases(mut environment: ZenTypeEnv) -> Option<ZenTypeEnv> {
    let input = environment.record(INPUT_RECORD)?;
    let aliases: Vec<(String, Type)> = input
        .fields
        .iter()
        .filter(|(name, _)| name.as_str() != "input" && name.as_str() != "slice")
        .map(|(name, field)| (name.clone(), field.ty.clone()))
        .collect();
    for (name, ty) in aliases {
        environment = environment.bind_operand(&name, ty).ok()?;
    }
    Some(environment)
}

fn action_input_fields(
    input_schema: &JsonValue,
    optional_fields: &BTreeSet<String>,
    required_fields: &BTreeSet<String>,
) -> Option<BTreeMap<String, Type>> {
    let schema = input_schema.as_object()?;
    if schema.get("type")?.as_string()? != "object" {
        return None;
    }
    let properties = schema.get("properties")?.as_object()?;
    let required: std::collections::BTreeSet<String> = match schema.get("required") {
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(JsonValue::as_string)
            .map(|value| value.map(str::to_owned))
            .collect::<Option<_>>()?,
        Some(_) => return None,
        None => std::collections::BTreeSet::new(),
    };
    let mut fields = BTreeMap::new();
    for (name, value) in properties {
        let mut ty = action_schema_type(value)?;
        if optional_fields.contains(name)
            || (!required.contains(name) && !required_fields.contains(name))
        {
            ty = ty.optional();
        }
        fields.insert(name.clone(), ty);
    }
    Some(fields)
}

/// Translate an action schema property to its final value type.
fn action_input_field_type(input_schema: Option<&JsonValue>, field: &str) -> Option<Type> {
    let properties = input_schema?.as_object()?.get("properties")?.as_object()?;
    action_schema_type(properties.get(field)?)
}

/// Derivations require a target schema whose value domain the expression
/// checker can prove. `Type::Json` is intentionally permissive, so accepting
/// it here would admit values that fail the final JSON Schema at invocation.
fn action_derivation_target_type(input_schema: Option<&JsonValue>, field: &str) -> Option<Type> {
    let ty = action_input_field_type(input_schema, field)?;
    (!contains_opaque_json(&ty)).then_some(ty)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn contains_opaque_json(ty: &Type) -> bool {
    match ty {
        Type::Json => true,
        Type::List(item) | Type::Optional(item) => contains_opaque_json(item),
        _ => false,
    }
}

fn action_input_field_is_required(payload: &BTreeMap<String, JsonValue>, field: &str) -> bool {
    let schema = payload.get("inputSchema").and_then(JsonValue::as_object);
    let schema_required = schema
        .and_then(|schema| schema.get("required"))
        .and_then(JsonValue::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|value| value.as_string() == Some(field))
        });
    let derivation_required = payload
        .get("derive")
        .and_then(|value| serde_json::to_value(value).ok())
        .and_then(|value| serde_json::from_value::<Vec<ActionInputDerivation>>(value).ok())
        .is_some_and(|derivations| {
            let targets: Vec<&ActionInputDerivation> = derivations
                .iter()
                .filter(|step| step.field == field)
                .collect();
            !targets.is_empty() && targets.iter().all(|step| step.input_required)
        });
    schema_required || derivation_required
}

fn action_input_key_is_guaranteed(
    payload: &BTreeMap<String, JsonValue>,
    field: &str,
    derivations: &ActionDerivationCheck,
) -> bool {
    if let Some(may_be_null) = derivations.final_may_be_null.get(field) {
        return !may_be_null;
    }
    action_input_field_is_required(payload, field)
        && !action_input_field_type(payload.get("inputSchema"), field)
            .as_ref()
            .is_some_and(is_optional_type)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn is_string_key_type(ty: &Type) -> bool {
    match ty {
        Type::Text { .. } | Type::Enum { .. } => true,
        Type::Optional(inner) => is_string_key_type(inner),
        _ => false,
    }
}

fn is_optional_type(ty: &Type) -> bool {
    matches!(ty, Type::Optional(_))
}

fn is_action_member_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Translate the closed `ActionIR` JSON Schema subset into the expression
/// profile's types. An object is opaque because `ZenTypeEnv::input_only` has
/// no nested-record declarations; an unknown type fails closed.
pub(crate) fn action_schema_type(schema: &JsonValue) -> Option<Type> {
    let schema = schema.as_object()?;
    let type_value = schema.get("type")?;
    if let Some(members) = type_value.as_array() {
        let names: Vec<&str> = members
            .iter()
            .map(JsonValue::as_string)
            .collect::<Option<_>>()?;
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        let json_domain: std::collections::BTreeSet<&str> =
            ["null", "boolean", "number", "string", "array", "object"]
                .into_iter()
                .collect();
        if names.len() == 6 && unique.len() == 6 && unique == json_domain {
            return (schema.len() == 1).then_some(Type::Json);
        }
        if names.len() == 2 && unique.len() == 2 && unique.contains("null") && schema.len() == 1 {
            return unique
                .iter()
                .copied()
                .find(|name| *name != "null")
                .and_then(|name| action_scalar_type(name).map(Type::optional));
        }
        return None;
    }
    match type_value.as_string()? {
        "string" => Some(Type::text()),
        "integer" => Some(Type::Integer),
        "number" => Some(Type::Number),
        "boolean" => Some(Type::Bool),
        "null" | "object" => Some(Type::Json),
        "array" => Some(action_schema_type(schema.get("items")?)?.list()),
        _ => None,
    }
}

fn action_scalar_type(name: &str) -> Option<Type> {
    match name {
        "string" => Some(Type::text()),
        "integer" => Some(Type::Integer),
        "number" => Some(Type::Number),
        "boolean" => Some(Type::Bool),
        _ => None,
    }
}

/// Compile and type-check one command expression, adding a located action
/// diagnostic for every expression-profile finding.
fn check_action_command_expression(
    source: &ZenExpression,
    expected: &Type,
    environment: &ZenTypeEnv,
    path: &str,
    label: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> bool {
    check_action_expression_with_cost(
        source,
        expected,
        environment,
        ActionExpressionSite {
            path,
            label,
            limits: &ResourceLimitProfile::norn_limits_v1(),
        },
        diagnostics,
        artifact_id,
    )
    .is_some()
}

#[derive(Clone, Copy)]
struct ActionExpressionSite<'a> {
    path: &'a str,
    label: &'a str,
    limits: &'a ResourceLimitProfile,
}

fn check_action_expression_with_cost(
    source: &ZenExpression,
    expected: &Type,
    environment: &ZenTypeEnv,
    site: ActionExpressionSite<'_>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<u64> {
    let ActionExpressionSite {
        path,
        label,
        limits,
    } = site;
    let expression = match Expression::compile(source.as_str()) {
        Ok(expression) => expression,
        Err(error) => {
            diagnostics.push(invalid_diagnostic(
                ACTION_CODE,
                format!(
                    "{label} expression {:?} does not compile: {error}",
                    source.as_str()
                ),
                path,
                artifact_id,
            ));
            return None;
        }
    };
    match check_expression_with_root_bounds(
        &expression,
        environment,
        expected,
        limits,
        &action_expression_root_bounds(environment),
    ) {
        Ok(checked) => Some(checked.cost().units()),
        Err(findings) => {
            for finding in findings {
                diagnostics.push(invalid_diagnostic(
                    ACTION_CODE,
                    format!(
                        "{label} expression {:?} is invalid: {} ({})",
                        source.as_str(),
                        finding.message,
                        finding.code
                    ),
                    path,
                    artifact_id,
                ));
            }
            None
        }
    }
}

/// Whether a rule payload is a `RuleIR` v2 JDM decision graph.
pub(crate) fn is_rule_ir_v2_payload(payload: &BTreeMap<String, JsonValue>) -> bool {
    payload.contains_key("graph")
}

/// Check a `RuleIR` v2 payload: the typed root members and the JDM graph under
/// the Norn profile. See the module docs for the T3-DEC-1 hook.
pub fn validate_rule_ir_v2_payload(
    payload: &BTreeMap<String, JsonValue>,
    dependencies: &[ArtifactDependency],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    require_literal_field(
        payload,
        "ruleVersion",
        &[RULE_IR_V2_VERSION],
        "payload.ruleVersion",
        diagnostics,
        artifact_id,
        RULE_CODE,
    );
    require_non_empty_field(
        payload,
        "ruleId",
        "payload.ruleId",
        diagnostics,
        artifact_id,
        RULE_CODE,
    );
    validate_json_object_field(
        payload,
        "inputSchema",
        "payload.inputSchema",
        diagnostics,
        artifact_id,
        RULE_CODE,
    );
    validate_json_object_field(
        payload,
        "outputSchema",
        "payload.outputSchema",
        diagnostics,
        artifact_id,
        RULE_CODE,
    );
    validate_non_empty_string_array_field(
        payload,
        "scopeRequired",
        "payload.scopeRequired",
        diagnostics,
        artifact_id,
        RULE_CODE,
    );
    if payload.contains_key("decision") {
        diagnostics.push(invalid_diagnostic(
            RULE_CODE,
            "decision (RuleIR v1) and graph (RuleIR v2) are mutually exclusive",
            "payload.decision",
            artifact_id,
        ));
    }
    for legacy in ["appliesTo", "expression", "severity"] {
        if payload.contains_key(legacy) {
            diagnostics.push(invalid_diagnostic(
                RULE_CODE,
                format!("{legacy} is a legacy rule field and is not allowed in RuleIR v2"),
                format!("payload.{legacy}"),
                artifact_id,
            ));
        }
    }
    if let Some(defaults) = payload.get("defaults")
        && !matches!(defaults, JsonValue::Object(_))
    {
        diagnostics.push(invalid_diagnostic(
            RULE_CODE,
            "payload.defaults must be an object",
            "payload.defaults",
            artifact_id,
        ));
    }
    reject_rule_side_effect_fields(payload, diagnostics, artifact_id);
    crate::jdm_profile::validate_rule_contract(payload, diagnostics, artifact_id);
    crate::rule_bindings::validate_rule_bindings(payload, diagnostics, artifact_id);
    crate::source_commands::validate(payload, diagnostics, artifact_id);
    if let Some(graph) = payload.get("graph") {
        crate::jdm_profile::validate_jdm_graph(graph, dependencies, diagnostics, artifact_id);
    }
}

/// `defaults` and `contract` belong to `RuleIR` v2 only; the v1 closed evaluator has
/// `decision.defaultOutput` instead.
pub(crate) fn reject_rule_ir_v2_members_in_v1(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if payload.contains_key("defaults") {
        diagnostics.push(invalid_diagnostic(
            RULE_CODE,
            "defaults is only allowed with a RuleIR v2 graph; use decision.defaultOutput",
            "payload.defaults",
            artifact_id,
        ));
    }
    if payload.contains_key("contract") {
        diagnostics.push(invalid_diagnostic(
            RULE_CODE,
            "contract is only allowed with a RuleIR v2 graph",
            "payload.contract",
            artifact_id,
        ));
    }
    for member in ["triggers", "slice", "sourceCommands"] {
        if payload.contains_key(member) {
            diagnostics.push(invalid_diagnostic(
                RULE_CODE,
                format!("{member} is only allowed with a RuleIR v2 decide graph"),
                format!("payload.{member}"),
                artifact_id,
            ));
        }
    }
}

pub use crate::jdm_profile::is_norn_profile_admitted;

/// Decode a strict typed root; when it fails, locate the failure at each
/// listed member (and each element of the listed array members), falling back
/// to the whole payload.
macro_rules! check_root {
    (
        $root:ty, $code:expr, $object:expr, $diagnostics:expr, $artifact_id:expr,
        members { $($field:literal => $ty:ty),* $(,)? }
        arrays { $($array_field:literal => $element:ty),* $(,)? }
    ) => {
        check_root!(
            $root, $code, $object, $diagnostics, $artifact_id, at "payload",
            members { $($field => $ty),* }
            arrays { $($array_field => $element),* }
        )
    };
    (
        $root:ty, $code:expr, $object:expr, $diagnostics:expr, $artifact_id:expr,
        at $prefix:literal,
        members { $($field:literal => $ty:ty),* $(,)? }
        arrays { $($array_field:literal => $element:ty),* $(,)? }
    ) => {{
        let whole = JsonValue::Object($object.clone());
        if serde_json::to_value(&whole)
            .and_then(serde_json::from_value::<$root>)
            .is_err()
        {
            let before = $diagnostics.len();
            $(
                if let Some(value) = $object.get($field) {
                    let _ = decode_typed!(
                        $ty, value, concat!($prefix, ".", $field), $code, $diagnostics, $artifact_id
                    );
                }
            )*
            $(
                match $object.get($array_field) {
                    Some(JsonValue::Array(items)) => {
                        for (index, item) in items.iter().enumerate() {
                            let _ = decode_typed!(
                                $element,
                                item,
                                format!("{}.{}[{index}]", $prefix, $array_field),
                                $code,
                                $diagnostics,
                                $artifact_id
                            );
                        }
                    }
                    Some(value) => {
                        let _ = decode_typed!(
                            Vec<$element>,
                            value,
                            concat!($prefix, ".", $array_field),
                            $code,
                            $diagnostics,
                            $artifact_id
                        );
                    }
                    None => {}
                }
            )*
            if $diagnostics.len() == before {
                let _ = decode_typed!($root, &whole, $prefix, $code, $diagnostics, $artifact_id);
            }
        }
    }};
}

/// Check that a payload its kind validator accepted decodes into the strict
/// typed root of an Access, Widget, Connector, Customization, View, Setting,
/// `BusinessPack`, Entity, or Relation artifact. See the module docs.
pub(crate) fn check_typed_root(
    payload: &ArtifactPayload,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let JsonValue::Object(object) = payload.to_json_value() else {
        return;
    };
    match payload {
        ArtifactPayload::Connector(_) => check_connector_root(&object, diagnostics, artifact_id),
        ArtifactPayload::Widget(_) => check_root!(
            norn_ir::WidgetRoot,
            "widget.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "widgetVersion" => norn_ir::IrVersion01,
                "widgetId" => String,
                "ux" => norn_ir::WidgetUx,
                "projectionHints" => norn_ir::WidgetProjectionHints,
                "proposal" => norn_ir::ProposalMetadata,
            }
            arrays {
                "dataDependencies" => norn_ir::WidgetDataDependency,
                "viewStates" => norn_ir::WidgetViewState,
                "actionBindings" => norn_ir::WidgetActionBinding,
                "permissions" => norn_ir::WidgetPermission,
            }
        ),
        ArtifactPayload::Customization(_) => check_root!(
            norn_ir::CustomizationRoot,
            "customization.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "customizationVersion" => norn_ir::IrVersion01,
                "customizationId" => String,
                "intent" => norn_ir::CustomizationIntent,
                "targetArtifact" => norn_ir::CustomizationTarget,
                "overlay" => norn_ir::CustomizationOverlay,
                "approval" => norn_ir::CustomizationApproval,
                "executable" => norn_ir::CustomizationExecutable,
            }
            arrays { "scopeRequired" => String }
        ),
        ArtifactPayload::Policy(_) => check_policy_root(&object, diagnostics, artifact_id),
        ArtifactPayload::Derived(_) => check_root!(
            norn_ir::ViewIrPayload,
            "derived.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "viewVersion" => norn_ir::IrVersion01,
                "viewId" => String,
                "calc" => norn_ir::CalcExpr,
                "outputSchema" => norn_ir::JsonObject,
                "maintenance" => norn_ir::ViewMaintenance,
                "temporalScope" => norn_ir::TemporalScope,
            }
            arrays { "inputs" => norn_ir::ViewInputRef }
        ),
        ArtifactPayload::Setting(_) => check_setting_root(&object, diagnostics, artifact_id),
        ArtifactPayload::BusinessPack(_) => {
            check_business_pack_root(&object, diagnostics, artifact_id);
        }
        ArtifactPayload::Entity(_) => check_entity_root(&object, diagnostics, artifact_id),
        ArtifactPayload::Relation(_) => check_root!(
            norn_ir::RelationRoot,
            "relation.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "from" => String,
                "to" => String,
                "cardinality" => norn_ir::RelationCardinalityV1,
            }
            arrays {}
        ),
        ArtifactPayload::SourceSchema(_) | ArtifactPayload::Mapping(_) => {
            check_document_root(payload, &object, diagnostics, artifact_id);
        }
        ArtifactPayload::Action(_) | ArtifactPayload::Rule(_) | ArtifactPayload::Law(_) => {}
    }
}

fn check_connector_root(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    check_root!(
        norn_ir::ConnectorRoot,
        "connector.validation.invalid_payload",
        object,
        diagnostics,
        artifact_id,
        members {
            "connectorVersion" => norn_ir::IrVersion01,
            "auth" => norn_ir::ConnectorAuth,
            "supportedVersions" => norn_ir::ConnectorSupportedVersions,
            "cursor" => norn_ir::ConnectorCursor,
            "deadLetterPolicy" => norn_ir::DeadLetterPolicy,
            "proposal" => norn_ir::ProposalMetadata,
        }
        arrays {
            "commands" => norn_ir::ConnectorCommand,
            "resources" => norn_ir::ConnectorResource,
            "mappings" => norn_ir::ConnectorMapping,
            "customStepHookIds" => String,
            "customStepHooks" => norn_ir::ConnectorCustomStepHook,
        }
    );
}

/// The strict roots of the `source_schema` and `mapping` kinds: serde
/// documents with no lenient members (T8-SRC-1, T8-MAP-1a).
fn check_document_root(
    payload: &ArtifactPayload,
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if matches!(payload, ArtifactPayload::Mapping(_)) {
        check_root!(
            norn_ir::mapping::MappingIr,
            "mapping.validation.invalid_document",
            object,
            diagnostics,
            artifact_id,
            members {}
            arrays {}
        );
    } else {
        check_root!(
            norn_ir::source_schema::SourceSchemaIr,
            "source_schema.validation.invalid_document",
            object,
            diagnostics,
            artifact_id,
            members {}
            arrays {}
        );
    }
}

/// Whether an entity payload is in the `EntityIR` v2 form: `entityVersion` is
/// present (its value is checked by [`validate_entity_ir_v2_payload`]).
pub(crate) fn is_entity_ir_v2_payload(payload: &BTreeMap<String, JsonValue>) -> bool {
    payload.contains_key("entityVersion")
}

/// `EntityIR` v1 members, which the v2 form does not admit.
const ENTITY_IR_V1_MEMBERS: [&str; 3] = ["objectKind", "fields", "overlaySources"];

/// Decode a definition object whose members already decoded one by one.
macro_rules! decode_object {
    ($ty:ty, $object:expr) => {
        serde_json::to_value(JsonValue::Object($object.clone()))
            .and_then(serde_json::from_value::<$ty>)
            .ok()
    };
}

/// Check an `EntityIR` v2 entity payload (T1-IR-1b, #2844): the version
/// literal, no v1 members, exactly one definition member (`objectType`,
/// `interface`, `objectTypeExtension`, `propertyTypes`, or `markings`), a mutation
/// authority only beside an object type, and a definition that decodes into
/// its `norn-ir` type and passes the per-definition checks
/// ([`crate::check_object_type_local`], [`crate::check_interface_local`],
/// [`crate::check_object_type_extension_local`],
/// [`crate::check_property_type_catalog_local`]).
///
/// This is the per-artifact (T2) check. Semantics that resolve against other
/// definitions — supertypes, interfaces, link targets and inverse roles,
/// shared property types, extension targets — are the artifact-set check
/// [`crate::check_artifact_set_entity_model`], because one artifact cannot
/// resolve them.
pub(crate) fn validate_entity_ir_v2_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    require_literal_field(
        payload,
        "entityVersion",
        &[norn_ir::ENTITY_IR_V2_VERSION],
        "payload.entityVersion",
        diagnostics,
        artifact_id,
        ENTITY_CODE,
    );
    for member in ENTITY_IR_V1_MEMBERS {
        if payload.contains_key(member) {
            diagnostics.push(invalid_diagnostic(
                ENTITY_CODE,
                format!(
                    "{member} is an EntityIR v1 member and is not allowed in EntityIR v2; declare the object type in objectType"
                ),
                format!("payload.{member}"),
                artifact_id,
            ));
        }
    }
    if let Some(authority) = payload.get("mutationAuthority") {
        crate::envelope::validate_entity_mutation_authority(authority, diagnostics, artifact_id);
    }
    let present: Vec<&str> = norn_ir::ENTITY_IR_V2_DEFINITION_MEMBERS
        .into_iter()
        .filter(|member| payload.contains_key(*member))
        .collect();
    let Some((definition, others)) = present.split_first() else {
        diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            "an EntityIR v2 entity artifact carries exactly one definition: objectType, interface, objectTypeExtension, propertyTypes, or markings",
            "payload",
            artifact_id,
        ));
        return;
    };
    for other in others {
        diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            format!(
                "{other} cannot accompany {definition}: an EntityIR v2 entity artifact carries exactly one definition"
            ),
            format!("payload.{other}"),
            artifact_id,
        ));
    }
    if *definition != "objectType" && payload.contains_key("mutationAuthority") {
        diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            format!(
                "mutationAuthority applies only to an objectType definition, not to {definition}"
            ),
            "payload.mutationAuthority",
            artifact_id,
        ));
    }
    let path = format!("payload.{definition}");
    let Some(value) = payload.get(*definition) else {
        return;
    };
    match (*definition, value) {
        ("objectType", JsonValue::Object(object)) => {
            check_object_type(object, diagnostics, artifact_id);
        }
        ("interface", JsonValue::Object(object)) => {
            check_interface(object, diagnostics, artifact_id);
        }
        ("objectTypeExtension", JsonValue::Object(object)) => {
            check_object_type_extension(object, diagnostics, artifact_id);
        }
        ("propertyTypes", JsonValue::Array(items)) => {
            check_property_type_catalog(items, diagnostics, artifact_id);
        }
        ("markings", JsonValue::Array(items)) => {
            check_marking_catalog(items, diagnostics, artifact_id);
        }
        ("markings" | "propertyTypes", _) => diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            format!("{path} must be an array"),
            path,
            artifact_id,
        )),
        _ => diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            format!("{path} must be an object"),
            path,
            artifact_id,
        )),
    }
}

/// Decode `payload.objectType`, locating a failure at the member or array
/// element that fails, then run the per-definition checks
/// ([`crate::check_object_type_local`]) on the decoded type. Its name is the
/// artifact's object kind, so the name grammar is among them.
fn check_object_type(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let before = diagnostics.len();
    check_root!(
        norn_ir::ObjectTypeDef,
        ENTITY_CODE,
        object,
        diagnostics,
        artifact_id,
        at "payload.objectType",
        members {
            "name" => norn_ir::ObjectTypeName,
            "description" => String,
            "status" => norn_ir::ResourceStatus<norn_ir::ObjectTypeName>,
            "extends" => norn_ir::ObjectTypeName,
        }
        arrays {
            "implements" => norn_ir::InterfaceName,
            "properties" => norn_ir::Property,
            "links" => norn_ir::Link,
            "identity" => norn_ir::KeySpec,
            "invariants" => norn_ir::Invariant,
        }
    );
    if diagnostics.len() != before {
        return;
    }
    if let Some(def) = decode_object!(norn_ir::ObjectTypeDef, object) {
        push_local(
            crate::check_object_type_local(&def),
            "payload.objectType",
            diagnostics,
            artifact_id,
        );
    }
}

/// Decode `payload.interface` and run [`crate::check_interface_local`].
fn check_interface(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let before = diagnostics.len();
    check_root!(
        norn_ir::InterfaceDef,
        ENTITY_CODE,
        object,
        diagnostics,
        artifact_id,
        at "payload.interface",
        members {
            "name" => norn_ir::InterfaceName,
            "description" => String,
            "status" => norn_ir::ResourceStatus<norn_ir::InterfaceName>,
        }
        arrays {
            "properties" => norn_ir::InterfaceProperty,
            "links" => norn_ir::InterfaceLink,
        }
    );
    if diagnostics.len() != before {
        return;
    }
    if let Some(def) = decode_object!(norn_ir::InterfaceDef, object) {
        push_local(
            crate::check_interface_local(&def),
            "payload.interface",
            diagnostics,
            artifact_id,
        );
    }
}

/// Decode `payload.objectTypeExtension` and run
/// [`crate::check_object_type_extension_local`].
fn check_object_type_extension(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let before = diagnostics.len();
    check_root!(
        norn_ir::ObjectTypeExtension,
        ENTITY_CODE,
        object,
        diagnostics,
        artifact_id,
        at "payload.objectTypeExtension",
        members {
            "namespace" => norn_ir::ExtensionNamespace,
            "target" => norn_ir::ObjectTypeName,
            "description" => String,
        }
        arrays {
            "properties" => norn_ir::Property,
            "links" => norn_ir::Link,
        }
    );
    if diagnostics.len() != before {
        return;
    }
    if let Some(def) = decode_object!(norn_ir::ObjectTypeExtension, object) {
        push_local(
            crate::check_object_type_extension_local(&def),
            "payload.objectTypeExtension",
            diagnostics,
            artifact_id,
        );
    }
}

/// Decode each element of `payload.propertyTypes` (non-empty), locating a
/// failure at the element, then run
/// [`crate::check_property_type_catalog_local`] on the catalog.
fn check_property_type_catalog(
    items: &[JsonValue],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if items.is_empty() {
        diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            "payload.propertyTypes must declare at least one property type",
            "payload.propertyTypes",
            artifact_id,
        ));
        return;
    }
    let mut defs = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        if let Some(def) = decode_typed!(
            norn_ir::PropertyTypeDef,
            item,
            format!("payload.propertyTypes[{index}]"),
            ENTITY_CODE,
            diagnostics,
            artifact_id
        ) {
            defs.push(def);
        }
    }
    if defs.len() == items.len() {
        push_local(
            crate::check_property_type_catalog_local(&defs),
            "payload.propertyTypes",
            diagnostics,
            artifact_id,
        );
    }
}

/// Decode each marking in `payload.markings` and check its local contract.
fn check_marking_catalog(
    items: &[JsonValue],
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if items.is_empty() {
        diagnostics.push(invalid_diagnostic(
            ENTITY_CODE,
            "payload.markings must declare at least one marking",
            "payload.markings",
            artifact_id,
        ));
        return;
    }
    let mut ids = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let path = format!("payload.markings[{index}]");
        let Some(marking) = decode_typed!(
            norn_ir::MarkingIr,
            item,
            path.clone(),
            ENTITY_CODE,
            diagnostics,
            artifact_id
        ) else {
            continue;
        };
        let id = marking.id.as_str();
        if !valid_marking_path(id) {
            diagnostics.push(invalid_diagnostic(
                ENTITY_CODE,
                format!("marking id {id:?} must be a dotted lower-case identifier"),
                format!("{path}.id"),
                artifact_id,
            ));
        }
        if !ids.insert(id.to_owned()) {
            diagnostics.push(invalid_diagnostic(
                ENTITY_CODE,
                format!("marking id {id:?} is declared twice"),
                format!("{path}.id"),
                artifact_id,
            ));
        }
        if marking.label.trim().is_empty() {
            diagnostics.push(invalid_diagnostic(
                ENTITY_CODE,
                "marking label must not be blank",
                format!("{path}.label"),
                artifact_id,
            ));
        }
        if !valid_marking_path(&marking.category) {
            diagnostics.push(invalid_diagnostic(
                ENTITY_CODE,
                format!(
                    "marking category {:?} must be a dotted lower-case identifier",
                    marking.category
                ),
                format!("{path}.category"),
                artifact_id,
            ));
        }
        if matches!(marking.mask, norn_ir::MaskPolicy::LastN { count: 0 }) {
            diagnostics.push(invalid_diagnostic(
                ENTITY_CODE,
                "last-n mask count must be greater than zero",
                format!("{path}.mask.count"),
                artifact_id,
            ));
        }
    }
}

fn valid_marking_path(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            let mut bytes = part.bytes();
            bytes.next().is_some_and(|first| first.is_ascii_lowercase())
                && bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

/// Report per-definition findings (spans relative to the definition) at
/// `prefix` in the legacy diagnostic model.
fn push_local(
    findings: Vec<crate::Diagnostic>,
    prefix: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    for finding in findings {
        let pointer = finding
            .span
            .as_ref()
            .map_or("", |span| span.pointer.as_str());
        diagnostics.push(Diagnostic {
            code: finding.code.as_str().to_owned(),
            severity: finding.severity,
            message: finding.message,
            artifact_id: artifact_id.map(str::to_owned),
            path: Some(legacy_path(prefix, pointer)),
            remediation: finding.remediation,
        });
    }
}

/// A legacy dotted path (`payload.objectType.properties[1].id`) for a JSON
/// pointer relative to `prefix` (`/properties/1/id`): array indices become
/// `[n]`, object keys `.key`. Object type member ids and member names never
/// contain `.`, `~`, or `/`, so no pointer escape is needed.
fn legacy_path(prefix: &str, pointer: &str) -> String {
    let mut path = prefix.to_owned();
    for segment in pointer.split('/').skip(1) {
        if !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()) {
            path.push('[');
            path.push_str(segment);
            path.push(']');
        } else {
            path.push('.');
            path.push_str(segment);
        }
    }
    path
}

/// Check the Entity root: the v2 definition form named by its definition
/// member when `entityVersion` is present, else the v1 form (selected exactly
/// as the validator does).
fn check_entity_root(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if !is_entity_ir_v2_payload(object) {
        check_root!(
            norn_ir::EntityRootV1,
            ENTITY_CODE,
            object,
            diagnostics,
            artifact_id,
            members {
                "objectKind" => String,
                "fields" => BTreeMap<String, norn_ir::EntityFieldV1>,
                "mutationAuthority" => norn_ir::EntityMutationAuthority,
            }
            arrays {
                "markings" => norn_ir::MarkingId,
            }
        );
    } else if object.contains_key("interface") {
        check_root!(
            norn_ir::EntityInterfaceRootV2,
            ENTITY_CODE,
            object,
            diagnostics,
            artifact_id,
            members {
                "entityVersion" => norn_ir::EntityIrVersion,
                "interface" => norn_ir::InterfaceDef,
            }
            arrays {}
        );
    } else if object.contains_key("objectTypeExtension") {
        check_root!(
            norn_ir::EntityExtensionRootV2,
            ENTITY_CODE,
            object,
            diagnostics,
            artifact_id,
            members {
                "entityVersion" => norn_ir::EntityIrVersion,
                "objectTypeExtension" => norn_ir::ObjectTypeExtension,
            }
            arrays {}
        );
    } else if object.contains_key("propertyTypes") {
        check_root!(
            norn_ir::PropertyTypeCatalogRootV2,
            ENTITY_CODE,
            object,
            diagnostics,
            artifact_id,
            members {
                "entityVersion" => norn_ir::EntityIrVersion,
            }
            arrays {
                "propertyTypes" => norn_ir::PropertyTypeDef,
            }
        );
    } else if object.contains_key("markings") {
        check_root!(
            norn_ir::MarkingCatalogRootV2,
            ENTITY_CODE,
            object,
            diagnostics,
            artifact_id,
            members {
                "entityVersion" => norn_ir::EntityIrVersion,
            }
            arrays {
                "markings" => norn_ir::MarkingIr,
            }
        );
    } else {
        check_root!(
            norn_ir::EntityRootV2,
            ENTITY_CODE,
            object,
            diagnostics,
            artifact_id,
            members {
                "entityVersion" => norn_ir::EntityIrVersion,
                "objectType" => norn_ir::ObjectTypeDef,
                "mutationAuthority" => norn_ir::EntityMutationAuthority,
            }
            arrays {}
        );
    }
}

/// Check the Setting root. `value` is not located: it is any JSON and its
/// agreement with `valueType` is the validator's rule.
fn check_setting_root(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    check_root!(
        norn_ir::SettingRoot,
        "setting.validation.invalid_payload",
        object,
        diagnostics,
        artifact_id,
        members {
            "settingVersion" => norn_ir::IrVersion01,
            "settingKey" => String,
            "valueType" => norn_ir::SettingValueType,
            "effectiveFrom" => String,
            "effectiveTo" => String,
            "supersedes" => norn_ir::SettingSupersedes,
            "review" => norn_ir::SettingReview,
        }
        arrays { "scopeRequired" => String }
    );
}

/// The `BusinessPack` root has two forms; select one exactly as the validator
/// does (`packKind: "ontology"` → ontology form, otherwise composition). Each
/// form reports under its validator's code.
fn check_business_pack_root(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if matches!(object.get("packKind"), Some(JsonValue::String(kind)) if kind == "ontology") {
        check_root!(
            norn_ir::OntologyPackRoot,
            "ontology_pack.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "packId" => String,
                "packVersion" => norn_ir::IrVersion01,
                "packKind" => norn_ir::OntologyPackKind,
            }
            arrays {
                "sources" => norn_ir::OntologySource,
                "terms" => norn_ir::OntologyTerm,
                "mappings" => norn_ir::OntologyMapping,
                "shapes" => norn_ir::OntologyShape,
            }
        );
    } else {
        check_root!(
            norn_ir::CompositionPackRoot,
            "business_pack.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "packId" => String,
                "packVersion" => String,
                "packKind" => norn_ir::CompositionPackKind,
            }
            arrays {
                "provides" => String,
                "artifacts" => norn_ir::PackArtifactRef,
            }
        );
    }
}

/// The Access root has three forms; select one exactly as the validator does.
fn check_policy_root(
    object: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if is_access_policy_payload(object) {
        check_root!(
            norn_ir::AccessIr,
            "policy.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "accessVersion" => norn_ir::IrVersion01,
                "policyId" => String,
            }
            arrays {
                "rules" => norn_ir::access::AccessRule,
                "genericRules" => norn_ir::access::GenericAccessRule,
            }
        );
    } else if is_modern_policy_payload(object) {
        check_root!(
            norn_ir::ObligationPolicyRoot,
            "policy.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "policyVersion" => norn_ir::IrVersion01,
                "policyId" => String,
            }
            arrays {
                "scopeRequired" => String,
                "obligations" => norn_ir::PolicyObligation,
            }
        );
    } else {
        check_root!(
            norn_ir::ApprovalPolicyRoot,
            "policy.validation.invalid_payload",
            object,
            diagnostics,
            artifact_id,
            members {
                "policyId" => String,
                "appliesTo" => String,
                "reviewRequiredWhen" => String,
            }
            arrays { "approvalRoles" => String }
        );
    }
}

fn envelope_diagnostic(
    code: &str,
    message: impl Into<String>,
    path: Option<&str>,
    artifact_id: Option<String>,
) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id,
        path: path.map(str::to_string),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

/// Decode an artifact envelope from its JSON value.
///
/// A retired kind (`process`, [`codes::RETIRED_KIND`]) and a kind outside the
/// closed set ([`codes::UNKNOWN_KIND`]) are reported at `kind`; any other
/// decode failure is [`codes::INVALID_ENVELOPE`]. The decoded envelope still
/// needs [`crate::validate_artifact`].
///
/// # Errors
///
/// Returns the diagnostics when the value is not a decodable envelope.
pub fn decode_artifact_json(
    value: &serde_json::Value,
) -> Result<ArtifactEnvelope, Vec<Diagnostic>> {
    let artifact_id = value
        .get("artifactId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    match value.get("kind") {
        Some(serde_json::Value::String(kind)) => {
            if let Some((_, reason)) = RETIRED_KINDS.iter().find(|(retired, _)| retired == kind) {
                return Err(vec![envelope_diagnostic(
                    codes::RETIRED_KIND,
                    *reason,
                    Some("kind"),
                    artifact_id,
                )]);
            }
            if serde_json::from_value::<ArtifactKind>(serde_json::Value::String(kind.clone()))
                .is_err()
            {
                return Err(vec![envelope_diagnostic(
                    codes::UNKNOWN_KIND,
                    format!("artifact kind '{kind}' is not a known kind"),
                    Some("kind"),
                    artifact_id,
                )]);
            }
        }
        Some(_) => {
            return Err(vec![envelope_diagnostic(
                codes::UNKNOWN_KIND,
                "kind must be a string",
                Some("kind"),
                artifact_id,
            )]);
        }
        None => {}
    }
    serde_json::from_value::<ArtifactEnvelope>(value.clone()).map_err(|error| {
        vec![envelope_diagnostic(
            codes::INVALID_ENVELOPE,
            format!("artifact envelope does not decode: {error}"),
            None,
            artifact_id,
        )]
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "unit tests"
    )]

    use std::collections::BTreeMap;

    use norn_expr::Type;
    use norn_ir::identifier_limits::MAX_ACTION_ID_BYTES;
    use norn_ir::{ActionRoot, ArtifactEnvelope};
    use serde_json::{Value, json};

    use super::codes;
    use crate::envelope::validate_artifact;

    const ACTION_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-reorder.action.json");
    const RELATION_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-sku-supplier.relation.json");
    const RULE_V2_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-reorder-routing.rule.json");

    fn fixture(source: &str) -> Value {
        serde_json::from_str(source).expect("fixture should parse")
    }

    /// `(code, path)` of every diagnostic for an envelope JSON value.
    fn findings(envelope: Value) -> Vec<(String, String)> {
        let envelope: ArtifactEnvelope =
            serde_json::from_value(envelope).expect("envelope should decode leniently");
        validate_artifact(&envelope)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.path.unwrap_or_default()))
            .collect()
    }

    fn has(findings: &[(String, String)], code: &str, path: &str) -> bool {
        findings
            .iter()
            .any(|(found_code, found_path)| found_code == code && found_path == path)
    }

    #[test]
    fn typed_action_and_rule_fixtures_are_valid() {
        assert_eq!(findings(fixture(ACTION_FIXTURE)), []);
        assert_eq!(findings(fixture(RULE_V2_FIXTURE)), []);
    }

    #[test]
    fn rule_ir_v2_rejects_function_and_custom_nodes_at_the_node_path() {
        let mut rule = fixture(RULE_V2_FIXTURE);
        rule["payload"]["graph"]["nodes"][1] = json!({
            "id": "fn", "name": "fn", "type": "functionNode",
            "content": { "source": "export const handler = () => ({})" }
        });
        // A custom node with malformed content is still reported as rejected,
        // not as a decode error.
        rule["payload"]["graph"]["nodes"][2] = json!({
            "id": "custom", "name": "custom", "type": "customNode", "content": 7
        });
        let found = findings(rule);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(has(
            &found,
            codes::JDM_FUNCTION_NODE_REJECTED,
            "payload.graph.nodes[1].type"
        ));
        assert!(has(
            &found,
            codes::JDM_CUSTOM_NODE_REJECTED,
            "payload.graph.nodes[2].type"
        ));
    }

    #[test]
    fn rule_ir_v2_locates_graph_decode_errors() {
        let mut rule = fixture(RULE_V2_FIXTURE);
        rule["payload"]["graph"]["nodes"][3]["content"]["executionMode"] = json!("batch");
        rule["payload"]["graph"]["edges"][0]["weight"] = json!(1);
        let found = findings(rule);
        assert!(has(
            &found,
            "rule.validation.invalid_payload",
            "payload.graph.nodes[3]"
        ));
        assert!(has(
            &found,
            "rule.validation.invalid_payload",
            "payload.graph.edges[0]"
        ));

        let mut rule = fixture(RULE_V2_FIXTURE);
        rule["payload"]["graph"]["tests"] = json!([]);
        assert!(has(
            &findings(rule),
            "rule.validation.invalid_payload",
            "payload.graph"
        ));
    }

    #[test]
    fn rule_ir_v2_root_members_are_checked() {
        let mut rule = fixture(RULE_V2_FIXTURE);
        rule["payload"]["ruleVersion"] = json!("0.1");
        rule["payload"]["decision"] = json!({ "kind": "literal", "value": {} });
        rule["payload"]["defaults"] = json!([]);
        let found = findings(rule);
        for path in [
            "payload.ruleVersion",
            "payload.decision",
            "payload.defaults",
        ] {
            assert!(
                has(&found, "rule.validation.invalid_payload", path),
                "{path}: {found:?}"
            );
        }
    }

    #[test]
    fn rule_ir_v1_rejects_v2_defaults() {
        let conformance: Value = serde_json::from_str(include_str!(
            "../../../verification/archive/rules-dmn-v1/fixtures/conformance.json"
        ))
        .expect("conformance fixture should parse");
        let mut rule = conformance["artifact"].clone();
        assert_eq!(findings(rule.clone()), []);
        rule["payload"]["defaults"] = json!({ "route": "manual" });
        assert!(has(
            &findings(rule),
            "rule.validation.invalid_payload",
            "payload.defaults"
        ));
    }

    #[test]
    fn typed_action_members_are_checked() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["title"] = json!("");
        action["payload"]["preconditions"] = json!(["quantity > 0", " "]);
        action["payload"]["writes"] = json!([
            { "entityKind": "retail.PurchaseRequest", "change": "created" },
            { "entityKind": "retail.PurchaseRequest", "change": "created" },
            { "entityKind": "retail.Sku", "change": "renamed" }
        ]);
        action["payload"]["compensation"] = json!({ "actionId": "" });
        action["payload"]["access"] = json!("retail.procurement.policy");
        let found = findings(action);
        for path in [
            "payload.title",
            "payload.preconditions[1]",
            "payload.writes",
            "payload.compensation.actionId",
            "payload.access",
        ] {
            assert!(
                has(&found, "action.validation.invalid_payload", path),
                "{path}: {found:?}"
            );
        }

        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["writes"] = json!([
            { "entityKind": "retail.PurchaseRequest", "change": "created" },
            { "entityKind": "retail.PurchaseRequest", "change": "created" }
        ]);
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.writes[1]"
        ));
    }

    #[test]
    fn action_derivations_are_ordered_typed_and_require_consistent_input_flags() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["inputSchema"] = json!({
            "type": "object",
            "properties": {
                "code": { "type": "string" },
                "brand": { "type": "string" }
            },
            "required": ["code"],
            "additionalProperties": false
        });
        action["payload"]["derive"] = json!([
            { "field": "code", "expression": "trim(input.code)" },
            { "field": "code", "expression": "upper(input.code)" },
            {
                "field": "brand",
                "expression": "input.brand ?? \"NORN\"",
                "inputRequired": false
            },
            {
                "field": "brand",
                "expression": "upper(input.brand)",
                "inputRequired": false
            }
        ]);
        action["payload"]["preconditions"] = json!([
            "code != \"\"",
            "brand == input.brand",
            "upper(upper(input.code)) != \"\""
        ]);
        assert_eq!(findings(action.clone()), []);

        let mut uninitialized = action.clone();
        uninitialized["payload"]["derive"][2]["expression"] = json!("upper(input.brand)");
        assert!(has(
            &findings(uninitialized),
            "action.validation.invalid_payload",
            "payload.derive[2].expression"
        ));

        let mut over_budget = action.clone();
        over_budget["payload"]["derive"][0]["expression"] =
            json!("upper(upper(upper(upper(upper(input.code)))))");
        let envelope: ArtifactEnvelope = serde_json::from_value(over_budget).expect("action");
        let diagnostics = validate_artifact(&envelope).expect_err("fuel remains bounded");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.path.as_deref() == Some("payload.derive[0].expression")
                    && diagnostic.message.contains("expr.cost.exceeds_budget")
            }),
            "{diagnostics:?}"
        );

        action["payload"]["derive"][1]["inputRequired"] = json!(false);
        assert!(has(
            &findings(action.clone()),
            "action.validation.invalid_payload",
            "payload.derive[1].inputRequired"
        ));

        action["payload"]["derive"][1]["inputRequired"] = json!(true);
        action["payload"]["derive"][2]["expression"] = json!("input.code > 0");
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.derive[2].expression"
        ));
    }

    #[test]
    fn action_derivations_reject_opaque_json_targets_but_keep_scalars_typed() {
        let object_schema = json!({
            "type": "object",
            "properties": { "label": { "type": "string" } },
            "required": ["label"],
            "additionalProperties": false
        });
        for (target_schema, expression) in [
            (object_schema.clone(), "\"oops\""),
            (json!({ "type": "null" }), "\"oops\""),
            (
                json!({ "type": "array", "items": object_schema }),
                "[\"oops\"]",
            ),
        ] {
            let mut action = fixture(ACTION_FIXTURE);
            action["payload"]["preconditions"] = json!([]);
            action["payload"]["inputSchema"] = json!({
                "type": "object",
                "properties": { "value": target_schema },
                "required": [],
                "additionalProperties": false
            });
            action["payload"]["derive"] = json!([
                {
                    "field": "value",
                    "expression": expression,
                    "inputRequired": false
                }
            ]);
            assert!(has(
                &findings(action),
                "action.validation.invalid_payload",
                "payload.derive[0].field"
            ));
        }

        let mut scalar = fixture(ACTION_FIXTURE);
        scalar["payload"]["preconditions"] = json!([]);
        scalar["payload"]["inputSchema"] = json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": [],
            "additionalProperties": false
        });
        scalar["payload"]["derive"] = json!([
            {
                "field": "value",
                "expression": "\"ok\"",
                "inputRequired": false
            }
        ]);
        assert_eq!(findings(scalar), []);
    }

    #[test]
    fn action_slices_reject_every_rule_only_trigger_key() {
        for key in [
            "triggerObject",
            "triggerBefore",
            "triggerAfter",
            "triggerChanges",
        ] {
            let mut action = fixture(ACTION_FIXTURE);
            action["payload"]["slice"] = json!([{
                "name": "trigger",
                "objectType": "fashion.Season",
                "key": key
            }]);
            assert!(has(
                &findings(action),
                "action.validation.invalid_payload",
                "payload.slice[0].key"
            ));
        }
    }

    #[test]
    fn action_slice_keys_are_declared_earlier_and_optional_when_input_may_be_missing() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["inputSchema"] = json!({
            "type": "object",
            "properties": { "code": { "type": "string" } },
            "required": [],
            "additionalProperties": false
        });
        action["payload"]["slice"] = json!([
            {
                "name": "existing",
                "objectType": "fashion.Season",
                "key": { "property": { "from": "input", "property": "code" } }
            }
        ]);
        let findings_without_optional = findings(action.clone());
        assert!(has(
            &findings_without_optional,
            "action.validation.invalid_payload",
            "payload.slice[0].key.property"
        ));

        action["payload"]["slice"][0]["optional"] = json!(true);
        assert_eq!(findings(action.clone()), []);

        action["payload"]["slice"][0]["optional"] = json!(false);
        action["payload"]["derive"] = json!([
            {
                "field": "code",
                "expression": "input.code ?? \"UNKNOWN\"",
                "inputRequired": false
            }
        ]);
        assert_eq!(findings(action.clone()), []);
        action["payload"]["derive"][0]["expression"] = json!("input.code");
        assert!(has(
            &findings(action.clone()),
            "action.validation.invalid_payload",
            "payload.slice[0].key.property"
        ));
        action["payload"]["derive"] = json!([]);

        action["payload"]["slice"][0]["key"] = json!({
            "property": { "from": "later", "property": "code" }
        });
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.slice[0].key"
        ));
    }

    #[test]
    fn action_derivation_and_slice_declaration_counts_are_bounded() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["derive"] = json!(
            (0..33)
                .map(|_| json!({ "field": "quantity", "expression": "input.quantity" }))
                .collect::<Vec<_>>()
        );
        assert!(has(
            &findings(action.clone()),
            "action.validation.invalid_payload",
            "payload.derive"
        ));

        action["payload"]["derive"] = json!([]);
        action["payload"]["slice"] = json!(
            (0..1001)
                .map(|index| json!({
                    "name": format!("existing_{index}"),
                    "objectType": "fashion.Season",
                    "key": { "property": { "from": "input", "property": "skuId" } }
                }))
                .collect::<Vec<_>>()
        );
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.slice"
        ));
    }

    #[test]
    fn action_slice_relation_depth_counts_property_and_link_hops() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["slice"] = json!((0..5)
            .map(|index| {
                let from = if index == 0 {
                    "input".to_owned()
                } else {
                    format!("entry_{}", index - 1)
                };
                json!({
                    "name": format!("entry_{index}"),
                    "objectType": "fashion.Season",
                    "key": { "property": { "from": from, "property": if index == 0 { "skuId" } else { "brand" } } }
                })
            })
            .collect::<Vec<_>>());
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.slice[4].key"
        ));
    }

    #[test]
    fn action_preconditions_typecheck_typed_slice_and_reserved_input_namespaces() {
        let root: ActionRoot = serde_json::from_value(json!({
            "actionId": "fashion.set-season-brand",
            "scopeRequired": ["orgId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "brand": { "type": "string" },
                    "slice": { "type": "string" },
                    "input": { "type": "string" }
                },
                "required": ["brand", "slice", "input"],
                "additionalProperties": false
            },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "preconditions": [
                "slice.existing.brand == input.brand",
                "brand != \"\"",
                "input.slice == \"requested-slice\"",
                "input.input == \"requested-input\""
            ]
        }))
        .expect("action root should decode");
        let records = BTreeMap::from([(
            "fashion.Season".to_owned(),
            BTreeMap::from([("brand".to_owned(), Type::text())]),
        )]);
        let slice_fields = BTreeMap::from([(
            "existing".to_owned(),
            Type::Record("fashion.Season".to_owned()),
        )]);
        assert!(
            super::check_action_slice_preconditions(
                &root,
                records.clone(),
                slice_fields.clone(),
                None,
            )
            .is_empty()
        );

        let mut invalid = root;
        invalid.preconditions[0] = norn_ir::ZenExpression("slice.existing.brand".to_owned());
        let diagnostics = super::check_action_slice_preconditions(
            &invalid,
            records,
            slice_fields,
            Some("fashion.set-season-brand"),
        );
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.path.as_deref() == Some("payload.preconditions[0]")
            })
        );
    }

    #[test]
    fn action_name_and_id_obey_shared_identifier_limit() {
        let mut action = fixture(ACTION_FIXTURE);
        action["name"] = json!("a".repeat(MAX_ACTION_ID_BYTES));
        action["payload"]["actionId"] = json!("a".repeat(MAX_ACTION_ID_BYTES));
        assert_eq!(findings(action), []);

        let mut action = fixture(ACTION_FIXTURE);
        action["name"] = json!("a".repeat(MAX_ACTION_ID_BYTES + 1));
        let name_findings = findings(action);
        assert!(has(
            &name_findings,
            "identifier.validation.too_long",
            "name"
        ));

        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["actionId"] = json!("a".repeat(MAX_ACTION_ID_BYTES + 1));
        assert!(has(
            &findings(action),
            "identifier.validation.too_long",
            "payload.actionId"
        ));
    }

    #[test]
    fn relation_name_obeys_shared_link_identifier_limit() {
        assert_eq!(findings(fixture(RELATION_FIXTURE)), []);

        let mut relation = fixture(RELATION_FIXTURE);
        relation["name"] = json!("l".repeat(norn_ir::identifier_limits::MAX_LINK_NAME_BYTES));
        assert_eq!(findings(relation), []);

        let mut relation = fixture(RELATION_FIXTURE);
        relation["name"] = json!("l".repeat(norn_ir::identifier_limits::MAX_LINK_NAME_BYTES + 1));
        assert!(has(
            &findings(relation),
            "identifier.validation.too_long",
            "name"
        ));
    }

    #[test]
    fn action_commands_admit_all_closed_operations_and_unknown_connectors() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["commands"] = json!([
            {
                "connector": "erpnext",
                "operation": "create",
                "doctype": "Sales Order",
                "fields": {
                    "customer": "input.skuId",
                    "qty": "input.quantity"
                }
            },
            {
                "connector": "future_system",
                "operation": "update",
                "doctype": "Sales Order",
                "name": "input.requestId",
                "fields": { "customer": "input.skuId" }
            },
            {
                "connector": "erpnext",
                "operation": "submit",
                "doctype": "Sales Order",
                "name": "input.requestId"
            }
        ]);

        assert_eq!(findings(action), []);
    }

    #[test]
    fn action_commands_reject_non_object_input_environments() {
        for schema in [
            json!({ "type": "string" }),
            json!({ "type": "integer" }),
            json!({ "type": "number" }),
            json!({ "type": "boolean" }),
            json!({ "type": "null" }),
            json!({ "type": "array", "items": { "type": "string" } }),
        ] {
            let mut action = fixture(ACTION_FIXTURE);
            // This test replaces the input contract, so the retail fixture's
            // quantity/skuId preconditions no longer belong to this action.
            action["payload"]["preconditions"] = json!([]);
            action["payload"]
                .as_object_mut()
                .expect("payload")
                .remove("effectProgram");
            action["payload"]["inputSchema"] = schema;
            action["payload"]["commands"] = json!([]);
            // A scalar schema remains admitted when no command needs a typed
            // input environment; ActionCall still supplies object values.
            assert_eq!(findings(action.clone()), []);
            action["payload"]["commands"] = json!([
                {
                    "connector": "erpnext", "operation": "submit",
                    "doctype": "Sales Order", "name": "'SO-001'"
                }
            ]);
            let envelope: ArtifactEnvelope =
                serde_json::from_value(action).expect("envelope should decode leniently");
            let diagnostics = validate_artifact(&envelope).expect_err("commands must fail closed");
            assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
            let diagnostic = diagnostics.first().expect("environment diagnostic");
            assert_eq!(
                diagnostic.code,
                codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE
            );
            assert_eq!(diagnostic.path.as_deref(), Some("payload.inputSchema"));
            assert_eq!(diagnostic.severity, norn_ir::DiagnosticSeverity::Error);
        }
    }

    #[test]
    fn action_input_admits_only_bounded_json_and_nullable_scalar_leaves() {
        let mut action = fixture(ACTION_FIXTURE);
        // This test replaces the input contract, so the retail fixture's
        // quantity/skuId preconditions no longer belong to this action.
        action["payload"]["preconditions"] = json!([]);
        action["payload"]["inputSchema"] = json!({
            "type": "object",
            "properties": {
                "profile": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                "email": { "type": ["string", "null"] }
            },
            "required": ["profile", "email"],
            "additionalProperties": false
        });
        assert_eq!(findings(action.clone()), []);

        for schema in [
            json!({ "type": ["string", "integer"] }),
            json!({ "type": ["string", "null", "number"] }),
            json!({ "type": ["object", "null"] }),
        ] {
            action["payload"]["inputSchema"]["properties"]["profile"] = schema;
            assert!(has(
                &findings(action.clone()),
                "action.validation.invalid_schema",
                "payload.inputSchema.properties.profile.type"
            ));
        }
        action["payload"]["inputSchema"]["properties"]["profile"] = json!({
            "type": ["null", "boolean", "number", "string", "array", "object"],
            "minimum": 0
        });
        assert!(has(
            &findings(action.clone()),
            "action.validation.invalid_schema",
            "payload.inputSchema.properties.profile.minimum"
        ));

        action["payload"]["inputSchema"] = json!({ "type": "string" });
        action["payload"]["outputSchema"] = json!({
            "type": "object", "properties": {}, "additionalProperties": false
        });
        assert!(findings(action.clone()).is_empty());
        action["payload"]["inputSchema"] = json!({
            "type": ["null", "boolean", "number", "string", "array", "object"]
        });
        assert!(has(
            &findings(action.clone()),
            "action.validation.invalid_schema",
            "payload.inputSchema.type"
        ));

        action["payload"]["inputSchema"] = json!({
            "type": "object", "properties": {}, "additionalProperties": false
        });
        action["payload"]["outputSchema"] = json!({
            "type": "object",
            "properties": { "result": { "type": ["string", "null"] } },
            "additionalProperties": false
        });
        assert!(has(
            &findings(action),
            "action.validation.invalid_schema",
            "payload.outputSchema.properties.result.type"
        ));
    }

    #[test]
    fn action_commands_type_check_bounded_json_and_nullable_scalars() {
        let mut action = fixture(ACTION_FIXTURE);
        // This test replaces the input contract, so the retail fixture's
        // quantity/skuId preconditions no longer belong to this action.
        action["payload"]["preconditions"] = json!([]);
        action["payload"]["inputSchema"] = json!({
            "type": "object",
            "properties": {
                "profile": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                "email": { "type": ["string", "null"] }
            },
            "required": ["profile", "email"],
            "additionalProperties": false
        });
        action["payload"]["commands"] = json!([{
            "connector": "erpnext",
            "operation": "create",
            "doctype": "Person",
            "name": "input.email ?? 'unknown'",
            "fields": { "email": "input.email ?? 'unknown'" }
        }]);
        assert_eq!(findings(action.clone()), []);

        // JSON fields can be carried into an upsert, but remain opaque to
        // command expressions under the existing expression profile.
        action["payload"]["commands"][0]["fields"] = json!({ "profile": "input.profile" });
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.commands[0].fields.profile"
        ));
    }

    #[test]
    fn action_commands_decode_strictly_with_non_object_input() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["inputSchema"] = json!({ "type": "string" });
        for (command, detail) in [
            (
                json!({
                    "connector": "erpnext", "operation": "create",
                    "doctype": "Sales Order", "unexpected": true
                }),
                "unknown field",
            ),
            (
                json!({ "operation": "create", "doctype": "Sales Order" }),
                "missing field `connector`",
            ),
            (
                json!({ "connector": "erpnext", "doctype": "Sales Order" }),
                "operation must be one of the strings",
            ),
            (
                json!({ "connector": "erpnext", "operation": "create" }),
                "missing field `doctype`",
            ),
            (
                json!({
                    "connector": "erpnext", "operation": "delete", "doctype": "Sales Order"
                }),
                "operation must be one of the strings",
            ),
            (
                json!({
                    "connector": "erpnext", "operation": "create",
                    "doctype": "Sales Order", "fields": { "customer": 7 }
                }),
                "invalid type",
            ),
        ] {
            action["payload"]["commands"] = json!([command]);
            let envelope: ArtifactEnvelope =
                serde_json::from_value(action.clone()).expect("envelope should decode leniently");
            let diagnostics = validate_artifact(&envelope).expect_err("malformed command");
            assert!(
                diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "action.validation.invalid_payload"
                        && diagnostic.path.as_deref() == Some("payload.commands[0]")
                        && diagnostic.message.contains(detail)
                }),
                "{diagnostics:?}"
            );
        }
    }

    #[test]
    fn action_commands_reject_serde_sequence_and_enum_object_forms() {
        for command in [
            json!(["erpnext", "create", "Sales Order", null, {}]),
            json!({
                "connector": "erpnext", "operation": { "create": null },
                "doctype": "Sales Order"
            }),
        ] {
            let mut action = fixture(ACTION_FIXTURE);
            action["payload"]["commands"] = json!([command]);
            assert_eq!(
                findings(action.clone()),
                vec![(
                    "action.validation.invalid_payload".to_owned(),
                    "payload.commands[0]".to_owned()
                )]
            );

            action["payload"]["inputSchema"] = json!({ "type": "string" });
            let found = findings(action);
            assert_eq!(found.len(), 2, "{found:?}");
            assert!(has(
                &found,
                "action.validation.invalid_payload",
                "payload.commands[0]"
            ));
            assert!(has(
                &found,
                codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
                "payload.inputSchema"
            ));
        }
    }

    #[test]
    fn action_commands_check_required_names_without_an_input_environment() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["inputSchema"] = json!({ "type": "string" });
        action["payload"]["commands"] = json!([
            { "connector": "erpnext", "operation": "submit", "doctype": "Sales Order" },
            { "connector": "erpnext", "operation": "update", "doctype": "Sales Order" },
            { "connector": "erpnext", "operation": "cancel", "doctype": "Sales Order" }
        ]);
        let found = findings(action);
        assert!(has(
            &found,
            codes::ACTION_COMMAND_ENVIRONMENT_UNDERIVABLE,
            "payload.inputSchema"
        ));
        for path in [
            "payload.commands[0].name",
            "payload.commands[1].name",
            "payload.commands[2].name",
        ] {
            assert!(
                has(&found, "action.validation.invalid_payload", path),
                "{found:?}"
            );
        }
    }

    #[test]
    fn action_commands_reject_invalid_expressions_and_missing_input_members() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["commands"] = json!([
            {
                "connector": "erpnext",
                "operation": "submit",
                "doctype": "Sales Order",
                "name": "input.quantity"
            },
            {
                "connector": "erpnext",
                "operation": "create",
                "doctype": "Sales Order",
                "fields": { "customer": "input.unknown" }
            },
            {
                "connector": "erpnext",
                "operation": "create",
                "doctype": "Sales Order",
                "fields": { "customer": "input.skuId +" }
            }
        ]);

        let found = findings(action);
        for path in [
            "payload.commands[0].name",
            "payload.commands[1].fields.customer",
            "payload.commands[2].fields.customer",
        ] {
            assert!(
                has(&found, "action.validation.invalid_payload", path),
                "{path}: {found:?}"
            );
        }
    }

    #[test]
    fn action_commands_reject_expected_modified_for_submit_and_cancel() {
        for operation in ["submit", "cancel"] {
            let mut action = fixture(ACTION_FIXTURE);
            action["payload"]["commands"] = json!([{
                "connector": "erpnext", "operation": operation,
                "doctype": "Sales Order", "name": "\"SO-1\"",
                "expectedModified": "\"2026-10-02 12:00:00\""
            }]);
            let found = findings(action);
            assert!(
                has(
                    &found,
                    "action.validation.invalid_payload",
                    "payload.commands[0].expectedModified"
                ),
                "{operation}: {found:?}"
            );
        }
    }

    #[test]
    fn action_commands_require_names_for_submit_update_and_cancel() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["commands"] = json!([
            { "connector": "erpnext", "operation": "submit", "doctype": "Sales Order" },
            { "connector": "erpnext", "operation": "update", "doctype": "Sales Order" },
            { "connector": "erpnext", "operation": "cancel", "doctype": "Sales Order" }
        ]);

        let found = findings(action);
        for path in [
            "payload.commands[0].name",
            "payload.commands[1].name",
            "payload.commands[2].name",
        ] {
            assert!(
                has(&found, "action.validation.invalid_payload", path),
                "{path}: {found:?}"
            );
        }
    }

    #[test]
    fn action_commands_require_non_empty_connector_doctype_and_field_names() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["commands"] = json!([
            {
                "connector": "  ",
                "operation": "create",
                "doctype": "",
                "fields": { " ": "input.skuId" }
            }
        ]);

        let found = findings(action);
        for path in [
            "payload.commands[0].connector",
            "payload.commands[0].doctype",
            "payload.commands[0].fields",
        ] {
            assert!(
                has(&found, "action.validation.invalid_payload", path),
                "{path}: {found:?}"
            );
        }
    }

    #[test]
    fn action_commands_reject_unknown_operation_and_members() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["commands"] = json!([
            {
                "connector": "erpnext",
                "operation": "delete",
                "doctype": "Sales Order"
            }
        ]);
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.commands[0]"
        ));

        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["commands"] = json!([
            {
                "connector": "erpnext",
                "operation": "create",
                "doctype": "Sales Order",
                "unexpected": "member"
            }
        ]);
        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.commands[0]"
        ));
    }

    #[test]
    fn action_command_expressions_do_not_read_nested_opaque_input_objects() {
        let mut action = fixture(ACTION_FIXTURE);
        action["payload"]["inputSchema"]["properties"]["customer"] = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": false
        });
        action["payload"]["commands"] = json!([
            {
                "connector": "erpnext",
                "operation": "create",
                "doctype": "Sales Order",
                "fields": { "customer_name": "input.customer.name" }
            }
        ]);

        assert!(has(
            &findings(action),
            "action.validation.invalid_payload",
            "payload.commands[0].fields.customer_name"
        ));
    }

    #[test]
    fn norn_profile_admission_is_one_predicate() {
        use norn_ir::JdmNodeType;

        use super::is_norn_profile_admitted;
        for (node_type, admitted) in [
            (JdmNodeType::InputNode, true),
            (JdmNodeType::OutputNode, true),
            (JdmNodeType::ExpressionNode, true),
            (JdmNodeType::DecisionTableNode, true),
            (JdmNodeType::SwitchNode, true),
            (JdmNodeType::DecisionNode, true),
            (JdmNodeType::FunctionNode, false),
            (JdmNodeType::CustomNode, false),
        ] {
            assert_eq!(
                is_norn_profile_admitted(node_type),
                admitted,
                "{node_type:?}"
            );
        }
    }

    /// Replace node 4 of the v2 fixture with a sub-decision so every node
    /// kind with nullable-in-Norn-but-not-in-zen members is present.
    fn rule_with_sub_decision() -> Value {
        let mut rule = fixture(RULE_V2_FIXTURE);
        rule["payload"]["graph"]["nodes"][4] = json!({
            "id": "other_table", "name": "pricing", "type": "decisionNode",
            "content": { "key": "retail.pricing.rule", "executionMode": "single", "passThrough": true }
        });
        rule["dependencies"] = json!([
            { "artifactId": "retail.pricing.rule", "kind": "rule", "version": "0.1.0", "required": true }
        ]);
        assert_eq!(findings(rule.clone()), []);
        rule
    }

    /// Members the pinned zen model lets you omit but rejects as `null`
    /// (`#[serde(default)]` non-`Option` members and string row values):
    /// omitted is valid, explicit `null` is a located decode diagnostic.
    #[test]
    fn jdm_members_may_be_omitted_but_not_null() {
        // (node index, JSON path inside the node, member)
        let cases: &[(usize, &[&str], &str)] = &[
            (0, &[], "content"),
            (5, &[], "content"),
            (1, &["content"], "executionMode"),
            (1, &["content"], "passThrough"),
            (2, &["content"], "hitPolicy"),
            (3, &["content"], "executionMode"),
            (3, &["content"], "passThrough"),
            (3, &["content", "inputs", "0"], "name"),
            (3, &["content", "outputs", "0"], "name"),
            (3, &["content", "rules", "1"], "_description"),
            (4, &["content"], "executionMode"),
            (4, &["content"], "passThrough"),
        ];
        for &(node, parents, member) in cases {
            let mut omitted = rule_with_sub_decision();
            let mut target = &mut omitted["payload"]["graph"]["nodes"][node];
            for parent in parents {
                target = match parent.parse::<usize>() {
                    Ok(index) => &mut target[index],
                    Err(_) => &mut target[*parent],
                };
            }
            target
                .as_object_mut()
                .expect("parent is an object")
                .remove(member);
            assert_eq!(
                findings(omitted.clone()),
                [],
                "node {node} {parents:?}.{member} omitted should be valid"
            );

            let mut null = omitted;
            let mut target = &mut null["payload"]["graph"]["nodes"][node];
            for parent in parents {
                target = match parent.parse::<usize>() {
                    Ok(index) => &mut target[index],
                    Err(_) => &mut target[*parent],
                };
            }
            target[member] = Value::Null;
            let found = findings(null);
            assert!(
                has(
                    &found,
                    "rule.validation.invalid_payload",
                    &format!("payload.graph.nodes[{node}]")
                ),
                "node {node} {parents:?}.{member} = null should be rejected: {found:?}"
            );
        }
    }

    /// Members that are `Option` / empty-string-is-none in zen, or editor-only
    /// members zen ignores, keep accepting `null`.
    #[test]
    fn jdm_members_nullable_in_zen_accept_null() {
        let cases: &[(usize, &[&str], &str)] = &[
            (0, &[], "position"),
            (3, &["content", "inputs", "0"], "field"),
            (3, &["content", "outputs", "0"], "type"),
            (3, &["content"], "inputField"),
            (3, &["content"], "outputPath"),
        ];
        for &(node, parents, member) in cases {
            let mut rule = rule_with_sub_decision();
            let mut target = &mut rule["payload"]["graph"]["nodes"][node];
            for parent in parents {
                target = match parent.parse::<usize>() {
                    Ok(index) => &mut target[index],
                    Err(_) => &mut target[*parent],
                };
            }
            target[member] = Value::Null;
            if member == "field" {
                // Without a field the column's cells are full expressions,
                // not unary tests (Norn profile, T3-DEC-1).
                rule["payload"]["graph"]["nodes"][node]["content"]["rules"][0]["c_total"] =
                    json!("total > 10000");
            }
            assert_eq!(
                findings(rule),
                [],
                "node {node} {parents:?}.{member} = null"
            );
        }
        // An edge that does not leave a switch (a switch edge needs its handle).
        let mut rule = rule_with_sub_decision();
        rule["payload"]["graph"]["edges"][4]["sourceHandle"] = Value::Null;
        assert_eq!(findings(rule), []);
    }

    const CONNECTOR_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-inventory.connector.json");
    const WIDGET_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-replenishment.widget.json");
    const POLICY_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-procurement.policy.json");
    const CUSTOMIZATION_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-sku-reorder-point.customization.json");
    const DERIVED_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-low-stock.derived.json");

    #[test]
    fn remaining_typed_root_fixtures_are_valid() {
        for source in [
            CONNECTOR_FIXTURE,
            WIDGET_FIXTURE,
            POLICY_FIXTURE,
            CUSTOMIZATION_FIXTURE,
            DERIVED_FIXTURE,
        ] {
            assert_eq!(findings(fixture(source)), []);
        }
        // The pre-versioned approval form of the Access root.
        let mut policy = fixture(POLICY_FIXTURE);
        policy["payload"] = json!({
            "policyId": "retail.procurement.policy",
            "appliesTo": "retail.sku.reorder",
            "reviewRequiredWhen": "quantity > 100",
            "approvalRoles": ["procurement_manager"]
        });
        assert_eq!(findings(policy), []);
    }

    /// Where the typed root and the validator disagree, the typed root is
    /// reported at the failing member: integral counts beyond 2^53 are not
    /// exact binary64 integers.
    #[test]
    fn typed_root_disagreement_is_located_at_the_member() {
        let mut connector = fixture(CONNECTOR_FIXTURE);
        connector["payload"]["deadLetterPolicy"]["maxFailures"] = json!(1e20);
        assert_eq!(
            findings(connector),
            [(
                "connector.validation.invalid_payload".to_string(),
                "payload.deadLetterPolicy".to_string()
            )]
        );
    }

    /// The locator reports array members per element (exercised directly,
    /// because the kind validators reject these shapes first).
    #[test]
    fn typed_root_check_locates_array_elements() {
        let mut payload = fixture(CONNECTOR_FIXTURE)["payload"].clone();
        payload["resources"][1]["delimiter"] = json!("|");
        payload["mappings"][0]["kind"] = json!("lookup");
        let payload: norn_ir::JsonValue =
            norn_ir::json_tree_from_value(payload).expect("payload decodes");
        let payload =
            norn_ir::ArtifactPayload::from_kind_json(&norn_ir::ArtifactKind::Connector, payload)
                .expect("payload is an object");
        let mut diagnostics = Vec::new();
        super::check_typed_root(&payload, &mut diagnostics, None);
        let paths: Vec<_> = diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.path.as_deref())
            .collect();
        assert_eq!(paths, ["payload.resources[1]", "payload.mappings[0]"]);
    }

    /// A member the validator rejects as `null` is reported once, by the
    /// validator; the typed-root check does not repeat it.
    #[test]
    fn null_members_are_reported_once() {
        for (source, member) in [
            (WIDGET_FIXTURE, "projectionHints"),
            (CONNECTOR_FIXTURE, "cursor"),
            (DERIVED_FIXTURE, "temporalScope"),
        ] {
            let mut artifact = fixture(source);
            artifact["payload"][member] = Value::Null;
            let found = findings(artifact);
            assert_eq!(found.len(), 1, "{member}: {found:?}");
            assert_eq!(found[0].1, format!("payload.{member}"));
        }
        // Omitting them is valid.
        for (source, member) in [
            (WIDGET_FIXTURE, "projectionHints"),
            (CONNECTOR_FIXTURE, "cursor"),
            (DERIVED_FIXTURE, "temporalScope"),
        ] {
            let mut artifact = fixture(source);
            artifact["payload"]
                .as_object_mut()
                .expect("payload object")
                .remove(member);
            assert_eq!(findings(artifact), [], "{member}");
        }
    }

    #[test]
    fn process_kind_is_rejected_with_a_located_diagnostic() {
        let mut process = fixture(CONNECTOR_FIXTURE);
        process["kind"] = json!("process");
        process["payload"] = json!({ "processId": "order_fulfillment", "steps": [] });
        let diagnostics = super::decode_artifact_json(&process).expect_err("process is retired");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, codes::RETIRED_KIND);
        assert_eq!(diagnostics[0].path.as_deref(), Some("kind"));
        assert!(diagnostics[0].message.contains("EntityIR"));

        let mut unknown = fixture(CONNECTOR_FIXTURE);
        unknown["kind"] = json!("workflow");
        let diagnostics = super::decode_artifact_json(&unknown).expect_err("unknown kind");
        assert_eq!(diagnostics[0].code, codes::UNKNOWN_KIND);

        let mut malformed = fixture(CONNECTOR_FIXTURE);
        malformed["payload"] = json!([]);
        let diagnostics = super::decode_artifact_json(&malformed).expect_err("not an object");
        assert_eq!(diagnostics[0].code, codes::INVALID_ENVELOPE);

        let decoded =
            super::decode_artifact_json(&fixture(CONNECTOR_FIXTURE)).expect("connector decodes");
        assert_eq!(decoded.kind(), norn_ir::ArtifactKind::Connector);
    }

    /// `extensions` on Action and Rule roots follows the #2821 roots: omitted
    /// or an object map is valid, explicit `null` is rejected once, at
    /// `payload.extensions` (#2823).
    #[test]
    fn action_and_rule_extensions_may_be_omitted_but_not_null() {
        for (name, source) in [("action", ACTION_FIXTURE), ("rule", RULE_V2_FIXTURE)] {
            let mut artifact = fixture(source);
            artifact["payload"]
                .as_object_mut()
                .expect("payload object")
                .remove("extensions");
            assert_eq!(findings(artifact.clone()), [], "{name}: omitted");
            artifact["payload"]["extensions"] =
                json!({ "example.com/audit/1.0.0": { "enabled": true } });
            assert_eq!(findings(artifact.clone()), [], "{name}: object map");
            artifact["payload"]["extensions"] = Value::Null;
            let found = findings(artifact);
            assert_eq!(found.len(), 1, "{name}: {found:?}");
            assert_eq!(found[0].1, "payload.extensions", "{name}");
        }
    }

    const SETTING_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-reorder-threshold.setting.json");
    const COMPOSITION_FIXTURE: &str =
        include_str!("../../../fixtures/ir/retail-core.business_pack.json");
    const ONTOLOGY_FIXTURE: &str =
        include_str!("../../../verification/ontology/packs/quantity-units-v0/pack.json");

    /// Setting, composition-pack, and ontology-pack artifacts the validators
    /// accept also decode into their typed roots (T1-IR-1e #2820).
    #[test]
    fn setting_and_business_pack_fixtures_are_valid() {
        for source in [SETTING_FIXTURE, COMPOSITION_FIXTURE, ONTOLOGY_FIXTURE] {
            assert_eq!(findings(fixture(source)), []);
        }
        // IR 0.1 composition packs predate `packKind`.
        let mut legacy = fixture(COMPOSITION_FIXTURE);
        let payload = legacy["payload"].as_object_mut().expect("payload object");
        payload.remove("packKind");
        payload.remove("provides");
        assert_eq!(findings(legacy), []);
        // A `json` setting may hold `null`.
        let mut setting = fixture(SETTING_FIXTURE);
        setting["payload"]["valueType"] = json!("json");
        setting["payload"]["value"] = Value::Null;
        assert_eq!(findings(setting), []);
        // An exact mapping with its ratification members, and optional
        // ratification members on another relation, typed the same (T1-IR-2).
        let mut ontology = fixture(ONTOLOGY_FIXTURE);
        ontology["payload"]["mappings"][0]["relation"] = json!("exact_match");
        ontology["payload"]["mappings"][0]["ratifiedBy"] = json!("ontology_steward");
        ontology["payload"]["mappings"][0]["ratifiedAt"] = json!("2026-09-01T00:00:00Z");
        ontology["payload"]["mappings"][1]["ratifiedBy"] = json!("ontology_steward");
        ontology["payload"]["mappings"][1]["ratifiedAt"] = json!("2026-09-01T00:00:00Z");
        assert_eq!(findings(ontology.clone()), []);
        for (member, invalid, message) in [
            (
                "ratifiedBy",
                Value::Null,
                "value must be a non-empty string",
            ),
            ("ratifiedAt", json!(7), "value must be a non-empty string"),
            (
                "ratifiedAt",
                json!("yesterday"),
                "ratifiedAt must use the supported UTC RFC3339 subset",
            ),
        ] {
            let mut candidate = ontology.clone();
            candidate["payload"]["mappings"][1][member] = invalid;
            let found = validate_artifact(
                &serde_json::from_value(candidate).expect("ontology pack decodes"),
            )
            .expect_err("malformed ratification member");
            assert_eq!(found.len(), 1, "{member}: {found:?}");
            assert_eq!(found[0].message, message);
            assert_eq!(
                found[0].path.as_deref(),
                Some(format!("payload.mappings[1].{member}").as_str())
            );
        }
    }

    /// Optional setting and pack members are reported once, by the validator,
    /// when they are an explicit `null`; omitting them is valid.
    #[test]
    fn setting_and_business_pack_null_members_are_reported_once() {
        let cases: [(&str, &[&str], &str, &str); 7] = [
            (SETTING_FIXTURE, &[], "effectiveTo", "payload.effectiveTo"),
            (SETTING_FIXTURE, &[], "supersedes", "payload.supersedes"),
            (
                SETTING_FIXTURE,
                &["review"],
                "reviewedBy",
                "payload.review.reviewedBy",
            ),
            (COMPOSITION_FIXTURE, &[], "provides", "payload.provides"),
            (COMPOSITION_FIXTURE, &[], "packKind", "payload.packKind"),
            (
                ONTOLOGY_FIXTURE,
                &["terms", "1"],
                "parentTermId",
                "payload.terms[1].parentTermId",
            ),
            (
                ONTOLOGY_FIXTURE,
                &["shapes", "0", "fields", "0"],
                "targetTermId",
                "payload.shapes[0].fields[0].targetTermId",
            ),
        ];
        for (source, parents, member, path) in cases {
            let mut omitted = fixture(source);
            let mut target = &mut omitted["payload"];
            for parent in parents {
                target = match parent.parse::<usize>() {
                    Ok(index) => &mut target[index],
                    Err(_) => &mut target[*parent],
                };
            }
            target
                .as_object_mut()
                .expect("parent is an object")
                .remove(member);
            assert_eq!(findings(omitted.clone()), [], "{path} omitted");

            let mut null = omitted;
            let mut target = &mut null["payload"];
            for parent in parents {
                target = match parent.parse::<usize>() {
                    Ok(index) => &mut target[index],
                    Err(_) => &mut target[*parent],
                };
            }
            target[member] = Value::Null;
            let found = findings(null);
            assert_eq!(found.len(), 1, "{path}: {found:?}");
            assert_eq!(found[0].1, path);
        }
    }

    /// `extensions` on Setting and `BusinessPack` roots: omitted or an object
    /// map is valid, explicit `null` is rejected once, at
    /// `payload.extensions`.
    #[test]
    fn setting_and_business_pack_extensions_may_be_omitted_but_not_null() {
        for source in [SETTING_FIXTURE, COMPOSITION_FIXTURE, ONTOLOGY_FIXTURE] {
            let mut artifact = fixture(source);
            artifact["payload"]["extensions"] =
                json!({ "example.com/audit/1.0.0": { "enabled": true } });
            assert_eq!(findings(artifact.clone()), []);
            artifact["payload"]["extensions"] = Value::Null;
            let found = findings(artifact);
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].1, "payload.extensions");
        }
    }

    /// The ontology validator bounds cardinalities below only; the typed root
    /// admits exact binary64 integers, so a larger count is located at the
    /// shape under the ontology-pack code.
    #[test]
    fn ontology_pack_typed_root_disagreement_is_located() {
        let mut ontology = fixture(ONTOLOGY_FIXTURE);
        ontology["payload"]["shapes"][0]["fields"][0]["maxCount"] = json!(1e20);
        assert_eq!(
            findings(ontology),
            [(
                "ontology_pack.validation.invalid_payload".to_string(),
                "payload.shapes[0]".to_string()
            )]
        );
    }

    /// The locator reports pack members per element under the form's code
    /// (exercised directly, because the kind validators reject these first).
    #[test]
    fn business_pack_typed_root_check_locates_each_form() {
        let check = |source: &str, edit: fn(&mut Value)| {
            let mut payload = fixture(source)["payload"].clone();
            edit(&mut payload);
            let payload: norn_ir::JsonValue =
                norn_ir::json_tree_from_value(payload).expect("payload decodes");
            let payload = norn_ir::ArtifactPayload::from_kind_json(
                &norn_ir::ArtifactKind::BusinessPack,
                payload,
            )
            .expect("payload is an object");
            let mut diagnostics = Vec::new();
            super::check_typed_root(&payload, &mut diagnostics, None);
            diagnostics
                .into_iter()
                .map(|diagnostic| (diagnostic.code, diagnostic.path.unwrap_or_default()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            check(COMPOSITION_FIXTURE, |payload| {
                payload["artifacts"][2]["kind"] = json!("derived");
            }),
            [(
                "business_pack.validation.invalid_payload".to_string(),
                "payload.artifacts[2]".to_string()
            )]
        );
        assert_eq!(
            check(ONTOLOGY_FIXTURE, |payload| {
                payload["terms"][1]["note"] = json!("unchecked");
            }),
            [(
                "ontology_pack.validation.invalid_payload".to_string(),
                "payload.terms[1]".to_string()
            )]
        );
    }

    const EFFECTFUL_TS_BOUNDARY_FIXTURE: &str =
        include_str!("../../../fixtures/invalid/effectful-ts-boundary.customization.json");
    const WASM_BOUNDARY_FIXTURE: &str =
        include_str!("../../../fixtures/invalid/wasm-boundary.customization.json");

    /// `effectful_ts` and `wasm` are retired (#2822): each is reported once,
    /// under its own code, and the same executable with an admitted boundary
    /// passes both the validator and the typed-root agreement check.
    #[test]
    fn retired_executable_boundaries_are_rejected_with_a_stable_code() {
        for source in [EFFECTFUL_TS_BOUNDARY_FIXTURE, WASM_BOUNDARY_FIXTURE] {
            let artifact = fixture(source);
            assert_eq!(
                findings(artifact.clone()),
                [(
                    "customization.validation.retired_boundary".to_string(),
                    "payload.executable.boundary".to_string()
                )]
            );
            for boundary in ["trusted_rust", "capability_rpc"] {
                let mut admitted = artifact.clone();
                admitted["payload"]["executable"]["boundary"] = json!(boundary);
                assert_eq!(findings(admitted), [], "{boundary}");
            }
        }
        let mut unknown = fixture(WASM_BOUNDARY_FIXTURE);
        unknown["payload"]["executable"]["boundary"] = json!("python");
        assert_eq!(
            findings(unknown),
            [(
                "customization.validation.invalid_payload".to_string(),
                "payload.executable.boundary".to_string()
            )]
        );
    }
}
