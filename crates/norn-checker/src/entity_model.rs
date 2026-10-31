//! Checks over an `EntityIR` v2 model as a whole (T1-ONT-1c, #2513):
//! resource status, single inheritance, interfaces, domain and extension
//! namespaces, and the tenant layer, on top of the per-type checks of
//! T1-ONT-1b ([`crate::object_types`]).
//!
//! A model is checked against an installed, read-only **base layer** (the
//! registry packages the layer builds on, already checked on their own) and,
//! for a tenant layer, the tenant's [`ExtensionNamespace`]. The rules:
//!
//! - **Names.** Object types and interfaces share one name space, within the
//!   model and against the base layer; a local definition may not take a base
//!   name (base definitions are read-only). The dotted prefix of a name is
//!   the definition's domain namespace.
//! - **Inheritance.** `extends` names one object type (local or base); the
//!   chain must not return to its start. A subtype inherits every property,
//!   key, and implemented interface. Redeclaring an inherited id is an
//!   override and may only **narrow**: the same type or a narrowing of it
//!   (see `docs/platform/entity-ir-v2.md`), never nullable where the inherited property is
//!   not, and never involving a computed property. The overridden property's
//!   constraints still apply.
//! - **Interfaces.** Each implemented interface's required ids must be
//!   properties of the type (declared or inherited) whose types narrow the
//!   requirement's; the requirement's constraints then apply to them. When
//!   several interfaces require one id (the diamond case: through `implements`
//!   and through supertypes), one property must meet every requirement, so
//!   all of them are checked together (types, common enum values, combined
//!   bounds); otherwise that is `interface_requirement_conflict`. Every
//!   effective default, declared or inherited, meets every constraint set
//!   that applies to it.
//! - **Extensions and tenants.** Base-layer property ids never contain `__`.
//!   Members a tenant type or an extension adds are `ns__name` in the layer's
//!   extension namespace, so they cannot shadow base members; an extension
//!   member that collides with a member its target already has is rejected,
//!   and a layer does not extend a type it defines itself.
//!   A tenant layer declares no interfaces, and its object types are
//!   subtypes named in the tenant namespace (`acme.LoyaltySku`).
//!
//! - **Declared lifecycles** (T3-LIFE-1, #2672) are checked in
//!   `lifecycles.rs`: the lifecycle property is a stored, non-nullable local
//!   enum, the machine is well formed, and subtypes only narrow it.
//! - **Links and invariants** (T1-ONT-1d, #2514) are checked in `links.rs`:
//!   properties, links, and inverse roles share one member name space per
//!   type; link overrides and interface link requirements only narrow; object
//!   and interface invariants are statically checked as bool, and implemented
//!   interface invariants join the effective object contract.
//!
//! Spans are JSON pointers into the document
//! `{ "interfaces": [...], "objectTypes": [...], "extensions": [...] }`.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use norn_expr::{EntityDefinitions, EnvOptions, ZenTypeEnv};
use norn_ir::identifier_limits::MAX_OBJECT_TYPE_NAME_BYTES;
use norn_ir::{
    EXTENSION_MEMBER_SEPARATOR, EXTENSION_NAMESPACE_PATTERN, ExtensionNamespace, InterfaceDef,
    InterfaceName, InterfaceProperty, MarkingId, Multiplicity, ObjectTypeDef, ObjectTypeExtension,
    ObjectTypeName, PropertyId, PropertyType, PropertyTypeDef, PropertyTypeName, ResourceStatus,
};

use crate::object_types::{
    MEMBER_ID_GRAMMAR, OBJECT_NAME_GRAMMAR, Origin, Resolved, check_default_against, check_keys,
    check_property_default, check_property_shape, codes, unsatisfiable, valid_date,
};
use crate::property_types::{
    ResolvedType, TypeResolution, error, matches_grammar, resolve_type, span_at,
};
use crate::{Diagnostic, interfaces};

mod lifecycles;
mod links;

use links::{LinkDecl, LinkTable};

static EXTENSION_NAMESPACE_GRAMMAR: std::sync::LazyLock<Option<regex_lite::Regex>> =
    std::sync::LazyLock::new(|| regex_lite::Regex::new(EXTENSION_NAMESPACE_PATTERN).ok());

/// The definitions of one layer: interfaces, object types, and extensions of
/// object types.
#[derive(Debug, Clone, Copy)]
pub struct EntityModel<'a> {
    /// Interface definitions.
    pub interfaces: &'a [InterfaceDef],
    /// Object type definitions.
    pub object_types: &'a [ObjectTypeDef],
    /// Extensions of object types the layer does not own.
    pub extensions: &'a [ObjectTypeExtension],
}

impl EntityModel<'_> {
    /// A layer with no definitions.
    pub const EMPTY: Self = Self {
        interfaces: &[],
        object_types: &[],
        extensions: &[],
    };
}

/// What a model is checked against.
#[derive(Debug, Clone, Copy)]
pub struct EntityContext<'a> {
    /// Every shared property type the model and its base layer may `ref`
    /// (checked on its own by [`crate::check_property_type_defs`]).
    pub catalog: &'a [PropertyTypeDef],
    /// Registry (Norn Core) value type names.
    pub registry: &'a BTreeSet<PropertyTypeName>,
    /// The installed, read-only base layer. Its definitions resolve
    /// `extends`, `implements`, and extension targets but are not re-checked.
    pub base: EntityModel<'a>,
    /// `Some` when the model is a tenant layer authored under this extension
    /// namespace.
    pub tenant: Option<&'a ExtensionNamespace>,
}

/// JSON-pointer roots of the three definition arrays.
pub(crate) struct Roots {
    interfaces: &'static [&'static str],
    object_types: &'static [&'static str],
    extensions: &'static [&'static str],
}

impl Roots {
    /// The model document `{ interfaces, objectTypes, extensions }`.
    const DOCUMENT: Self = Self {
        interfaces: &["interfaces"],
        object_types: &["objectTypes"],
        extensions: &["extensions"],
    };
    /// A bare object type array (`check_object_type_defs`).
    pub(crate) const BARE: Self = Self {
        interfaces: &["interfaces"],
        object_types: &[],
        extensions: &["extensions"],
    };
}

/// Check an entity model against its context. Returns every finding; an
/// empty result means the model is valid.
///
/// Per-type findings (property ids, inline types, keys, defaults, computed
/// properties) use the `object_type.validation.*` and
/// `property_type.validation.*` codes of T1-ONT-1a/1b; interface definitions
/// use `interface.validation.*`.
#[must_use]
pub fn check_entity_model(model: &EntityModel<'_>, context: &EntityContext<'_>) -> Vec<Diagnostic> {
    check(model, context, &Roots::DOCUMENT)
}

/// Check one object type on its own: the per-definition (T2) subset of
/// [`check_entity_model`], for per-artifact validation of an `EntityIR` v2
/// entity artifact (T1-IR-1b).
///
/// Runs the model check over this one type with no catalog, registry, base
/// layer, or tenant, and keeps only findings that cannot depend on another
/// definition: names, ids, duplicate members, keys, defaults, constraints,
/// computed properties, link and invariant checks that do not need context.
/// Dropped, because one
/// definition cannot decide them (they belong to the whole-model check):
///
/// - every code the registry places in tier T3 (supertypes, interfaces, link
///   targets and inverses, overrides, member conflicts, extensions);
/// - `property_type.validation.unknown_ref` (the property type catalog is
///   another definition set), so defaults and constraints on an unresolved
///   `ref` are not checked here either;
/// - `object_type.validation.reserved_member_separator` and
///   `unnamespaced_member` (whether `ns__` members are required or forbidden
///   depends on the layer the type is installed in);
/// - `object_type.validation.unknown_key_property` and
///   `lifecycle.validation.unknown_property` when the type `extends`
///   another (a key or a lifecycle may name an inherited property).
///
/// Spans are JSON pointers into the definition (`/properties/1/id`).
#[must_use]
pub fn check_object_type_local(def: &ObjectTypeDef) -> Vec<Diagnostic> {
    let model = EntityModel {
        object_types: std::slice::from_ref(def),
        ..EntityModel::EMPTY
    };
    check_one_local(&model, "/objectTypes/0", def.extends.is_some())
}

/// Check one interface on its own: the per-definition (T2) subset of
/// [`check_entity_model`] for an `EntityIR` v2 `interface` entity artifact
/// (#2844). Drops the same context-dependent findings as
/// [`check_object_type_local`]. Spans are JSON pointers into the definition.
#[must_use]
pub fn check_interface_local(def: &InterfaceDef) -> Vec<Diagnostic> {
    let model = EntityModel {
        interfaces: std::slice::from_ref(def),
        ..EntityModel::EMPTY
    };
    check_one_local(&model, "/interfaces/0", false)
}

/// Check one object type extension on its own: the per-definition (T2)
/// subset of [`check_entity_model`] for an `EntityIR` v2
/// `objectTypeExtension` entity artifact (#2844). Its target, and so every
/// rule about the members it adds to that target, is another definition, so
/// most of an extension's findings belong to the artifact-set check.
/// Spans are JSON pointers into the definition.
#[must_use]
pub fn check_object_type_extension_local(def: &ObjectTypeExtension) -> Vec<Diagnostic> {
    let model = EntityModel {
        extensions: std::slice::from_ref(def),
        ..EntityModel::EMPTY
    };
    check_one_local(&model, "/extensions/0", false)
}

