//! `AccessIR` → Cedar policy compiler (T5-AZ-2, #2577).
//!
//! Every [`AccessRule`] lowers to exactly one Cedar `permit`:
//!
//! ```text
//! @id("<ruleId>")
//! permit (
//!   principal is Norn::Actor,
//!   action in [Norn::Action::"<actionId>", ...],
//!   resource is <a::b::Type>
//! )
//! when {
//!   principal.scope == context.scope &&
//!   resource.scope == context.scope &&
//!   context.scope has tenant &&          // one per listed optional dimension
//!   <allOf conjuncts, in order>
//! };
//! ```
//!
//! The first two lines are the **mandatory structural scope conjunct**: they
//! are emitted for every rule and cannot be weakened by the IR. Cedar is
//! default-deny and the compiler emits no `forbid`, so a request is allowed
//! only by a rule whose whole conjunction holds; zero rules deny everything.
//!
//! Identifiers and literals from the IR are never spliced as Cedar syntax:
//! entity references are rendered by `cedar-policy`'s `EntityUid` printer,
//! attribute names and strings as escaped string literals (`resource["role"]`,
//! `context has "name"`), with the same `escape_debug` escaping Cedar's own
//! printer uses.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use cedar_policy::{EntityId, EntityTypeName, EntityUid};
use norn_ir::access::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, CompareOp, ContextCondition,
    EntitlementMatchKind, MarkingCondition, PrincipalAttributeOperand, RelCondition, RelEndpoint,
    ResourceCompareOp, ResourceCondition, ResourceOperand, RoleCondition, ScopeDimension,
};
use norn_projections::{CedarAttribute, CedarPrincipalAttribute, CedarSchemaProjection};

use crate::admit::{CompiledPolicy, ValidatedPolicySet, admit};
use crate::base::AuthzSchema;
use crate::diag::{AuthzCode, AuthzDiagnostic, AuthzError};
use crate::request::EntitlementCatalog;

/// Cedar entity type of every principal.
pub const ACTOR_TYPE: &str = "Norn::Actor";
/// Cedar entity type of roles; an actor is a member of its roles.
pub const ROLE_TYPE: &str = "Norn::Role";
/// Cedar entity type of every action.
pub const ACTION_TYPE: &str = "Norn::Action";
/// The context value, principal attribute, and resource attribute holding a
/// `Norn::Scope`. Reserved: a context condition may not name it.
pub const SCOPE_ATTRIBUTE: &str = "scope";
/// Synthetic resource type used for per-marking authorization.
pub const MARKING_RESOURCE_TYPE: &str = "norn.Marking";
/// Scoped capability and `AccessIR` action for revealing marked values.
pub const REVEAL_ACTION_ID: &str = "norn.reveal";
/// Scoped capability and `AccessIR` action for declassifying marked values.
pub const DECLASSIFY_ACTION_ID: &str = "norn.declassify";
/// Entity literal whose `scope` attribute is typed `Norn::Scope` in the base
/// fragment; used only by the admission-time scope-type probe.
const SCOPE_WITNESS: &str = r#"Norn::ScopeWitness::"scope""#;

/// Compile an `AccessIR` document and admit the result at T2: lower every rule
/// to one Cedar `permit`, then parse and strictly validate the set against
/// `schema`.
///
/// # Errors
///
/// Structural findings (`authz.access.*`) for every malformed rule, reported
/// together; otherwise the admission findings of [`AuthzSchema`] validation
/// (`authz.policy.invalid`, `authz.policy.lint`).
pub fn compile(schema: &AuthzSchema, access: &AccessIr) -> Result<ValidatedPolicySet, AuthzError> {
    compile_all(schema, std::slice::from_ref(access))
}

/// [`compile`] for several `AccessIR` documents admitted together (for
/// example every `policy` artifact of a definition set), into one policy set.
/// Policy ids must be unique across the documents.
///
/// # Errors
///
/// As [`compile`]; a repeated `policyId` is `authz.access.rule_duplicate`.
pub fn compile_all(
    schema: &AuthzSchema,
    documents: &[AccessIr],
) -> Result<ValidatedPolicySet, AuthzError> {
    compile_all_with_projection(schema, documents, None, None)
}

