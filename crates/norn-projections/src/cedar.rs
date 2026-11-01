//! Cedar schema projection and its loss report (T1-PROJ-3 #2530).
//!
//! Object types are emitted in Cedar namespaces. Every projected entity and
//! the Norn actor carry the required `scope`; business properties and links
//! are optional because Cedar schema attributes cannot encode Norn's
//! nullability and default semantics without changing authorization behavior.
//! Inherited declarations are flattened onto each subtype. A link reference is
//! represented by a `{ type: String, key: String }` record (or a set of those
//! records), so the Cedar schema does not constrain the target entity type.
//!
//! `CedarSchemaProjection::attributes` preserves Norn-specific type metadata
//! for callers that compile Cedar attributes. Exact decimals are rendered as
//! strings; callers must not apply numeric comparisons to them. Unsupported
//! semantics are listed in the sorted, deduplicated loss report.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_ir::{
    ActionEffectProgram, ActionRoot, Link, Multiplicity, ObjectTypeDef, ObjectTypeName, Property,
    PropertyConstraints, PropertyType, PropertyTypeDef,
};

/// Host-maintained attributes every projected object type carries (#2950):
/// `acted_by`, the actors whose action calls committed on the object. Optional:
/// an object no action touched has none.
pub const HOST_ATTRIBUTES: &str = "acted_by?: Set<Norn::Actor>";

/// The host resources and system actions of a dev cell, as a second Cedar
/// fragment composed with [`project_cedar_schema`]'s (#2950).
#[must_use]
pub fn project_host_access_schema(object_types: &[ObjectTypeDef]) -> String {
    let mut resources: BTreeSet<String> = object_types
        .iter()
        .filter_map(|object_type| cedar_entity_name(object_type.name.as_str()))
        .collect();
    resources.insert("norn::Ledger".to_owned());
    resources.insert("norn::Definitions".to_owned());
    let admin_action = if resources.contains("norn::CommandTask") {
        "  action \"norn.admin\" appliesTo {\n    principal: [Actor],\n    resource: [norn::CommandTask],\n    context: {\n      scope: Scope\n    }\n  };\n"
    } else {
        ""
    };
    let resources: Vec<String> = resources.into_iter().collect();
    format!(
        "namespace norn {{\n  entity Ledger = {{\n    scope: Norn::Scope\n  }};\n  entity Definitions = {{\n    scope: Norn::Scope\n  }};\n  entity Marking = {{\n    scope: Norn::Scope,\n    id: String\n  }};\n  entity Source = {{\n    scope: Norn::Scope\n  }};\n}}\nnamespace Norn {{\n  action \"norn.read\" appliesTo {{\n    principal: [Actor],\n    resource: [{}],\n    context: {{\n      scope: Scope\n    }}\n  }};\n{}  action \"norn.observe\" appliesTo {{\n    principal: [Actor],\n    resource: [norn::Source],\n    context: {{\n      scope: Scope\n    }}\n  }};\n  action \"norn.reveal\" appliesTo {{\n    principal: [Actor],\n    resource: [norn::Marking],\n    context: {{\n      scope: Scope\n    }}\n  }};\n  action \"norn.declassify\" appliesTo {{\n    principal: [Actor],\n    resource: [norn::Marking],\n    context: {{\n      scope: Scope\n    }}\n  }};\n}}\n",
        resources.join(", "),
        admin_action
    )
}

/// One thing the projection did not represent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CedarLoss {
    /// What was not projected (`<type>.<member>`, or an action id).
    pub subject: String,
    /// Why.
    pub reason: &'static str,
}

/// Cedar-facing metadata for an emitted business attribute.
///
/// `Decimal` is a String in the Cedar schema, but remains distinct here so an
/// adapter can reject Cedar comparisons that would treat exact decimals as
/// lexical strings. `Link` identifies the generic entity-reference record
/// shape; the `many` bit distinguishes a record from a set of records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CedarAttribute {
    /// A Boolean value.
    Bool,
    /// A signed 64-bit integer.
    Long,
    /// A string-valued Norn property.
    String,
    /// An exact decimal encoded as a Cedar String.
    Decimal,
    /// A generic `{ type, key }` entity reference or a set of references.
    Link {
        /// Whether the value is a set of references.
        many: bool,
    },
}

/// The Cedar type of a trusted principal attribute referenced by `AccessIR`.
/// Decimal and link values are intentionally absent: they cannot be compared
/// with a projected resource scalar without changing Norn semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CedarPrincipalAttribute {
    /// A boolean scalar.
    Bool,
    /// A signed 64-bit integer scalar.
    Long,
    /// A string scalar.
    String,
    /// A set of booleans.
    BoolSet,
    /// A set of signed 64-bit integers.
    LongSet,
    /// A set of strings.
    StringSet,
}

