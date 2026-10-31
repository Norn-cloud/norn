//! Checks over `EntityIR` v2 object types (T1-ONT-1b, #2512): property ids,
//! identity (single and composite keys, identity rules), defaults, and
//! computed properties.
//!
//! `norn-ir` only decodes [`ObjectTypeDef`]s; every consistency rule is a
//! diagnostic here. A property's type is resolved through the catalog of
//! shared property types (checked on its own by
//! [`crate::check_property_type_defs`]); inline types and property-level
//! constraints reuse that check's `property_type.validation.*` codes.
//!
//! Status, inheritance, interfaces, and namespaces (T1-ONT-1c, #2513) are
//! whole-model rules in [`crate::entity_model`]; their codes are listed here
//! too, because they are findings about object types. A type's keys and
//! defaults are checked against its effective properties (declared,
//! inherited, and added by extensions).
//!
//! Every ZEN Expression site of the model (computed properties, invariants,
//! lifecycle guards) must parse (T1-ONT-2, #2515; `check_zen_syntax`).
//! Invariant expressions are also bound and required to type-check as bool in
//! `entity_model::links`; other expression-site typing follows T1-EXPR-5
//! (#2523). Sites read through `self`
//! (`self.net_weight * self.reorder_quantity`), never bare property names
//! (`docs/platform/entity-ir-v2.md`, "Expression sites").
//!
//! Spans are JSON pointers into the checked definition array
//! (`/0/identity/1/properties/0`), or into the model document for
//! [`crate::check_entity_model`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES;
use norn_ir::{
    DefaultValue, IdentityRule, JsonValue, KeyName, KeySpec, MEMBER_ID_PATTERN, MarkingId,
    NamingResetPolicy, NamingSeriesIR, Normalization, OBJECT_TYPE_NAME_PATTERN, ObjectTypeDef,
    ObjectTypeName, PROPERTY_TYPE_NAME_PATTERN, Property, PropertyConstraints, PropertyId,
    PropertyKind, PropertyType, PropertyTypeDef, PropertyTypeName, ResolutionPolicyName,
    TextFormat, TimezonePolicy,
};

use crate::Diagnostic;
use crate::entity_model::{EntityContext, EntityModel, Roots};
use crate::naming_series::{NamingPatternToken, parse_naming_series_pattern};
use crate::property_types::{
    self, Decimal, PATTERN_SIZE_LIMIT, ResolvedType, TypeResolution, check_constraints,
    check_type_members, error, matches_grammar, resolve_type, span_at,
};

/// Diagnostic codes emitted by [`check_object_type_defs`].
pub mod codes {
    /// An object type name is not `namespace.PascalName`.
    pub const INVALID_NAME: &str = "object_type.validation.invalid_name";
    /// Two object types share a name.
    pub const DUPLICATE_NAME: &str = "object_type.validation.duplicate_name";
    /// A property id does not follow `[a-z][a-z0-9_]*`.
    pub const INVALID_PROPERTY_ID: &str = "object_type.validation.invalid_property_id";
    /// Two properties of one object type share an id.
    pub const DUPLICATE_PROPERTY_ID: &str = "object_type.validation.duplicate_property_id";
    /// A key name does not follow `[a-z][a-z0-9_]*`.
    pub const INVALID_KEY_NAME: &str = "object_type.validation.invalid_key_name";
    /// Two keys of one object type share a name.
    pub const DUPLICATE_KEY_NAME: &str = "object_type.validation.duplicate_key_name";
    /// A key declares no properties.
    pub const EMPTY_KEY: &str = "object_type.validation.empty_key";
    /// A key names a property the object type does not declare.
    pub const UNKNOWN_KEY_PROPERTY: &str = "object_type.validation.unknown_key_property";
    /// A key lists the same property twice.
    pub const DUPLICATE_KEY_COMPONENT: &str = "object_type.validation.duplicate_key_component";
    /// A key component's kind has no canonical equality (`json-ext`, or a
    /// `timestamp` that keeps its source offset).
    pub const INADMISSIBLE_KEY_KIND: &str = "object_type.validation.inadmissible_key_kind";
    /// A key component is nullable.
    pub const NULLABLE_KEY_COMPONENT: &str = "object_type.validation.nullable_key_component";
    /// A key component is a computed property.
    pub const COMPUTED_KEY_COMPONENT: &str = "object_type.validation.computed_key_component";
    /// Two keys have the same effective identity: the same components (in any
    /// order), each normalized by the same steps, under the same match policy,
    /// so they enforce the same uniqueness.
    pub const REDUNDANT_KEY: &str = "object_type.validation.redundant_key";
    /// A naming-series declaration has an invalid property, pattern, timezone,
    /// scope, or reset contract.
    pub const INVALID_NAMING_SERIES: &str = "object_type.validation.invalid_naming_series";
    /// An identity rule declares neither normalization nor a match policy.
    pub const EMPTY_IDENTITY_RULE: &str = "object_type.validation.empty_identity_rule";
    /// A normalization names a property that is not a component of its key.
    pub const NORMALIZATION_NOT_IN_KEY: &str = "object_type.validation.normalization_not_in_key";
    /// Two normalizations of one key name the same component.
    pub const DUPLICATE_NORMALIZATION: &str = "object_type.validation.duplicate_normalization";
    /// A normalization declares no steps.
    pub const EMPTY_NORMALIZATION: &str = "object_type.validation.empty_normalization";
    /// A normalization applies to a component that is not `text`.
    pub const NORMALIZATION_NOT_APPLICABLE: &str =
        "object_type.validation.normalization_not_applicable";
    /// A match policy name is not a dotted lower-case name.
    pub const INVALID_MATCH_POLICY: &str = "object_type.validation.invalid_match_policy";
    /// A default does not have the property's type (JSON shape, literal
    /// grammar, scale, format, calendar or timezone rule, enum membership).
    pub const DEFAULT_TYPE_MISMATCH: &str = "object_type.validation.default_type_mismatch";
    /// A decimal default is not exactly representable as a ZEN Expression
    /// decimal, or an integer default is outside the signed 64-bit range.
    pub const DEFAULT_OUT_OF_RANGE: &str = "object_type.validation.default_out_of_range";
    /// A default violates a declared constraint (bounds, lengths, pattern).
    pub const DEFAULT_VIOLATES_CONSTRAINT: &str =
        "object_type.validation.default_violates_constraint";
    /// A default cannot be type-checked here (a registry value type, a
    /// `duration`, or an `email`/`uri` format), so it is rejected.
    pub const DEFAULT_UNCHECKED: &str = "object_type.validation.default_unchecked";
    /// A computed property also declares a default.
    pub const COMPUTED_WITH_DEFAULT: &str = "object_type.validation.computed_with_default";
    /// A computed expression is blank.
    pub const EMPTY_COMPUTED: &str = "object_type.validation.empty_computed";
    /// A computed expression does not parse as a ZEN Expression (T1-ONT-2,
    /// #2515; syntax only, typing is T1-EXPR-5).
    pub const INVALID_COMPUTED: &str = "object_type.validation.invalid_computed";

    // T1-ONT-1c (#2513): status, inheritance, interfaces, namespaces.

