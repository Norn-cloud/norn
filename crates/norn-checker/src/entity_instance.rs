//! Instance semantics of `EntityIR` v2 object types (T1-ONT-4b, #2848): what
//! an authoritative object write of a type must satisfy.
//!
//! The engine validates objects against the **effective** object type, never
//! against a raw definition. [`EffectiveCatalog`] builds it from a set of
//! `entity` artifacts with the whole-model check's own tables
//! ([`crate::check_entity_model`]): inherited and overridden properties,
//! interface requirements (whose constraints conjoin), extension members,
//! and `ref`s resolved through the property type catalog, with every
//! constraint set on the chain. Nothing here re-derives inheritance.
//!
//! The instance rules:
//!
//! - **Values.** [`EffectiveProperty::check_value`] checks one present value:
//!   `text` (with `minLength`/`maxLength`/`pattern`, and the `uuid` format),
//!   `integer` (a JSON integer literal, with `min`/`max`), `boolean`, `enum`,
//!   `date`, `timestamp`, and `json-ext` (any non-null value, or the one-keyword
//!   `{ "type": "number" | "object" | "array" }` schema a lowered v1 type
//!   carries). Patterns use `regex-lite`, the dialect the checker compiles
//!   them in, with JSON Schema's unanchored matching.
//! - **Defaults.** [`EffectiveProperty::default_value`] is the value a create
//!   fills in for an absent property: the literal default the checker
//!   type-checked at admission, in instance encoding (an `integer` default is
//!   the decimal string `"10"` in IR and the JSON integer `10` in an object).
//! - **Identity.** [`EffectiveKey::value`] is an object's normalized key
//!   tuple: components in key order, each `text` component normalized by its
//!   declared steps ([`normalize_text`]). Uniqueness of the tuples among live
//!   objects is the store's commit-time rule.
//!
//! Features whose instance semantics are not implemented yet are reported by
//! [`EffectiveObjectType::unsupported`], so callers fail closed: `decimal`
//! (instance encoding is T3-NUM-1, #2770), the `email` and
//! `uri` text formats (T1-ONT-3), registry value types (T8-STD-1), `json-ext`
//! schemas other than the one-keyword form, keys with a match policy
//! (T8-ID-1), links (no instance representation of a v2 link exists), and
//! invariants (they need ZEN Expression evaluation at commit).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, DefaultValue, InterfaceName, JsonValue, KeyName, KeySpec,
    Lifecycle, LinkCardinality, LinkId, NamingSeriesIR, Normalization, ObjectTypeDef,
    ObjectTypeName, OwnedChildCollection, PropertyConstraints, PropertyId, PropertyType,
    PropertyTypeDef, PropertyTypeName, ResolutionPolicyName, RuntimeValue, TextFormat,
};
use serde_json::Value;

use crate::entity_artifacts::{
    EntityArtifactSetContext, Layer, check_artifact_set_entity_model, codes as set_codes,
};
use crate::entity_lowering::{EntityForm, entity_object_type};
use crate::entity_model::{EntityContext, EntityModel, effective_object_types};
use crate::object_types::{Resolved, UUID_GRAMMAR, codes as object_codes};
use crate::property_types::{Decimal, PATTERN_SIZE_LIMIT, TypeResolution, matches_grammar};
use crate::{Diagnostic, DiagnosticSeverity};

/// The effective view of one object type: what an object of the type must
/// satisfy.
#[derive(Debug, Clone)]
pub struct EffectiveObjectType {
    /// The type name (the object kind).
    pub name: ObjectTypeName,
    /// The type, then its supertypes, nearest first.
    pub lineage: Vec<ObjectTypeName>,
    /// Interfaces the type implements, directly or through a supertype.
    pub interfaces: Vec<InterfaceName>,
    /// Effective properties: declared, inherited, and added by extensions,
    /// ordered by id.
    pub properties: Vec<EffectiveProperty>,
    /// Owned child collections declared by this type and inherited types,
    /// nearest declaration first with unique ids.
    pub owned_collections: Vec<OwnedChildCollection>,
    /// Keys declared by the type and its supertypes.
    pub keys: Vec<EffectiveKey>,
    /// Whether the type or a supertype declares a link, or an extension adds
    /// one.
    pub has_links: bool,
    /// Whether the type, a supertype, or an implemented interface declares an invariant.
    pub has_invariants: bool,
    /// Effective type-level markings inherited through supertypes and
    /// implemented interfaces.
    pub markings: Vec<norn_ir::MarkingId>,
    /// Every shared property type the type reads, through the whole `ref`
    /// chain: of its effective properties, of every declaration along its
    /// lineage and its extensions, and of its interfaces' requirements.
    pub referenced_property_types: BTreeSet<PropertyTypeName>,
    /// The effective lifecycles (T3-LIFE-1/2): declared or inherited,
    /// restricted to the states the type's enum keeps, one per lifecycle
    /// property, ordered by property id.
    pub lifecycles: Vec<EffectiveLifecycle>,
}

/// One effective link contract, including its resolved link-property
/// contracts.
#[derive(Debug, Clone)]
pub struct EffectiveLink {
    /// The forward link role.
    pub id: LinkId,
    /// The object type or interface the link targets.
    pub target: ObjectTypeName,
    /// Multiplicity and requiredness of both ends.
    pub cardinality: LinkCardinality,
    /// Whether the forward role must remain acyclic at commit.
    pub acyclic: bool,
    /// The inverse role, when declared.
    pub inverse: Option<LinkId>,
    /// Effective link properties, resolved through the shared type catalog.
    pub properties: Vec<EffectiveProperty>,
}

/// One effective lifecycle of an object type: the machine the checker
/// enforces and the diagram renders (ruling 2026-09-25 §2, "this one list").
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveLifecycle {
    /// The lifecycle property's effective enum values (the states), in
    /// presentation order.
    pub states: Vec<String>,
    /// The effective machine as a lifecycle: inherited guards first, states
    /// in presentation order.
    pub lifecycle: Lifecycle,
}

impl EffectiveObjectType {
    /// The effective lifecycle of property `property`, if it has one.
    #[must_use]
    pub fn lifecycle(&self, property: &str) -> Option<&EffectiveLifecycle> {
        self.lifecycles
            .iter()
            .find(|lifecycle| lifecycle.lifecycle.property.as_str() == property)
    }
}

/// A feature of an object type whose instance semantics are not implemented,
/// so writes of the type fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// What the type uses, for a message (`links`, `property kind 'decimal'
    /// on 'price'`).
    pub feature: String,
    /// Dotted path of the member in the artifact.
    pub path: &'static str,
}

impl EffectiveObjectType {
    /// The effective view of one definition on its own, with no catalog,
    /// supertype, interface, or extension: the view of a lowered `EntityIR`
    /// v1 root (T1-ONT-4, #2517). A `ref` stays unresolved, so it is
    /// unsupported.
    ///
    /// # Errors
    ///
    /// Returns the unsupported feature when the definition names a supertype
    /// or an interface, which only resolve in a catalog.
    pub fn standalone(def: &ObjectTypeDef) -> Result<Self, Unsupported> {
        if def.extends.is_some() {
            return Err(Unsupported {
                feature: "a supertype outside an entity catalog".to_owned(),
                path: "artifact.payload.objectType.extends",
            });
        }
        if !def.implements.is_empty() {
            return Err(Unsupported {
                feature: "interfaces outside an entity catalog".to_owned(),
                path: "artifact.payload.objectType.implements",
            });
        }
        let mut properties: BTreeMap<&PropertyId, EffectiveProperty> = BTreeMap::new();
        for property in &def.properties {
            properties.entry(&property.id).or_insert_with(|| {
                let mut effective = EffectiveProperty::new(
                    property,
                    match &property.property_type {
                        PropertyType::Ref { .. } => EffectiveKind::Unresolved,
                        base @ (PropertyType::Text { .. }
                        | PropertyType::Integer { .. }
                        | PropertyType::Decimal { .. }
                        | PropertyType::Boolean { .. }
                        | PropertyType::Timestamp { .. }
                        | PropertyType::Date { .. }
                        | PropertyType::Duration { .. }
                        | PropertyType::Enum { .. }
                        | PropertyType::Record { .. }
                        | PropertyType::JsonExt { .. }) => EffectiveKind::Base(base.clone()),
                    },
                    vec![property.constraints.clone()],
                    property.constraints.nullable,
                );
                effective.prepare_record(&[], 0);
                effective.markings.extend(def.markings.iter().cloned());
                effective.markings.extend(property.markings.iter().cloned());
                effective.markings.sort();
                effective.markings.dedup();
                effective
            });
        }
        Ok(Self {
            name: def.name.clone(),
            lineage: vec![def.name.clone()],
            interfaces: Vec::new(),
            properties: properties.into_values().collect(),
            owned_collections: def.owned_collections.clone(),
            keys: def
                .identity
                .iter()
                .map(|key| EffectiveKey::new(&def.name, key))
                .collect(),
            has_links: !def.links.is_empty(),
            has_invariants: !def.invariants.is_empty(),
            markings: def.markings.clone(),
            referenced_property_types: def
                .properties
                .iter()
                .filter_map(|property| match &property.property_type {
                    PropertyType::Ref { name } => Some(name.clone()),
                    PropertyType::Text { .. }
                    | PropertyType::Integer { .. }
                    | PropertyType::Decimal { .. }
                    | PropertyType::Boolean { .. }
                    | PropertyType::Timestamp { .. }
                    | PropertyType::Date { .. }
                    | PropertyType::Duration { .. }
                    | PropertyType::Enum { .. }
                    | PropertyType::Record { .. }
                    | PropertyType::JsonExt { .. } => None,
                })
                .collect(),
            // A standalone view is a lowered v1 root, which declares none.
            lifecycles: Vec::new(),
        })
    }