/// A projected Cedar schema fragment and its loss report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CedarSchemaProjection {
    /// The fragment, deterministic for the same definitions.
    pub schema: String,
    /// Everything not represented, sorted and deduplicated.
    pub losses: Vec<CedarLoss>,
    /// Cedar type metadata by `EntityIR` object type name (dotted) and attribute
    /// name. The fixed `scope`, host-owned `acted_by`, and `Norn::Actor` fields
    /// are omitted.
    pub attributes: BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    /// Inferred or explicitly supplied trusted attributes emitted on
    /// `Norn::Actor`.
    pub principal_attributes: BTreeMap<String, CedarPrincipalAttribute>,
    /// Admitted object types supplied to the projection, sorted by name.
    pub object_types: Vec<ObjectTypeName>,
    /// Action ids applicable to each admitted resource type. This mirrors the
    /// exact target-plus-subtype set emitted in each Cedar `appliesTo` block.
    pub resource_actions: BTreeMap<ObjectTypeName, Vec<String>>,
}

/// Project with no shared property type catalog.
///
/// Use [`project_cedar_schema_with_catalog`] when properties may use `ref`.
#[must_use]
pub fn project_cedar_schema(
    object_types: &[ObjectTypeDef],
    interfaces: &[norn_ir::InterfaceDef],
    actions: &[ActionRoot],
) -> CedarSchemaProjection {
    project_cedar_schema_with_catalog_and_principal_attributes(
        object_types,
        interfaces,
        &[],
        actions,
        &BTreeMap::new(),
    )
}

/// Project `EntityIR` v2 object types and `ActionIR` actions, adding the
/// declared trusted principal attributes to `Norn::Actor`.
#[must_use]
pub fn project_cedar_schema_with_principal_attributes(
    object_types: &[ObjectTypeDef],
    interfaces: &[norn_ir::InterfaceDef],
    actions: &[ActionRoot],
    principal_attributes: &BTreeMap<String, CedarPrincipalAttribute>,
) -> CedarSchemaProjection {
    project_cedar_schema_with_catalog_and_principal_attributes(
        object_types,
        interfaces,
        &[],
        actions,
        principal_attributes,
    )
}

/// Project `EntityIR` v2 object types and `ActionIR` actions into a Cedar
/// schema fragment. Property type references resolve through `property_types`.
///
/// Object type inheritance is flattened. Forward and inverse links are
/// optional Cedar attributes with a generic `{ type: String, key: String }`
/// value; `Many` multiplicities become sets. The `Norn::Actor` gets an
/// optional `represents` attribute with that same reference shape plus any
/// supplied typed principal attributes. Every
/// action's resource is its effect target type plus every projected subtype.
/// Only the structural `scope` value is typed in action context.
#[must_use]
pub fn project_cedar_schema_with_catalog(
    object_types: &[ObjectTypeDef],
    interfaces: &[norn_ir::InterfaceDef],
    property_types: &[PropertyTypeDef],
    actions: &[ActionRoot],
) -> CedarSchemaProjection {
    project_cedar_schema_with_catalog_and_principal_attributes(
        object_types,
        interfaces,
        property_types,
        actions,
        &BTreeMap::new(),
    )
}

/// Project `EntityIR` v2 and `ActionIR`, with the inferred trusted principal
/// attributes emitted as optional attributes on `Norn::Actor`.
#[must_use]
pub fn project_cedar_schema_with_catalog_and_principal_attributes(
    object_types: &[ObjectTypeDef],
    interfaces: &[norn_ir::InterfaceDef],
    property_types: &[PropertyTypeDef],
    actions: &[ActionRoot],
    principal_attributes: &BTreeMap<String, CedarPrincipalAttribute>,
) -> CedarSchemaProjection {
    let mut losses = BTreeSet::new();
    let types = object_type_index(object_types, &mut losses);
    let (catalog, duplicate_catalog_names) = property_type_catalog(property_types);
    let mut attributes =
        project_object_attributes(&types, &catalog, &duplicate_catalog_names, &mut losses);
    project_inverse_attributes(&types, interfaces, &mut attributes, &mut losses);

    let principal_attributes = project_principal_attributes(principal_attributes, &mut losses);

    let schema = render_schema(
        &types,
        &attributes,
        &principal_attributes,
        actions,
        &mut losses,
    );

    CedarSchemaProjection {
        schema,
        losses: losses.into_iter().collect(),
        attributes,
        principal_attributes,
        object_types: types
            .by_name
            .keys()
            .map(|name| ObjectTypeName((*name).to_owned()))
            .collect(),
        resource_actions: project_resource_actions(&types, actions),
    }
}