    /// A local object type has the name of an installed base object type or
    /// interface; base types are read-only.
    pub const SHADOWS_BASE: &str = "object_type.validation.shadows_base";
    /// A `deprecated` status's `since` is not a calendar date.
    pub const INVALID_STATUS_DATE: &str = "object_type.validation.invalid_status_date";
    /// A `deprecated` status's `replacedBy` names no object type, or the
    /// deprecated type itself.
    pub const UNKNOWN_REPLACEMENT: &str = "object_type.validation.unknown_replacement";
    /// `extends` names no object type.
    pub const UNKNOWN_SUPERTYPE: &str = "object_type.validation.unknown_supertype";
    /// The `extends` chain returns to the type it started from.
    pub const INHERITANCE_CYCLE: &str = "object_type.validation.inheritance_cycle";
    /// An override's type is neither the inherited type nor a narrowing of it.
    pub const OVERRIDE_TYPE_MISMATCH: &str = "object_type.validation.override_type_mismatch";
    /// An override is nullable where the inherited property is not.
    pub const OVERRIDE_WIDENS_NULLABILITY: &str =
        "object_type.validation.override_widens_nullability";
    /// An override redeclares a computed property, or makes a stored one
    /// computed.
    pub const OVERRIDE_COMPUTED: &str = "object_type.validation.override_computed";
    /// `implements` names no interface.
    pub const UNKNOWN_INTERFACE: &str = "object_type.validation.unknown_interface";
    /// `implements` lists an interface twice.
    pub const DUPLICATE_INTERFACE: &str = "object_type.validation.duplicate_interface";
    /// The type has no property with the id an implemented interface
    /// requires.
    pub const MISSING_INTERFACE_PROPERTY: &str =
        "object_type.validation.missing_interface_property";
    /// The property satisfying an interface requirement does not have the
    /// required type or a narrowing of it.
    pub const INTERFACE_PROPERTY_TYPE_MISMATCH: &str =
        "object_type.validation.interface_property_type_mismatch";
    /// The property satisfying an interface requirement is nullable where the
    /// requirement is not.
    pub const INTERFACE_PROPERTY_WIDENS_NULLABILITY: &str =
        "object_type.validation.interface_property_widens_nullability";
    /// The interfaces a type is held to require one member id with
    /// requirements no single member can meet together: property types that
    /// do not both narrow, enums with no common value, bounds that jointly
    /// admit no value, link targets that no one type can narrow, or a
    /// property and a link under one id.
    pub const INTERFACE_REQUIREMENT_CONFLICT: &str =
        "object_type.validation.interface_requirement_conflict";
    /// A base-layer member id (a property, link, inverse role, or
    /// invariant) contains the reserved extension separator `__`.
    pub const RESERVED_MEMBER_SEPARATOR: &str = "object_type.validation.reserved_member_separator";
    /// A member a tenant type or an extension adds (a property, link, inverse
    /// role, or invariant) is not named `<namespace>__<name>` in its layer's
    /// extension namespace.
    pub const UNNAMESPACED_MEMBER: &str = "object_type.validation.unnamespaced_member";
    /// A tenant object type is not in the tenant's namespace.
    pub const TENANT_TYPE_OUTSIDE_NAMESPACE: &str =
        "object_type.validation.tenant_type_outside_namespace";
    /// A tenant object type does not extend a type: tenants add subtypes,
    /// never new base types.
    pub const TENANT_TYPE_NOT_SUBTYPE: &str = "object_type.validation.tenant_type_not_subtype";
    /// An extension namespace is not one lower-case alphanumeric segment.
    pub const INVALID_EXTENSION_NAMESPACE: &str =
        "object_type.validation.invalid_extension_namespace";
    /// A tenant layer's extension is not in the tenant's namespace.
    pub const EXTENSION_NAMESPACE_MISMATCH: &str =
        "object_type.validation.extension_namespace_mismatch";
    /// An extension's `target` names no object type.
    pub const UNKNOWN_EXTENSION_TARGET: &str = "object_type.validation.unknown_extension_target";
    /// An extension property or link has the id of a member its target
    /// already has.
    pub const EXTENSION_SHADOWS_MEMBER: &str = "object_type.validation.extension_shadows_member";
    /// An extension targets an object type its own layer defines; a layer
    /// declares its own types' members directly.
    pub const EXTENSION_OF_LOCAL_TYPE: &str = "object_type.validation.extension_of_local_type";
    /// The constraints that apply to a property together (its own, those of
    /// the property it overrides, and those of the interface requirements it
    /// satisfies) admit no value: a `min` above a `max`, or a `minLength`
    /// above a `maxLength`.
    pub const UNSATISFIABLE_CONSTRAINTS: &str = "object_type.validation.unsatisfiable_constraints";
    /// An accounting leg declaration references invalid effective properties
    /// or changes the mapping inherited from a marked supertype.
    pub const INVALID_ACCOUNTING_LEG_DECLARATION: &str =
        "object_type.validation.invalid_accounting_leg_declaration";
    /// An owned collection does not match its many/from-one link, exact child
    /// type, stable text id, ordering integer, or platform row bound.
    pub const INVALID_OWNED_COLLECTION: &str = "object_type.validation.invalid_owned_collection";
    /// A kernel-owned row identity or ordering field cannot require rule-only writes.
    pub const PROTECTED_OWNED_ROW_FIELD: &str = "object_type.validation.protected_owned_row_field";

    // T1-ONT-1d (#2514): links and invariants.

    /// A link id or inverse role does not follow `[a-z][a-z0-9_]*`.
    pub const INVALID_LINK_ID: &str = "object_type.validation.invalid_link_id";
    /// One declaration (an object type or an extension) declares a link id
    /// twice, or as both a property and a link.
    pub const DUPLICATE_LINK_ID: &str = "object_type.validation.duplicate_link_id";
    /// A link's `target` names no object type or interface.
    pub const UNKNOWN_LINK_TARGET: &str = "object_type.validation.unknown_link_target";
    /// A link or an inverse role takes the id of another member of a type it
    /// belongs to: a property, a link, or an inverse role, declared,
    /// inherited, or arriving with an interface.
    pub const MEMBER_ID_CONFLICT: &str = "object_type.validation.member_id_conflict";
    /// A link declared without an extension namespace names an inverse role
    /// on a target another layer owns; base definitions are read-only.
    pub const FOREIGN_INVERSE: &str = "object_type.validation.foreign_inverse";
    /// A link override's target is neither the inherited target nor a
    /// narrowing of it.
    pub const LINK_OVERRIDE_TARGET_MISMATCH: &str =
        "object_type.validation.link_override_target_mismatch";
    /// A link override's cardinality is wider than the inherited one (`many`
    /// where it was `one`, or optional where it was required).
    pub const LINK_OVERRIDE_WIDENS_CARDINALITY: &str =
        "object_type.validation.link_override_widens_cardinality";
    /// An override changes the inherited relation's acyclic constraint.
    pub const LINK_OVERRIDE_CHANGES_ACYCLIC: &str =
        "object_type.validation.link_override_changes_acyclic";
    /// Edge values have no classification snapshot or reveal projection yet.
    pub const LINK_PROPERTY_MARKINGS_UNSUPPORTED: &str =
        "object_type.validation.link_property_markings_unsupported";
    /// A link override redeclares the inverse role or link properties, which
    /// belong to the inherited link.
    pub const LINK_OVERRIDE_REDECLARES: &str = "object_type.validation.link_override_redeclares";
    /// The type has no link with the id an implemented interface requires.
    pub const MISSING_INTERFACE_LINK: &str = "object_type.validation.missing_interface_link";
    /// The link satisfying an interface requirement does not target the
    /// required target or a narrowing of it.
    pub const INTERFACE_LINK_TARGET_MISMATCH: &str =
        "object_type.validation.interface_link_target_mismatch";
    /// The link satisfying an interface requirement has a wider cardinality
    /// than the requirement.
    pub const INTERFACE_LINK_WIDENS_CARDINALITY: &str =
        "object_type.validation.interface_link_widens_cardinality";
    /// An invariant id does not follow `[a-z][a-z0-9_]*`.
    pub const INVALID_INVARIANT_ID: &str = "object_type.validation.invalid_invariant_id";
    /// An invariant id is declared twice in the effective type contract, or
    /// is shared by implemented interfaces (an invariant is never replaced).
    pub const DUPLICATE_INVARIANT_ID: &str = "object_type.validation.duplicate_invariant_id";
    /// An invariant's expression is blank.
    pub const EMPTY_INVARIANT_EXPRESSION: &str =
        "object_type.validation.empty_invariant_expression";
    /// An invariant's expression does not parse, bind, or type-check as a
    /// boolean ZEN Expression.
    pub const INVALID_INVARIANT_EXPRESSION: &str =
        "object_type.validation.invalid_invariant_expression";
    /// An invariant's message is blank.
    pub const EMPTY_INVARIANT_MESSAGE: &str = "object_type.validation.empty_invariant_message";
}