    /// The effective property with this id.
    #[must_use]
    pub fn property(&self, id: &str) -> Option<&EffectiveProperty> {
        self.properties
            .iter()
            .find(|property| property.id.as_str() == id)
    }

    /// Check a value written to a stored property of this effective type.
    /// Unknown and computed properties are not writable in stored facts.
    ///
    /// # Errors
    ///
    /// Returns why the property is not a writable stored property or why
    /// its runtime value violates the effective contract.
    pub fn check_stored_property_value(
        &self,
        id: &str,
        value: &RuntimeValue,
    ) -> Result<(), InstanceError> {
        let Some(property) = self.property(id) else {
            return Err(InstanceError::ConstraintViolation(format!(
                "unknown stored property {id:?}"
            )));
        };
        if property.computed {
            return Err(InstanceError::ConstraintViolation(format!(
                "computed property {id:?} is read-only"
            )));
        }
        property.check_runtime_value(value)
    }

    /// The first feature of the type whose instance semantics are not
    /// implemented; `None` when every object write of the type can be
    /// validated completely.
    #[must_use]
    pub fn unsupported(&self) -> Option<Unsupported> {
        if self.has_links {
            return Some(Unsupported {
                feature: "links (a v2 link has no instance representation in storage yet)"
                    .to_owned(),
                path: "artifact.payload.objectType.links",
            });
        }
        if let Some(key) = self.keys.iter().find(|key| key.match_policy.is_some()) {
            return Some(Unsupported {
                feature: format!(
                    "identity key '{}' with a match policy (entity resolution is T8-ID-1)",
                    key.name.as_str()
                ),
                path: "artifact.payload.objectType.identity",
            });
        }
        if self.has_invariants {
            return Some(Unsupported {
                feature: "invariants (commit-time ZEN Expression evaluation is pending)".to_owned(),
                path: "artifact.payload.objectType.invariants",
            });
        }
        self.properties.iter().find_map(|property| {
            property.unsupported().map(|feature| Unsupported {
                feature,
                path: "artifact.payload.objectType.properties",
            })
        })
    }
}

/// What a property's type resolves to.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectiveKind {
    /// A local, non-`ref` type.
    Base(PropertyType),
    /// A registry value type, whose definition is not available (T8-STD-1).
    Registry(PropertyTypeName),
    /// A `ref` that does not resolve.
    Unresolved,
}

/// One effective property with every constraint set that applies to it.
#[derive(Debug, Clone)]
pub struct EffectiveProperty {
    /// Only declared rule outputs may update this stored property.
    pub protected: bool,
    /// The property id: the key in object payloads.
    pub id: PropertyId,
    /// The resolved type.
    pub kind: EffectiveKind,
    /// Effective markings from the property, its shared types, inherited
    /// overrides, interfaces, type-level declarations, and computed inputs.
    pub markings: Vec<norn_ir::MarkingId>,
    /// Direct effective-property dependencies read by a computed expression.
    /// Stored properties have an empty list.
    pub computed_inputs: Vec<String>,
    /// Whether the computed expression reads or passes through the whole
    /// object root, so runtime metadata must union every property input.
    pub computed_reads_all: bool,
    /// Every constraint set that applies: the property's own, each catalog
    /// definition's on its `ref` chain, and each one it narrows (an
    /// overridden property, an interface requirement). They conjoin.
    pub constraints: Vec<PropertyConstraints>,
    /// Whether a value may be absent.
    pub nullable: bool,
    /// The effective literal default (IR encoding); `None` for a computed
    /// property.
    pub default: Option<DefaultValue>,
    /// Whether the property is computed (derived and read-only).
    pub computed: bool,
    /// Every `pattern` of `constraints` with its compiled form (`None`
    /// when it does not compile).
    patterns: Vec<(String, Option<regex_lite::Regex>)>,
    record_members: Vec<EffectiveProperty>,
}

/// Why an instance value is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceError {
    /// The property uses a feature whose instance semantics are not
    /// implemented (see [`EffectiveObjectType::unsupported`]).
    Unsupported(String),
    /// The value is not of the property's type; holds the type's label.
    TypeMismatch(&'static str),
    /// The value is of the type but violates a constraint; holds why.
    ConstraintViolation(String),
}

impl EffectiveProperty {
    /// Resolved members of the closed record, including referenced constraints.
    pub(crate) fn record_members(&self) -> &[Self] {
        &self.record_members
    }

    /// Resolve one declared property without inheritance or interface requirements.
    /// The complete artifact set must still pass admission before runtime use.
    #[must_use]
    pub fn from_declared_property(
        property: &norn_ir::Property,
        catalog: &[PropertyTypeDef],
    ) -> Self {
        let resolved =
            crate::property_types::resolve_type(&property.property_type, catalog, &BTreeSet::new());
        let mut constraints = vec![property.constraints.clone()];
        constraints.extend(resolved.chain.iter().map(|set| (*set).clone()));
        let nullable = constraints.iter().any(|set| set.nullable);
        let mut effective = Self::new(property, resolved_kind(&resolved), constraints, nullable);
        effective.markings =
            crate::property_types::structured_metadata(&property.property_type, catalog)
                .1
                .into_iter()
                .collect();
        effective.prepare_record(catalog, 0);
        effective
    }

    fn new(
        property: &norn_ir::Property,
        kind: EffectiveKind,
        constraints: Vec<PropertyConstraints>,
        nullable: bool,
    ) -> Self {
        let computed = property.is_read_only();
        let patterns = compile_patterns(&constraints);
        Self {
            id: property.id.clone(),
            kind,
            markings: Vec::new(),
            computed_inputs: Vec::new(),
            computed_reads_all: false,
            constraints,
            nullable,
            default: if computed {
                None
            } else {
                property.default.clone()
            },
            computed,
            protected: property.protected,
            patterns,
            record_members: Vec::new(),
        }
    }

    pub(crate) fn from_resolved(
        resolved: &Resolved<'_>,
        markings: Vec<norn_ir::MarkingId>,
    ) -> Self {
        let kind = resolved_kind(&resolved.ty);
        let constraints = resolved.constraints().into_iter().cloned().collect();
        let mut property = Self::new(resolved.property, kind, constraints, resolved.nullable());
        // An override without its own default keeps the inherited one.
        property.default = resolved.default.cloned();
        property.markings = markings;
        property.prepare_record(resolved.catalog, 0);
        property
    }

    fn prepare_record(&mut self, catalog: &[PropertyTypeDef], depth: usize) {
        self.prepare_record_bounded(catalog, depth, &mut 4096);
    }

    fn prepare_record_bounded(
        &mut self,
        catalog: &[PropertyTypeDef],
        depth: usize,
        remaining: &mut usize,
    ) {
        let EffectiveKind::Base(PropertyType::Record { members }) = &self.kind else {
            return;
        };
        if depth >= 64 {
            return;
        }
        for member in members {
            if *remaining == 0 {
                break;
            }
            *remaining = remaining.saturating_sub(1);
            let property = norn_ir::Property {
                protected: false,
                id: member.id.clone(),
                description: None,
                property_type: member.property_type.clone(),
                constraints: member.constraints.clone(),
                default: None,
                markings: Vec::new(),
                computed: None,
            };
            let resolved = crate::property_types::resolve_type(
                &member.property_type,
                catalog,
                &BTreeSet::new(),
            );
            let mut constraints = vec![member.constraints.clone()];
            constraints.extend(resolved.chain.iter().map(|value| (*value).clone()));
            let nullable = constraints.iter().any(|value| value.nullable);
            let kind = resolved_kind(&resolved);
            let mut effective = Self::new(&property, kind, constraints, nullable);
            effective.markings = resolved
                .markings
                .iter()
                .map(|marking| (*marking).clone())
                .collect();
            effective.prepare_record_bounded(catalog, depth.saturating_add(1), remaining);
            self.record_members.push(effective);
        }
        self.markings.extend(
            self.record_members
                .iter()
                .flat_map(|member| member.markings.iter().cloned()),
        );
        self.markings.sort();
        self.markings.dedup();
    }

    fn check_record(&self, value: &RuntimeValue) -> Result<(), InstanceError> {
        if !matches!(value, RuntimeValue::Object(_)) {
            return Err(InstanceError::TypeMismatch("record object"));
        }
        let EffectiveKind::Base(PropertyType::Record { members }) = &self.kind else {
            return Err(InstanceError::TypeMismatch("record"));
        };
        if self.record_members.len() != members.len() {
            return Err(InstanceError::Unsupported(
                "record exceeds the depth or expanded member budget".to_owned(),
            ));
        }
        let stored = crate::property_codec::stored_value(norn_ir::PropertyKind::Record, value)
            .map_err(|error| {
                InstanceError::ConstraintViolation(format!(
                    "record cannot be represented in storage: {error:?}"
                ))
            })?;
        let RuntimeValue::Object(values) = stored else {
            return Err(InstanceError::TypeMismatch("record object"));
        };
        if values.len() != self.record_members.len()
            || values.keys().any(|name| {
                !self
                    .record_members
                    .iter()
                    .any(|member| member.id.as_str() == name)
            })
        {
            return Err(InstanceError::ConstraintViolation(
                "record must contain exactly its declared members".to_owned(),
            ));
        }
        for member in &self.record_members {
            let value = values.get(member.id.as_str()).ok_or_else(|| {
                InstanceError::ConstraintViolation(format!(
                    "missing record member {}",
                    member.id.as_str()
                ))
            })?;
            member.check_runtime_value(value)?;
        }
        Ok(())
    }