/// Compile several `AccessIR` documents with the metadata emitted by the
/// `EntityIR` → Cedar projection. Resource comparisons and ontology links are
/// admitted only when this metadata identifies their declared kinds.
///
/// # Errors
///
/// As [`compile_all`], plus stable findings for unprojected resource
/// conditions, unsupported relations, and decimal comparisons.
pub fn compile_all_projected(
    schema: &AuthzSchema,
    documents: &[AccessIr],
    projection: &CedarSchemaProjection,
) -> Result<ValidatedPolicySet, AuthzError> {
    let catalog = EntitlementCatalog::from_projection(projection);
    compile_all_with_projection(schema, documents, Some(projection), Some(&catalog))
}

/// Compile projected `AccessIR` and expand its fixed generic entitlement rules
/// against the admitted type/action pairs. Source entitlement rows never
/// create or change Cedar policies.
///
/// # Errors
///
/// Rejects a catalog that differs from the admitted projection, invalid rule
/// templates, or policies that fail Cedar parsing and strict validation.
pub fn compile_all_projected_with_entitlements(
    schema: &AuthzSchema,
    documents: &[AccessIr],
    projection: &CedarSchemaProjection,
    catalog: &EntitlementCatalog,
) -> Result<ValidatedPolicySet, AuthzError> {
    if catalog != &EntitlementCatalog::from_projection(projection) {
        return Err(AuthzError::one(
            AuthzCode::AccessGenericRuleInvalid,
            "entitlement catalog does not match the admitted Cedar projection",
        ));
    }
    compile_all_with_projection(schema, documents, Some(projection), Some(catalog))
}

/// Infer the Cedar types of all trusted principal attributes referenced by
/// resource conditions. The resource property's projected scalar type supplies
/// the value type; `eq`/`ne` infer a scalar and `in` infers a set.
///
/// # Errors
///
/// Returns stable `AccessIR` diagnostics for an absent/unprojected property,
/// decimal or link property, invalid/reserved principal name, unsupported
/// operator, or conflicting uses across policies.
pub fn infer_principal_attributes(
    documents: &[AccessIr],
    projection: &CedarSchemaProjection,
) -> Result<BTreeMap<String, CedarPrincipalAttribute>, AuthzError> {
    infer_principal_attributes_with_projection(documents, Some(projection))
}

fn infer_principal_attributes_with_projection(
    documents: &[AccessIr],
    projection: Option<&CedarSchemaProjection>,
) -> Result<BTreeMap<String, CedarPrincipalAttribute>, AuthzError> {
    let mut inferred: BTreeMap<String, CedarPrincipalAttribute> = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for document in documents {
        if !document.generic_rules.is_empty() {
            for name in ["entitlement_kinds"] {
                if let Some(diagnostic) = register_principal_attribute(
                    &mut inferred,
                    name,
                    CedarPrincipalAttribute::StringSet,
                ) {
                    diagnostics.push(diagnostic.for_policy(&document.policy_id));
                }
            }
        }
        for rule in &document.rules {
            for condition in &rule.all_of {
                let AccessCondition::Resource(resource) = condition else {
                    continue;
                };
                let ResourceOperand::PrincipalAttribute(principal) = &resource.value else {
                    continue;
                };
                let policy = format!("{}/{}", document.policy_id, rule.rule_id);
                match inferred_principal_attribute(
                    rule.resource_type.as_str(),
                    resource,
                    principal,
                    projection,
                ) {
                    Ok((name, expected)) => {
                        if let Some(diagnostic) =
                            register_principal_attribute(&mut inferred, &name, expected)
                        {
                            diagnostics.push(diagnostic.for_policy(policy));
                        }
                    }
                    Err(diagnostic) => diagnostics.push(diagnostic.for_policy(policy)),
                }
            }
        }
    }
    if diagnostics.is_empty() {
        Ok(inferred)
    } else {
        Err(AuthzError::new(diagnostics))
    }
}

fn inferred_principal_attribute(
    resource_type: &str,
    resource: &ResourceCondition,
    principal: &PrincipalAttributeOperand,
    projection: Option<&CedarSchemaProjection>,
) -> Result<(String, CedarPrincipalAttribute), AuthzDiagnostic> {
    let expected = infer_principal_attribute_type(resource_type, resource, projection)?;
    let name = principal.principal_attribute.as_str();
    if !is_context_name(name) {
        return Err(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("principal attribute '{name}' is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"),
        ));
    }
    if matches!(
        name,
        SCOPE_ATTRIBUTE
            | "represents"
            | "acted_by"
            | "entitlement_kinds"
            | "entitlement_roles"
            | "entitlement_groups"
    ) {
        return Err(AuthzDiagnostic::new(
            AuthzCode::AccessPrincipalReserved,
            format!("principal attribute '{name}' is reserved"),
        ));
    }
    Ok((name.to_owned(), expected))
}