pub(crate) static OBJECT_NAME_GRAMMAR: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(OBJECT_TYPE_NAME_PATTERN).ok());
pub(crate) static MEMBER_ID_GRAMMAR: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(MEMBER_ID_PATTERN).ok());
static POLICY_NAME_GRAMMAR: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(PROPERTY_TYPE_NAME_PATTERN).ok());
/// RFC 9562 UUID, hyphenated text form.
pub(crate) static UUID_GRAMMAR: LazyLock<Option<regex_lite::Regex>> = LazyLock::new(|| {
    regex_lite::Regex::new(
        r"^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$",
    )
    .ok()
});

/// Check object type definitions against a catalog of shared property types.
///
/// `catalog` is the artifact set's shared property types and `registry` the
/// registry (Norn Core) value type names they and the object types may
/// reference. The catalog's own findings come from
/// [`crate::check_property_type_defs`] and are not repeated here. Returns
/// every finding; an empty result means the object types are valid.
///
/// This is [`crate::check_entity_model`] over a model of only these object
/// types, with no installed base layer and no tenant, and with spans rooted
/// at the definition array (`/0/name`). A supertype or interface outside
/// `defs` is therefore unknown here.
#[must_use]
pub fn check_object_type_defs(
    defs: &[ObjectTypeDef],
    catalog: &[PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
) -> Vec<Diagnostic> {
    let model = EntityModel {
        object_types: defs,
        ..EntityModel::EMPTY
    };
    let context = EntityContext {
        catalog,
        registry,
        base: EntityModel::EMPTY,
        tenant: None,
    };
    crate::entity_model::check(&model, &context, &Roots::BARE)
}

/// Where a member of an object type's effective property table comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin<'a> {
    /// Declared (or overridden) by this object type.
    Type(&'a ObjectTypeName),
    /// Added by the extension at this position of its layer, `local` when
    /// it is in the checked model.
    Extension { index: usize, local: bool },
}

/// One property with its resolved type: a member of an object type's
/// effective property table.
#[derive(Clone)]
pub(crate) struct Resolved<'a> {
    pub(crate) catalog: &'a [PropertyTypeDef],
    pub(crate) property: &'a Property,
    pub(crate) ty: ResolvedType<'a>,
    /// Constraint sets of every declaration this property narrows: the
    /// inherited property it overrides (with that one's own inherited sets)
    /// and each interface requirement it satisfies. They conjoin with the
    /// property's own.
    pub(crate) inherited: Vec<&'a PropertyConstraints>,
    /// Markings supplied by the property, referenced types, overrides, and
    /// interface requirements.
    pub(crate) markings: Vec<MarkingId>,
    /// The effective default: the property's own, or, for an override that
    /// declares none, the default of the property it overrides. A computed
    /// property has none.
    pub(crate) default: Option<&'a DefaultValue>,
    pub(crate) origin: Origin<'a>,
}

impl<'a> Resolved<'a> {
    pub(crate) fn new(
        property: &'a Property,
        catalog: &'a [PropertyTypeDef],
        registry: &BTreeSet<PropertyTypeName>,
        origin: Origin<'a>,
    ) -> Self {
        let ty = resolve_type(&property.property_type, catalog, registry);
        let mut markings = property.markings.clone();
        markings.extend(ty.markings.iter().map(|marking| (*marking).clone()));
        markings.extend(property_types::structured_metadata(&property.property_type, catalog).1);
        markings.sort();
        markings.dedup();
        Self {
            catalog,
            property,
            ty,
            inherited: Vec::new(),
            markings,
            default: if property.is_read_only() {
                None
            } else {
                property.default.as_ref()
            },
            origin,
        }
    }

    /// The resolved base type, when it is known locally.
    pub(crate) const fn base(&self) -> Option<&'a PropertyType> {
        match self.ty.resolution {
            TypeResolution::Base(base) => Some(base),
            TypeResolution::Registry | TypeResolution::Unresolved { .. } => None,
        }
    }

    /// The property's own constraint sets: its own and those of each catalog
    /// definition on its `ref` chain.
    pub(crate) fn own_constraints(&self) -> impl Iterator<Item = &'a PropertyConstraints> {
        std::iter::once(&self.property.constraints).chain(self.ty.chain.iter().copied())
    }

    /// Every constraint set that applies: its own and every inherited one.
    pub(crate) fn constraints(&self) -> Vec<&'a PropertyConstraints> {
        self.own_constraints()
            .chain(self.inherited.iter().copied())
            .collect()
    }

    /// Nullability widens along the chain: a value may be absent when the
    /// property or any definition it reuses says so. Inherited declarations
    /// do not widen it; an override may not be nullable where the overridden
    /// property is not.
    pub(crate) fn nullable(&self) -> bool {
        self.own_constraints()
            .any(|constraints| constraints.nullable)
    }
}