    /// Why values of this property cannot be validated yet; `None` when they
    /// can.
    #[must_use]
    pub fn unsupported(&self) -> Option<String> {
        let id = self.id.as_str();
        let base = match &self.kind {
            EffectiveKind::Base(base) => base,
            EffectiveKind::Registry(name) => {
                return Some(format!(
                    "registry value type '{}' on '{id}' (registry definitions are T8-STD-1)",
                    name.as_str()
                ));
            }
            EffectiveKind::Unresolved => return Some(format!("an unresolved ref on '{id}'")),
        };
        let unsupported_kind = match base {
            PropertyType::Text {
                format: Some(format @ (TextFormat::Email | TextFormat::Uri)),
            } => {
                let format = if *format == TextFormat::Email {
                    "email"
                } else {
                    "uri"
                };
                return Some(format!(
                    "text format '{format}' on '{id}' (format validation is T1-ONT-3)"
                ));
            }
            PropertyType::Record { .. }
            | PropertyType::Text { .. }
            | PropertyType::Integer {}
            | PropertyType::Boolean {}
            | PropertyType::Enum { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date {}
            | PropertyType::JsonExt { schema: None } => false,
            PropertyType::JsonExt { .. } => json_ext_type(base).is_none(),
            PropertyType::Decimal { .. } | PropertyType::Duration {} | PropertyType::Ref { .. } => {
                true
            }
        };
        if unsupported_kind {
            return Some(match base {
                PropertyType::JsonExt { .. } => format!(
                    "a json-ext JSON Schema on '{id}' (only {{\"type\": \"number\" | \"object\" | \"array\"}} is interpreted)"
                ),
                other @ (PropertyType::Text { .. }
                | PropertyType::Integer { .. }
                | PropertyType::Decimal { .. }
                | PropertyType::Boolean { .. }
                | PropertyType::Timestamp { .. }
                | PropertyType::Date { .. }
                | PropertyType::Duration { .. }
                | PropertyType::Enum { .. }
                | PropertyType::Record { .. }
                | PropertyType::Ref { .. }) => format!(
                    "property kind '{}' on '{id}' (instance encoding is T3-NUM-1, #2770)",
                    other.kind().as_str()
                ),
            });
        }
        self.patterns
            .iter()
            .find(|(_, regex)| regex.is_none())
            .map(|(pattern, _)| format!("pattern {pattern:?} on '{id}', which does not compile"))
    }

    /// Check one present value against the property's type and every
    /// constraint set.
    ///
    /// # Errors
    ///
    /// Returns why the value is rejected.
    pub fn check_value(&self, value: &Value) -> Result<(), InstanceError> {
        if let Some(reason) = self.unsupported() {
            return Err(InstanceError::Unsupported(reason));
        }
        let EffectiveKind::Base(base) = &self.kind else {
            return Err(InstanceError::Unsupported(format!(
                "an unresolved type on '{}'",
                self.id.as_str()
            )));
        };
        match base {
            PropertyType::Record { .. } => {
                let runtime = RuntimeValue::from_json(value.clone())
                    .map_err(|_inexact| InstanceError::TypeMismatch("exact record"))?;
                self.check_record(&runtime)
            }

            PropertyType::Text { format } => {
                let Value::String(text) = value else {
                    return Err(InstanceError::TypeMismatch("text"));
                };
                if *format == Some(TextFormat::Uuid) && !matches_grammar(&UUID_GRAMMAR, text) {
                    return Err(InstanceError::TypeMismatch("uuid text"));
                }
                self.check_text(text)
            }
            PropertyType::Enum { values } => match value {
                Value::String(text) if values.iter().any(|entry| &entry.value == text) => Ok(()),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_)
                | Value::Object(_) => Err(InstanceError::TypeMismatch("enum")),
            },
            PropertyType::Integer {} => {
                let integer =
                    integer_literal(value).ok_or(InstanceError::TypeMismatch("integer"))?;
                self.check_bounds(&integer.to_string())
            }
            PropertyType::Boolean {} => {
                if value.is_boolean() {
                    Ok(())
                } else {
                    Err(InstanceError::TypeMismatch("boolean"))
                }
            }
            PropertyType::Date {} => match value.as_str() {
                Some(text) if crate::object_types::valid_date(text) => Ok(()),
                Some(_) => Err(InstanceError::ConstraintViolation(
                    "must be a valid calendar date in YYYY-MM-DD form".to_owned(),
                )),
                None => Err(InstanceError::TypeMismatch("date string")),
            },
            PropertyType::Timestamp { timezone } => match value.as_str() {
                Some(text) if crate::object_types::valid_timestamp(text, *timezone) => Ok(()),
                Some(_) => Err(InstanceError::ConstraintViolation(
                    "must be an RFC 3339 date-time with an admitted explicit offset".to_owned(),
                )),
                None => Err(InstanceError::TypeMismatch("timestamp string")),
            },
            PropertyType::JsonExt { .. } => {
                let (matches, label) = match json_ext_type(base) {
                    Some("number") => (value.is_number(), "number"),
                    Some("object") => (value.is_object(), "object"),
                    Some("array") => (value.is_array(), "array"),
                    _ => (!value.is_null(), "json-ext"),
                };
                if matches {
                    Ok(())
                } else {
                    Err(InstanceError::TypeMismatch(label))
                }
            }
            other @ (PropertyType::Decimal { .. }
            | PropertyType::Duration { .. }
            | PropertyType::Ref { .. }) => Err(InstanceError::Unsupported(format!(
                "property kind '{}' on '{}'",
                other.kind().as_str(),
                self.id.as_str()
            ))),
        }
    }

    /// Check an exact cell runtime value against this effective property.
    /// Unlike [`Self::check_value`], this path supports exact decimal values
    /// and honors the effective nullable contract for `null`. JSON extension
    /// values also support the shared closed decision-schema dialect.
    ///
    /// # Errors
    ///
    /// Returns why the value is rejected.
    pub fn check_runtime_value(&self, value: &RuntimeValue) -> Result<(), InstanceError> {
        if value.is_null() {
            return if self.nullable {
                Ok(())
            } else {
                Err(InstanceError::ConstraintViolation(
                    "null is not allowed".to_owned(),
                ))
            };
        }
        let EffectiveKind::Base(base) = &self.kind else {
            return Err(InstanceError::Unsupported(format!(
                "an unresolved type on '{}'",
                self.id.as_str()
            )));
        };
        let is_decimal = matches!(base, PropertyType::Decimal { .. });
        let closed_json_schema = if let PropertyType::JsonExt {
            schema: Some(schema),
        } = base
        {
            norn_expr::schema::Schema::parse(&JsonValue::Object(schema.clone()), self.id.as_str())
                .ok()
        } else {
            None
        };
        if !is_decimal
            && closed_json_schema.is_none()
            && let Some(reason) = self.unsupported()
        {
            return Err(InstanceError::Unsupported(reason));
        }
        match base {
            PropertyType::Record { .. } => self.check_record(value),
            PropertyType::Text { format } => {
                let Some(text) = value.as_str() else {
                    return Err(InstanceError::TypeMismatch("text"));
                };
                if *format == Some(TextFormat::Uuid) && !matches_grammar(&UUID_GRAMMAR, text) {
                    return Err(InstanceError::TypeMismatch("uuid text"));
                }
                self.check_text(text)
            }
            PropertyType::Enum { values } => match value.as_str() {
                Some(text) if values.iter().any(|entry| entry.value == text) => Ok(()),
                _ => Err(InstanceError::TypeMismatch("enum")),
            },
            PropertyType::Integer {} => {
                let Some(integer) = value.as_i64() else {
                    return Err(InstanceError::TypeMismatch("integer"));
                };
                self.check_runtime_bounds(&integer.to_string(), "integer")
            }
            PropertyType::Decimal { scale, .. } => {
                let Some(decimal) = value.as_decimal() else {
                    return Err(InstanceError::TypeMismatch("decimal"));
                };
                if u64::from(decimal.scale()) > scale.0 {
                    return Err(InstanceError::ConstraintViolation(format!(
                        "has scale {}, more than declared scale {}",
                        decimal.scale(),
                        scale.0
                    )));
                }
                self.check_runtime_bounds(&decimal.canonical(), "decimal")
            }
            PropertyType::Boolean {} => {
                if value.as_bool().is_some() {
                    Ok(())
                } else {
                    Err(InstanceError::TypeMismatch("boolean"))
                }
            }
            PropertyType::Date {} => match value.as_str() {
                Some(text) if crate::object_types::valid_date(text) => Ok(()),
                Some(_) => Err(InstanceError::ConstraintViolation(
                    "must be a valid calendar date in YYYY-MM-DD form".to_owned(),
                )),
                None => Err(InstanceError::TypeMismatch("date string")),
            },
            PropertyType::Timestamp { timezone } => match value.as_str() {
                Some(text) if crate::object_types::valid_timestamp(text, *timezone) => Ok(()),
                Some(_) => Err(InstanceError::ConstraintViolation(
                    "must be an RFC 3339 date-time with an admitted explicit offset".to_owned(),
                )),
                None => Err(InstanceError::TypeMismatch("timestamp string")),
            },
            PropertyType::JsonExt { .. } => {
                self.check_runtime_json(value, closed_json_schema, base)
            }
            other @ (PropertyType::Duration { .. } | PropertyType::Ref { .. }) => {
                Err(InstanceError::Unsupported(format!(
                    "property kind '{}' on '{}'",
                    other.kind().as_str(),
                    self.id.as_str()
                )))
            }
        }
    }