/// Run the model check over a model of exactly one definition, rooted at
/// `prefix` in the model document, with no catalog, registry, base layer, or
/// tenant; keep the findings that cannot depend on another definition, with
/// spans relative to the definition.
fn check_one_local(model: &EntityModel<'_>, prefix: &str, subtype: bool) -> Vec<Diagnostic> {
    let registry = BTreeSet::new();
    let context = EntityContext {
        catalog: &[],
        registry: &registry,
        base: EntityModel::EMPTY,
        tenant: None,
    };
    check(model, &context, &Roots::DOCUMENT)
        .into_iter()
        .filter(|finding| !depends_on_other_definitions(finding.code.as_str(), subtype))
        .map(|mut finding| {
            if let Some(span) = &mut finding.span
                && let Some(rest) = span.pointer.strip_prefix(prefix)
            {
                span.pointer = rest.to_owned();
            }
            finding
        })
        .collect()
}

/// Whether a finding of `code` on one definition, checked alone, may be an
/// artifact of the missing context (see [`check_object_type_local`]);
/// `subtype` when the definition is an object type that `extends` another.
fn depends_on_other_definitions(code: &str, subtype: bool) -> bool {
    crate::codes::lookup(code).is_none_or(|entry| entry.tier != crate::Tier::T2)
        || code == crate::property_types::codes::UNKNOWN_REF
        || code == codes::RESERVED_MEMBER_SEPARATOR
        || code == codes::UNNAMESPACED_MEMBER
        || (subtype
            && (code == codes::UNKNOWN_KEY_PROPERTY
                || code == crate::lifecycles::codes::UNKNOWN_PROPERTY))
}

pub(crate) fn check(
    model: &EntityModel<'_>,
    context: &EntityContext<'_>,
    roots: &Roots,
) -> Vec<Diagnostic> {
    let mut checker = Checker::new(model, context, roots);
    checker.run();
    checker.out
}

/// The effective view of every registered local object type of `model`
/// (instance semantics, #2848), by its position in `model.object_types`.
/// A duplicate definition of a name is not registered and has no entry.
///
/// This builds the same effective property tables the model check checks
/// (inherited, overridden, interface-constrained, and extension members), so
/// instance validation never re-derives inheritance. It does not check the
/// model: callers must reject a model with findings first
/// ([`crate::entity_instance::EffectiveCatalog`] does).
#[must_use]
pub fn effective_object_types(
    model: &EntityModel<'_>,
    context: &EntityContext<'_>,
) -> BTreeMap<
    usize,
    (
        crate::entity_instance::EffectiveObjectType,
        Vec<crate::entity_instance::EffectiveLink>,
    ),
> {
    let mut checker = Checker::new(model, context, &Roots::DOCUMENT);
    checker.register();
    checker.find_cycles();
    checker.register_inverses();
    let mut out = BTreeMap::new();
    for (index, def) in model.object_types.iter().enumerate() {
        let registered = checker
            .types
            .get(&def.name)
            .is_some_and(|entry| entry.local == Some(index));
        if registered {
            let object_type = checker.effective(def);
            let links = checker.effective_links(&def.name);
            out.insert(index, (object_type, links));
        }
    }
    out
}

impl<'a, 'r> Checker<'a, 'r> {
    fn new(model: &EntityModel<'a>, context: &EntityContext<'a>, roots: &'r Roots) -> Self {
        let expression_env = has_invariants(model, context).then(|| expression_env(model, context));
        Checker {
            model: *model,
            context: *context,
            expression_env,
            roots,
            types: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            extensions: BTreeMap::new(),
            cyclic: BTreeSet::new(),
            tables: BTreeMap::new(),
            incoming: BTreeMap::new(),
            link_tables: BTreeMap::new(),
            machines: BTreeMap::new(),
            reported: BTreeSet::new(),
            out: Vec::new(),
        }
    }

    /// The effective view of a registered object type: its lineage, its
    /// effective property table, every key declared along the lineage, and
    /// whether links or invariants apply to it.
    #[allow(
        clippy::too_many_lines,
        reason = "one effective view keeps lineage, property markings, and metadata assembly coherent"
    )]
    fn effective(&mut self, def: &'a ObjectTypeDef) -> crate::entity_instance::EffectiveObjectType {
        let table = self.table(&def.name);
        let lineage: Vec<&'a ObjectTypeName> = std::iter::once(&def.name)
            .chain(self.ancestors(&def.name))
            .collect();
        let lineage_defs: Vec<&'a ObjectTypeDef> = lineage
            .iter()
            .filter_map(|name| self.types.get(*name).map(|entry| entry.def))
            .collect();
        let keys: Vec<_> = lineage_defs
            .iter()
            .flat_map(|owner| {
                owner
                    .identity
                    .iter()
                    .map(|key| crate::entity_instance::EffectiveKey::new(&owner.name, key))
            })
            .collect();
        let extension_links = lineage.iter().any(|name| {
            self.extensions.get(*name).is_some_and(|extensions| {
                extensions
                    .iter()
                    .any(|(_, _, extension)| !extension.links.is_empty())
            })
        });
        let interfaces = lineage_defs
            .iter()
            .flat_map(|owner| &owner.implements)
            .flat_map(|name| {
                std::iter::once(name.clone()).chain(
                    self.interfaces
                        .get(name)
                        .into_iter()
                        .flat_map(|entry| self.interface_lineage(entry.def))
                        .map(|definition| definition.name.clone()),
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        // Every shared property type the effective type reads: the ref
        // chains of its effective properties, of every declaration along its
        // lineage and its extensions (overridden ones included), and of the
        // requirements of every interface it implements.
        let catalog = self.context.catalog;
        let mut referenced: BTreeSet<PropertyTypeName> = table
            .values()
            .flat_map(|member| {
                crate::property_types::structured_metadata(&member.property.property_type, catalog)
                    .0
            })
            .collect();
        let extension_properties = lineage.iter().flat_map(|name| {
            self.extensions
                .get(*name)
                .into_iter()
                .flatten()
                .flat_map(|(_, _, extension)| &extension.properties)
        });
        for property in lineage_defs
            .iter()
            .flat_map(|owner| &owner.properties)
            .chain(extension_properties)
        {
            referenced.extend(
                crate::property_types::structured_metadata(&property.property_type, catalog).0,
            );
        }
        let interface_defs: Vec<&'a InterfaceDef> = lineage_defs
            .iter()
            .flat_map(|owner| &owner.implements)
            .filter_map(|name| self.interfaces.get(name).map(|entry| entry.def))
            .flat_map(|def| self.interface_lineage(def))
            .collect();
        let mut object_markings: BTreeSet<MarkingId> = lineage_defs
            .iter()
            .flat_map(|owner| owner.markings.iter().cloned())
            .collect();
        object_markings.extend(
            interface_defs
                .iter()
                .flat_map(|interface| interface.markings.iter().cloned()),
        );
        let mut property_markings: BTreeMap<String, BTreeSet<MarkingId>> = table
            .values()
            .map(|resolved| {
                let mut markings: BTreeSet<MarkingId> = resolved.markings.iter().cloned().collect();
                markings.extend(object_markings.iter().cloned());
                (resolved.property.id.as_str().to_owned(), markings)
            })
            .collect();
        let computed_reads: BTreeMap<String, norn_expr::ExpressionReads> = table
            .values()
            .filter_map(|resolved| {
                resolved.property.computed.as_ref().map(|expression| {
                    let reads = norn_expr::expression_reads(expression.as_str(), "self")
                        .unwrap_or_else(|_| norn_expr::ExpressionReads {
                            whole_root: true,
                            ..Default::default()
                        });
                    (resolved.property.id.as_str().to_owned(), reads)
                })
            })
            .collect();
        // Naming-series keys are also derived from effective property values:
        // every declared scope field contributes its classification to the
        // generated identity component, including keys inherited by subtypes.
        let mut property_dependencies = BTreeMap::<String, (BTreeSet<String>, bool)>::new();
        for (property, reads) in &computed_reads {
            property_dependencies.insert(
                property.clone(),
                (reads.fields.iter().cloned().collect(), reads.whole_root),
            );
        }
        for key in &keys {
            let Some(series) = &key.naming_series else {
                continue;
            };
            let [generated] = key.components.as_slice() else {
                continue;
            };
            property_dependencies
                .entry(generated.as_str().to_owned())
                .or_default()
                .0
                .extend(
                    series
                        .scope_fields
                        .iter()
                        .map(|field| field.as_str().to_owned()),
                );
        }
        loop {
            let snapshot = property_markings.clone();
            let mut changed = false;
            for (property, (inputs, reads_all)) in &property_dependencies {
                let mut inherited = BTreeSet::new();
                if *reads_all {
                    inherited.extend(snapshot.values().flatten().cloned());
                }
                for field in inputs {
                    if let Some(markings) = snapshot.get(field) {
                        inherited.extend(markings.iter().cloned());
                    }
                }
                if let Some(markings) = property_markings.get_mut(property) {
                    let before = markings.len();
                    markings.extend(inherited);
                    changed |= markings.len() != before;
                }
            }
            if !changed {
                break;
            }
        }
        let has_interface_invariants = interface_defs
            .iter()
            .any(|interface| !interface.invariants.is_empty());
        for interface in interface_defs {
            for requirement in self.requirements(interface) {
                referenced.extend(
                    crate::property_types::structured_metadata(
                        &requirement.property.property_type,
                        catalog,
                    )
                    .0,
                );
            }
        }
        crate::entity_instance::EffectiveObjectType {
            referenced_property_types: referenced,
            name: def.name.clone(),
            lineage: lineage.into_iter().cloned().collect(),
            interfaces,
            markings: object_markings.into_iter().collect(),
            properties: table
                .values()
                .map(|resolved| {
                    let markings = property_markings
                        .get(resolved.property.id.as_str())
                        .map(|markings| markings.iter().cloned().collect())
                        .unwrap_or_default();
                    let mut property = crate::entity_instance::EffectiveProperty::from_resolved(
                        resolved, markings,
                    );
                    property.protected |= lineage_defs
                        .iter()
                        .flat_map(|owner| &owner.properties)
                        .any(|inherited| inherited.id == property.id && inherited.protected);
                    if let Some(reads) = computed_reads.get(resolved.property.id.as_str()) {
                        property.computed_inputs = reads.fields.iter().cloned().collect();
                        property.computed_reads_all = reads.whole_root;
                    }
                    property
                })
                .collect(),
            owned_collections: {
                let mut seen = BTreeSet::new();
                lineage_defs
                    .iter()
                    .flat_map(|owner| owner.owned_collections.iter())
                    .filter(|collection| seen.insert(collection.id.as_str().to_owned()))
                    .cloned()
                    .collect()
            },
            keys,
            has_links: extension_links || lineage_defs.iter().any(|owner| !owner.links.is_empty()),
            has_invariants: lineage_defs
                .iter()
                .any(|owner| !owner.invariants.is_empty())
                || has_interface_invariants,
            lifecycles: self
                .machines(&def.name)
                .iter()
                .map(|(property, machine)| machine.effective(property))
                .collect(),
        }
    }
}

/// A registered definition: `local` is its position in the checked model,
/// `None` for a base definition.
struct Entry<'a, D> {
    def: &'a D,
    local: Option<usize>,
}

/// An object type's effective properties: declared, inherited, and added by
/// extensions, each resolved.
type Table<'a> = BTreeMap<&'a PropertyId, Resolved<'a>>;

/// One interface requirement, resolved.
struct Requirement<'a> {
    interface: &'a InterfaceName,
    property: &'a InterfaceProperty,
    resolved: ResolvedType<'a>,
}