fn project_resource_actions(
    types: &CedarObjectTypes<'_>,
    actions: &[ActionRoot],
) -> BTreeMap<ObjectTypeName, Vec<String>> {
    let mut pairs: BTreeMap<ObjectTypeName, BTreeSet<String>> = BTreeMap::new();
    for action in actions {
        let Some(program) = &action.effect_program else {
            continue;
        };
        let target_type = program.target().object_kind;
        for (object_type, lineage) in &types.lineages {
            if types.cedar_names.contains_key(*object_type)
                && lineage
                    .iter()
                    .any(|ancestor| ancestor.name.as_str() == target_type)
            {
                pairs
                    .entry(ObjectTypeName((*object_type).to_owned()))
                    .or_default()
                    .insert(action.action_id.clone());
            }
        }
    }
    pairs
        .into_iter()
        .map(|(object_type, action_ids)| (object_type, action_ids.into_iter().collect()))
        .collect()
}

struct CedarObjectTypes<'a> {
    all_names: BTreeSet<&'a str>,
    by_name: BTreeMap<&'a str, &'a ObjectTypeDef>,
    cedar_names: BTreeMap<&'a str, String>,
    namespaces: BTreeMap<String, BTreeSet<String>>,
    lineages: BTreeMap<&'a str, Vec<&'a ObjectTypeDef>>,
}

fn object_type_index<'a>(
    object_types: &'a [ObjectTypeDef],
    losses: &mut BTreeSet<CedarLoss>,
) -> CedarObjectTypes<'a> {
    let mut grouped_types: BTreeMap<&str, Vec<&ObjectTypeDef>> = BTreeMap::new();
    for object_type in object_types {
        grouped_types
            .entry(object_type.name.as_str())
            .or_default()
            .push(object_type);
    }

    let mut by_name: BTreeMap<&str, &ObjectTypeDef> = BTreeMap::new();
    for (name, definitions) in &grouped_types {
        if definitions.len() == 1 {
            if let Some(definition) = definitions.first() {
                by_name.insert(name, *definition);
            }
        } else {
            add_loss(
                losses,
                (*name).to_owned(),
                "duplicate object type name is ambiguous",
            );
        }
    }

    let mut cedar_names = BTreeMap::new();
    let mut namespaces: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, object_type) in &by_name {
        let Some(cedar_name) = cedar_entity_name(name) else {
            add_loss(
                losses,
                (*name).to_owned(),
                "object type name cannot be represented as a Cedar entity type",
            );
            continue;
        };
        let Some((namespace, local)) = cedar_name.rsplit_once("::") else {
            add_loss(
                losses,
                (*name).to_owned(),
                "object type name has no Cedar namespace",
            );
            continue;
        };
        namespaces
            .entry(namespace.to_owned())
            .or_default()
            .insert(local.to_owned());
        cedar_names.insert(*name, cedar_name);
        report_object_losses(object_type, losses);
    }

    let mut lineages = BTreeMap::new();
    for name in by_name.keys().copied() {
        let (lineage, issue) = object_lineage(name, &by_name);
        if let Some(issue) = issue {
            add_loss(losses, issue.subject, issue.reason);
        }
        lineages.insert(name, lineage);
    }

    CedarObjectTypes {
        all_names: grouped_types.keys().copied().collect(),
        by_name,
        cedar_names,
        namespaces,
        lineages,
    }
}

fn project_object_attributes(
    types: &CedarObjectTypes<'_>,
    catalog: &BTreeMap<&str, &PropertyTypeDef>,
    duplicate_catalog_names: &BTreeSet<&str>,
    losses: &mut BTreeSet<CedarLoss>,
) -> BTreeMap<String, BTreeMap<String, CedarAttribute>> {
    let mut attributes = BTreeMap::new();
    for name in types.cedar_names.keys() {
        let Some(lineage) = types.lineages.get(name).map(Vec::as_slice) else {
            continue;
        };
        let mut effective_properties: BTreeMap<&str, &Property> = BTreeMap::new();
        let mut effective_links: BTreeMap<&str, &Link> = BTreeMap::new();
        for owner in lineage {
            for property in &owner.properties {
                effective_properties.insert(property.id.as_str(), property);
            }
            for link in &owner.links {
                effective_links.insert(link.id.as_str(), link);
            }
        }

        let mut projected = BTreeMap::new();
        for (property_id, property) in effective_properties {
            let subject = format!("{name}.{property_id}");
            if let Some(attribute) =
                project_property(&subject, property, catalog, duplicate_catalog_names, losses)
            {
                insert_attribute(&mut projected, property_id, attribute, name, losses);
            }
        }
        for (link_id, link) in effective_links {
            let subject = format!("{name}.{link_id}");
            report_link_losses(&subject, link, losses);
            insert_attribute(
                &mut projected,
                link_id,
                CedarAttribute::Link {
                    many: link.cardinality.to == Multiplicity::Many,
                },
                name,
                losses,
            );
        }
        attributes.insert((*name).to_owned(), projected);
    }
    attributes
}