    /// Canonicalize a valid timestamp runtime value for storage. Invalid source
    /// strings are preserved so observation admission can record them as
    /// quality findings; strict commit admission rejects them separately.
    #[must_use]
    pub fn canonical_runtime_value(&self, value: &RuntimeValue) -> RuntimeValue {
        if matches!(self.kind, EffectiveKind::Base(PropertyType::Record { .. })) {
            let RuntimeValue::Object(values) = value else {
                return value.clone();
            };
            return RuntimeValue::Object(
                values
                    .iter()
                    .map(|(name, value)| {
                        let canonical = self
                            .record_members
                            .iter()
                            .find(|member| member.id.as_str() == name)
                            .map_or_else(
                                || value.clone(),
                                |member| member.canonical_runtime_value(value),
                            );
                        (name.clone(), canonical)
                    })
                    .collect(),
            );
        }
        let EffectiveKind::Base(PropertyType::Timestamp { .. }) = &self.kind else {
            return value.clone();
        };
        let Some(timestamp) = value.as_str() else {
            return value.clone();
        };
        norn_expr::temporal::canonical_timestamp(timestamp)
            .map_or_else(|_| value.clone(), RuntimeValue::String)
    }

    fn check_runtime_json(
        &self,
        value: &RuntimeValue,
        schema: Option<norn_expr::schema::Schema>,
        base: &PropertyType,
    ) -> Result<(), InstanceError> {
        if let Some(schema) = schema {
            let value = norn_expr::Value::try_from(value)
                .map_err(|error| InstanceError::ConstraintViolation(error.to_string()))?;
            return schema.check(&value, self.id.as_str()).map_err(|violation| {
                InstanceError::ConstraintViolation(format!(
                    "{}: {}",
                    violation.path, violation.reason
                ))
            });
        }
        let matches = match json_ext_type(base) {
            Some("number") => value.as_decimal().is_some(),
            Some("object") => value.as_object().is_some(),
            Some("array") => value.as_array().is_some(),
            _ => true,
        };
        if matches {
            Ok(())
        } else {
            Err(InstanceError::TypeMismatch("json-ext"))
        }
    }

    /// Predicate bounds need the value's type, not the stored column's bounds.
    /// Optional paths admit null for equality even when the terminal is non-null.
    pub(crate) fn check_predicate_literal(
        &self,
        value: &RuntimeValue,
        nullable: bool,
    ) -> Result<(), InstanceError> {
        let mut literal_type = self.clone();
        literal_type.constraints.clear();
        literal_type.patterns.clear();
        literal_type.nullable = nullable;
        literal_type.check_runtime_value(value)
    }

    /// Check a value under the candidate contract while accounting for its
    /// previous meaning. A stored decimal cannot be reinterpreted across a
    /// unit change without an explicit conversion.
    ///
    /// # Errors
    ///
    /// Returns why the value is rejected or why its prior meaning cannot be
    /// preserved.
    pub fn check_runtime_value_from(
        &self,
        previous: &Self,
        value: &RuntimeValue,
    ) -> Result<(), InstanceError> {
        if !value.is_null()
            && let (
                EffectiveKind::Base(PropertyType::Decimal {
                    unit: previous_unit,
                    ..
                }),
                EffectiveKind::Base(PropertyType::Decimal {
                    unit: candidate_unit,
                    ..
                }),
            ) = (&previous.kind, &self.kind)
            && previous_unit != candidate_unit
        {
            return Err(InstanceError::Unsupported(format!(
                "decimal unit changed from {previous_unit:?} to {candidate_unit:?}; stored value needs an explicit conversion"
            )));
        }
        if let RuntimeValue::Object(values) = value {
            for member in &self.record_members {
                if let (Some(previous), Some(value)) = (
                    previous
                        .record_members
                        .iter()
                        .find(|old| old.id == member.id),
                    values.get(member.id.as_str()),
                ) {
                    member.check_runtime_value_from(previous, value)?;
                }
            }
        }
        self.check_runtime_value(value)
    }

    /// Whether `self` and `previous` admit the same effective values. Exact
    /// comparison of conjunctive constraints is conservative when equivalent
    /// patterns have different spellings.
    #[must_use]
    pub fn same_contract(&self, previous: &Self) -> bool {
        self.nullable == previous.nullable
            && same_effective_kind(&self.kind, &previous.kind)
            && effective_constraints_equal(&self.constraints, &previous.constraints)
            && self.record_members.len() == previous.record_members.len()
            && self.record_members.iter().all(|member| {
                previous
                    .record_members
                    .iter()
                    .any(|old| old.id == member.id && member.same_contract(old))
            })
    }

    /// Candidate-side reasons this property may admit fewer values than the
    /// installed `previous` contract. Incomparable type/pattern changes are
    /// conservatively reported for an explicit narrowing decision.
    #[must_use]
    pub fn narrowing_reasons_from(&self, previous: &Self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.record_members.len() != previous.record_members.len()
            || self.record_members.iter().any(|member| {
                !previous
                    .record_members
                    .iter()
                    .any(|old| old.id == member.id && member.same_contract(old))
            })
        {
            reasons.push("record member contract changed".to_owned());
        }
        if !same_effective_kind(&self.kind, &previous.kind)
            && kind_narrows(&self.kind, &previous.kind) != Some(false)
        {
            reasons.push("property type became narrower or changed".to_owned());
        }
        if !self.nullable && previous.nullable {
            reasons.push("property no longer allows null".to_owned());
        }
        if constraint_bound_narrows(&self.constraints, &previous.constraints, "min") {
            reasons.push("minimum bound increased".to_owned());
        }
        if constraint_bound_narrows(&self.constraints, &previous.constraints, "max") {
            reasons.push("maximum bound decreased".to_owned());
        }
        if constraint_length_narrows(&self.constraints, &previous.constraints, true) {
            reasons.push("minimum length increased".to_owned());
        }
        if constraint_length_narrows(&self.constraints, &previous.constraints, false) {
            reasons.push("maximum length decreased".to_owned());
        }
        let old_patterns = effective_patterns(&previous.constraints);
        let new_patterns = effective_patterns(&self.constraints);
        if new_patterns
            .iter()
            .any(|pattern| !old_patterns.contains(pattern))
        {
            reasons.push("pattern added or changed".to_owned());
        }
        reasons
    }

    /// Materialize an IR default without passing exact numeric literals through
    /// binary JSON numbers. Record members use their own resolved types.
    ///
    /// # Errors
    /// Returns an error for an inexact or incorrectly encoded numeric default.
    pub fn runtime_default_literal(
        &self,
        literal: &JsonValue,
    ) -> Result<RuntimeValue, norn_ir::RuntimeValueError> {
        match (&self.kind, literal) {
            (
                EffectiveKind::Base(PropertyType::Integer {} | PropertyType::Decimal { .. }),
                JsonValue::String(text),
            ) => text.parse::<norn_ir::Decimal>().map(RuntimeValue::Number),
            (EffectiveKind::Base(PropertyType::Integer {} | PropertyType::Decimal { .. }), _) => {
                Err(norn_ir::RuntimeValueError::InvalidJson {
                    message: "numeric IR defaults must be exact decimal strings".to_owned(),
                })
            }
            (EffectiveKind::Base(PropertyType::Record { .. }), JsonValue::Object(values)) => values
                .iter()
                .map(|(name, value)| {
                    let value = match self
                        .record_members
                        .iter()
                        .find(|member| member.id.as_str() == name)
                    {
                        Some(member) if !matches!(value, JsonValue::Null) => {
                            member.runtime_default_literal(value)?
                        }
                        _ => RuntimeValue::from_ir(value)?,
                    };
                    Ok((name.clone(), value))
                })
                .collect::<Result<_, _>>()
                .map(RuntimeValue::Object),
            _ => RuntimeValue::from_ir(literal),
        }
    }

    /// The value a create fills in when the property is absent: the effective
    /// default in instance encoding. `None` when the property has no default
    /// or is computed.
    ///
    /// # Errors
    ///
    /// Returns why the default cannot be materialized (an unsupported
    /// property, or a default that does not check against the instance rules,
    /// which admission should have rejected).
    pub fn default_value(&self) -> Result<Option<Value>, InstanceError> {
        let Some(default) = self.default.as_ref().filter(|_| !self.computed) else {
            return Ok(None);
        };
        if let Some(reason) = self.unsupported() {
            return Err(InstanceError::Unsupported(reason));
        }
        let value = match (&self.kind, &default.0) {
            // An integer default is an exact decimal string in IR.
            (EffectiveKind::Base(PropertyType::Integer {}), JsonValue::String(text)) => text
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_out_of_range| InstanceError::TypeMismatch("integer"))?,
            (EffectiveKind::Base(PropertyType::Integer {}), _) => {
                return Err(InstanceError::TypeMismatch("integer"));
            }
            (EffectiveKind::Base(PropertyType::Record { .. }), literal) => {
                let value = self
                    .runtime_default_literal(literal)
                    .map_err(|_inexact| InstanceError::TypeMismatch("exact record default"))?;
                serde_json::to_value(&value)
                    .map_err(|_inexact| InstanceError::TypeMismatch("exact record default"))?
            }
            (_, literal) => instance_json(literal)?,
        };
        self.check_value(&value)?;
        Ok(Some(value))
    }

    fn check_text(&self, text: &str) -> Result<(), InstanceError> {
        let length = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        for constraints in &self.constraints {
            if let Some(min) = constraints.min_length
                && length < min.0
            {
                return Err(InstanceError::ConstraintViolation(format!(
                    "has {length} characters, fewer than minLength {}",
                    min.0
                )));
            }
            if let Some(max) = constraints.max_length
                && length > max.0
            {
                return Err(InstanceError::ConstraintViolation(format!(
                    "has {length} characters, more than maxLength {}",
                    max.0
                )));
            }
        }
        for (pattern, regex) in &self.patterns {
            // A pattern that does not compile is reported by `unsupported`.
            if !regex.as_ref().is_some_and(|regex| regex.is_match(text)) {
                return Err(InstanceError::ConstraintViolation(format!(
                    "does not match pattern {pattern:?}"
                )));
            }
        }
        Ok(())
    }

    fn check_bounds(&self, literal: &str) -> Result<(), InstanceError> {
        let value = Decimal::parse_str(literal).ok_or(InstanceError::TypeMismatch("integer"))?;
        for constraints in &self.constraints {
            for (member, bound, violated) in [
                ("min", &constraints.min, std::cmp::Ordering::Less),
                ("max", &constraints.max, std::cmp::Ordering::Greater),
            ] {
                let Some(bound) = bound else { continue };
                let limit = Decimal::parse_str(bound.as_str()).ok_or_else(|| {
                    InstanceError::Unsupported(format!(
                        "malformed {member} bound {:?} on '{}'",
                        bound.as_str(),
                        self.id.as_str()
                    ))
                })?;
                if value.cmp(&limit) == violated {
                    return Err(InstanceError::ConstraintViolation(format!(
                        "{literal} is outside {member} {:?}",
                        bound.as_str()
                    )));
                }
            }
        }
        Ok(())
    }

    fn check_runtime_bounds(&self, literal: &str, kind: &'static str) -> Result<(), InstanceError> {
        let value = Decimal::parse_str(literal).ok_or(InstanceError::TypeMismatch(kind))?;
        for constraints in &self.constraints {
            for (member, bound, violated) in [
                ("min", &constraints.min, std::cmp::Ordering::Less),
                ("max", &constraints.max, std::cmp::Ordering::Greater),
            ] {
                let Some(bound) = bound else { continue };
                let limit = Decimal::parse_str(bound.as_str()).ok_or_else(|| {
                    InstanceError::Unsupported(format!(
                        "malformed {member} bound {:?} on '{}'",
                        bound.as_str(),
                        self.id.as_str()
                    ))
                })?;
                if value.cmp(&limit) == violated {
                    return Err(InstanceError::ConstraintViolation(format!(
                        "{literal} is outside {member} {:?}",
                        bound.as_str()
                    )));
                }
            }
        }
        Ok(())
    }
}