/// Type, constraint, and computed checks of one declared property (its
/// default is checked by [`check_default`] once every inherited constraint
/// set is known). `pointer` is the JSON pointer of the property.
pub(crate) fn check_property_shape(
    pointer: &[&str],
    property: &Property,
    catalog: &[PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
    out: &mut Vec<Diagnostic>,
) {
    if !matches_grammar(&MEMBER_ID_GRAMMAR, property.id.as_str()) {
        out.push(error(
            codes::INVALID_PROPERTY_ID,
            format!(
                "property id {:?} must match [a-z][a-z0-9_]*",
                property.id.as_str()
            ),
            span_at(pointer, &["id"]),
        ));
    }
    check_type_members(pointer, &property.property_type, out);
    property_types::check_record_refs(pointer, &property.property_type, catalog, registry, out);
    let resolved = resolve_type(&property.property_type, catalog, registry);
    if let (PropertyType::Ref { name }, TypeResolution::Unresolved { direct: true }) =
        (&property.property_type, &resolved.resolution)
    {
        out.push(error(
            property_types::codes::UNKNOWN_REF,
            format!(
                "ref {:?} names no property type in the catalog or registry",
                name.as_str()
            ),
            span_at(pointer, &["type", "name"]),
        ));
    }
    let base = match resolved.resolution {
        TypeResolution::Base(base) => Some(base),
        TypeResolution::Registry | TypeResolution::Unresolved { .. } => None,
    };
    check_constraints(pointer, &property.constraints, base, out);

    if let Some(computed) = &property.computed {
        let at = span_at(pointer, &["computed"]);
        if computed.as_str().trim().is_empty() {
            out.push(error(
                codes::EMPTY_COMPUTED,
                "computed expression is blank".to_owned(),
                at,
            ));
        } else {
            check_zen_syntax(
                computed.as_str(),
                codes::INVALID_COMPUTED,
                &format!("computed expression of {:?}", property.id.as_str()),
                at,
                out,
            );
        }
        if property.default.is_some() {
            out.push(error(
                codes::COMPUTED_WITH_DEFAULT,
                format!(
                    "computed property {:?} is read-only and cannot declare a default",
                    property.id.as_str()
                ),
                span_at(pointer, &["default"]),
            ));
        }
    }
}

/// Report `code` at `at` when the non-blank ZEN Expression `source` does not
/// parse. Syntax only: `norn_expr::Expression::compile` is the engine's own
/// parser, so an admitted site never fails to compile at runtime. Names,
/// the `self` binding, and types are checked by T1-EXPR-5 (#2523).
pub(crate) fn check_zen_syntax(
    source: &str,
    code: &str,
    what: &str,
    at: crate::Span,
    out: &mut Vec<Diagnostic>,
) {
    if let Err(parse_error) = norn_expr::Expression::compile(source) {
        out.push(error(
            code,
            format!("{what} is not a ZEN Expression: {parse_error}"),
            at,
        ));
    }
}

/// Check the default of a declared property against its resolved type and
/// every constraint set that applies. `pointer` is the JSON pointer of the
/// property.
///
/// An override that declares no default inherits the overridden one, which
/// is then checked against the override (and reported on the property).
pub(crate) fn check_property_default(
    pointer: &[&str],
    member: &Resolved<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(default) = member.default else {
        return;
    };
    let at = if member.property.default.is_some() {
        span_at(pointer, &["default"])
    } else {
        span_at(pointer, &[])
    };
    check_default(&at, default, member, &member.constraints(), out);
}

/// Check an effective default against additional constraint sets only (an
/// interface a subtype implements on an inherited property). Type findings
/// belong to the declaring type, so only constraint violations are kept.
pub(crate) fn check_default_against(
    at: &crate::Span,
    member: &Resolved<'_>,
    sets: &[&PropertyConstraints],
    out: &mut Vec<Diagnostic>,
) {
    let Some(default) = member.default else {
        return;
    };
    let mut findings = Vec::new();
    check_default(at, default, member, sets, &mut findings);
    out.extend(
        findings
            .into_iter()
            .filter(|finding| finding.code.as_str() == codes::DEFAULT_VIOLATES_CONSTRAINT),
    );
}

/// Why the conjunction of `sets` admits no value, if it does not: the
/// greatest `min` above the least `max`, or the greatest `minLength` above
/// the least `maxLength`. Malformed bounds are reported where declared and
/// ignored here.
pub(crate) fn unsatisfiable(sets: &[&PropertyConstraints]) -> Option<String> {
    let min = sets
        .iter()
        .filter_map(|set| set.min.as_ref())
        .filter_map(|bound| Decimal::parse_str(bound.as_str()).map(|value| (value, bound)))
        .max_by(|left, right| left.0.cmp(&right.0));
    let max = sets
        .iter()
        .filter_map(|set| set.max.as_ref())
        .filter_map(|bound| Decimal::parse_str(bound.as_str()).map(|value| (value, bound)))
        .min_by(|left, right| left.0.cmp(&right.0));
    if let (Some((low, low_text)), Some((high, high_text))) = (min, max)
        && low > high
    {
        return Some(format!(
            "min {:?} exceeds max {:?}",
            low_text.as_str(),
            high_text.as_str()
        ));
    }
    let min_length = sets
        .iter()
        .filter_map(|set| set.min_length)
        .map(|n| n.0)
        .max();
    let max_length = sets
        .iter()
        .filter_map(|set| set.max_length)
        .map(|n| n.0)
        .min();
    if let (Some(low), Some(high)) = (min_length, max_length)
        && low > high
    {
        return Some(format!("minLength {low} exceeds maxLength {high}"));
    }
    None
}

/// Check the keys a type declares against its effective property table.
/// `inherited_keys` are the keys of its supertypes (not re-checked), each
/// with the supertype that declares it.
pub(crate) fn check_keys<'a>(
    base: &[&str],
    identity: &'a [KeySpec],
    inherited_keys: &[(&ObjectTypeName, &'a KeySpec)],
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let mut key_names: BTreeMap<&KeyName, Option<&ObjectTypeName>> = BTreeMap::new();
    let mut key_sets: BTreeMap<EffectiveKey<'_>, &KeySpec> = BTreeMap::new();
    for (owner, key) in inherited_keys {
        key_names.entry(&key.name).or_insert(Some(owner));
        let components = key.properties.iter().collect();
        key_sets
            .entry(effective_key(key, components))
            .or_insert(key);
    }
    let mut has_naming_series = inherited_keys
        .iter()
        .any(|(_, key)| key.naming_series.is_some());
    for (owner, key) in inherited_keys {
        if let Some(series) = &key.naming_series {
            check_inherited_naming_series(base, owner, key, series, properties, out);
        }
    }
    for (index, key) in identity.iter().enumerate() {
        let position = index.to_string();
        let pointer = [base, &["identity", position.as_str()]].concat();
        if !matches_grammar(&MEMBER_ID_GRAMMAR, key.name.as_str()) {
            out.push(error(
                codes::INVALID_KEY_NAME,
                format!(
                    "key name {:?} must match [a-z][a-z0-9_]*",
                    key.name.as_str()
                ),
                span_at(&pointer, &["name"]),
            ));
        }
        match key_names.get(&key.name) {
            Some(None) => out.push(error(
                codes::DUPLICATE_KEY_NAME,
                format!("key {:?} is declared twice", key.name.as_str()),
                span_at(&pointer, &["name"]),
            )),
            Some(Some(owner)) => out.push(error(
                codes::DUPLICATE_KEY_NAME,
                format!(
                    "key {:?} is already declared by supertype {:?}",
                    key.name.as_str(),
                    owner.as_str()
                ),
                span_at(&pointer, &["name"]),
            )),
            None => {
                key_names.insert(&key.name, None);
            }
        }
        if let Some(components) = check_key(&pointer, key, properties, out) {
            let effective = effective_key(key, components);
            if let Some(earlier) = key_sets.get(&effective) {
                out.push(error(
                    codes::REDUNDANT_KEY,
                    format!(
                        "key {:?} has the same components, normalization, and match policy as key {:?}; component order does not change uniqueness",
                        key.name.as_str(),
                        earlier.name.as_str()
                    ),
                    span_at(&pointer, &["properties"]),
                ));
            } else {
                key_sets.insert(effective, key);
            }
        }
        if let Some(naming_series) = &key.naming_series {
            if has_naming_series {
                out.push(error(
                    codes::INVALID_NAMING_SERIES,
                    "an effective object type may declare only one naming series across its inheritance lineage".to_owned(),
                    span_at(&pointer, &["namingSeries"]),
                ));
            }
            has_naming_series = true;
            check_naming_series(&pointer, key, naming_series, properties, out);
        }
    }
}

fn check_inherited_naming_series(
    base: &[&str],
    owner: &ObjectTypeName,
    key: &KeySpec,
    series: &NamingSeriesIR,
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let inherited_span = span_at(base, &["extends"]);
    let pattern_lengths = parse_naming_series_pattern(&series.pattern)
        .ok()
        .map(|tokens| naming_pattern_lengths(&tokens, properties));
    if let Some((minimum_bytes, _, _)) = pattern_lengths {
        check_naming_series_byte_limit(minimum_bytes, inherited_span.clone(), out);
    }
    if let Some(generated) = key.properties.first()
        && let Some(property) = properties.get(generated)
    {
        if !naming_series_text_property(property) || property.default.is_some() {
            out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!(
                    "inherited naming-series key from {:?} requires effective generated property {:?} to remain stored, required plain text without a default",
                    owner.as_str(),
                    generated.as_str()
                ),
                inherited_span.clone(),
            ));
        }
        if let Some((_, minimum_characters, maximum_characters)) = pattern_lengths {
            check_naming_series_text_length(
                generated,
                property,
                minimum_characters,
                maximum_characters,
                Some(owner),
                inherited_span.clone(),
                out,
            );
        }
    }
    for scope in &series.scope_fields {
        if let Some(property) = properties.get(scope) {
            if naming_series_text_property(property) {
                check_naming_series_scope_constraints(
                    scope,
                    property,
                    Some(owner),
                    inherited_span.clone(),
                    out,
                );
            } else {
                out.push(error(
                    codes::INVALID_NAMING_SERIES,
                    format!(
                        "inherited naming-series key from {:?} requires effective scope property {:?} to remain stored, required plain text",
                        owner.as_str(),
                        scope.as_str()
                    ),
                    inherited_span.clone(),
                ));
            }
        }
    }
}