fn infer_principal_attribute_type(
    resource_type: &str,
    resource: &ResourceCondition,
    projection: Option<&CedarSchemaProjection>,
) -> Result<CedarPrincipalAttribute, AuthzDiagnostic> {
    if !is_context_name(&resource.name) {
        return Err(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!(
                "resource property '{}' is not an identifier ([A-Za-z_][A-Za-z0-9_]*)",
                resource.name
            ),
        ));
    }
    let Some(projection) = projection else {
        return Err(AuthzDiagnostic::new(
            AuthzCode::AccessProjectionRequired,
            "principal comparisons require EntityIR Cedar projection metadata",
        ));
    };
    let Some(attribute) = projection
        .attributes
        .get(resource_type)
        .and_then(|attributes| attributes.get(&resource.name))
    else {
        return Err(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!(
                "'{resource_type}.{}' is not a projected scalar property",
                resource.name
            ),
        ));
    };
    match (attribute, resource.op) {
        (CedarAttribute::Bool, ResourceCompareOp::Eq | ResourceCompareOp::Ne) => {
            Ok(CedarPrincipalAttribute::Bool)
        }
        (CedarAttribute::Long, ResourceCompareOp::Eq | ResourceCompareOp::Ne) => {
            Ok(CedarPrincipalAttribute::Long)
        }
        (CedarAttribute::String, ResourceCompareOp::Eq | ResourceCompareOp::Ne) => {
            Ok(CedarPrincipalAttribute::String)
        }
        (CedarAttribute::Bool, ResourceCompareOp::In) => Ok(CedarPrincipalAttribute::BoolSet),
        (CedarAttribute::Long, ResourceCompareOp::In) => Ok(CedarPrincipalAttribute::LongSet),
        (CedarAttribute::String, ResourceCompareOp::In) => Ok(CedarPrincipalAttribute::StringSet),
        (CedarAttribute::Decimal, _) => Err(AuthzDiagnostic::new(
            AuthzCode::AccessDecimalComparison,
            format!(
                "decimal property '{resource_type}.{}' cannot be compared to a principal attribute",
                resource.name
            ),
        )),
        (CedarAttribute::Link { .. }, _) => Err(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!(
                "resource property '{resource_type}.{}' is a link, not a scalar",
                resource.name
            ),
        )),
        (
            _,
            ResourceCompareOp::Lt
            | ResourceCompareOp::Le
            | ResourceCompareOp::Gt
            | ResourceCompareOp::Ge,
        ) => Err(AuthzDiagnostic::new(
            AuthzCode::AccessPrincipalOperator,
            "principal operands support only `eq`, `ne`, and `in`",
        )),
    }
}

fn register_principal_attribute(
    inferred: &mut BTreeMap<String, CedarPrincipalAttribute>,
    name: &str,
    expected: CedarPrincipalAttribute,
) -> Option<AuthzDiagnostic> {
    match inferred.get(name) {
        Some(previous) if *previous != expected => Some(AuthzDiagnostic::new(
            AuthzCode::AccessPrincipalConflict,
            format!(
                "principal attribute '{name}' is inferred as both {previous:?} and {expected:?}"
            ),
        )),
        Some(_) => None,
        None => {
            inferred.insert(name.to_owned(), expected);
            None
        }
    }
}

fn compile_all_with_projection(
    schema: &AuthzSchema,
    documents: &[AccessIr],
    projection: Option<&CedarSchemaProjection>,
    catalog: Option<&EntitlementCatalog>,
) -> Result<ValidatedPolicySet, AuthzError> {
    let principal_attributes = infer_principal_attributes_with_projection(documents, projection)?;
    if let Some(projection) = projection
        && projection.principal_attributes != principal_attributes
    {
        return Err(AuthzError::one(
            AuthzCode::AccessPrincipalProjectionRequired,
            format!(
                "Cedar projection principal attributes {:?} do not match inferred attributes {:?}",
                projection.principal_attributes, principal_attributes
            ),
        ));
    }

    let mut diagnostics = Vec::new();
    let mut compiled = Vec::new();
    let mut policy_ids = BTreeSet::new();
    for access in documents {
        if !policy_ids.insert(access.policy_id.as_str()) {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessRuleDuplicate,
                format!("policy id '{}' is declared twice", access.policy_id),
            ));
            continue;
        }
        lower_document(access, projection, catalog, &mut compiled, &mut diagnostics);
    }
    if !diagnostics.is_empty() {
        return Err(AuthzError::new(diagnostics));
    }
    admit(schema, compiled)
}