fn resolved_kind(resolved: &crate::property_types::ResolvedType<'_>) -> EffectiveKind {
    match resolved.resolution {
        TypeResolution::Base(base) => EffectiveKind::Base(base.clone()),
        TypeResolution::Registry => resolved
            .names
            .last()
            .map_or(EffectiveKind::Unresolved, |name| {
                EffectiveKind::Registry((*name).clone())
            }),
        TypeResolution::Unresolved { .. } => EffectiveKind::Unresolved,
    }
}

fn same_effective_kind(left: &EffectiveKind, right: &EffectiveKind) -> bool {
    match (left, right) {
        (EffectiveKind::Base(left), EffectiveKind::Base(right)) => match (left, right) {
            (PropertyType::Enum { values: left }, PropertyType::Enum { values: right }) => {
                enum_values(left) == enum_values(right)
            }
            (PropertyType::Timestamp { .. }, PropertyType::Timestamp { .. }) => true,
            _ => left == right,
        },
        (EffectiveKind::Registry(left), EffectiveKind::Registry(right)) => left == right,
        (EffectiveKind::Unresolved, EffectiveKind::Unresolved) => true,
        _ => false,
    }
}

fn kind_narrows(candidate: &EffectiveKind, previous: &EffectiveKind) -> Option<bool> {
    let (EffectiveKind::Base(candidate), EffectiveKind::Base(previous)) = (candidate, previous)
    else {
        return None;
    };
    match (candidate, previous) {
        (PropertyType::Text { format: candidate }, PropertyType::Text { format: previous }) => {
            match (candidate, previous) {
                (_, None) => Some(true),
                (Some(candidate), Some(previous)) if candidate == previous => Some(true),
                (None, Some(_)) => Some(false),
                (Some(_), Some(_)) => None,
            }
        }
        (PropertyType::Enum { values: candidate }, PropertyType::Enum { values: previous }) => {
            let candidate = enum_values(candidate);
            let previous = enum_values(previous);
            if candidate.is_subset(&previous) {
                Some(true)
            } else if previous.is_subset(&candidate) {
                Some(false)
            } else {
                None
            }
        }
        (
            PropertyType::Decimal {
                scale: candidate_scale,
                unit: candidate_unit,
                currency_source: candidate_currency,
            },
            PropertyType::Decimal {
                scale: previous_scale,
                unit: previous_unit,
                currency_source: previous_currency,
            },
        ) if candidate_unit == previous_unit && candidate_currency == previous_currency => {
            Some(candidate_scale.0 <= previous_scale.0)
        }
        (
            PropertyType::JsonExt { schema: candidate },
            PropertyType::JsonExt { schema: previous },
        ) => match (candidate, previous) {
            (_, None) => Some(true),
            (Some(candidate), Some(previous)) if candidate == previous => Some(true),
            (Some(_), Some(_)) => None,
            (None, Some(_)) => Some(false),
        },
        _ if candidate == previous => Some(true),
        _ => None,
    }
}

fn enum_values(values: &[norn_ir::EnumValue]) -> BTreeSet<&str> {
    values.iter().map(|value| value.value.as_str()).collect()
}

fn effective_constraints_equal(
    left: &[PropertyConstraints],
    right: &[PropertyConstraints],
) -> bool {
    effective_bounds_equal(left, right, "min")
        && effective_bounds_equal(left, right, "max")
        && effective_length(left, true) == effective_length(right, true)
        && effective_length(left, false) == effective_length(right, false)
        && effective_patterns(left) == effective_patterns(right)
}

fn effective_bounds_equal(
    left: &[PropertyConstraints],
    right: &[PropertyConstraints],
    name: &str,
) -> bool {
    match (effective_bound(left, name), effective_bound(right, name)) {
        (None, None) => true,
        (Some(left), Some(right)) => match (
            Decimal::parse_str(left.as_str()),
            Decimal::parse_str(right.as_str()),
        ) {
            (Some(left), Some(right)) => left.cmp(&right) == std::cmp::Ordering::Equal,
            _ => left == right,
        },
        _ => false,
    }
}

fn effective_bound<'a>(
    constraints: &'a [PropertyConstraints],
    name: &str,
) -> Option<&'a norn_ir::DecimalLiteral> {
    constraints
        .iter()
        .filter_map(|constraint| match name {
            "min" => constraint.min.as_ref(),
            "max" => constraint.max.as_ref(),
            _ => None,
        })
        .reduce(|best, next| {
            let ordering = Decimal::parse_str(next.as_str())
                .zip(Decimal::parse_str(best.as_str()))
                .map(|(next, best)| next.cmp(&best));
            match (name, ordering) {
                ("min", Some(std::cmp::Ordering::Greater))
                | ("max", Some(std::cmp::Ordering::Less)) => next,
                _ => best,
            }
        })
}

fn effective_length(constraints: &[PropertyConstraints], minimum: bool) -> Option<u64> {
    constraints
        .iter()
        .filter_map(|constraint| {
            if minimum {
                constraint.min_length.map(|length| length.0)
            } else {
                constraint.max_length.map(|length| length.0)
            }
        })
        .reduce(|best, next| {
            if (minimum && next > best) || (!minimum && next < best) {
                next
            } else {
                best
            }
        })
}

fn effective_patterns(constraints: &[PropertyConstraints]) -> BTreeSet<String> {
    constraints
        .iter()
        .filter_map(|constraint| constraint.pattern.clone())
        .collect()
}

fn constraint_bound_narrows(
    candidate: &[PropertyConstraints],
    previous: &[PropertyConstraints],
    name: &str,
) -> bool {
    let (Some(candidate), Some(previous)) = (
        effective_bound(candidate, name),
        effective_bound(previous, name),
    ) else {
        return effective_bound(candidate, name).is_some()
            && effective_bound(previous, name).is_none();
    };
    let Some(candidate_value) = Decimal::parse_str(candidate.as_str()) else {
        return candidate != previous;
    };
    let Some(previous_value) = Decimal::parse_str(previous.as_str()) else {
        return candidate != previous;
    };
    matches!(
        (name, candidate_value.cmp(&previous_value)),
        ("min", std::cmp::Ordering::Greater) | ("max", std::cmp::Ordering::Less)
    )
}

fn constraint_length_narrows(
    candidate: &[PropertyConstraints],
    previous: &[PropertyConstraints],
    minimum: bool,
) -> bool {
    match (
        effective_length(candidate, minimum),
        effective_length(previous, minimum),
    ) {
        (Some(candidate), Some(previous)) => {
            (minimum && candidate > previous) || (!minimum && candidate < previous)
        }
        (Some(_), None) => true,
        _ => false,
    }
}

/// Compile every `pattern` in `constraints`, in order, with the checker's
/// size limit.
fn compile_patterns(
    constraints: &[PropertyConstraints],
) -> Vec<(String, Option<regex_lite::Regex>)> {
    constraints
        .iter()
        .filter_map(|set| set.pattern.as_ref())
        .map(|pattern| {
            let regex = regex_lite::RegexBuilder::new(pattern)
                .size_limit(PATTERN_SIZE_LIMIT)
                .build()
                .ok();
            (pattern.clone(), regex)
        })
        .collect()
}

/// The JSON type a `json-ext` with the one-keyword schema constrains values
/// to (see [`crate::entity_lowering::json_ext_primitive_type`]).
fn json_ext_type(property_type: &PropertyType) -> Option<&'static str> {
    match crate::entity_lowering::json_ext_primitive_type(property_type)? {
        "number" => Some("number"),
        "object" => Some("object"),
        "array" => Some("array"),
        _ => None,
    }
}