impl<'a> Requirement<'a> {
    fn shape(&self) -> Shape<'_, 'a> {
        Shape {
            declared: &self.property.property_type,
            resolved: &self.resolved,
            nullable: self.property.constraints.nullable
                || self
                    .resolved
                    .chain
                    .iter()
                    .any(|constraints| constraints.nullable),
        }
    }
}

struct Checker<'a, 'r> {
    model: EntityModel<'a>,
    context: EntityContext<'a>,
    expression_env: Option<Result<ZenTypeEnv, Vec<norn_expr::EnvError>>>,
    roots: &'r Roots,
    types: BTreeMap<&'a ObjectTypeName, Entry<'a, ObjectTypeDef>>,
    interfaces: BTreeMap<&'a InterfaceName, Entry<'a, InterfaceDef>>,
    /// Extensions by target: position, whether local, and the extension.
    extensions: BTreeMap<&'a ObjectTypeName, Vec<(usize, bool, &'a ObjectTypeExtension)>>,
    /// Object types whose `extends` chain returns to them.
    cyclic: BTreeSet<&'a ObjectTypeName>,
    tables: BTreeMap<&'a ObjectTypeName, Rc<Table<'a>>>,
    /// Links with an inverse role, by the name of their target.
    incoming: BTreeMap<&'a str, Vec<LinkDecl<'a>>>,
    /// Effective member tables (properties, links, inverse roles), memoized.
    link_tables: BTreeMap<&'a ObjectTypeName, Rc<LinkTable<'a>>>,
    /// Effective lifecycle machines, memoized.
    machines: BTreeMap<&'a ObjectTypeName, Rc<crate::lifecycles::Machines<'a>>>,
    /// `(code, pointer)` of member conflicts already reported: one conflict
    /// is seen from the table of every type it reaches.
    reported: BTreeSet<(String, String)>,
    out: Vec<Diagnostic>,
}

/// Whether this layer or its base has invariants that need static checking.
fn has_invariants(model: &EntityModel<'_>, context: &EntityContext<'_>) -> bool {
    context
        .base
        .interfaces
        .iter()
        .chain(model.interfaces)
        .any(|interface| !interface.invariants.is_empty())
        || context
            .base
            .object_types
            .iter()
            .chain(model.object_types)
            .any(|object_type| !object_type.invariants.is_empty())
}

/// Derive the shared ZEN type environment for invariant expressions. Keep
/// errors so invariant checking can fail closed instead of silently skipping
/// static typing when an unrelated definition prevents derivation.
fn expression_env(
    model: &EntityModel<'_>,
    context: &EntityContext<'_>,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    let interfaces = context
        .base
        .interfaces
        .iter()
        .chain(model.interfaces)
        .cloned()
        .collect::<Vec<_>>();
    let object_types = context
        .base
        .object_types
        .iter()
        .chain(model.object_types)
        .cloned()
        .collect::<Vec<_>>();
    let extensions = context
        .base
        .extensions
        .iter()
        .chain(model.extensions)
        .cloned()
        .collect::<Vec<_>>();
    let definitions = EntityDefinitions {
        property_types: context.catalog,
        interfaces: &interfaces,
        object_types: &object_types,
        extensions: &extensions,
    };
    ZenTypeEnv::from_artifact_set_for_checking(
        &definitions,
        context.registry,
        EnvOptions::default(),
    )
}