fn lower_document(
    access: &AccessIr,
    projection: Option<&CedarSchemaProjection>,
    catalog: Option<&EntitlementCatalog>,
    compiled: &mut Vec<CompiledPolicy>,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) {
    if access.policy_id.trim().is_empty() {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessIdEmpty,
            "policyId is empty",
        ));
    }
    let mut seen = BTreeSet::new();
    for rule in &access.rules {
        let id = format!("{}/{}", access.policy_id, rule.rule_id);
        if !seen.insert(rule.rule_id.as_str()) {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessRuleDuplicate,
                    format!("rule id '{}' is declared twice", rule.rule_id),
                )
                .for_policy(id.as_str()),
            );
            continue;
        }
        if rule
            .all_of
            .iter()
            .any(|condition| matches!(condition, AccessCondition::Entitlement(_)))
        {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessGenericRuleInvalid,
                    "entitlement conditions are only valid in the three fixed generic rules",
                )
                .for_policy(id.as_str()),
            );
            continue;
        }
        let mut rule_diagnostics = Vec::new();
        let (text, scope_probe) = lower_rule(rule, projection, &mut rule_diagnostics);
        if rule_diagnostics.is_empty() {
            compiled.push(CompiledPolicy {
                id,
                source_policy_id: access.policy_id.clone(),
                source_rule_id: rule.rule_id.clone(),
                text,
                scope_probe,
                uses_entitlements: false,
            });
        } else {
            diagnostics.extend(
                rule_diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.for_policy(id.as_str())),
            );
        }
    }
    lower_generic_rules(
        access,
        projection,
        catalog,
        &mut seen,
        compiled,
        diagnostics,
    );
}

fn lower_generic_rules<'a>(
    access: &'a AccessIr,
    projection: Option<&CedarSchemaProjection>,
    catalog: Option<&EntitlementCatalog>,
    seen: &mut BTreeSet<&'a str>,
    compiled: &mut Vec<CompiledPolicy>,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) {
    for rule in &access.generic_rules {
        let id = format!("{}/{}", access.policy_id, rule.rule_id);
        if !seen.insert(rule.rule_id.as_str()) {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessRuleDuplicate,
                    format!("rule id '{}' is declared twice", rule.rule_id),
                )
                .for_policy(id.as_str()),
            );
            continue;
        }
        let Some(catalog) = catalog else {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessProjectionRequired,
                    "generic entitlement rules require an admitted type/action catalog",
                )
                .for_policy(id.as_str()),
            );
            continue;
        };
        let Some((condition_kind, expected_id)) = (match rule.all_of.as_slice() {
            [AccessCondition::Entitlement(EntitlementMatchKind::None)] => {
                Some(("none", "entitlement.none"))
            }
            [AccessCondition::Entitlement(EntitlementMatchKind::OwnerOnly)] => {
                Some(("ownerOnly", "entitlement.owner_only"))
            }
            [AccessCondition::Entitlement(EntitlementMatchKind::LinkedRecord)] => {
                Some(("linkedRecord", "entitlement.linked_record"))
            }
            _ => None,
        }) else {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessGenericRuleInvalid,
                    "generic entitlement rules require exactly one closed entitlement condition",
                )
                .for_policy(id.as_str()),
            );
            continue;
        };
        if rule.rule_id != expected_id {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessGenericRuleInvalid,
                    format!("generic entitlement rule id must be '{expected_id}' for kind '{condition_kind}'"),
                )
                .for_policy(id.as_str()),
            );
            continue;
        }
        if projection.is_none() {
            diagnostics.push(
                AuthzDiagnostic::new(
                    AuthzCode::AccessProjectionRequired,
                    "generic entitlement rules require Cedar projection metadata",
                )
                .for_policy(id.as_str()),
            );
            continue;
        }
        for (resource_type, action_ids) in &catalog.resource_actions {
            if action_ids.is_empty() {
                continue;
            }
            let expansion_id = format!("{}@{}", rule.rule_id, resource_type.as_str());
            let lowered = AccessRule {
                rule_id: expansion_id.clone(),
                actions: action_ids.clone(),
                resource_type: resource_type.clone(),
                scope: rule.scope.clone(),
                all_of: rule.all_of.clone(),
            };
            let cedar_id = format!("{}/{}", access.policy_id, expansion_id);
            let mut rule_diagnostics = Vec::new();
            let (text, scope_probe) = lower_rule(&lowered, projection, &mut rule_diagnostics);
            if rule_diagnostics.is_empty() {
                compiled.push(CompiledPolicy {
                    id: cedar_id.clone(),
                    source_policy_id: access.policy_id.clone(),
                    source_rule_id: rule.rule_id.clone(),
                    text,
                    scope_probe,
                    uses_entitlements: true,
                });
            } else {
                diagnostics.extend(
                    rule_diagnostics
                        .into_iter()
                        .map(|diagnostic| diagnostic.for_policy(cedar_id.as_str())),
                );
            }
        }
    }
}