/// The value of a JSON integer literal (no fraction or exponent, in the
/// `u64` or `i64` range); classified from the literal, so the result does not
/// depend on `serde_json/arbitrary_precision` (#2440).
fn integer_literal(value: &Value) -> Option<i128> {
    let Value::Number(number) = value else {
        return None;
    };
    match norn_canonical::JsonNumber::from_serde(number).ok()?.kind() {
        norn_canonical::JsonNumberKind::NonNegativeInteger(value) => Some(i128::from(value)),
        norn_canonical::JsonNumberKind::NegativeInteger(value) => Some(i128::from(value)),
        norn_canonical::JsonNumberKind::Float => None,
    }
}

/// An IR literal as an instance value. A number is re-read from its
/// shortest decimal rendering, so an integral value is a JSON integer.
pub(crate) fn instance_json(literal: &JsonValue) -> Result<Value, InstanceError> {
    Ok(match literal {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Bool(*value),
        JsonValue::Number(number) => serde_json::from_str::<Value>(&number.to_string())
            .map_err(|_unrenderable| InstanceError::TypeMismatch("number"))?,
        JsonValue::String(text) => Value::String(text.clone()),
        JsonValue::Array(items) => {
            Value::Array(items.iter().map(instance_json).collect::<Result<_, _>>()?)
        }
        JsonValue::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(name, value)| Ok((name.clone(), instance_json(value)?)))
                .collect::<Result<_, InstanceError>>()?,
        ),
    })
}

/// One declared key of an effective object type.
#[derive(Debug, Clone)]
pub struct EffectiveKey {
    /// The key name.
    pub name: KeyName,
    /// The type that declares the key: its objects and its subtypes'
    /// objects share one key space.
    pub declared_by: ObjectTypeName,
    /// Components, in tuple order.
    pub components: Vec<PropertyId>,
    /// Normalization steps per component.
    pub normalize: BTreeMap<PropertyId, Vec<Normalization>>,
    /// The entity-resolution policy, when declared (T8-ID-1).
    pub match_policy: Option<ResolutionPolicyName>,
    /// The business-numbering contract, when this key is generated by a
    /// naming series.
    pub naming_series: Option<NamingSeriesIR>,
}

/// One normalized key component value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyComponent {
    /// A `text` or `enum` value, after normalization.
    Text(String),
    /// An `integer` value.
    Integer(i128),
    /// A `boolean` value.
    Boolean(bool),
}