fn project_inverse_attributes(
    types: &CedarObjectTypes<'_>,
    interfaces: &[norn_ir::InterfaceDef],
    attributes: &mut BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    losses: &mut BTreeSet<CedarLoss>,
) {
    for (source_name, source_type) in &types.by_name {
        if !types.cedar_names.contains_key(source_name) {
            continue;
        }
        for link in &source_type.links {
            let Some(inverse) = &link.inverse else {
                continue;
            };
            let target_name = link.target.as_str();
            let target_is_object = types.all_names.contains(target_name);
            let mut targets = Vec::new();
            for candidate_name in types.by_name.keys().copied() {
                if !types.cedar_names.contains_key(candidate_name) {
                    continue;
                }
                let eligible = if target_is_object {
                    types.lineages.get(candidate_name).is_some_and(|lineage| {
                        lineage
                            .iter()
                            .any(|ancestor| ancestor.name.as_str() == target_name)
                    })
                } else {
                    types.lineages.get(candidate_name).is_some_and(|lineage| {
                        lineage.iter().any(|ancestor| {
                            ancestor.implements.iter().any(|interface| {
                                interface.as_str() == target_name
                                    || interfaces
                                        .iter()
                                        .find(|definition| definition.name == *interface)
                                        .is_some_and(|definition| {
                                            definition
                                                .contract_owners(|name| {
                                                    interfaces
                                                        .iter()
                                                        .find(|parent| parent.name == *name)
                                                })
                                                .iter()
                                                .any(|parent| parent.name.as_str() == target_name)
                                        })
                            })
                        })
                    })
                };
                if eligible {
                    targets.push(candidate_name);
                }
            }
            if targets.is_empty() {
                add_loss(
                    losses,
                    format!("{source_name}.{}", link.id.as_str()),
                    "inverse role has no projected object target or interface implementor",
                );
            }
            for target in targets {
                if !types.cedar_names.contains_key(target) {
                    continue;
                }
                let projected = attributes.entry(target.to_owned()).or_default();
                let subject = format!("{target}.{}", inverse.as_str());
                insert_attribute(
                    projected,
                    inverse.as_str(),
                    CedarAttribute::Link {
                        many: link.cardinality.from == Multiplicity::Many,
                    },
                    target,
                    losses,
                );
                add_loss(
                    losses,
                    subject,
                    "inverse link target type is not constrained by the generic Cedar record",
                );
            }
        }
    }
}

fn render_schema(
    types: &CedarObjectTypes<'_>,
    attributes: &BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_attributes: &BTreeMap<String, CedarPrincipalAttribute>,
    actions: &[ActionRoot],
    losses: &mut BTreeSet<CedarLoss>,
) -> String {
    let mut schema = String::new();
    render_object_types(&mut schema, &types.namespaces, attributes);
    render_actor(&mut schema, principal_attributes);
    render_actions(&mut schema, types, actions, losses);
    schema.push_str("}\n");
    schema
}

fn render_object_types(
    schema: &mut String,
    namespaces: &BTreeMap<String, BTreeSet<String>>,
    attributes: &BTreeMap<String, BTreeMap<String, CedarAttribute>>,
) {
    for (namespace, locals) in namespaces {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = writeln!(schema, "namespace {namespace} {{");
        for local in locals {
            let ir_name = format!("{}.{}", namespace.replace("::", "."), local);
            let mut fields = vec!["scope: Norn::Scope".to_owned(), HOST_ATTRIBUTES.to_owned()];
            if let Some(projected) = attributes.get(&ir_name) {
                fields.extend(projected.iter().map(|(name, attribute)| {
                    format!("{name}?: {}", cedar_attribute_type(*attribute))
                }));
            }
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = writeln!(schema, "  entity {local} = {{");
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = writeln!(schema, "    {}", fields.join(",\n    "));
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = writeln!(schema, "  }};");
        }
        schema.push_str("}\n");
    }
}