/// Lower one rule to its policy text and its scope-type probe. Findings go to
/// `diagnostics`; the texts are meaningful only when none were pushed.
fn lower_rule(
    rule: &AccessRule,
    projection: Option<&CedarSchemaProjection>,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> (String, String) {
    if rule.rule_id.trim().is_empty() {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessIdEmpty,
            "ruleId is empty",
        ));
    }
    if rule.actions.is_empty() {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessActionsEmpty,
            "a rule must permit at least one action",
        ));
    }
    let actions: Vec<String> = rule
        .actions
        .iter()
        .filter_map(|action| entity_uid(ACTION_TYPE, action, diagnostics))
        .collect();
    let resource = cedar_type_name(rule.resource_type.as_str(), diagnostics);

    let mut conjuncts = scope_conjuncts(&rule.scope.dimensions, diagnostics);
    let has_marking_condition = rule
        .all_of
        .iter()
        .any(|condition| matches!(condition, AccessCondition::Marking(_)));
    if has_marking_condition {
        if rule.resource_type.as_str() != MARKING_RESOURCE_TYPE {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessMarkingResource,
                format!("a marking condition requires resourceType '{MARKING_RESOURCE_TYPE}'"),
            ));
        }
        if rule
            .actions
            .iter()
            .any(|action| !matches!(action.as_str(), REVEAL_ACTION_ID | DECLASSIFY_ACTION_ID))
        {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessMarkingAction,
                format!(
                    "a marking condition is only valid for '{REVEAL_ACTION_ID}' or '{DECLASSIFY_ACTION_ID}'"
                ),
            ));
        }
    }
    for condition in &rule.all_of {
        if let Some(conjunct) = lower_condition(
            condition,
            rule.resource_type.as_str(),
            projection,
            diagnostics,
        ) {
            conjuncts.push(conjunct);
        }
    }

    let head = format!(
        "permit (\n  principal is {ACTOR_TYPE},\n  action in [{actions}],\n  resource is {resource}\n)",
        actions = actions.join(", "),
        resource = resource.unwrap_or_default(),
    );
    let text = format!(
        "@id({id})\n{head}\nwhen {{\n  {when}\n}};\n",
        id = string_literal(&rule.rule_id),
        when = conjuncts.join(" &&\n  "),
    );
    (text, scope_type_probe(&head))
}

/// A validation-only policy over the same head that type-checks only when the
/// principal, resource, and context `scope` are all exactly `Norn::Scope`:
/// each is compared with the base fragment's `Norn::ScopeWitness` attribute,
/// and strict validation rejects `==` between different types. Never
/// admitted or evaluated.
fn scope_type_probe(head: &str) -> String {
    let witness = format!("{SCOPE_WITNESS}.{SCOPE_ATTRIBUTE}");
    let when = ["principal", "resource", "context"]
        .map(|subject| format!("{subject}.{SCOPE_ATTRIBUTE} == {witness}"))
        .join(" &&\n  ");
    format!("{head}\nwhen {{\n  {when}\n}};\n")
}

/// The mandatory structural scope conjunct: exact three-way scope equality,
/// then presence of every listed optional dimension in the request scope.
fn scope_conjuncts(
    dimensions: &[ScopeDimension],
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Vec<String> {
    let listed: BTreeSet<ScopeDimension> = dimensions.iter().copied().collect();
    if !listed.contains(&ScopeDimension::Org) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessScopeOrgRequired,
            "the scope conjunct must list `org`",
        ));
    }
    let mut conjuncts = vec![
        format!("principal.{SCOPE_ATTRIBUTE} == context.{SCOPE_ATTRIBUTE}"),
        format!("resource.{SCOPE_ATTRIBUTE} == context.{SCOPE_ATTRIBUTE}"),
    ];
    conjuncts.extend(
        listed
            .into_iter()
            .filter_map(optional_dimension_attribute)
            .map(|attribute| format!("context.{SCOPE_ATTRIBUTE} has {attribute}")),
    );
    conjuncts
}