impl EffectiveKey {
    pub(crate) fn new(owner: &ObjectTypeName, key: &KeySpec) -> Self {
        let rule = key.rule.as_ref();
        Self {
            name: key.name.clone(),
            declared_by: owner.clone(),
            components: key.properties.clone(),
            normalize: rule
                .map(|rule| {
                    rule.normalize
                        .iter()
                        .map(|entry| (entry.property.clone(), entry.steps.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            match_policy: rule.and_then(|rule| rule.match_policy.clone()),
            naming_series: key.naming_series.clone(),
        }
    }

    /// The normalized key tuple of an object payload, read with the
    /// component types of `object_type`; `None` when a component is absent
    /// or not of its type (such an object has no value for this key).
    #[must_use]
    pub fn value(
        &self,
        object_type: &EffectiveObjectType,
        payload: &serde_json::Map<String, Value>,
    ) -> Option<Vec<KeyComponent>> {
        self.components
            .iter()
            .map(|id| {
                let property = object_type.property(id.as_str())?;
                let value = payload.get(id.as_str())?;
                let EffectiveKind::Base(base) = &property.kind else {
                    return None;
                };
                match (base, value) {
                    (
                        PropertyType::Text { .. } | PropertyType::Enum { .. },
                        Value::String(text),
                    ) => {
                        let steps = self.normalize.get(id).map_or(&[][..], Vec::as_slice);
                        Some(KeyComponent::Text(normalize_text(text, steps)))
                    }
                    (PropertyType::Integer {}, value) => {
                        integer_literal(value).map(KeyComponent::Integer)
                    }
                    (PropertyType::Boolean {}, Value::Bool(value)) => {
                        Some(KeyComponent::Boolean(*value))
                    }
                    _ => None,
                }
            })
            .collect()
    }
}

/// Apply identity-key normalization steps to `text`, in order.
#[must_use]
pub fn normalize_text(text: &str, steps: &[Normalization]) -> String {
    steps.iter().fold(text.to_owned(), |text, step| match step {
        Normalization::Trim => text.trim().to_owned(),
        Normalization::CollapseWhitespace => collapse_whitespace(&text),
        Normalization::Lowercase => text.to_lowercase(),
        Normalization::Uppercase => text.to_uppercase(),
        Normalization::Nfc => norn_canonical::nfc(&text),
        Normalization::Nfkc => norn_canonical::nfkc(&text),
    })
}

/// Replace each run of Unicode whitespace with one space (ends included).
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if character.is_whitespace() {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(character);
            in_run = false;
        }
    }
    out
}

/// The effective object types of one set of `entity` artifacts (a scope's
/// active catalog), by the artifact that defines each.
///
/// `EntityIR` v2 artifacts use the whole-model checker; v1 object roots are
/// lowered to standalone effective contracts. The set is
/// checked as one registry layer with [`check_artifact_set_entity_model`];
/// findings that depend on the layer an artifact is installed in
/// (`scope.tenantId`, member namespaces) are ignored, because a store scope
/// is not a declared layer (so an extension may sit beside its target).
/// Identifier byte-limit findings are also ignored here: this is an effective
/// contract projection used for reads and runtime checks, and definitions
/// already admitted under a historical profile must remain usable. Admission
/// callers must run [`crate::check_artifact_set`] (or a receipt-bound variant)
/// separately; that separate path continues to reject over-limit new
/// definitions. Any other error finding on an artifact a type depends on —
/// its defining artifact, its supertypes', its interfaces', the extensions of
/// its lineage,
/// and every catalog artifact defining a shared property type it reads
/// through a `ref` chain — makes that type unavailable, so writes of it fail
/// closed. A finding on an unrelated artifact does not.
#[derive(Debug, Clone, Default)]
pub struct EffectiveCatalog {
    by_artifact: BTreeMap<String, Result<EffectiveObjectType, Diagnostic>>,
    by_name: BTreeMap<String, String>,
    interface_names: BTreeSet<String>,
    links_by_name: BTreeMap<String, Vec<EffectiveLink>>,
    lineages: BTreeMap<String, Vec<ObjectTypeName>>,
}

/// Codes whose findings depend on the layer an artifact is installed in.
pub(crate) const LAYER_DEPENDENT: &[&str] = &[
    set_codes::LAYER_SCOPE_MISMATCH,
    object_codes::RESERVED_MEMBER_SEPARATOR,
    object_codes::UNNAMESPACED_MEMBER,
    // A scope holds a type and the extensions of it side by side.
    object_codes::EXTENSION_OF_LOCAL_TYPE,
];

impl EffectiveCatalog {
    /// Build the catalog of `artifacts` (see the type docs).
    #[must_use]
    pub fn from_artifacts(artifacts: &[ArtifactEnvelope]) -> Self {
        Self::from_artifacts_with_registry(artifacts, &BTreeSet::new())
    }

    /// Build the catalog of `artifacts` against the registry (Norn Core)
    /// value type names `registry`, so a type whose `ref` names a registry
    /// type is available (the artifact-set check passes its context's
    /// registry).
    #[must_use]
    pub fn from_artifacts_with_registry(
        artifacts: &[ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        let findings: Vec<Diagnostic> = check_artifact_set_entity_model(
            artifacts,
            &EntityArtifactSetContext {
                registry,
                base: &[],
                tenant: None,
            },
        )
        .into_iter()
        .filter(|finding| {
            finding.severity == DiagnosticSeverity::Error
                && !LAYER_DEPENDENT.contains(&finding.code.as_str())
                // These are new-admission limits, not read/runtime
                // compatibility rules. The separate full artifact-set
                // admission path still reports them.
                && finding.code.as_str() != crate::codes::IDENTIFIER_TOO_LONG
        })
        .collect();

        let mut layer = Layer::default();
        for (index, artifact) in artifacts.iter().enumerate() {
            if let crate::entity_artifacts::Decoded::Root(root) =
                crate::entity_artifacts::decode_v2(artifact)
            {
                layer.push(index, *root);
            }
        }
        let catalog: Vec<PropertyTypeDef> =
            layer.catalog.iter().map(|(_, def)| def.clone()).collect();
        let context = EntityContext {
            catalog: &catalog,
            registry,
            base: EntityModel::EMPTY,
            tenant: None,
        };
        let mut effective = effective_object_types(&layer.as_model(), &context);
        let owners = Owners::new(&layer, artifacts);
        let blocking = |dependencies: &BTreeSet<&str>| {
            findings.iter().find(|finding| {
                finding
                    .artifact_id
                    .as_deref()
                    .is_none_or(|id| dependencies.contains(id))
            })
        };

        let mut out = Self::default();
        out.interface_names.extend(
            layer
                .interfaces
                .iter()
                .map(|interface| interface.name.as_str().to_owned()),
        );
        for (index, owner) in layer.object_type_owners.iter().enumerate() {
            let Some(object_type_def) = layer.object_types.get(index) else {
                continue;
            };
            let type_name = object_type_def.name.as_str().to_owned();
            let Some(own_id) = artifacts
                .get(*owner)
                .map(|artifact| artifact.artifact_id.as_str())
            else {
                continue;
            };
            let result = if let Some((object_type, links)) = effective.remove(&index) {
                if let Some(finding) = blocking(&owners.dependencies(own_id, &object_type)) {
                    Err(finding.clone())
                } else {
                    out.lineages
                        .entry(object_type.name.as_str().to_owned())
                        .or_insert_with(|| object_type.lineage.clone());
                    out.links_by_name
                        .entry(object_type.name.as_str().to_owned())
                        .or_insert(links);
                    Ok(object_type)
                }
            } else {
                // A duplicate definition of a registered name.
                Err(blocking(&BTreeSet::from([own_id]))
                    .cloned()
                    .unwrap_or_else(|| {
                        crate::property_types::error(
                            object_codes::DUPLICATE_NAME,
                            format!("artifact {own_id:?} does not define a registered object type"),
                            crate::Span::pointer("/payload/objectType/name"),
                        )
                    }))
            };
            out.by_name
                .entry(type_name)
                .or_insert_with(|| own_id.to_owned());
            out.by_artifact.entry(own_id.to_owned()).or_insert(result);
        }
        out.add_v1_artifacts(artifacts);
        out
    }

    fn add_v1_artifacts(&mut self, artifacts: &[ArtifactEnvelope]) {
        for artifact in artifacts {
            let ArtifactPayload::Entity(payload) = &artifact.payload else {
                continue;
            };
            let Ok(Some(entity)) = entity_object_type(payload) else {
                continue;
            };
            if entity.form != EntityForm::V1 {
                continue;
            }
            let Ok(object_type) = EffectiveObjectType::standalone(&entity.root.object_type) else {
                continue;
            };
            self.lineages
                .entry(object_type.name.as_str().to_owned())
                .or_insert_with(|| object_type.lineage.clone());
            self.links_by_name
                .entry(object_type.name.as_str().to_owned())
                .or_default();
            self.by_name
                .entry(object_type.name.as_str().to_owned())
                .or_insert_with(|| artifact.artifact_id.clone());
            self.by_artifact
                .entry(artifact.artifact_id.clone())
                .or_insert(Ok(object_type));
        }
    }

    /// Declared object type names, including unavailable definitions. Consumers
    /// must resolve each with `object_type_named` before using its contract.
    pub fn object_type_names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// The effective object type the artifact `artifact_id` defines: `None`
    /// when the artifact defines no `EntityIR` v2 object type in the set,
    /// `Some(Err(finding))` when the type is unavailable because of a finding
    /// on an artifact it depends on.
    #[must_use]
    pub fn object_type(
        &self,
        artifact_id: &str,
    ) -> Option<Result<&EffectiveObjectType, &Diagnostic>> {
        self.by_artifact.get(artifact_id).map(Result::as_ref)
    }

    /// The effective object type by declared object-type name: `None` when
    /// there is no available definition with that name.
    #[must_use]
    pub fn object_type_named(
        &self,
        object_type: &str,
    ) -> Option<Result<&EffectiveObjectType, &Diagnostic>> {
        self.by_name
            .get(object_type)
            .and_then(|artifact_id| self.object_type(artifact_id))
    }

    /// Concrete object types admitted by a link target that names an object
    /// type or interface. Object-type targets include their subtypes;
    /// interface targets include direct and inherited implementors. `None`
    /// means the target is unknown, ambiguous, or the effective catalog is
    /// incomplete. A known interface with no implementors returns an empty
    /// set.
    #[must_use]
    pub fn target_types(&self, target: &str) -> Option<BTreeSet<String>> {
        let object_type_target = self.by_name.contains_key(target);
        let interface_target = self.interface_names.contains(target);
        if object_type_target == interface_target {
            return None;
        }

        let mut target_types = BTreeSet::new();
        for object_type_name in self.by_name.keys() {
            let Some(Ok(object_type)) = self.object_type_named(object_type_name) else {
                return None;
            };
            let admitted = if object_type_target {
                object_type
                    .lineage
                    .iter()
                    .any(|ancestor| ancestor.as_str() == target)
            } else {
                object_type
                    .interfaces
                    .iter()
                    .any(|interface| interface.as_str() == target)
            };
            if admitted {
                target_types.insert(object_type_name.clone());
            }
        }
        Some(target_types)
    }

    /// The effective forward links of an object type, including inherited
    /// and extension links. `None` means the type is unavailable.
    #[must_use]
    pub fn links_for_object_type(&self, object_type: &str) -> Option<&[EffectiveLink]> {
        self.links_by_name.get(object_type).map(Vec::as_slice)
    }

    /// The lineage (the type, then its supertypes) of an available object
    /// type by name.
    #[must_use]
    pub fn lineage(&self, object_kind: &str) -> Option<&[ObjectTypeName]> {
        self.lineages.get(object_kind).map(Vec::as_slice)
    }
}

/// The artifact that owns each definition of a layer.
struct Owners<'s> {
    types: BTreeMap<&'s str, &'s str>,
    interfaces: BTreeMap<&'s str, &'s str>,
    /// Extension artifacts by extended type.
    extensions: BTreeMap<&'s str, Vec<&'s str>>,
    /// Catalog artifacts defining each shared property type name (a
    /// duplicate name has several).
    property_types: BTreeMap<&'s str, Vec<&'s str>>,
}

impl<'s> Owners<'s> {
    fn new(layer: &'s Layer, artifacts: &'s [ArtifactEnvelope]) -> Self {
        let id = |index: &usize| {
            artifacts
                .get(*index)
                .map(|artifact| artifact.artifact_id.as_str())
        };
        let mut owners = Self {
            types: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            extensions: BTreeMap::new(),
            property_types: BTreeMap::new(),
        };
        for ((owner, _), def) in &layer.catalog {
            if let Some(owner) = id(owner) {
                owners
                    .property_types
                    .entry(def.name.as_str())
                    .or_default()
                    .push(owner);
            }
        }
        for (def, owner) in layer.object_types.iter().zip(&layer.object_type_owners) {
            if let Some(owner) = id(owner) {
                owners.types.entry(def.name.as_str()).or_insert(owner);
            }
        }
        for (def, owner) in layer.interfaces.iter().zip(&layer.interface_owners) {
            if let Some(owner) = id(owner) {
                owners.interfaces.entry(def.name.as_str()).or_insert(owner);
            }
        }
        for (extension, owner) in layer.extensions.iter().zip(&layer.extension_owners) {
            if let Some(owner) = id(owner) {
                owners
                    .extensions
                    .entry(extension.target.as_str())
                    .or_default()
                    .push(owner);
            }
        }
        owners
    }

    /// The artifacts `object_type` (defined by `own_id`) depends on: its own,
    /// its supertypes', its interfaces', the extensions of its lineage, and
    /// every catalog artifact defining a shared property type it reads
    /// (through the whole `ref` chain; each definition of a duplicated name).
    /// A finding in an unrelated catalog does not block the type.
    fn dependencies(
        &self,
        own_id: &'s str,
        object_type: &EffectiveObjectType,
    ) -> BTreeSet<&'s str> {
        let mut dependencies = BTreeSet::from([own_id]);
        for name in &object_type.lineage {
            dependencies.extend(self.types.get(name.as_str()));
            dependencies.extend(self.extensions.get(name.as_str()).into_iter().flatten());
        }
        for name in &object_type.interfaces {
            dependencies.extend(self.interfaces.get(name.as_str()));
        }
        for name in &object_type.referenced_property_types {
            dependencies.extend(self.property_types.get(name.as_str()).into_iter().flatten());
        }
        dependencies
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn_ir::{
        ArtifactLifecycle, ArtifactProvenance, ProvenanceSource, SUPPORTED_IR_VERSION, ScopeRef,
    };

    fn effective_property(value: Value) -> EffectiveProperty {
        let mut root = serde_json::Map::new();
        root.insert(
            "name".to_owned(),
            Value::String("commerce.Record".to_owned()),
        );
        root.insert("properties".to_owned(), Value::Array(vec![value]));
        let definition: ObjectTypeDef =
            serde_json::from_value(Value::Object(root)).expect("test property should decode");
        EffectiveObjectType::standalone(&definition)
            .expect("standalone test type should resolve")
            .properties
            .into_iter()
            .next()
            .expect("one property")
    }

    #[test]
    fn normalization_steps_apply_in_order() {
        use Normalization::{CollapseWhitespace, Lowercase, Nfc, Nfkc, Trim, Uppercase};
        assert_eq!(normalize_text("  A  b\t\nC ", &[Trim]), "A  b\t\nC");
        assert_eq!(
            normalize_text("  A  b\t\nC ", &[CollapseWhitespace]),
            " A b C "
        );
        assert_eq!(
            normalize_text("  A  b\t\nC ", &[CollapseWhitespace, Trim, Lowercase]),
            "a b c"
        );
        assert_eq!(normalize_text("abc", &[Uppercase]), "ABC");
        // "e" + combining acute composes under NFC; the "fi" ligature only
        // under NFKC.
        assert_eq!(normalize_text("e\u{301}", &[Nfc]), "\u{e9}");
        assert_eq!(normalize_text("\u{fb01}", &[Nfc]), "\u{fb01}");
        assert_eq!(normalize_text("\u{fb01}", &[Nfkc]), "fi");
        assert_eq!(normalize_text("x", &[]), "x");
    }

    #[test]
    fn integer_literals_are_classified_from_the_literal() {
        assert_eq!(integer_literal(&serde_json::json!(7)), Some(7));
        assert_eq!(integer_literal(&serde_json::json!(-7)), Some(-7));
        assert_eq!(integer_literal(&serde_json::json!(7.5)), None);
        assert_eq!(integer_literal(&serde_json::json!("7")), None);
    }

    #[test]
    fn ir_literals_become_instance_json() {
        assert_eq!(
            instance_json(&JsonValue::Number(5.0)),
            Ok(serde_json::json!(5))
        );
        assert_eq!(
            instance_json(&JsonValue::Number(0.5)),
            Ok(serde_json::json!(0.5))
        );
    }

    #[test]
    fn runtime_json_contract_reuses_exact_closed_schema_validation() {
        let property = effective_property(serde_json::json!({
            "id": "slots",
            "type": { "kind": "json-ext", "schema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "mode": { "type": "string", "enum": ["open", "closed"] },
                    "count": { "type": "integer" }
                }, "required": ["mode", "count"]
            }}
        }));
        for (source, valid) in [
            (r#"{"mode":"open","count":9007199254740993}"#, true),
            (r#"{"mode":"bogus","count":1}"#, false),
            (r#"{"mode":"open","count":1.5}"#, false),
            (r#"{"mode":"open","count":1,"unknown":true}"#, false),
        ] {
            let value = RuntimeValue::from_json_text(source).expect("exact runtime record");
            assert_eq!(
                property.check_runtime_value(&value).is_ok(),
                valid,
                "{source}"
            );
        }
        let unsupported = effective_property(serde_json::json!({
            "id": "slots", "type": { "kind": "json-ext", "schema": {
                "type": "number", "minimum": 0
            }}
        }));
        assert!(matches!(
            unsupported.check_runtime_value(&RuntimeValue::from_json_text("1").expect("number")),
            Err(InstanceError::Unsupported(_))
        ));
    }

    #[test]
    fn runtime_integer_bounds_remain_exact_above_binary_float_precision() {
        let property = effective_property(serde_json::json!({
            "id": "sequence",
            "type": { "kind": "integer" },
            "constraints": { "min": "9007199254740993" }
        }));
        let at_bound = RuntimeValue::from_json_text("9007199254740993").expect("exact integer");
        let below = RuntimeValue::from_json_text("9007199254740992").expect("exact integer");

        assert_eq!(property.check_runtime_value(&at_bound), Ok(()));
        assert!(matches!(
            property.check_runtime_value(&below),
            Err(InstanceError::ConstraintViolation(_))
        ));
    }

    #[test]
    fn runtime_decimal_checks_scale_and_exact_bounds() {
        let property = effective_property(serde_json::json!({
            "id": "price",
            "type": { "kind": "decimal", "scale": 2 },
            "constraints": { "min": "1.21", "max": "2.00" }
        }));
        let valid = RuntimeValue::from_json_text("1.21").expect("exact decimal");
        let below = RuntimeValue::from_json_text("1.20").expect("exact decimal");
        let too_precise = RuntimeValue::from_json_text("1.234").expect("exact decimal");

        assert_eq!(property.check_runtime_value(&valid), Ok(()));
        assert!(property.check_runtime_value(&below).is_err());
        assert!(property.check_runtime_value(&too_precise).is_err());
        assert!(matches!(
            property.check_value(&serde_json::json!(1.21)),
            Err(InstanceError::Unsupported(_))
        ));
    }

    #[test]
    fn runtime_decimal_unit_changes_fail_closed_for_existing_values() {
        let previous = effective_property(serde_json::json!({
            "id": "mass",
            "type": { "kind": "decimal", "scale": 2, "unit": { "system": "ucum", "code": "kg" } }
        }));
        let candidate = effective_property(serde_json::json!({
            "id": "mass",
            "type": { "kind": "decimal", "scale": 2, "unit": { "system": "ucum", "code": "g" } }
        }));
        let stored = RuntimeValue::from_json_text("100").expect("exact decimal");

        assert!(matches!(
            candidate.check_runtime_value_from(&previous, &stored),
            Err(InstanceError::Unsupported(_))
        ));
        assert!(!candidate.narrowing_reasons_from(&previous).is_empty());
    }

    #[test]
    fn runtime_nullable_text_checks_scalar_length_and_pattern() {
        let property = effective_property(serde_json::json!({
            "id": "code",
            "type": { "kind": "text" },
            "constraints": { "nullable": true, "minLength": 2, "pattern": "^AB" }
        }));
        let null = RuntimeValue::Null;
        let short = RuntimeValue::String("A".to_owned());
        let wrong_prefix = RuntimeValue::String("ZZ".to_owned());
        let accepted = RuntimeValue::String("AB12".to_owned());

        assert_eq!(property.check_runtime_value(&null), Ok(()));
        assert!(property.check_runtime_value(&short).is_err());
        assert!(property.check_runtime_value(&wrong_prefix).is_err());
        assert_eq!(property.check_runtime_value(&accepted), Ok(()));
    }

    #[test]
    fn runtime_temporal_values_validate_calendar_and_canonicalize_instants() {
        let date = effective_property(serde_json::json!({
            "id": "service_date",
            "type": { "kind": "date" }
        }));
        assert_eq!(
            date.check_runtime_value(&RuntimeValue::String("2024-02-29".to_owned())),
            Ok(())
        );
        for invalid in ["2025-02-29", "2024-02-30", "2024-13-01", "24-02-29"] {
            assert!(
                date.check_runtime_value(&RuntimeValue::String(invalid.to_owned()))
                    .is_err(),
                "{invalid} must not be admitted as a calendar date"
            );
        }

        let utc = effective_property(serde_json::json!({
            "id": "occurred_at",
            "type": { "kind": "timestamp", "timezone": "utc" }
        }));
        let offset = effective_property(serde_json::json!({
            "id": "occurred_at",
            "type": { "kind": "timestamp", "timezone": "offset" }
        }));
        let source = RuntimeValue::String("2024-02-29T12:30:00+02:30".to_owned());
        let canonical = RuntimeValue::String("2024-02-29T10:00:00Z".to_owned());
        assert_eq!(utc.check_runtime_value(&source), Ok(()));
        assert_eq!(offset.check_runtime_value(&source), Ok(()));
        assert_eq!(utc.canonical_runtime_value(&source), canonical);
        assert_eq!(offset.canonical_runtime_value(&source), canonical);
        assert!(utc.same_contract(&offset));
        assert!(utc.narrowing_reasons_from(&offset).is_empty());
        for invalid in [
            "2025-02-29T12:30:00Z",
            "2024-02-29T12:30:00",
            "2024-02-29 12:30:00Z",
        ] {
            assert!(
                utc.check_runtime_value(&RuntimeValue::String(invalid.to_owned()))
                    .is_err(),
                "{invalid} must not be admitted as a timestamp"
            );
        }
    }

    #[test]
    fn enum_reordering_is_stable_but_partial_replacement_is_narrowing() {
        let previous = effective_property(serde_json::json!({
            "id": "state",
            "type": { "kind": "enum", "values": [
                { "value": "open", "description": "Open" },
                { "value": "closed" }
            ] }
        }));
        let reordered = effective_property(serde_json::json!({
            "id": "state",
            "type": { "kind": "enum", "values": [
                { "value": "closed", "description": "Done" },
                { "value": "open", "description": "Available" }
            ] }
        }));
        let partial_replacement = effective_property(serde_json::json!({
            "id": "state",
            "type": { "kind": "enum", "values": [
                { "value": "open" },
                { "value": "archived" }
            ] }
        }));

        assert!(reordered.same_contract(&previous));
        assert!(reordered.narrowing_reasons_from(&previous).is_empty());
        assert!(
            !partial_replacement
                .narrowing_reasons_from(&previous)
                .is_empty()
        );
    }

    #[test]
    fn refactoring_equivalent_conjunctive_bounds_does_not_change_contract() {
        let previous = effective_property(serde_json::json!({
            "id": "price",
            "type": { "kind": "decimal", "scale": 2 },
            "constraints": { "min": "1.50", "max": "9.00" }
        }));
        let refactored = effective_property(serde_json::json!({
            "id": "price",
            "type": { "kind": "decimal", "scale": 2 },
            "constraints": { "min": "0", "max": "10" }
        }));
        let with_equivalent_bound = effective_property(serde_json::json!({
            "id": "price",
            "type": { "kind": "decimal", "scale": 2 },
            "constraints": { "min": "1.5", "max": "9" }
        }));

        assert!(!refactored.same_contract(&previous));
        assert!(with_equivalent_bound.same_contract(&previous));
        assert!(
            with_equivalent_bound
                .narrowing_reasons_from(&previous)
                .is_empty()
        );
    }

    #[test]
    fn v1_roots_resolve_enum_and_requiredness_through_catalog() {
        let payload: JsonValue = serde_json::from_value(serde_json::json!({
            "objectKind": "commerce.LegacyOrder",
            "fields": {
                "status": { "type": "enum", "values": ["open", "closed"], "required": true },
                "note": { "type": "text", "required": false }
            }
        }))
        .expect("v1 payload JSON");
        let artifact = ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_owned(),
            artifact_id: "commerce.entity.legacy-order".to_owned(),
            namespace: "commerce".to_owned(),
            name: "LegacyOrder".to_owned(),
            version: "1.0.0".to_owned(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: ArtifactProvenance {
                source: ProvenanceSource::System,
                source_path: None,
                actor_id: None,
                created_at: None,
            },
            payload: ArtifactPayload::from_kind_json(&norn_ir::ArtifactKind::Entity, payload)
                .expect("entity payload object"),
        };
        let catalog = EffectiveCatalog::from_artifacts(&[artifact]);
        let object_type = catalog
            .object_type_named("commerce.LegacyOrder")
            .expect("v1 object type indexed")
            .expect("v1 object type available");
        let status = object_type.property("status").expect("enum field");
        let note = object_type.property("note").expect("optional field");

        assert!(!status.nullable);
        assert!(note.nullable);
        assert_eq!(
            status.check_runtime_value(&RuntimeValue::String("open".to_owned())),
            Ok(())
        );
        assert!(
            status
                .check_runtime_value(&RuntimeValue::String("other".to_owned()))
                .is_err()
        );
        assert!(status.check_runtime_value(&RuntimeValue::Null).is_err());
        assert_eq!(note.check_runtime_value(&RuntimeValue::Null), Ok(()));
    }
}