fn render_actor(
    schema: &mut String,
    principal_attributes: &BTreeMap<String, CedarPrincipalAttribute>,
) {
    schema.push_str("namespace Norn {\n  entity Actor in [Role] = {\n");
    let mut fields = vec!["represents?: { type: String, key: String }".to_owned()];
    fields.extend(principal_attributes.iter().map(|(name, attribute)| {
        format!("{name}?: {}", cedar_principal_attribute_type(*attribute))
    }));
    fields.push("scope: Scope".to_owned());
    #[expect(
        clippy::let_underscore_must_use,
        reason = "fmt::Write for String is infallible; its trait signature still returns Result"
    )]
    let _ = writeln!(schema, "    {}\n  }};", fields.join(",\n    "));
}

fn project_principal_attributes(
    attributes: &BTreeMap<String, CedarPrincipalAttribute>,
    losses: &mut BTreeSet<CedarLoss>,
) -> BTreeMap<String, CedarPrincipalAttribute> {
    let mut projected = BTreeMap::new();
    for (name, attribute) in attributes {
        if !is_cedar_identifier(name) {
            add_loss(
                losses,
                format!("Norn::Actor.{name}"),
                "principal attribute name cannot be represented as a Cedar identifier",
            );
            continue;
        }
        if matches!(name.as_str(), "scope" | "represents" | "acted_by") {
            add_loss(
                losses,
                format!("Norn::Actor.{name}"),
                "principal attribute collides with a reserved Cedar actor attribute",
            );
            continue;
        }
        projected.insert(name.clone(), *attribute);
    }
    projected
}

fn cedar_principal_attribute_type(attribute: CedarPrincipalAttribute) -> &'static str {
    match attribute {
        CedarPrincipalAttribute::Bool => "Bool",
        CedarPrincipalAttribute::Long => "Long",
        CedarPrincipalAttribute::String => "String",
        CedarPrincipalAttribute::BoolSet => "Set<Bool>",
        CedarPrincipalAttribute::LongSet => "Set<Long>",
        CedarPrincipalAttribute::StringSet => "Set<String>",
    }
}

fn render_actions(
    schema: &mut String,
    types: &CedarObjectTypes<'_>,
    actions: &[ActionRoot],
    losses: &mut BTreeSet<CedarLoss>,
) {
    let mut grouped_actions: BTreeMap<&str, Vec<&ActionRoot>> = BTreeMap::new();
    for action in actions {
        grouped_actions
            .entry(action.action_id.as_str())
            .or_default()
            .push(action);
    }
    for (action_id, definitions) in grouped_actions {
        if definitions.len() != 1 {
            add_loss(
                losses,
                action_id.to_owned(),
                "duplicate action id is ambiguous",
            );
            continue;
        }
        let Some(action) = definitions.first().copied() else {
            continue;
        };
        add_loss(
            losses,
            action_id.to_owned(),
            "ActionIR context values are not typed by the projection; only scope is declared",
        );
        report_action_losses(action, losses);
        let Some(program) = &action.effect_program else {
            add_loss(
                losses,
                action_id.to_owned(),
                "action has no effect program, so no resource type can be projected",
            );
            continue;
        };
        let kind = program.target().object_kind;
        let mut resource_kinds = BTreeSet::from([kind]);
        if let ActionEffectProgram::MutateParentWithRows { collections, .. } = program {
            let lineage = types.lineages.get(kind).cloned().unwrap_or_default();
            for collection in collections {
                let child_type = lineage.iter().find_map(|object_type| {
                    object_type
                        .links
                        .iter()
                        .find(|link| link.id.as_str() == collection.id.as_str())
                        .map(|link| link.target.as_str())
                });
                if let Some(child_type) = child_type {
                    resource_kinds.insert(child_type);
                } else {
                    add_loss(
                        losses,
                        format!("{action_id}/{}", collection.id.as_str()),
                        "owned collection has no projected parent link",
                    );
                }
            }
        }
        let resources: Vec<&str> = types
            .lineages
            .iter()
            .filter(|(name, lineage)| {
                types.cedar_names.contains_key(*name)
                    && lineage
                        .iter()
                        .any(|ancestor| resource_kinds.contains(ancestor.name.as_str()))
            })
            .filter_map(|(name, _)| types.cedar_names.get(name).map(String::as_str))
            .collect();
        if resources.is_empty() {
            add_loss(
                losses,
                action_id.to_owned(),
                "action object kind is not a projected object type",
            );
            continue;
        }
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = writeln!(
            schema,
            "  action \"{}\" appliesTo {{\n    principal: [Actor],\n    resource: [{}],\n    context: {{\n      scope: Scope\n    }}\n  }};",
            action_id.escape_debug(),
            resources.join(", ")
        );
    }
}