/// The `Norn::Scope` attribute of an optional dimension; `None` for `org`,
/// which the record type already requires.
fn optional_dimension_attribute(dimension: ScopeDimension) -> Option<&'static str> {
    match dimension {
        ScopeDimension::Org => None,
        ScopeDimension::Tenant => Some("tenant"),
        ScopeDimension::Workspace => Some("workspace"),
        ScopeDimension::Environment => Some("environment"),
        ScopeDimension::BusinessUnit => Some("businessUnit"),
    }
}

fn lower_condition(
    condition: &AccessCondition,
    resource_type: &str,
    projection: Option<&CedarSchemaProjection>,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    match condition {
        AccessCondition::Role(role) => lower_role(role, diagnostics),
        AccessCondition::Rel(rel) => lower_rel(rel, resource_type, projection, diagnostics),
        AccessCondition::Context(context) => lower_context(context, diagnostics),
        AccessCondition::Resource(resource) => {
            lower_resource(resource, resource_type, projection, diagnostics)
        }
        AccessCondition::Marking(marking) => lower_marking(marking, resource_type, diagnostics),
        AccessCondition::Entitlement(kind) => Some(lower_entitlement_kind(*kind)),
    }
}

/// Semantic admission of the IR's `MARKING_ID_PATTERN` at the authorization
/// boundary, including directly constructed policies and PDP requests.
pub(crate) fn canonical_marking_id(value: &str) -> bool {
    value.split('.').all(|segment| {
        segment
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

/// A marking condition is compiled only against the synthetic, scoped marking
/// resource. Its `id` is data, never Cedar syntax.
fn lower_marking(
    marking: &MarkingCondition,
    resource_type: &str,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    if resource_type != MARKING_RESOURCE_TYPE {
        return None;
    }
    if marking.any_of.is_empty() {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessMarkingsEmpty,
            "a marking condition must name at least one marking id",
        ));
        return None;
    }
    let mut comparisons = Vec::with_capacity(marking.any_of.len());
    for id in &marking.any_of {
        let id = id.as_str();
        if !canonical_marking_id(id) {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessMarkingIdInvalid,
                "a marking condition requires a canonical lowercase dotted marking id",
            ));
            continue;
        }
        comparisons.push(format!(
            "resource[{}] == {}",
            string_literal("id"),
            string_literal(id)
        ));
    }
    if comparisons.is_empty() {
        return None;
    }
    Some(format!(
        "(resource has {} && ({}))",
        string_literal("id"),
        comparisons.join(" || ")
    ))
}

/// This reserved principal set is populated only by the PDP from its typed
/// entitlement snapshot; caller-supplied values are rejected before Cedar.
fn lower_entitlement_kind(kind: EntitlementMatchKind) -> String {
    let value = match kind {
        EntitlementMatchKind::None => "none",
        EntitlementMatchKind::OwnerOnly => "ownerOnly",
        EntitlementMatchKind::LinkedRecord => "linkedRecord",
    };
    format!(
        "(principal has \"entitlement_kinds\" && principal[\"entitlement_kinds\"].contains({}))",
        string_literal(value)
    )
}

/// `principal in [Norn::Role::"a", ...]`: membership in any listed role.
fn lower_role(role: &RoleCondition, diagnostics: &mut Vec<AuthzDiagnostic>) -> Option<String> {
    if role.any_of.is_empty() {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessRolesEmpty,
            "a role condition must name at least one role",
        ));
        return None;
    }
    let mut roles = Vec::with_capacity(role.any_of.len());
    for name in &role.any_of {
        if name.is_empty() {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessIdEmpty,
                "a role id is empty",
            ));
        } else if let Some(uid) = entity_uid(ROLE_TYPE, name, diagnostics) {
            roles.push(uid);
        }
    }
    Some(format!("principal in [{}]", roles.join(", ")))
}