fn naming_pattern_lengths(
    tokens: &[NamingPatternToken<'_>],
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
) -> (usize, u64, Option<u64>) {
    tokens.iter().fold(
        (0_usize, 0_u64, Some(0_u64)),
        |(minimum_bytes, minimum_characters, maximum_characters), token| {
            let (token_bytes, token_characters, fixed_characters) = match token {
                NamingPatternToken::Literal(value) => {
                    let characters = u64::try_from(value.chars().count()).unwrap_or(u64::MAX);
                    (value.len(), characters, Some(characters))
                }
                NamingPatternToken::YearFour => (4, 4, Some(4)),
                NamingPatternToken::YearTwo
                | NamingPatternToken::Month
                | NamingPatternToken::Day => (2, 2, Some(2)),
                NamingPatternToken::ScopeField(field) => {
                    let (minimum_characters, maximum_characters) =
                        naming_scope_lengths(field, properties);
                    let minimum_bytes = usize::try_from(minimum_characters).unwrap_or(usize::MAX);
                    (minimum_bytes, minimum_characters, maximum_characters)
                }
                NamingPatternToken::Counter { width } => {
                    let width = *width;
                    let characters = u64::from(width);
                    (usize::from(width), characters, Some(characters))
                }
            };
            let maximum_characters = match (maximum_characters, fixed_characters) {
                (Some(total), Some(token)) => Some(total.saturating_add(token)),
                _ => None,
            };
            (
                minimum_bytes.saturating_add(token_bytes),
                minimum_characters.saturating_add(token_characters),
                maximum_characters,
            )
        },
    )
}

fn naming_scope_lengths(
    field: &str,
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
) -> (u64, Option<u64>) {
    properties
        .iter()
        .find(|(id, _)| id.as_str() == field)
        .map_or((1, None), |(_, property)| {
            let constraints = property.constraints();
            let minimum = constraints
                .iter()
                .filter_map(|constraints| constraints.min_length)
                .map(|length| length.0)
                .max()
                .unwrap_or(1)
                .max(1);
            let maximum = constraints
                .iter()
                .filter_map(|constraints| constraints.max_length)
                .map(|length| length.0)
                .min();
            (minimum, maximum)
        })
}

fn check_naming_series_byte_limit(bytes: usize, at: crate::Span, out: &mut Vec<Diagnostic>) {
    if bytes > MAX_OBJECT_KEY_BYTES {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "pattern requires at least {bytes} rendered UTF-8 bytes, exceeding the object-key limit of {MAX_OBJECT_KEY_BYTES}"
            ),
            at,
        ));
    }
}

fn check_naming_series_scope_constraints(
    scope: &PropertyId,
    property: &Resolved<'_>,
    owner: Option<&ObjectTypeName>,
    at: crate::Span,
    out: &mut Vec<Diagnostic>,
) {
    let constraints = property.constraints();
    let source = owner.map_or_else(
        || format!("scope property {:?}", scope.as_str()),
        |owner| {
            format!(
                "inherited naming-series scope property {:?} from {:?}",
                scope.as_str(),
                owner.as_str()
            )
        },
    );
    if constraints
        .iter()
        .any(|constraints| constraints.max_length.is_some_and(|length| length.0 == 0))
    {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!("{source} cannot use maxLength 0 because scope values must be nonempty"),
            at.clone(),
        ));
    }
    if constraints
        .iter()
        .any(|constraints| constraints.pattern.is_some())
    {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "{source} cannot use an effective regex constraint because compatibility with nonempty ASCII scope values is not checked"
            ),
            at,
        ));
    }
}