fn report_object_losses(object_type: &ObjectTypeDef, losses: &mut BTreeSet<CedarLoss>) {
    let name = object_type.name.as_str();
    if object_type.description.is_some() {
        add_loss(
            losses,
            name.to_owned(),
            "object type description is not represented",
        );
    }
    if !object_type.status.is_active() {
        add_loss(
            losses,
            name.to_owned(),
            "object type status is not represented",
        );
    }
    if object_type.extends.is_some() {
        add_loss(
            losses,
            name.to_owned(),
            "object type inheritance is flattened; Cedar type inheritance is not emitted",
        );
    }
    if !object_type.implements.is_empty() {
        add_loss(
            losses,
            name.to_owned(),
            "implemented interface identity is not represented in Cedar",
        );
    }
    if !object_type.identity.is_empty() {
        add_loss(
            losses,
            format!("{name}.identity"),
            "business key constraints are not represented in Cedar",
        );
    }
    for invariant in &object_type.invariants {
        add_loss(
            losses,
            format!("{name}.{}", invariant.id.as_str()),
            "object invariant is not represented in Cedar schema",
        );
    }
    for lifecycle in &object_type.lifecycles {
        add_loss(
            losses,
            format!("{name}.{}", lifecycle.property.as_str()),
            "property lifecycle is not represented in Cedar schema",
        );
    }
}

fn report_action_losses(action: &ActionRoot, losses: &mut BTreeSet<CedarLoss>) {
    let action_id = action.action_id.as_str();
    if action.title.is_some() || action.description.is_some() {
        add_loss(
            losses,
            action_id.to_owned(),
            "action title or description is not represented",
        );
    }
    if !action.preconditions.is_empty() {
        add_loss(
            losses,
            action_id.to_owned(),
            "action preconditions are not represented in Cedar schema",
        );
    }
    if !action.writes.is_empty() {
        add_loss(
            losses,
            action_id.to_owned(),
            "action write-set semantics are not represented in Cedar schema",
        );
    }
    if action.access.is_some() {
        add_loss(
            losses,
            action_id.to_owned(),
            "ActionIR access reference is not represented in Cedar schema",
        );
    }
    if !action.scope_required.is_empty() {
        add_loss(
            losses,
            action_id.to_owned(),
            "action-specific required scope dimensions are not represented",
        );
    }
    add_loss(
        losses,
        action_id.to_owned(),
        "action input and output schemas are not represented in Cedar schema",
    );
}

fn report_link_losses(subject: &str, link: &Link, losses: &mut BTreeSet<CedarLoss>) {
    if link.description.is_some() {
        add_loss(
            losses,
            subject.to_owned(),
            "link description is not represented",
        );
    }
    add_loss(
        losses,
        subject.to_owned(),
        "link target type is not constrained by the generic Cedar record",
    );
    add_loss(
        losses,
        subject.to_owned(),
        "EntityIR has no hierarchy marker, so Cedar memberOf cannot be inferred",
    );
    if link.cardinality.required {
        add_loss(
            losses,
            subject.to_owned(),
            "link minimum cardinality is not represented by an optional Cedar attribute",
        );
    }
    for property in &link.properties {
        add_loss(
            losses,
            format!("{subject}.{}", property.id.as_str()),
            "link property is not represented by the generic entity-reference record",
        );
    }
}

fn project_property(
    subject: &str,
    property: &Property,
    catalog: &BTreeMap<&str, &PropertyTypeDef>,
    duplicate_catalog_names: &BTreeSet<&str>,
    losses: &mut BTreeSet<CedarLoss>,
) -> Option<CedarAttribute> {
    let (property_type, catalog_constraints) =
        match resolve_property_type(&property.property_type, catalog, duplicate_catalog_names) {
            Ok(resolved) => resolved,
            Err(ResolveTypeError::Missing(name)) => {
                add_loss(
                    losses,
                    format!("{subject} -> {name}"),
                    "property type reference is absent from the supplied catalog",
                );
                report_property_metadata(subject, property, &[], losses);
                return None;
            }
            Err(ResolveTypeError::Duplicate(name)) => {
                add_loss(
                    losses,
                    format!("{subject} -> {name}"),
                    "property type reference is ambiguous in the supplied catalog",
                );
                report_property_metadata(subject, property, &[], losses);
                return None;
            }
            Err(ResolveTypeError::Cycle(name)) => {
                add_loss(
                    losses,
                    format!("{subject} -> {name}"),
                    "property type reference chain is cyclic",
                );
                report_property_metadata(subject, property, &[], losses);
                return None;
            }
            Err(ResolveTypeError::TooDeep) => {
                add_loss(
                    losses,
                    subject.to_owned(),
                    "property type reference chain exceeds the catalog size",
                );
                report_property_metadata(subject, property, &[], losses);
                return None;
            }
        };

    report_property_metadata(subject, property, &catalog_constraints, losses);
    project_value_type(subject, &property_type, losses)
}