impl<'a> Checker<'a, '_> {
    fn run(&mut self) {
        self.register();
        self.find_cycles();
        self.register_inverses();
        for (index, def) in self.model.interfaces.iter().enumerate() {
            self.check_interface(index, def);
        }
        for (index, def) in self.model.object_types.iter().enumerate() {
            self.check_object_type(index, def);
        }
        for (index, extension) in self.model.extensions.iter().enumerate() {
            self.check_extension(index, extension);
        }
        self.check_member_conflicts();
    }

    // ----- registration -------------------------------------------------

    fn register(&mut self) {
        let base_names = self.register_base();
        self.register_object_types(&base_names);
        self.register_interfaces(&base_names);
        let layers = [
            (self.context.base.extensions, false),
            (self.model.extensions, true),
        ];
        for (extensions, local) in layers {
            for (index, extension) in extensions.iter().enumerate() {
                self.extensions
                    .entry(&extension.target)
                    .or_default()
                    .push((index, local, extension));
            }
        }
    }

    /// Register the base layer, which owns its names; returns them.
    fn register_base(&mut self) -> BTreeSet<&'a str> {
        for def in self.context.base.object_types {
            self.types
                .entry(&def.name)
                .or_insert(Entry { def, local: None });
        }
        for def in self.context.base.interfaces {
            self.interfaces
                .entry(&def.name)
                .or_insert(Entry { def, local: None });
        }
        self.types
            .keys()
            .map(|name| name.as_str())
            .chain(self.interfaces.keys().map(|name| name.as_str()))
            .collect()
    }

    fn register_object_types(&mut self, base_names: &BTreeSet<&str>) {
        for (index, def) in self.model.object_types.iter().enumerate() {
            let position = index.to_string();
            let base = pointer(self.roots.object_types, &position);
            if def.name.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES {
                self.out.push(error(
                    crate::codes::IDENTIFIER_TOO_LONG,
                    format!(
                        "object type name is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                        def.name.as_str().len()
                    ),
                    span_at(&base, &["name"]),
                ));
            }
            if !matches_grammar(&OBJECT_NAME_GRAMMAR, def.name.as_str()) {
                self.out.push(error(
                    codes::INVALID_NAME,
                    format!(
                        "object type name {:?} must be dotted lower-case namespace segments and a PascalCase name, such as \"retail.Sku\"",
                        def.name.as_str()
                    ),
                    span_at(&base, &["name"]),
                ));
            }
            if base_names.contains(def.name.as_str()) {
                self.out.push(error(
                    codes::SHADOWS_BASE,
                    format!(
                        "{:?} is an installed base definition, which is read-only; extend it or add a subtype instead",
                        def.name.as_str()
                    ),
                    span_at(&base, &["name"]),
                ));
            } else if let Some(first) = self.types.get(&def.name).and_then(|entry| entry.local) {
                self.out.push(error(
                    codes::DUPLICATE_NAME,
                    format!(
                        "object type {:?} is already defined at index {first}",
                        def.name.as_str()
                    ),
                    span_at(&base, &["name"]),
                ));
            } else {
                self.types.insert(
                    &def.name,
                    Entry {
                        def,
                        local: Some(index),
                    },
                );
            }
        }
    }

    fn register_interfaces(&mut self, base_names: &BTreeSet<&str>) {
        for (index, def) in self.model.interfaces.iter().enumerate() {
            let position = index.to_string();
            let base = pointer(self.roots.interfaces, &position);
            let name = def.name.as_str();
            if name.len() > MAX_OBJECT_TYPE_NAME_BYTES {
                self.out.push(error(
                    crate::codes::IDENTIFIER_TOO_LONG,
                    format!(
                        "interface name is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                        name.len()
                    ),
                    span_at(&base, &["name"]),
                ));
            }
            if self.context.tenant.is_some() {
                self.out.push(error(
                    interfaces::codes::TENANT_INTERFACE,
                    format!(
                        "interface {name:?} is declared by a tenant layer; interfaces are registry definitions a tenant implements, never authors"
                    ),
                    span_at(&base, &["name"]),
                ));
            }
            if !matches_grammar(&OBJECT_NAME_GRAMMAR, name) {
                self.out.push(error(
                    interfaces::codes::INVALID_NAME,
                    format!(
                        "interface name {name:?} must be dotted lower-case namespace segments and a PascalCase name, such as \"party.Party\""
                    ),
                    span_at(&base, &["name"]),
                ));
            }
            if base_names.contains(name) {
                self.out.push(error(
                    interfaces::codes::SHADOWS_BASE,
                    format!("{name:?} is an installed base definition, which is read-only"),
                    span_at(&base, &["name"]),
                ));
            } else if let Some(first) = self.interfaces.get(&def.name).and_then(|entry| entry.local)
            {
                self.out.push(error(
                    interfaces::codes::DUPLICATE_NAME,
                    format!("interface {name:?} is already defined at index {first}"),
                    span_at(&base, &["name"]),
                ));
            } else if self
                .types
                .keys()
                .any(|object_type| object_type.as_str() == name)
            {
                self.out.push(error(
                    interfaces::codes::DUPLICATE_NAME,
                    format!(
                        "interface {name:?} has the name of an object type; interfaces and object types share one name space"
                    ),
                    span_at(&base, &["name"]),
                ));
            } else {
                self.interfaces.insert(
                    &def.name,
                    Entry {
                        def,
                        local: Some(index),
                    },
                );
            }
        }
    }

    fn find_cycles(&mut self) {
        for &start in self.types.keys() {
            let mut seen = BTreeSet::new();
            let mut current = start;
            while let Some(parent) = self
                .types
                .get(current)
                .and_then(|entry| entry.def.extends.as_ref())
                .and_then(|parent| self.types.get_key_value(parent))
                .map(|(name, _)| *name)
            {
                if parent == start {
                    self.cyclic.insert(start);
                    break;
                }
                if !seen.insert(parent) {
                    break;
                }
                current = parent;
            }
        }
    }

    /// The resolvable, acyclic supertype of a registered type.
    fn parent(&self, name: &ObjectTypeName) -> Option<&'a ObjectTypeName> {
        if self.cyclic.contains(name) {
            return None;
        }
        self.types
            .get(name)
            .and_then(|entry| entry.def.extends.as_ref())
            .and_then(|parent| self.types.get_key_value(parent))
            .map(|(parent, _)| *parent)
    }

    /// The registered supertypes of `name`, nearest first.
    fn ancestors(&self, name: &ObjectTypeName) -> Vec<&'a ObjectTypeName> {
        let mut ancestors = Vec::new();
        let mut current = self.parent(name);
        while let Some(parent) = current {
            if ancestors.contains(&parent) {
                break;
            }
            ancestors.push(parent);
            current = self.parent(parent);
        }
        ancestors
    }

    fn requirements(&self, def: &'a InterfaceDef) -> Vec<Requirement<'a>> {
        let mut seen = BTreeSet::new();
        self.interface_lineage(def)
            .into_iter()
            .flat_map(|owner| {
                owner
                    .properties
                    .iter()
                    .filter(|property| seen.insert((&owner.name, &property.id)))
                    .map(|property| Requirement {
                        interface: &owner.name,
                        property,
                        resolved: resolve_type(
                            &property.property_type,
                            self.context.catalog,
                            self.context.registry,
                        ),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn interface_lineage(&self, def: &'a InterfaceDef) -> Vec<&'a InterfaceDef> {
        norn_expr::env::interface_lineage(def, |name| {
            self.interfaces.get(name).map(|entry| entry.def)
        })
        .unwrap_or_else(|_| vec![def])
    }

    // ----- effective property tables -----------------------------------

    /// The effective property table of a registered type (memoized, so
    /// extension findings are reported once).
    fn table(&mut self, name: &'a ObjectTypeName) -> Rc<Table<'a>> {
        if let Some(table) = self.tables.get(name) {
            return Rc::clone(table);
        }
        let Some(entry) = self.types.get(name) else {
            return Rc::default();
        };
        let def = entry.def;
        let parent = self.parent(name);
        let table = Rc::new(self.table_for(def, parent, true));
        self.tables.insert(name, Rc::clone(&table));
        table
    }

    /// Build a type's table: the supertype's members, then the type's own
    /// declarations (overrides inherit the overridden constraint sets), then
    /// the constraint sets of the interfaces it implements directly, then the
    /// members its extensions add (when `with_extensions`).
    fn table_for(
        &mut self,
        def: &'a ObjectTypeDef,
        parent: Option<&'a ObjectTypeName>,
        with_extensions: bool,
    ) -> Table<'a> {
        let mut table = parent.map_or_else(Table::new, |parent| (*self.table(parent)).clone());
        let (catalog, registry) = (self.context.catalog, self.context.registry);
        for property in &def.properties {
            let mut member = Resolved::new(property, catalog, registry, Origin::Type(&def.name));
            match table.get(&property.id) {
                // A second declaration in this type: reported as a duplicate.
                Some(existing) if existing.origin == Origin::Type(&def.name) => continue,
                // A valid override keeps the overridden constraint sets; an
                // invalid one is reported and conjoins nothing.
                Some(inherited)
                    if !inherited.property.is_read_only()
                        && !property.is_read_only()
                        && type_narrows(&member.shape(), &inherited.shape()) == Some(true) =>
                {
                    member.inherited = inherited
                        .own_constraints()
                        .chain(inherited.inherited.iter().copied())
                        .collect();
                    member.markings.extend(inherited.markings.iter().cloned());
                    // An override without a default keeps the inherited one,
                    // checked against the override.
                    if property.default.is_none() {
                        member.default = inherited.default;
                    }
                }
                Some(_) | None => {}
            }
            table.insert(&property.id, member);
        }

        self.apply_interface_requirements(def, &mut table);

        if with_extensions {
            let extensions = self.extensions.get(&def.name).cloned().unwrap_or_default();
            for (index, local, extension) in extensions {
                let origin = Origin::Extension { index, local };
                for (position, property) in extension.properties.iter().enumerate() {
                    match table.get(&property.id) {
                        // Declared twice in one extension: reported there.
                        Some(existing) if existing.origin == origin => {}
                        Some(existing) => {
                            if local {
                                let (index, position) = (index.to_string(), position.to_string());
                                let at = [
                                    self.roots.extensions,
                                    &[index.as_str(), "properties", position.as_str()],
                                ]
                                .concat();
                                let owner = match existing.origin {
                                    Origin::Type(owner) => {
                                        format!("object type {:?}", owner.as_str())
                                    }
                                    Origin::Extension { .. } => "another extension".to_owned(),
                                };
                                self.out.push(error(
                                    codes::EXTENSION_SHADOWS_MEMBER,
                                    format!(
                                        "extension property {:?} shadows the member of the same id that {owner} declares on {:?}",
                                        property.id.as_str(),
                                        def.name.as_str()
                                    ),
                                    span_at(&at, &["id"]),
                                ));
                            }
                        }
                        None => {
                            table.insert(
                                &property.id,
                                Resolved::new(property, catalog, registry, origin),
                            );
                        }
                    }
                }
            }
        }
        table
    }

    fn apply_interface_requirements(&self, def: &'a ObjectTypeDef, table: &mut Table<'a>) {
        let mut implemented = BTreeSet::new();
        for interface in &def.implements {
            if !implemented.insert(interface) {
                continue;
            }
            let Some(entry) = self.interfaces.get(interface) else {
                continue;
            };
            for requirement in self.requirements(entry.def) {
                if let Some(member) = table.get_mut(&requirement.property.id)
                    && type_narrows(&member.shape(), &requirement.shape()) == Some(true)
                {
                    member.inherited.push(&requirement.property.constraints);
                    member
                        .inherited
                        .extend(requirement.resolved.chain.iter().copied());
                    member.markings.extend(
                        self.interface_lineage(entry.def)
                            .iter()
                            .flat_map(|owner| owner.markings.iter().cloned()),
                    );
                    member
                        .markings
                        .extend(requirement.property.markings.iter().cloned());
                    member.markings.extend(
                        requirement
                            .resolved
                            .markings
                            .iter()
                            .map(|marking| (*marking).clone()),
                    );
                    member.markings.sort();
                    member.markings.dedup();
                }
            }
        }
    }

    // ----- interfaces ----------------------------------------------------

    fn check_interface(&mut self, index: usize, def: &'a InterfaceDef) {
        let position = index.to_string();
        let base = pointer(self.roots.interfaces, &position);
        self.check_interface_parents(&base, def);
        let known = |name: &InterfaceName| self.interfaces.contains_key(name);
        let findings = check_status(
            &base,
            &def.status,
            def.name.as_str(),
            InterfaceName::as_str,
            known,
            (
                interfaces::codes::INVALID_STATUS_DATE,
                interfaces::codes::UNKNOWN_REPLACEMENT,
            ),
        );
        self.out.extend(findings);
        interfaces::check_requirements(
            &base,
            def,
            self.context.catalog,
            self.context.registry,
            &mut self.out,
        );
        let known = |target: &ObjectTypeName| self.definition_known(target);
        let findings = interfaces::check_link_requirements(&base, def, known);
        self.out.extend(findings);
        self.check_interface_invariants(&base, def);
    }

    fn check_interface_parents(&mut self, base: &[&str], def: &'a InterfaceDef) {
        if def.extends.is_empty() {
            return;
        }
        let mut seen = BTreeSet::new();
        for (index, parent) in def.extends.iter().enumerate() {
            if !seen.insert(parent) {
                self.out.push(error(
                    interfaces::codes::INVALID_INHERITANCE,
                    format!("interface parent {:?} is listed twice", parent.as_str()),
                    span_at(base, &["extends", &index.to_string()]),
                ));
            }
        }
        if let Err(name) = norn_expr::env::interface_lineage(def, |name| {
            self.interfaces.get(name).map(|entry| entry.def)
        }) {
            self.out.push(error(
                interfaces::codes::INVALID_INHERITANCE,
                format!(
                    "interface ancestry contains missing or cyclic parent {:?}",
                    name.as_str()
                ),
                span_at(base, &["extends"]),
            ));
            return;
        }
        let requirements = self.requirements(def);
        let group: Vec<_> = requirements
            .iter()
            .map(|requirement| (Some(0), requirement))
            .collect();
        let mut groups: BTreeMap<&PropertyId, Vec<_>> = BTreeMap::new();
        for requirement in group {
            groups
                .entry(&requirement.1.property.id)
                .or_default()
                .push(requirement);
        }
        for group in groups.values() {
            let conflict = requirement_conflict(group);
            let widens = group.iter().any(|(_, child)| {
                child.interface == &def.name
                    && group.iter().any(|(_, parent)| {
                        parent.interface != &def.name
                            && matches!(
                                narrowing(&child.shape(), &parent.shape()),
                                Narrowing::TypeMismatch | Narrowing::WidensNullability
                            )
                    })
            });
            if conflict.is_some() || widens {
                self.out.push(error(
                    interfaces::codes::INVALID_INHERITANCE,
                    conflict.unwrap_or_else(|| {
                        "interface redeclaration widens a parent member".to_owned()
                    }),
                    span_at(base, &["extends"]),
                ));
            }
        }
        self.check_interface_parent_links(base, def);
    }

    // ----- object types ----------------------------------------------------

    fn check_object_type(&mut self, index: usize, def: &'a ObjectTypeDef) {
        let position = index.to_string();
        let base = pointer(self.roots.object_types, &position);
        if let Some(parent) = &def.extends
            && parent.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES
        {
            self.out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "supertype name is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                    parent.as_str().len()
                ),
                span_at(&base, &["extends"]),
            ));
        }
        for (index, interface) in def.implements.iter().enumerate() {
            if interface.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES {
                self.out.push(error(
                    crate::codes::IDENTIFIER_TOO_LONG,
                    format!(
                        "interface reference is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                        interface.as_str().len()
                    ),
                    span_at(&base, &["implements", index.to_string().as_str()]),
                ));
            }
        }
        let registered = self
            .types
            .get(&def.name)
            .is_some_and(|entry| entry.local == Some(index));

        let known = |name: &ObjectTypeName| self.types.contains_key(name);
        let findings = check_status(
            &base,
            &def.status,
            def.name.as_str(),
            ObjectTypeName::as_str,
            known,
            (codes::INVALID_STATUS_DATE, codes::UNKNOWN_REPLACEMENT),
        );
        self.out.extend(findings);

        if let Some(parent) = &def.extends {
            if !self.types.contains_key(parent) {
                self.out.push(error(
                    codes::UNKNOWN_SUPERTYPE,
                    format!("extends {:?}, which names no object type", parent.as_str()),
                    span_at(&base, &["extends"]),
                ));
            } else if registered && self.cyclic.contains(&def.name) {
                self.out.push(error(
                    codes::INHERITANCE_CYCLE,
                    format!("the extends chain of {:?} returns to it", def.name.as_str()),
                    span_at(&base, &["extends"]),
                ));
            }
        }

        self.check_tenant_type(&base, def);

        // Duplicates of a registered name are checked standalone.
        let parent = if registered {
            self.parent(&def.name)
        } else {
            def.extends
                .as_ref()
                .and_then(|parent| self.types.get_key_value(parent))
                .map(|(parent, _)| *parent)
        };
        let inherited = parent.map_or_else(|| Rc::new(Table::new()), |parent| self.table(parent));
        self.check_own_properties(&base, def, &inherited);

        let table = if registered {
            self.table(&def.name)
        } else {
            Rc::new(self.table_for(def, parent, false))
        };
        let ancestors = parent.map_or_else(Vec::new, |parent| {
            let mut chain = vec![parent];
            chain.extend(self.ancestors(parent));
            chain
        });
        let inherited_keys: Vec<_> = ancestors
            .iter()
            .filter_map(|ancestor| self.types.get(ancestor).map(|entry| (*ancestor, entry.def)))
            .flat_map(|(ancestor, ancestor_def)| {
                ancestor_def.identity.iter().map(move |key| (ancestor, key))
            })
            .collect();
        check_keys(&base, &def.identity, &inherited_keys, &table, &mut self.out);

        let conflicted = self.check_implements(&base, def, &ancestors, &table, &inherited);

        self.check_declared_members(&base, def, &table, &conflicted);

        self.check_type_links(&base, def, registered, parent);
        self.check_owned_collections(&base, def, parent);
        self.check_link_requirements(&base, def, registered, &ancestors, &conflicted);
        self.check_invariants(&base, def, &ancestors);
        self.check_lifecycles(&base, def, parent, &table);
        self.check_accounting_leg_declaration(&base, def, &ancestors, &table);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "owned collection admission checks inheritance, link cardinality, identifiers, bounds, and the two reserved row properties in one diagnostic pass"
    )]
    fn check_owned_collections(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        parent: Option<&'a ObjectTypeName>,
    ) {
        let inherited_collections = parent
            .into_iter()
            .flat_map(|parent| std::iter::once(parent).chain(self.ancestors(parent)))
            .filter_map(|ancestor| self.types.get(ancestor))
            .flat_map(|entry| entry.def.owned_collections.iter().cloned())
            .collect::<Vec<_>>();
        for (index, link) in def.links.iter().enumerate() {
            let Some(collection) = inherited_collections
                .iter()
                .find(|collection| collection.id == link.id)
            else {
                continue;
            };
            let target_mismatch = link.target != collection.child_type;
            let cardinality_mismatch = link.cardinality.to != Multiplicity::Many
                || link.cardinality.from != Multiplicity::One;
            if target_mismatch || cardinality_mismatch {
                let index = index.to_string();
                let member = if target_mismatch {
                    "target"
                } else {
                    "cardinality"
                };
                let detail = if target_mismatch {
                    format!(
                        "link {:?} is pinned by an inherited owned collection to exact child type {:?}",
                        collection.id.as_str(),
                        collection.child_type.as_str()
                    )
                } else {
                    format!(
                        "link {:?} is pinned by an inherited owned collection to many/from-one cardinality",
                        collection.id.as_str()
                    )
                };
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    detail,
                    span_at(base, &["links", index.as_str(), member]),
                ));
            }
        }
        if inherited_collections.is_empty() && def.owned_collections.is_empty() {
            return;
        }
        // Resolve inherited link properties too: Add supplies no link values.
        let effective_links = self.effective_links(&def.name);
        for collection in inherited_collections.iter().chain(&def.owned_collections) {
            if effective_links
                .iter()
                .find(|link| link.id == collection.id)
                .is_some_and(|link| {
                    link.properties.iter().any(|property| {
                        !property.computed && !property.nullable && property.default.is_none()
                    })
                })
            {
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    format!("owned link {:?} must default every required stored link property; row Add supplies no link values", collection.id.as_str()),
                    span_at(base, &["ownedCollections"]),
                ));
            }
        }
        let mut ids = BTreeSet::new();
        for (index, collection) in def.owned_collections.iter().enumerate() {
            let index = index.to_string();
            let at = [base, &["ownedCollections", index.as_str()]].concat();
            let id = collection.id.as_str();
            if inherited_collections
                .iter()
                .any(|inherited| inherited.id.as_str() == id)
            {
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    format!(
                        "owned collection {id:?} is inherited and cannot be redeclared on a subtype"
                    ),
                    span_at(&at, &["id"]),
                ));
                continue;
            }
            if !ids.insert(id) {
                self.out.push(error(
                    codes::DUPLICATE_LINK_ID,
                    format!("owned collection {id:?} is declared more than once"),
                    span_at(&at, &["id"]),
                ));
                continue;
            }
            if !(1..=100).contains(&collection.max_rows.0) {
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    "maxRows must be between 1 and the platform limit of 100".to_owned(),
                    span_at(&at, &["maxRows"]),
                ));
            }
            let mut source = Some(&def.name);
            let mut link = None;
            while let Some(name) = source {
                if let Some(owner) = self.types.get(name)
                    && let Some(found) = owner.def.links.iter().find(|link| link.id.as_str() == id)
                {
                    link = Some(found.clone());
                    break;
                }
                source = self.parent(name);
            }
            match link {
                Some(link)
                    if link.target == collection.child_type
                        && link.cardinality.to == Multiplicity::Many
                        && link.cardinality.from == Multiplicity::One => {}
                _ => self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    format!("owned collection {id:?} requires a declared many/from-one link to exact child type {:?}", collection.child_type.as_str()),
                    span_at(&at, &["id"]),
                )),
            }
            if !self.types.contains_key(&collection.child_type) {
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    format!(
                        "owned collection {id:?} names unknown child type {:?}",
                        collection.child_type.as_str()
                    ),
                    span_at(&at, &["childType"]),
                ));
                continue;
            }
            let nested = std::iter::once(&collection.child_type)
                .chain(self.ancestors(&collection.child_type))
                .filter_map(|name| self.types.get(name))
                .any(|entry| !entry.def.owned_collections.is_empty());
            if nested {
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    "owned child types may not themselves own collections, including inherited collections".to_owned(),
                    span_at(&at, &["childType"]),
                ));
            }
            let child_table = self.table(&collection.child_type);
            for (property_id, expected, member) in [
                (&collection.row_id_property, "text", "rowIdProperty"),
                (&collection.ordering_key, "integer", "orderingKey"),
            ] {
                let valid = child_table.get(property_id).is_some_and(|property| {
                    let kind = matches!(
                        (expected, property.base()),
                        ("text", Some(PropertyType::Text { .. }))
                            | ("integer", Some(PropertyType::Integer { .. }))
                    );
                    kind && !property.nullable() && !property.property.is_read_only()
                });
                if !valid {
                    self.out.push(error(
                        codes::INVALID_OWNED_COLLECTION,
                        format!("{member} must name a stored, required {expected} property on child type {:?}", collection.child_type.as_str()),
                        span_at(&at, &[member]),
                    ));
                }
            }
            if (1..=100).contains(&collection.max_rows.0)
                && child_table
                    .get(&collection.ordering_key)
                    .is_some_and(|property| {
                        matches!(property.base(), Some(PropertyType::Integer {}))
                            && !ordering_key_admits_dense_positions(property, collection.max_rows.0)
                    })
            {
                self.out.push(error(
                    codes::INVALID_OWNED_COLLECTION,
                    "orderingKey constraints must admit every kernel-generated row position from 1 through maxRows".to_owned(),
                    span_at(&at, &["orderingKey"]),
                ));
            }
        }
    }

    fn check_accounting_leg_declaration(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        ancestors: &[&'a ObjectTypeName],
        table: &Table<'a>,
    ) {
        let inherited = ancestors.iter().find_map(|ancestor| {
            self.types
                .get(ancestor)
                .and_then(|entry| entry.def.accounting_leg.as_ref())
        });
        if let (Some(inherited), Some(declared)) = (inherited, def.accounting_leg.as_ref())
            && inherited != declared
        {
            self.out.push(error(
                codes::INVALID_ACCOUNTING_LEG_DECLARATION,
                format!(
                    "type {:?} must preserve the accounting leg property mapping inherited from its supertype",
                    def.name.as_str()
                ),
                span_at(base, &["accountingLeg"]),
            ));
        }

        let declaration = def.accounting_leg.as_ref().or(inherited);
        let Some(declaration) = declaration else {
            return;
        };
        if declaration.journal_id_property == declaration.leg_property {
            self.out.push(error(
                codes::INVALID_ACCOUNTING_LEG_DECLARATION,
                "journalIdProperty and legProperty must name different properties".to_owned(),
                span_at(base, &["accountingLeg"]),
            ));
            return;
        }

        for (property_id, member, expected) in [
            (
                &declaration.journal_id_property,
                "journalIdProperty",
                "text",
            ),
            (&declaration.leg_property, "legProperty", "json-ext"),
        ] {
            let Some(resolved) = table.get(property_id) else {
                self.out.push(error(
                    codes::INVALID_ACCOUNTING_LEG_DECLARATION,
                    format!(
                        "accounting leg {member} names unknown property {:?}",
                        property_id.as_str()
                    ),
                    span_at(base, &["accountingLeg", member]),
                ));
                continue;
            };
            let valid_kind = matches!(
                (expected, resolved.base()),
                ("text", Some(PropertyType::Text { .. }))
                    | ("json-ext", Some(PropertyType::JsonExt { .. }))
            );
            if !valid_kind || resolved.property.is_read_only() || resolved.nullable() {
                self.out.push(error(
                    codes::INVALID_ACCOUNTING_LEG_DECLARATION,
                    format!(
                        "accounting leg {member} must reference a stored, non-nullable {expected} property"
                    ),
                    span_at(base, &["accountingLeg", member]),
                ));
            }
        }
    }

    /// A tenant layer's object types are subtypes named in its namespace.
    fn check_tenant_type(&mut self, base: &[&str], def: &'a ObjectTypeDef) {
        if let Some(tenant) = self.context.tenant {
            if def.extends.is_none() {
                self.out.push(error(
                    codes::TENANT_TYPE_NOT_SUBTYPE,
                    format!(
                        "tenant object type {:?} must extend a type: a tenant adds subtypes of installed types, never new base types",
                        def.name.as_str()
                    ),
                    span_at(base, &["name"]),
                ));
            }
            if def.name.as_str().split('.').next() != Some(tenant.as_str()) {
                self.out.push(error(
                    codes::TENANT_TYPE_OUTSIDE_NAMESPACE,
                    format!(
                        "tenant object type {:?} must be named in the tenant namespace, such as \"{}.{}\"",
                        def.name.as_str(),
                        tenant.as_str(),
                        def.name.local_name()
                    ),
                    span_at(base, &["name"]),
                ));
            }
        }
    }

    /// Defaults and joint constraints of declared properties, once every
    /// inherited and interface constraint set is known. Ids whose interface
    /// requirements conflict are reported there.
    fn check_declared_members(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        table: &Table<'a>,
        conflicted: &BTreeSet<&'a PropertyId>,
    ) {
        for (position, property) in def.properties.iter().enumerate() {
            if let Some(member) = table.get(&property.id)
                && std::ptr::eq(member.property, property)
            {
                let position = position.to_string();
                let at = [base, &["properties", position.as_str()]].concat();
                check_property_default(&at, member, &mut self.out);
                // One inverted set alone is `inverted_range` where declared.
                if !member.inherited.is_empty()
                    && !conflicted.contains(&property.id)
                    && member
                        .constraints()
                        .iter()
                        .all(|set| unsatisfiable(&[*set]).is_none())
                    && let Some(reason) = unsatisfiable(&member.constraints())
                {
                    self.out.push(unsatisfiable_constraints(
                        property.id.as_str(),
                        &reason,
                        span_at(&at, &[]),
                    ));
                }
            }
        }
    }

    /// Id grammar, duplicates, types, overrides, and member namespaces of the
    /// properties a type declares.
    fn check_own_properties(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        inherited: &Table<'a>,
    ) {
        let (catalog, registry) = (self.context.catalog, self.context.registry);
        let mut ids = BTreeSet::new();
        for (position, property) in def.properties.iter().enumerate() {
            let position = position.to_string();
            let at = [base, &["properties", position.as_str()]].concat();
            check_property_shape(&at, property, catalog, registry, &mut self.out);
            if !ids.insert(&property.id) {
                self.out.push(error(
                    codes::DUPLICATE_PROPERTY_ID,
                    format!("property id {:?} is declared twice", property.id.as_str()),
                    span_at(&at, &["id"]),
                ));
                continue;
            }
            if let Some(overridden) = inherited.get(&property.id) {
                let member = Resolved::new(property, catalog, registry, Origin::Type(&def.name));
                self.check_override(&at, &member, overridden);
            } else {
                let tenant = self.context.tenant;
                self.check_member_namespace(&at, "id", "property", property.id.as_str(), tenant);
            }
        }
    }

    fn check_override(&mut self, at: &[&str], member: &Resolved<'a>, overridden: &Resolved<'a>) {
        let id = member.property.id.as_str();
        let owner = match overridden.origin {
            Origin::Type(owner) => format!("supertype {:?}", owner.as_str()),
            Origin::Extension { .. } => "an extension of a supertype".to_owned(),
        };
        if overridden.property.is_read_only() || member.property.is_read_only() {
            self.out.push(error(
                codes::OVERRIDE_COMPUTED,
                format!(
                    "property {id:?} overrides the one {owner} declares; a computed property is never overridden, and an override is never computed"
                ),
                span_at(at, &[]),
            ));
            return;
        }
        match narrowing(&member.shape(), &overridden.shape()) {
            Narrowing::TypeMismatch => self.out.push(error(
                codes::OVERRIDE_TYPE_MISMATCH,
                format!(
                    "property {id:?} overrides the one {owner} declares with a type that is not the same or narrower; an override may only narrow"
                ),
                span_at(at, &["type"]),
            )),
            Narrowing::WidensNullability => self.out.push(error(
                codes::OVERRIDE_WIDENS_NULLABILITY,
                format!(
                    "property {id:?} is nullable, but the property {owner} declares is not; an override may only narrow"
                ),
                span_at(at, &[]),
            )),
            Narrowing::Narrows | Narrowing::Unknown => {}
        }
    }

    /// A new member (`noun`: a property, link, inverse role, or invariant,
    /// whose id is at `at`/`field`) of a tenant type or an extension is
    /// `<namespace>__<name>`; a new member of a base-layer type never
    /// contains `__`.
    fn check_member_namespace(
        &mut self,
        at: &[&str],
        field: &str,
        noun: &str,
        id: &str,
        namespace: Option<&ExtensionNamespace>,
    ) {
        if !matches_grammar(&MEMBER_ID_GRAMMAR, id) {
            return; // an invalid id is reported where declared
        }
        match namespace {
            Some(namespace) => {
                if !is_namespaced(id, namespace) {
                    self.out.push(error(
                        codes::UNNAMESPACED_MEMBER,
                        format!(
                            "added {noun} {id:?} must be named \"{}__<name>\" in its layer's extension namespace, so it cannot shadow a base member",
                            namespace.as_str()
                        ),
                        span_at(at, &[field]),
                    ));
                }
            }
            None => {
                if id.contains(EXTENSION_MEMBER_SEPARATOR) {
                    self.out.push(error(
                        codes::RESERVED_MEMBER_SEPARATOR,
                        format!(
                            "{noun} id {id:?} contains \"__\", which is reserved for extension members (\"<namespace>__<name>\")"
                        ),
                        span_at(at, &[field]),
                    ));
                }
            }
        }
    }

    /// `implements`: known, listed once, conformance of each directly
    /// implemented interface, and conflicts among every requirement the type
    /// is held to.
    fn check_implements(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        ancestors: &[&'a ObjectTypeName],
        table: &Table<'a>,
        inherited: &Table<'a>,
    ) -> BTreeSet<&'a PropertyId> {
        let mut listed = BTreeSet::new();
        let mut direct = Vec::new();
        for (position, interface) in def.implements.iter().enumerate() {
            let at = span_at(base, &["implements", position.to_string().as_str()]);
            if !listed.insert(interface) {
                self.out.push(error(
                    codes::DUPLICATE_INTERFACE,
                    format!("interface {:?} is listed twice", interface.as_str()),
                    at,
                ));
                continue;
            }
            match self.interfaces.get(interface) {
                Some(entry) => direct.push((position, entry.def)),
                None => self.out.push(error(
                    codes::UNKNOWN_INTERFACE,
                    format!(
                        "implements {:?}, which names no interface",
                        interface.as_str()
                    ),
                    at,
                )),
            }
        }

        let all = self.held_requirements(&direct, ancestors);
        let ancestor_interfaces: BTreeSet<&InterfaceName> = ancestors
            .iter()
            .filter_map(|ancestor| self.types.get(ancestor))
            .flat_map(|entry| entry.def.implements.iter())
            .collect();
        let conflicted = self.report_conflicts(base, &all);
        // An id some interface requires as a link conflicts with a property
        // requirement; that is reported with the link requirements.
        let required_links: BTreeSet<&str> = direct
            .iter()
            .map(|(_, interface)| *interface)
            .chain(
                ancestor_interfaces
                    .iter()
                    .filter_map(|name| self.interfaces.get(*name).map(|entry| entry.def)),
            )
            .flat_map(|interface| self.interface_lineage(interface))
            .flat_map(|interface| interface.links.iter().map(|link| link.id.as_str()))
            .collect();

        for (position, requirement) in &all {
            let Some(position) = position else { continue };
            let id = &requirement.property.id;
            if conflicted.contains(id) || required_links.contains(id.as_str()) {
                continue;
            }
            let at = span_at(base, &["implements", position.to_string().as_str()]);
            let interface = requirement.interface.as_str();
            let Some(member) = table.get(id) else {
                self.out.push(error(
                    codes::MISSING_INTERFACE_PROPERTY,
                    format!(
                        "interface {interface:?} requires property {:?}, which the type neither declares nor inherits",
                        id.as_str()
                    ),
                    at,
                ));
                continue;
            };
            match narrowing(&member.shape(), &requirement.shape()) {
                Narrowing::TypeMismatch => self.out.push(error(
                    codes::INTERFACE_PROPERTY_TYPE_MISMATCH,
                    format!(
                        "property {:?} does not have the type interface {interface:?} requires, or a narrowing of it",
                        id.as_str()
                    ),
                    at,
                )),
                Narrowing::WidensNullability => self.out.push(error(
                    codes::INTERFACE_PROPERTY_WIDENS_NULLABILITY,
                    format!(
                        "property {:?} is nullable, but interface {interface:?} requires a value",
                        id.as_str()
                    ),
                    at,
                )),
                Narrowing::Narrows => {
                    let newly_applied = member.origin != Origin::Type(&def.name)
                        && !ancestor_interfaces.contains(requirement.interface);
                    if newly_applied {
                        self.check_inherited_member(&at, member, requirement, inherited);
                    }
                }
                Narrowing::Unknown => {}
            }
        }
        conflicted
    }

    /// Every requirement a type is held to: those of the interfaces it lists
    /// (with their position), then those of interfaces its supertypes
    /// implement.
    fn held_requirements(
        &self,
        direct: &[(usize, &'a InterfaceDef)],
        ancestors: &[&'a ObjectTypeName],
    ) -> Vec<(Option<usize>, Requirement<'a>)> {
        let mut all: Vec<(Option<usize>, Requirement<'a>)> = Vec::new();
        for (position, interface) in direct {
            all.extend(
                self.requirements(interface)
                    .into_iter()
                    .map(|requirement| (Some(*position), requirement)),
            );
        }
        let mut seen: BTreeSet<&InterfaceName> = direct.iter().map(|(_, def)| &def.name).collect();
        for ancestor in ancestors {
            let Some(entry) = self.types.get(ancestor) else {
                continue;
            };
            for interface in &entry.def.implements {
                if let Some(interface) = self.interfaces.get(interface)
                    && seen.insert(&interface.def.name)
                {
                    all.extend(
                        self.requirements(interface.def)
                            .into_iter()
                            .map(|requirement| (None, requirement)),
                    );
                }
            }
        }

        all
    }

    /// An interface a type implements holds a member it inherits (or gets
    /// from an extension) to new constraints: its effective default must meet
    /// them, and together with the inherited ones they must admit a value.
    fn check_inherited_member(
        &mut self,
        at: &crate::Span,
        member: &Resolved<'a>,
        requirement: &Requirement<'a>,
        inherited: &Table<'a>,
    ) {
        let sets: Vec<_> = std::iter::once(&requirement.property.constraints)
            .chain(requirement.resolved.chain.iter().copied())
            .collect();
        check_default_against(at, member, &sets, &mut self.out);
        let before = inherited
            .get(&member.property.id)
            .map_or_else(Vec::new, Resolved::constraints);
        if unsatisfiable(&before).is_none()
            && let Some(reason) = unsatisfiable(&member.constraints())
        {
            self.out.push(unsatisfiable_constraints(
                member.property.id.as_str(),
                &reason,
                at.clone(),
            ));
        }
    }

    /// Report each property id whose requirements, from every interface the
    /// type is held to, no single property can meet together: two types that
    /// neither narrows, inline enums with no value common to all of them, or
    /// bounds that jointly admit no value. Only ids required by at least one
    /// interface the type lists itself are reported here; the rest are the
    /// supertype's. Returns the conflicted ids.
    fn report_conflicts(
        &mut self,
        base: &[&str],
        all: &[(Option<usize>, Requirement<'a>)],
    ) -> BTreeSet<&'a PropertyId> {
        let mut groups: BTreeMap<&'a PropertyId, Vec<(Option<usize>, &Requirement<'a>)>> =
            BTreeMap::new();
        for (position, requirement) in all {
            groups
                .entry(&requirement.property.id)
                .or_default()
                .push((*position, requirement));
        }
        let mut conflicted = BTreeSet::new();
        for (id, group) in groups {
            let Some(position) = group.iter().find_map(|(position, _)| *position) else {
                continue;
            };
            if group.len() < 2 {
                continue;
            }
            let Some(reason) = requirement_conflict(&group) else {
                continue;
            };
            conflicted.insert(id);
            let names: Vec<&str> = group
                .iter()
                .map(|(_, requirement)| requirement.interface.as_str())
                .collect();
            self.out.push(error(
                codes::INTERFACE_REQUIREMENT_CONFLICT,
                format!(
                    "interfaces {names:?} all require member {:?}, but no single member can meet them together: {reason}",
                    id.as_str()
                ),
                span_at(base, &["implements", position.to_string().as_str()]),
            ));
        }
        conflicted
    }

    // ----- extensions ------------------------------------------------------

    fn check_extension(&mut self, index: usize, extension: &'a ObjectTypeExtension) {
        let position = index.to_string();
        let base = pointer(self.roots.extensions, &position);
        let namespace = &extension.namespace;
        if extension.target.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES {
            self.out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "extension target name is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                    extension.target.as_str().len()
                ),
                span_at(&base, &["target"]),
            ));
        }
        let valid_namespace = matches_grammar(&EXTENSION_NAMESPACE_GRAMMAR, namespace.as_str());
        if !valid_namespace {
            self.out.push(error(
                codes::INVALID_EXTENSION_NAMESPACE,
                format!(
                    "extension namespace {:?} must be one lower-case alphanumeric segment ([a-z][a-z0-9]*)",
                    namespace.as_str()
                ),
                span_at(&base, &["namespace"]),
            ));
        }
        if let Some(tenant) = self.context.tenant
            && tenant != namespace
        {
            self.out.push(error(
                codes::EXTENSION_NAMESPACE_MISMATCH,
                format!(
                    "extension namespace {:?} is not the tenant namespace {:?}",
                    namespace.as_str(),
                    tenant.as_str()
                ),
                span_at(&base, &["namespace"]),
            ));
        }
        let target = self
            .types
            .get_key_value(&extension.target)
            .map(|(target, _)| *target);
        if self
            .types
            .get(&extension.target)
            .is_some_and(|entry| entry.local.is_some())
        {
            self.out.push(error(
                codes::EXTENSION_OF_LOCAL_TYPE,
                format!(
                    "extension target {:?} is defined in the same layer; declare its properties on the type itself",
                    extension.target.as_str()
                ),
                span_at(&base, &["target"]),
            ));
        }
        if target.is_none() {
            self.out.push(error(
                codes::UNKNOWN_EXTENSION_TARGET,
                format!(
                    "extension target {:?} names no object type",
                    extension.target.as_str()
                ),
                span_at(&base, &["target"]),
            ));
        }

        let (catalog, registry) = (self.context.catalog, self.context.registry);
        let mut ids = BTreeSet::new();
        for (position, property) in extension.properties.iter().enumerate() {
            let position = position.to_string();
            let at = [base.as_slice(), &["properties", position.as_str()]].concat();
            check_property_shape(&at, property, catalog, registry, &mut self.out);
            if !ids.insert(&property.id) {
                self.out.push(error(
                    codes::DUPLICATE_PROPERTY_ID,
                    format!("property id {:?} is declared twice", property.id.as_str()),
                    span_at(&at, &["id"]),
                ));
                continue;
            }
            if valid_namespace {
                self.check_member_namespace(
                    &at,
                    "id",
                    "property",
                    property.id.as_str(),
                    Some(namespace),
                );
            }
            let member = Resolved::new(
                property,
                catalog,
                registry,
                Origin::Extension { index, local: true },
            );
            check_property_default(&at, &member, &mut self.out);
        }
        self.check_extension_links(&base, index, extension, valid_namespace);
        // Shadowing is found while building the target's table.
        if let Some(target) = target {
            self.table(target);
        }
    }
}

/// The kernel assigns a dense one-based ordering value to every row, so the
/// effective integer bounds must admit every position through the collection
/// maximum. Malformed or out-of-range bounds are diagnosed by property
/// validation; this check only compares bounds already parsed as integers.
fn ordering_key_admits_dense_positions(property: &Resolved<'_>, max_rows: u64) -> bool {
    let Ok(last_position) = i64::try_from(max_rows) else {
        return false;
    };
    property.constraints().into_iter().all(|constraints| {
        constraints.min.as_ref().is_none_or(|bound| {
            bound
                .as_str()
                .parse::<i64>()
                .ok()
                .is_none_or(|minimum| minimum <= 1)
        }) && constraints.max.as_ref().is_none_or(|bound| {
            bound
                .as_str()
                .parse::<i64>()
                .ok()
                .is_none_or(|maximum| maximum >= last_position)
        })
    })
}

fn unsatisfiable_constraints(id: &str, reason: &str, at: crate::Span) -> Diagnostic {
    error(
        codes::UNSATISFIABLE_CONSTRAINTS,
        format!("the constraints that apply to property {id:?} together admit no value: {reason}"),
        at,
    )
}

/// Why a group of requirements for one id cannot be met by one property.
fn requirement_conflict(group: &[(Option<usize>, &Requirement<'_>)]) -> Option<String> {
    for (index, (_, first)) in group.iter().enumerate() {
        for (_, second) in group.iter().skip(index.saturating_add(1)) {
            if !jointly_satisfiable(&first.shape(), &second.shape()) {
                return Some(format!(
                    "the types of {:?} and {:?} are not both narrowable",
                    first.interface.as_str(),
                    second.interface.as_str()
                ));
            }
        }
    }
    // Enums: one property's values must be a subset of every requirement's.
    let enums: Option<Vec<_>> = group
        .iter()
        .map(|(_, requirement)| match requirement.resolved.resolution {
            TypeResolution::Base(PropertyType::Enum { values }) => Some(values),
            TypeResolution::Base(_)
            | TypeResolution::Registry
            | TypeResolution::Unresolved { .. } => None,
        })
        .collect();
    if let Some(enums) = enums
        && let Some((first, rest)) = enums.split_first()
        && !first.iter().any(|value| {
            rest.iter()
                .all(|values| values.iter().any(|other| other.value == value.value))
        })
    {
        return Some("the enum values have no value in common".to_owned());
    }
    let sets: Vec<_> = group
        .iter()
        .flat_map(|(_, requirement)| {
            std::iter::once(&requirement.property.constraints)
                .chain(requirement.resolved.chain.iter().copied())
        })
        .collect();
    unsatisfiable(&sets)
}

/// `root` followed by one array position.
fn pointer<'s>(root: &[&'s str], position: &'s str) -> Vec<&'s str> {
    root.iter().copied().chain([position]).collect()
}

/// `id` is `<namespace>__<name>` with `name` a plain member id.
fn is_namespaced(id: &str, namespace: &ExtensionNamespace) -> bool {
    id.strip_prefix(namespace.as_str())
        .and_then(|rest| rest.strip_prefix(EXTENSION_MEMBER_SEPARATOR))
        .is_some_and(|name| {
            matches_grammar(&MEMBER_ID_GRAMMAR, name) && !name.contains(EXTENSION_MEMBER_SEPARATOR)
        })
}

/// A resource status's `since` and `replacedBy`.
fn check_status<N>(
    base: &[&str],
    status: &ResourceStatus<N>,
    own_name: &str,
    as_str: fn(&N) -> &str,
    known: impl Fn(&N) -> bool,
    (date_code, replacement_code): (&str, &str),
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let ResourceStatus::Deprecated { since, replaced_by } = status else {
        return out;
    };
    if !valid_date(since.as_str()) {
        out.push(error(
            date_code,
            format!(
                "deprecated since {:?}, which is not a calendar date \"YYYY-MM-DD\"",
                since.as_str()
            ),
            span_at(base, &["status", "since"]),
        ));
    }
    if let Some(replacement) = replaced_by {
        if as_str(replacement).len() > MAX_OBJECT_TYPE_NAME_BYTES {
            out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "replacement name is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                    as_str(replacement).len()
                ),
                span_at(base, &["status", "replacedBy"]),
            ));
        }
        if !known(replacement) || as_str(replacement) == own_name {
            out.push(error(
                replacement_code,
                format!(
                    "replacedBy {:?} must name another definition of the same kind",
                    as_str(replacement)
                ),
                span_at(base, &["status", "replacedBy"]),
            ));
        }
    }
    out
}