fn check_naming_series_text_length(
    generated: &PropertyId,
    property: &Resolved<'_>,
    minimum_characters: u64,
    maximum_characters: Option<u64>,
    owner: Option<&ObjectTypeName>,
    at: crate::Span,
    out: &mut Vec<Diagnostic>,
) {
    let constraints = property.constraints();
    let key_byte_limit = u64::try_from(MAX_OBJECT_KEY_BYTES).unwrap_or(u64::MAX);
    let maximum_characters =
        maximum_characters.map_or(key_byte_limit, |maximum| maximum.min(key_byte_limit));
    let source = owner.map_or_else(
        || "naming-series pattern".to_owned(),
        |owner| format!("inherited naming-series pattern from {:?}", owner.as_str()),
    );
    if constraints
        .iter()
        .any(|constraint| constraint.pattern.is_some())
    {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "{source} cannot use generated property {:?} with an effective regex constraint because regex compatibility is not checked",
                generated.as_str()
            ),
            at.clone(),
        ));
    }
    if let Some(maximum) = constraints
        .iter()
        .filter_map(|constraints| constraints.max_length)
        .map(|length| length.0)
        .min()
        && minimum_characters > maximum
    {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "{source} needs at least {minimum_characters} Unicode scalar values, exceeding effective maxLength {maximum} on generated property {:?}",
                generated.as_str()
            ),
            at.clone(),
        ));
    }
    if let Some(minimum) = constraints
        .iter()
        .filter_map(|constraints| constraints.min_length)
        .map(|length| length.0)
        .max()
        && minimum > maximum_characters
    {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "{source} renders at most {maximum_characters} Unicode scalar values, fewer than effective minLength {minimum} on generated property {:?}",
                generated.as_str()
            ),
            at,
        ));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "generated property, scope, pattern, and reset checks admit one naming-series contract"
)]
fn check_naming_series(
    key_pointer: &[&str],
    key: &KeySpec,
    series: &NamingSeriesIR,
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    let series_pointer = [key_pointer, &["namingSeries"]].concat();
    if key.properties.len() != 1 {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            "a naming series requires a single-property key".to_owned(),
            span_at(&series_pointer, &[]),
        ));
    }
    if key.rule.is_some() {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            "a generated key cannot declare an IdentityRule".to_owned(),
            span_at(&series_pointer, &[]),
        ));
    }

    let generated = key.properties.first();
    if let Some(id) = generated {
        match properties.get(id) {
            Some(property)
                if naming_series_text_property(property)
                    && property.default.is_none() => {}
            Some(_) => out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!(
                    "generated key property {:?} must be stored, required plain text without a default",
                    id.as_str()
                ),
                span_at(&series_pointer, &[]),
            )),
            None => {}
        }
    }

    if norn_expr::temporal::validate_timezone(&series.timezone).is_err() {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "timezone {:?} is not present in the pinned IANA timezone database",
                series.timezone
            ),
            span_at(&series_pointer, &["timezone"]),
        ));
    }

    let mut declared_scope = BTreeSet::new();
    for (index, field) in series.scope_fields.iter().enumerate() {
        let position = index.to_string();
        let pointer = span_at(&series_pointer, &["scopeFields", position.as_str()]);
        if !matches_grammar(&MEMBER_ID_GRAMMAR, field.as_str()) {
            out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!(
                    "scope field {:?} must match [a-z][a-z0-9_]*",
                    field.as_str()
                ),
                pointer.clone(),
            ));
        }
        if !declared_scope.insert(field.as_str()) {
            out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!("scope field {:?} is listed more than once", field.as_str()),
                pointer,
            ));
        }
        if Some(field) == generated {
            out.push(error(
                codes::INVALID_NAMING_SERIES,
                "the generated key property cannot also be a scope field".to_owned(),
                span_at(&series_pointer, &["scopeFields", position.as_str()]),
            ));
        }
        match properties.get(field) {
            Some(property) if naming_series_text_property(property) => {
                check_naming_series_scope_constraints(
                    field,
                    property,
                    None,
                    span_at(&series_pointer, &["scopeFields", position.as_str()]),
                    out,
                );
            }
            Some(_) => out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!(
                    "scope property {:?} must be stored, required plain text",
                    field.as_str()
                ),
                span_at(&series_pointer, &["scopeFields", position.as_str()]),
            )),
            None => out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!(
                    "scope field {:?} names no property of the object type",
                    field.as_str()
                ),
                span_at(&series_pointer, &["scopeFields", position.as_str()]),
            )),
        }
    }

    let tokens = match parse_naming_series_pattern(&series.pattern) {
        Ok(tokens) => tokens,
        Err(pattern_error) => {
            out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!("pattern is invalid: {pattern_error}"),
                span_at(&series_pointer, &["pattern"]),
            ));
            return;
        }
    };

    let (minimum_rendered_bytes, minimum_rendered_characters, maximum_rendered_characters) =
        naming_pattern_lengths(&tokens, properties);
    check_naming_series_byte_limit(
        minimum_rendered_bytes,
        span_at(&series_pointer, &["pattern"]),
        out,
    );
    if let Some(id) = generated
        && let Some(property) = properties.get(id)
    {
        check_naming_series_text_length(
            id,
            property,
            minimum_rendered_characters,
            maximum_rendered_characters,
            None,
            span_at(&series_pointer, &["pattern"]),
            out,
        );
    }

    let mut scope_counts = BTreeMap::<&str, usize>::new();
    let mut year_four = false;
    let mut month = false;
    let mut day = false;
    for token in tokens {
        match token {
            NamingPatternToken::YearFour => year_four = true,
            NamingPatternToken::Month => month = true,
            NamingPatternToken::Day => day = true,
            NamingPatternToken::ScopeField(field) => {
                if !declared_scope.contains(field) {
                    out.push(error(
                        codes::INVALID_NAMING_SERIES,
                        format!("pattern scope token {{{field}}} is not declared in scopeFields"),
                        span_at(&series_pointer, &["pattern"]),
                    ));
                }
                let count = scope_counts.entry(field).or_default();
                *count = count.saturating_add(1);
            }
            NamingPatternToken::Literal(_)
            | NamingPatternToken::YearTwo
            | NamingPatternToken::Counter { .. } => {}
        }
    }
    for field in &series.scope_fields {
        if scope_counts
            .get(field.as_str())
            .copied()
            .unwrap_or_default()
            != 1
        {
            out.push(error(
                codes::INVALID_NAMING_SERIES,
                format!(
                    "scope field {:?} must appear exactly once as a pattern token",
                    field.as_str()
                ),
                span_at(&series_pointer, &["scopeFields"]),
            ));
        }
    }

    let missing_period_tokens = match series.reset_policy {
        NamingResetPolicy::Never => false,
        NamingResetPolicy::Year => !year_four,
        NamingResetPolicy::Month => !year_four || !month,
        NamingResetPolicy::Day => !year_four || !month || !day,
    };
    if missing_period_tokens {
        out.push(error(
            codes::INVALID_NAMING_SERIES,
            format!(
                "reset policy {:?} requires its local period to be rendered with {{YYYY}}, {{MM}}, and {{DD}} as applicable",
                series.reset_policy
            ),
            span_at(&series_pointer, &["resetPolicy"]),
        ));
    }
}

fn naming_series_text_property(property: &Resolved<'_>) -> bool {
    !property.property.is_read_only()
        && !property.nullable()
        && matches!(property.base(), Some(PropertyType::Text { format: None }))
}

/// What a key's uniqueness depends on: its component set (order does not
/// change uniqueness), each component's normalization steps (empty when the
/// component compares as stored; step order matters), and its match policy.
type EffectiveKey<'a> = (
    BTreeMap<&'a PropertyId, &'a [Normalization]>,
    Option<&'a ResolutionPolicyName>,
);

fn effective_key<'a>(key: &'a KeySpec, components: BTreeSet<&'a PropertyId>) -> EffectiveKey<'a> {
    let rule = key.rule.as_ref();
    let steps = components
        .into_iter()
        .map(|id| {
            // The first normalization of a component applies; a second one is
            // reported as `duplicate_normalization`.
            let steps = rule
                .and_then(|rule| rule.normalize.iter().find(|entry| &entry.property == id))
                .map_or(&[][..], |entry| entry.steps.as_slice());
            (id, steps)
        })
        .collect();
    (steps, rule.and_then(|rule| rule.match_policy.as_ref()))
}