fn project_value_type(
    subject: &str,
    property_type: &PropertyType,
    losses: &mut BTreeSet<CedarLoss>,
) -> Option<CedarAttribute> {
    match property_type {
        PropertyType::Text { format } => {
            if format.is_some() {
                add_loss(
                    losses,
                    subject.to_owned(),
                    "text format constraint is not represented",
                );
            }
            Some(CedarAttribute::String)
        }
        PropertyType::Integer {} => Some(CedarAttribute::Long),
        PropertyType::Decimal { unit, .. } => {
            add_loss(
                losses,
                subject.to_owned(),
                "decimal scale and numeric comparison semantics are not represented; Cedar uses String",
            );
            if unit.is_some() {
                add_loss(
                    losses,
                    subject.to_owned(),
                    "decimal unit is not represented",
                );
            }
            Some(CedarAttribute::Decimal)
        }
        PropertyType::Boolean {} => Some(CedarAttribute::Bool),
        PropertyType::Timestamp { .. } => {
            add_loss(
                losses,
                subject.to_owned(),
                "timestamp timezone semantics are not represented; Cedar uses String",
            );
            Some(CedarAttribute::String)
        }
        PropertyType::Date {} | PropertyType::Duration {} => {
            add_loss(
                losses,
                subject.to_owned(),
                "date or duration semantics are not represented; Cedar uses String",
            );
            Some(CedarAttribute::String)
        }
        PropertyType::Enum { .. } => {
            add_loss(
                losses,
                subject.to_owned(),
                "closed enum values are projected as String without their allowed-value constraint",
            );
            Some(CedarAttribute::String)
        }
        PropertyType::Record { .. } => {
            add_loss(
                losses,
                subject.to_owned(),
                "structured record property types are not represented in Cedar",
            );
            None
        }
        PropertyType::JsonExt { .. } => {
            add_loss(
                losses,
                subject.to_owned(),
                "opaque JSON property types are not represented in Cedar",
            );
            None
        }
        PropertyType::Ref { .. } => {
            add_loss(
                losses,
                subject.to_owned(),
                "property type reference did not resolve to a concrete value type",
            );
            None
        }
    }
}

fn report_property_metadata(
    subject: &str,
    property: &Property,
    catalog_constraints: &[PropertyConstraints],
    losses: &mut BTreeSet<CedarLoss>,
) {
    let mut constraints = Vec::new();
    constraints.push(&property.constraints);
    constraints.extend(catalog_constraints.iter());
    let nullable = constraints.iter().any(|constraint| constraint.nullable);
    if nullable {
        add_loss(
            losses,
            subject.to_owned(),
            "Cedar has no null value; nullable is represented only as an optional attribute",
        );
    } else {
        add_loss(
            losses,
            subject.to_owned(),
            "required property presence is not enforced because Cedar attributes are optional",
        );
    }
    for constraint in constraints {
        if constraint.min.is_some() || constraint.max.is_some() {
            add_loss(
                losses,
                subject.to_owned(),
                "numeric bounds are not represented in Cedar schema",
            );
        }
        if constraint.min_length.is_some()
            || constraint.max_length.is_some()
            || constraint.pattern.is_some()
        {
            add_loss(
                losses,
                subject.to_owned(),
                "text length or pattern constraints are not represented in Cedar schema",
            );
        }
    }
    if property.description.is_some() {
        add_loss(
            losses,
            subject.to_owned(),
            "property description is not represented",
        );
    }
    if property.default.is_some() {
        add_loss(
            losses,
            subject.to_owned(),
            "property default is not represented in Cedar schema",
        );
    }
    if property.computed.is_some() {
        add_loss(
            losses,
            subject.to_owned(),
            "computed property expression is not represented in Cedar schema",
        );
    }
}