/// Directional `rel(from, role)`: the other endpoint is (a member of) the
/// entity or entity set reached from `from` by the link role. `in` covers
/// both a single-entity and a set-valued role.
fn lower_rel(
    rel: &RelCondition,
    resource_type: &str,
    projection: Option<&CedarSchemaProjection>,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    let role = rel.role.as_str();
    if !is_member_id(role) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("link role '{role}' is not a member id ([a-z][a-z0-9_]*)"),
        ));
        return None;
    }
    let attribute = string_literal(role);
    let Some(projection) = projection else {
        let (start, other) = match rel.from {
            RelEndpoint::Resource => ("resource", "principal"),
            RelEndpoint::Principal => ("principal", "resource"),
        };
        return Some(format!(
            "({start} has {attribute} && {other} in {start}[{attribute}])"
        ));
    };

    if role == "acted_by" && rel.from == RelEndpoint::Resource {
        // #2950's host-maintained relation remains a set of Actor entities.
        return Some(format!(
            "(resource has {attribute} && principal in resource[{attribute}])"
        ));
    }
    if rel.from != RelEndpoint::Resource {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessRelUnsupported,
            "projected rel conditions currently require a resource-side ontology link",
        ));
        return None;
    }
    let Some(CedarAttribute::Link { many }) = projection
        .attributes
        .get(resource_type)
        .and_then(|attributes| attributes.get(role))
    else {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("'{resource_type}.{role}' is not a projected ontology link"),
        ));
        return None;
    };
    let represents = string_literal("represents");
    let relation_test = if *many {
        format!("resource[{attribute}].contains(principal[{represents}])")
    } else {
        format!("resource[{attribute}] == principal[{represents}]")
    };
    Some(format!(
        "(resource has {attribute} && principal has {represents} && {relation_test})"
    ))
}

/// `context.<name> <op> <literal>` over a typed context value.
fn lower_context(
    context: &ContextCondition,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    let name = context.name.as_str();
    if !is_context_name(name) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("context name '{name}' is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"),
        ));
        return None;
    }
    if name == SCOPE_ATTRIBUTE {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessContextReserved,
            "context `scope` is the structural scope conjunct; it is not a condition",
        ));
        return None;
    }
    Some(lower_comparison("context", context))
}

/// `resource.<name> <op> <operand>` over a declared projected scalar.
fn lower_resource(
    resource: &ResourceCondition,
    resource_type: &str,
    projection: Option<&CedarSchemaProjection>,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    let name = resource.name.as_str();
    if !is_context_name(name) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("resource property '{name}' is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"),
        ));
        return None;
    }
    let Some(projection) = projection else {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessProjectionRequired,
            "resource comparisons require EntityIR Cedar projection metadata",
        ));
        return None;
    };
    match projection
        .attributes
        .get(resource_type)
        .and_then(|attributes| attributes.get(name))
    {
        Some(CedarAttribute::Decimal) => {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessDecimalComparison,
                format!("decimal property '{resource_type}.{name}' cannot be compared in AccessIR"),
            ));
            None
        }
        Some(CedarAttribute::Bool | CedarAttribute::Long | CedarAttribute::String) => {
            match &resource.value {
                ResourceOperand::Literal(value) => {
                    let Some(op) = resource_literal_op(resource.op) else {
                        diagnostics.push(AuthzDiagnostic::new(
                            AuthzCode::AccessResourceOperator,
                            "`in` requires a principal attribute operand",
                        ));
                        return None;
                    };
                    Some(lower_typed_comparison(
                        "resource",
                        &resource.name,
                        op,
                        value,
                    ))
                }
                ResourceOperand::PrincipalAttribute(principal) => {
                    lower_principal_comparison(resource, principal, diagnostics)
                }
            }
        }
        Some(CedarAttribute::Link { .. }) => {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessNameInvalid,
                format!("resource property '{resource_type}.{name}' is a link, not a scalar"),
            ));
            None
        }
        None => {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessNameInvalid,
                format!("resource property '{resource_type}.{name}' is not projected"),
            ));
            None
        }
    }
}

fn lower_comparison(subject: &str, condition: &ContextCondition) -> String {
    let op = match condition.op {
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Lt => "<",
        CompareOp::Le => "<=",
        CompareOp::Gt => ">",
        CompareOp::Ge => ">=",
    };
    lower_typed_comparison(subject, &condition.name, op, &condition.value)
}