/// Components, kinds, nullability, and the identity rule of one key. Returns
/// the component set when the key's components are well-formed.
fn check_key<'a>(
    pointer: &[&str],
    key: &'a KeySpec,
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
    out: &mut Vec<Diagnostic>,
) -> Option<BTreeSet<&'a PropertyId>> {
    if key.properties.is_empty() {
        out.push(error(
            codes::EMPTY_KEY,
            format!("key {:?} declares no properties", key.name.as_str()),
            span_at(pointer, &["properties"]),
        ));
        return None;
    }
    let mut components = BTreeSet::new();
    let mut well_formed = true;
    for (index, id) in key.properties.iter().enumerate() {
        let position = index.to_string();
        let at = span_at(pointer, &["properties", position.as_str()]);
        if !components.insert(id) {
            well_formed = false;
            out.push(error(
                codes::DUPLICATE_KEY_COMPONENT,
                format!("key component {:?} is listed twice", id.as_str()),
                at,
            ));
            continue;
        }
        let Some(resolved) = properties.get(id) else {
            well_formed = false;
            out.push(error(
                codes::UNKNOWN_KEY_PROPERTY,
                format!(
                    "key component {:?} names no property of the object type",
                    id.as_str()
                ),
                at,
            ));
            continue;
        };
        if resolved.property.is_read_only() {
            out.push(error(
                codes::COMPUTED_KEY_COMPONENT,
                format!(
                    "key component {:?} is computed; identity is stored, not derived",
                    id.as_str()
                ),
                at.clone(),
            ));
        }
        if resolved.nullable() {
            out.push(error(
                codes::NULLABLE_KEY_COMPONENT,
                format!("key component {:?} is nullable", id.as_str()),
                at.clone(),
            ));
        }
        match resolved.base() {
            Some(PropertyType::Record { .. } | PropertyType::JsonExt { .. }) => out.push(error(
                codes::INADMISSIBLE_KEY_KIND,
                format!(
                    "key component {:?} is json-ext, which has no canonical equality",
                    id.as_str()
                ),
                at,
            )),
            Some(PropertyType::Timestamp {
                timezone: TimezonePolicy::Offset,
            }) => out.push(error(
                codes::INADMISSIBLE_KEY_KIND,
                format!(
                    "key component {:?} is a timestamp that keeps its source offset, so one instant has several key values; use timezone \"utc\"",
                    id.as_str()
                ),
                at,
            )),
            _ => {}
        }
    }

    if let Some(rule) = &key.rule {
        check_identity_rule(&[pointer, &["rule"]].concat(), key, rule, properties, out);
    }

    well_formed.then_some(components)
}

/// A key's identity rule: non-empty, a well-formed policy name, and
/// normalizations of distinct `text` components of the key.
fn check_identity_rule(
    rule_pointer: &[&str],
    key: &KeySpec,
    rule: &IdentityRule,
    properties: &BTreeMap<&PropertyId, Resolved<'_>>,
    out: &mut Vec<Diagnostic>,
) {
    if rule.normalize.is_empty() && rule.match_policy.is_none() {
        out.push(error(
            codes::EMPTY_IDENTITY_RULE,
            format!(
                "identity rule of key {:?} declares neither normalize nor matchPolicy; omit it",
                key.name.as_str()
            ),
            span_at(rule_pointer, &[]),
        ));
    }
    if let Some(policy) = &rule.match_policy
        && !matches_grammar(&POLICY_NAME_GRAMMAR, policy.as_str())
    {
        out.push(error(
            codes::INVALID_MATCH_POLICY,
            format!(
                "match policy {:?} must be dot-separated segments of [a-z][a-z0-9_]*",
                policy.as_str()
            ),
            span_at(rule_pointer, &["matchPolicy"]),
        ));
    }
    let mut normalized = BTreeSet::new();
    for (index, normalization) in rule.normalize.iter().enumerate() {
        let position = index.to_string();
        let entry = [rule_pointer, &["normalize", position.as_str()]].concat();
        let id = &normalization.property;
        if normalization.steps.is_empty() {
            out.push(error(
                codes::EMPTY_NORMALIZATION,
                format!("normalization of {:?} declares no steps", id.as_str()),
                span_at(&entry, &["steps"]),
            ));
        }
        let at = span_at(&entry, &["property"]);
        if !key.properties.contains(id) {
            out.push(error(
                codes::NORMALIZATION_NOT_IN_KEY,
                format!(
                    "normalization names {:?}, which is not a component of key {:?}",
                    id.as_str(),
                    key.name.as_str()
                ),
                at,
            ));
            continue;
        }
        if !normalized.insert(id) {
            out.push(error(
                codes::DUPLICATE_NORMALIZATION,
                format!("component {:?} is normalized twice", id.as_str()),
                at,
            ));
            continue;
        }
        // Registry types: applicability is the registry's (T8-STD-1).
        if let Some(base) = properties.get(id).and_then(Resolved::base)
            && base.kind() != PropertyKind::Text
        {
            out.push(error(
                codes::NORMALIZATION_NOT_APPLICABLE,
                format!(
                    "normalization applies to text, but component {:?} is {}",
                    id.as_str(),
                    base.kind().as_str()
                ),
                at,
            ));
        }
    }
}

/// Type-check a literal default against the property's resolved type and
/// every constraint that applies to it.
fn check_default(
    at: &crate::Span,
    default: &DefaultValue,
    resolved: &Resolved<'_>,
    sets: &[&PropertyConstraints],
    out: &mut Vec<Diagnostic>,
) {
    let base = match resolved.ty.resolution {
        TypeResolution::Base(base) => base,
        TypeResolution::Registry => {
            out.push(unchecked(
                at,
                "the property's type is a registry value type whose definition is not available here (T8-STD-1)",
            ));
            return;
        }
        // The unresolved ref is reported on the type.
        TypeResolution::Unresolved { .. } => return,
    };
    let value = &default.0;
    let kind = base.kind();
    let mismatch = |expected: &str| default_type_mismatch(at, kind, expected);
    match base {
        PropertyType::Text { format } => {
            let JsonValue::String(text) = value else {
                out.push(mismatch("a string"));
                return;
            };
            match format {
                Some(TextFormat::Uuid) if !matches_grammar(&UUID_GRAMMAR, text) => {
                    out.push(mismatch("a hyphenated UUID"));
                    return;
                }
                Some(TextFormat::Email | TextFormat::Uri) => {
                    out.push(unchecked(
                        at,
                        "email and uri formats are validated with instance values (T1-ONT-3)",
                    ));
                    return;
                }
                _ => {}
            }
            check_text_default(at, text, sets, out);
        }
        PropertyType::Integer {} | PropertyType::Decimal { .. } => {
            check_numeric_default(at, value, base, sets, out);
        }
        PropertyType::Boolean {} => {
            if !matches!(value, JsonValue::Bool(_)) {
                out.push(mismatch("true or false"));
            }
        }
        PropertyType::Timestamp { timezone } => {
            let valid = match value {
                JsonValue::String(text) => valid_timestamp(text, *timezone),
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::Array(_)
                | JsonValue::Object(_) => false,
            };
            if !valid {
                out.push(mismatch(
                    "an RFC 3339 date-time with an explicit offset, such as \"2026-01-01T09:00:00+02:00\"",
                ));
            }
        }
        PropertyType::Date {} => {
            let valid = match value {
                JsonValue::String(text) => valid_date(text),
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::Array(_)
                | JsonValue::Object(_) => false,
            };
            if !valid {
                out.push(mismatch("a calendar date \"YYYY-MM-DD\""));
            }
        }
        PropertyType::Duration {} => out.push(unchecked(
            at,
            "the duration literal encoding is not fixed yet (T3-NUM-1)",
        )),
        PropertyType::Enum { values } => {
            let declared = match value {
                JsonValue::String(text) => values.iter().any(|entry| &entry.value == text),
                JsonValue::Null
                | JsonValue::Bool(_)
                | JsonValue::Number(_)
                | JsonValue::Array(_)
                | JsonValue::Object(_) => false,
            };
            if !declared {
                out.push(mismatch("one of the declared enum values"));
            }
        }
        // `json-ext` is opaque: its schema is not interpreted, and `null` is
        // rejected at decode. `resolve_type` never ends on a `ref`.
        PropertyType::Record { .. } => check_record_default(at, value, resolved, out),
        PropertyType::JsonExt { .. } | PropertyType::Ref { .. } => {}
    }
}