fn insert_attribute(
    attributes: &mut BTreeMap<String, CedarAttribute>,
    name: &str,
    attribute: CedarAttribute,
    owner: &str,
    losses: &mut BTreeSet<CedarLoss>,
) {
    if !is_cedar_identifier(name) {
        add_loss(
            losses,
            format!("{owner}.{name}"),
            "attribute name cannot be represented as a Cedar identifier",
        );
        return;
    }
    if matches!(name, "scope" | "acted_by") {
        add_loss(
            losses,
            format!("{owner}.{name}"),
            "attribute collides with a reserved Cedar scope or host attribute",
        );
        return;
    }
    if attributes.contains_key(name) {
        add_loss(
            losses,
            format!("{owner}.{name}"),
            "duplicate projected member name is ambiguous",
        );
        return;
    }
    attributes.insert(name.to_owned(), attribute);
}

fn cedar_attribute_type(attribute: CedarAttribute) -> &'static str {
    match attribute {
        CedarAttribute::Bool => "Bool",
        CedarAttribute::Long => "Long",
        CedarAttribute::String | CedarAttribute::Decimal => "String",
        CedarAttribute::Link { many: false } => "{ type: String, key: String }",
        CedarAttribute::Link { many: true } => "Set<{ type: String, key: String }>",
    }
}

fn property_type_catalog(
    property_types: &[PropertyTypeDef],
) -> (BTreeMap<&str, &PropertyTypeDef>, BTreeSet<&str>) {
    let mut catalog = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for property_type in property_types {
        let name = property_type.name.as_str();
        if duplicates.contains(name) {
            continue;
        }
        if catalog.insert(name, property_type).is_some() {
            catalog.remove(name);
            duplicates.insert(name);
        }
    }
    (catalog, duplicates)
}

enum ResolveTypeError {
    Missing(String),
    Duplicate(String),
    Cycle(String),
    TooDeep,
}

fn resolve_property_type(
    property_type: &PropertyType,
    catalog: &BTreeMap<&str, &PropertyTypeDef>,
    duplicate_names: &BTreeSet<&str>,
) -> Result<(PropertyType, Vec<PropertyConstraints>), ResolveTypeError> {
    let mut current = property_type;
    let mut constraints = Vec::new();
    let mut visited = BTreeSet::new();
    for _ in 0..=catalog.len() {
        let PropertyType::Ref { name } = current else {
            return Ok((current.clone(), constraints));
        };
        let name = name.as_str();
        if !visited.insert(name) {
            return Err(ResolveTypeError::Cycle(name.to_owned()));
        }
        if duplicate_names.contains(name) {
            return Err(ResolveTypeError::Duplicate(name.to_owned()));
        }
        let Some(definition) = catalog.get(name) else {
            return Err(ResolveTypeError::Missing(name.to_owned()));
        };
        constraints.push(definition.constraints.clone());
        current = &definition.property_type;
    }
    Err(ResolveTypeError::TooDeep)
}

struct LineageIssue {
    subject: String,
    reason: &'static str,
}

fn object_lineage<'a>(
    name: &'a str,
    by_name: &BTreeMap<&'a str, &'a ObjectTypeDef>,
) -> (Vec<&'a ObjectTypeDef>, Option<LineageIssue>) {
    let mut lineage = Vec::new();
    let mut visited = BTreeSet::new();
    let mut current = Some(name);
    let mut issue = None;
    while let Some(current_name) = current {
        if !visited.insert(current_name) {
            issue = Some(LineageIssue {
                subject: name.to_owned(),
                reason: "object type inheritance cycle could not be flattened",
            });
            break;
        }
        let Some(definition) = by_name.get(current_name) else {
            issue = Some(LineageIssue {
                subject: format!("{name} extends {current_name}"),
                reason: "supertype is absent or ambiguous in the supplied object types",
            });
            break;
        };
        lineage.push(*definition);
        current = definition
            .extends
            .as_ref()
            .map(norn_ir::ObjectTypeName::as_str);
    }
    lineage.reverse();
    (lineage, issue)
}

fn cedar_entity_name(name: &str) -> Option<String> {
    let (domain, local) = name.rsplit_once('.')?;
    if !is_cedar_identifier(local) || !domain.split('.').all(is_cedar_identifier) {
        return None;
    }
    Some(format!("{}::{local}", domain.replace('.', "::")))
}

fn is_cedar_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn add_loss(losses: &mut BTreeSet<CedarLoss>, subject: String, reason: &'static str) {
    losses.insert(CedarLoss { subject, reason });
}