// ----- narrowing ---------------------------------------------------------------

/// A property declaration as the narrowing relation sees it.
pub(crate) struct Shape<'r, 'a> {
    declared: &'a PropertyType,
    resolved: &'r ResolvedType<'a>,
    nullable: bool,
}

impl<'a> Resolved<'a> {
    pub(crate) fn shape(&self) -> Shape<'_, 'a> {
        Shape {
            declared: &self.property.property_type,
            resolved: &self.ty,
            nullable: self.nullable(),
        }
    }
}

/// How a declaration relates to one it must narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Narrowing {
    Narrows,
    TypeMismatch,
    WidensNullability,
    /// A type does not resolve; that is reported where it is declared.
    Unknown,
}

/// `sub` narrows `sup` when every value `sub` admits, `sup` admits too:
/// its type narrows `sup`'s ([`type_narrows`]), and it is not nullable unless
/// `sup` is. Constraints never widen: `sup`'s conjoin with `sub`'s.
fn narrowing(sub: &Shape<'_, '_>, sup: &Shape<'_, '_>) -> Narrowing {
    match type_narrows(sub, sup) {
        None => Narrowing::Unknown,
        Some(false) => Narrowing::TypeMismatch,
        Some(true) if sub.nullable && !sup.nullable => Narrowing::WidensNullability,
        Some(true) => Narrowing::Narrows,
    }
}