fn default_type_mismatch(at: &crate::Span, kind: PropertyKind, expected: &str) -> Diagnostic {
    error(
        codes::DEFAULT_TYPE_MISMATCH,
        format!(
            "default for a {} property must be {expected}",
            kind.as_str()
        ),
        at.clone(),
    )
}

fn check_record_default(
    at: &crate::Span,
    value: &JsonValue,
    resolved: &Resolved<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let effective = crate::entity_instance::EffectiveProperty::from_resolved(
        resolved,
        resolved.markings.clone(),
    );
    let runtime = effective.runtime_default_literal(value);
    if runtime
        .as_ref()
        .map_or(true, |value| effective.check_runtime_value(value).is_err())
    {
        out.push(error(
            codes::DEFAULT_TYPE_MISMATCH,
            "record default must contain every member with a valid typed value".to_owned(),
            at.clone(),
        ));
    }
}

fn check_text_default(
    at: &crate::Span,
    text: &str,
    sets: &[&PropertyConstraints],
    out: &mut Vec<Diagnostic>,
) {
    let length = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
    for constraints in sets {
        if let Some(min) = constraints.min_length
            && length < min.0
        {
            out.push(violates(
                at,
                format!(
                    "default has {length} characters, fewer than minLength {}",
                    min.0
                ),
            ));
        }
        if let Some(max) = constraints.max_length
            && length > max.0
        {
            out.push(violates(
                at,
                format!(
                    "default has {length} characters, more than maxLength {}",
                    max.0
                ),
            ));
        }
        // A pattern that does not compile is reported on the constraint.
        if let Some(pattern) = &constraints.pattern
            && let Ok(regex) = regex_lite::RegexBuilder::new(pattern)
                .size_limit(PATTERN_SIZE_LIMIT)
                .build()
            && !regex.is_match(text)
        {
            out.push(violates(
                at,
                format!("default {text:?} does not match pattern {pattern:?}"),
            ));
        }
    }
}

fn check_numeric_default(
    at: &crate::Span,
    value: &JsonValue,
    base: &PropertyType,
    sets: &[&PropertyConstraints],
    out: &mut Vec<Diagnostic>,
) {
    let kind = base.kind().as_str();
    let JsonValue::String(text) = value else {
        out.push(error(
            codes::DEFAULT_TYPE_MISMATCH,
            format!(
                "default for a {kind} property must be an exact decimal string such as \"12.50\", not a JSON number"
            ),
            at.clone(),
        ));
        return;
    };
    let Some(parsed) = Decimal::parse_str(text) else {
        out.push(error(
            codes::DEFAULT_TYPE_MISMATCH,
            format!("default {text:?} is not an exact decimal literal"),
            at.clone(),
        ));
        return;
    };
    match base {
        PropertyType::Integer {} => {
            if !parsed.fraction().is_empty() {
                out.push(error(
                    codes::DEFAULT_TYPE_MISMATCH,
                    format!("default {text:?} of an integer property has a fraction"),
                    at.clone(),
                ));
                return;
            }
            if text.parse::<i64>().is_err() {
                out.push(error(
                    codes::DEFAULT_OUT_OF_RANGE,
                    format!("default {text:?} is outside the signed 64-bit integer range"),
                    at.clone(),
                ));
                return;
            }
        }
        PropertyType::Decimal { scale, .. } => {
            if u64::try_from(parsed.fraction().len()).map_or(true, |digits| digits > scale.0) {
                out.push(error(
                    codes::DEFAULT_TYPE_MISMATCH,
                    format!(
                        "default {text:?} has more fraction digits than the declared scale {}",
                        scale.0
                    ),
                    at.clone(),
                ));
                return;
            }
            // The engine's own constructor decides representability.
            if let Err(range_error) = text.parse::<norn_expr::Number>() {
                out.push(error(
                    codes::DEFAULT_OUT_OF_RANGE,
                    format!(
                        "default {text:?} is outside the ZEN Expression decimal range: {range_error}"
                    ),
                    at.clone(),
                ));
                return;
            }
        }
        PropertyType::Text { .. }
        | PropertyType::Boolean { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date { .. }
        | PropertyType::Duration { .. }
        | PropertyType::Enum { .. }
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => return,
    }
    for constraints in sets {
        for (member, bound, violated) in [
            ("min", &constraints.min, std::cmp::Ordering::Less),
            ("max", &constraints.max, std::cmp::Ordering::Greater),
        ] {
            // Malformed bounds are reported on the constraint.
            let Some(bound) = bound else { continue };
            if Decimal::parse_str(bound.as_str())
                .is_some_and(|limit| parsed.cmp(&limit) == violated)
            {
                out.push(violates(
                    at,
                    format!("default {text:?} is outside {member} {:?}", bound.as_str()),
                ));
            }
        }
    }
}

/// An RFC 3339 date-time with an explicit, known offset. Both stored timezone
/// policies accept offset and `Z` spellings; runtime values normalize to UTC.
pub(crate) fn valid_timestamp(text: &str, _timezone: TimezonePolicy) -> bool {
    norn_expr::temporal::canonical_timestamp(text).is_ok()
}

/// A proleptic-Gregorian calendar date `YYYY-MM-DD`.
pub(crate) fn valid_date(text: &str) -> bool {
    norn_expr::temporal::validate_date(text).is_ok()
}

fn unchecked(at: &crate::Span, reason: &str) -> Diagnostic {
    error(
        codes::DEFAULT_UNCHECKED,
        format!("default cannot be type-checked: {reason}; omit it"),
        at.clone(),
    )
}

fn violates(at: &crate::Span, message: String) -> Diagnostic {
    error(codes::DEFAULT_VIOLATES_CONSTRAINT, message, at.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_follow_the_gregorian_calendar() {
        for valid in ["2024-02-29", "2000-02-29", "2026-12-31", "0001-01-01"] {
            assert!(valid_date(valid), "{valid}");
        }
        for invalid in [
            "2026-02-29",
            "1900-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2026-1-01",
            "26-01-01",
        ] {
            assert!(!valid_date(invalid), "{invalid}");
        }
    }

    #[test]
    fn timestamps_accept_explicit_offsets_for_both_storage_policies() {
        assert!(valid_timestamp("2026-01-01T00:00:00Z", TimezonePolicy::Utc));
        assert!(valid_timestamp(
            "2026-01-01T00:00:00.125Z",
            TimezonePolicy::Utc
        ));
        assert!(valid_timestamp(
            "2026-01-01T02:00:00+02:00",
            TimezonePolicy::Utc
        ));
        assert!(valid_timestamp(
            "2026-01-01T02:00:00+02:00",
            TimezonePolicy::Offset
        ));
        assert!(valid_timestamp(
            "2026-01-01T02:00:00Z",
            TimezonePolicy::Offset
        ));
        for invalid in [
            "2026-01-01T02:00:00",
            "2026-01-01T02:00:00-00:00",
            "2026-01-01 02:00:00Z",
            "2026-01-01t02:00:00z",
            "2026-02-30T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:60Z",
            "2026-01-01T00:00:00+24:00",
        ] {
            assert!(
                !valid_timestamp(invalid, TimezonePolicy::Offset),
                "{invalid}"
            );
        }
    }
}