fn lower_typed_comparison(subject: &str, name: &str, op: &str, value: &AccessLiteral) -> String {
    let value = match value {
        AccessLiteral::Bool(value) => value.to_string(),
        AccessLiteral::Long(value) => value.to_string(),
        AccessLiteral::String(value) => string_literal(value),
    };
    let attribute = string_literal(name);
    format!("({subject} has {attribute} && {subject}[{attribute}] {op} {value})")
}

fn resource_literal_op(op: ResourceCompareOp) -> Option<&'static str> {
    Some(match op {
        ResourceCompareOp::Eq => "==",
        ResourceCompareOp::Ne => "!=",
        ResourceCompareOp::Lt => "<",
        ResourceCompareOp::Le => "<=",
        ResourceCompareOp::Gt => ">",
        ResourceCompareOp::Ge => ">=",
        ResourceCompareOp::In => return None,
    })
}

fn lower_principal_comparison(
    resource: &ResourceCondition,
    principal: &PrincipalAttributeOperand,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    let name = principal.principal_attribute.as_str();
    if !is_context_name(name) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("principal attribute '{name}' is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"),
        ));
        return None;
    }
    if matches!(
        name,
        SCOPE_ATTRIBUTE
            | "represents"
            | "acted_by"
            | "entitlement_kinds"
            | "entitlement_roles"
            | "entitlement_groups"
    ) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessPrincipalReserved,
            format!("principal attribute '{name}' is reserved"),
        ));
        return None;
    }
    let resource_name = string_literal(&resource.name);
    let principal_name = string_literal(name);
    let relation = match resource.op {
        ResourceCompareOp::Eq => {
            format!("principal[{principal_name}] == resource[{resource_name}]")
        }
        ResourceCompareOp::Ne => {
            format!("principal[{principal_name}] != resource[{resource_name}]")
        }
        ResourceCompareOp::In => {
            format!("principal[{principal_name}].contains(resource[{resource_name}])")
        }
        ResourceCompareOp::Lt
        | ResourceCompareOp::Le
        | ResourceCompareOp::Gt
        | ResourceCompareOp::Ge => {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessPrincipalOperator,
                "principal operands support only `eq`, `ne`, and `in`",
            ));
            return None;
        }
    };
    Some(format!(
        "(resource has {resource_name} && principal has {principal_name} && {relation})"
    ))
}

/// `a.b.C` → the Cedar entity type `a::b::C`, checked by Cedar's own parser
/// (which also rejects Cedar-reserved segments).
pub(crate) fn cedar_type_name(
    object_type: &str,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    if !is_object_type_name(object_type) {
        diagnostics.push(AuthzDiagnostic::new(
            AuthzCode::AccessNameInvalid,
            format!("resource type '{object_type}' is not an object type name (domain.Type)"),
        ));
        return None;
    }
    let cedar = object_type.replace('.', "::");
    match EntityTypeName::from_str(&cedar) {
        Ok(name) => Some(name.to_string()),
        Err(error) => {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessNameInvalid,
                format!("resource type '{object_type}' is not a Cedar type name: {error}"),
            ));
            None
        }
    }
}

/// `Type::"id"` rendered by `cedar-policy` (escaped id).
pub(crate) fn entity_uid(
    entity_type: &str,
    id: &str,
    diagnostics: &mut Vec<AuthzDiagnostic>,
) -> Option<String> {
    match EntityTypeName::from_str(entity_type) {
        Ok(type_name) => {
            Some(EntityUid::from_type_name_and_id(type_name, EntityId::new(id)).to_string())
        }
        Err(error) => {
            diagnostics.push(AuthzDiagnostic::new(
                AuthzCode::AccessNameInvalid,
                format!("entity type '{entity_type}': {error}"),
            ));
            None
        }
    }
}

/// A Cedar string literal, escaped exactly as Cedar's printer escapes one
/// (`escape_debug`, `cedar-policy-core` `ast/literal.rs`).
pub(crate) fn string_literal(value: &str) -> String {
    format!("\"{}\"", value.escape_debug())
}

/// `EntityIR` v2 object type name: one or more `[a-z][a-z0-9_]*` domain
/// segments, then a `[A-Z][A-Za-z0-9]*` local name, joined by `.`.
fn is_object_type_name(value: &str) -> bool {
    match value.rsplit_once('.') {
        Some((domain, local)) => {
            let mut chars = local.chars();
            domain.split('.').all(is_member_id)
                && chars.next().is_some_and(|first| first.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// `[a-z][a-z0-9_]*`, the `EntityIR` v2 member id grammar.
fn is_member_id(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
pub(crate) fn is_context_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