/// The type relation of [`narrowing`]:
///
/// - `sup` is `ref N`: `sub`'s `ref` chain passes through `N`, so it is `N`
///   or a shared type built on it.
/// - `sup` is inline: `sub` resolves to a local type that equals it, except
///   that an `enum` may keep a subset of the values, a `text` may add a
///   `format`, and a `json-ext` may add a `schema`.
///
/// `None` when either side does not resolve.
fn type_narrows(sub: &Shape<'_, '_>, sup: &Shape<'_, '_>) -> Option<bool> {
    let unresolved = |shape: &Shape<'_, '_>| {
        matches!(shape.resolved.resolution, TypeResolution::Unresolved { .. })
    };
    if unresolved(sub) || unresolved(sup) {
        return None;
    }
    if let PropertyType::Ref { name } = sup.declared {
        return Some(sub.resolved.names.contains(&name));
    }
    match (sub.resolved.resolution, sup.resolved.resolution) {
        (TypeResolution::Base(narrow), TypeResolution::Base(wide)) => {
            Some(base_narrows(narrow, wide))
        }
        _ => Some(false),
    }
}

fn base_narrows(narrow: &PropertyType, wide: &PropertyType) -> bool {
    match (narrow, wide) {
        (PropertyType::Text { format: narrow }, PropertyType::Text { format: wide }) => {
            wide.is_none() || narrow == wide
        }
        (PropertyType::Enum { values: narrow }, PropertyType::Enum { values: wide }) => narrow
            .iter()
            .all(|value| wide.iter().any(|allowed| allowed.value == value.value)),
        (PropertyType::JsonExt { schema: narrow }, PropertyType::JsonExt { schema: wide }) => {
            wide.is_none() || narrow == wide
        }
        _ => narrow == wide,
    }
}

/// Whether one property could narrow both requirements: one narrows the
/// other, or both are inline `enum`s with a common value.
fn jointly_satisfiable(first: &Shape<'_, '_>, second: &Shape<'_, '_>) -> bool {
    match (type_narrows(first, second), type_narrows(second, first)) {
        (None | Some(true), _) | (_, None | Some(true)) => true,
        (Some(false), Some(false)) => match (
            first.declared,
            second.declared,
            first.resolved.resolution,
            second.resolved.resolution,
        ) {
            (
                PropertyType::Enum { .. },
                PropertyType::Enum { .. },
                TypeResolution::Base(PropertyType::Enum { values: first }),
                TypeResolution::Base(PropertyType::Enum { values: second }),
            ) => first
                .iter()
                .any(|value| second.iter().any(|other| other.value == value.value)),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_ids_split_at_the_first_separator() {
        let acme = ExtensionNamespace("acme".to_owned());
        assert!(is_namespaced("acme__loyalty_tier", &acme));
        assert!(!is_namespaced("acme_loyalty_tier", &acme));
        assert!(!is_namespaced("acmex__tier", &acme));
        assert!(!is_namespaced("acme__", &acme));
        assert!(!is_namespaced("acme___tier", &acme));
        assert!(!is_namespaced("acme__a__b", &acme));
        assert!(!is_namespaced("other__tier", &acme));
    }

    #[test]
    fn narrowing_is_reflexive_and_admits_only_the_listed_narrowings() {
        let text = PropertyType::Text { format: None };
        let email = PropertyType::Text {
            format: Some(norn_ir::TextFormat::Email),
        };
        assert!(base_narrows(&text, &text));
        assert!(base_narrows(&email, &text));
        assert!(!base_narrows(&text, &email));
        let values = |names: &[&str]| PropertyType::Enum {
            values: names
                .iter()
                .map(|value| norn_ir::EnumValue {
                    value: (*value).to_owned(),
                    description: None,
                })
                .collect(),
        };
        assert!(base_narrows(&values(&["a"]), &values(&["a", "b"])));
        assert!(!base_narrows(&values(&["a", "c"]), &values(&["a", "b"])));
        assert!(!base_narrows(&PropertyType::Integer {}, &text));
    }
}
