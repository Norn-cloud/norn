//! The versioned ZEN Expression type environment (T1-EXPR-2, #2519), derived
//! from an `EntityIR` v2 artifact set, with the closed `actor` operand (#2419).
//!
//! A [`ZenTypeEnv`] is declarative data: every object type and interface of
//! the model as a [`RecordType`] (properties as typed fields, links as typed
//! references with their declared cardinality, inverse roles on the target),
//! the fixed [`actor`] record, and the variables an expression
//! site may read (`actor`, and `self`/`input` once bound), plus any explicit
//! site-specific operands. It is versioned
//! ([`TYPE_ENV_ID`]/[`TYPE_ENV_VERSION`]) and content-addressed with the Norn1
//! canonical JSON hash profile, so a checker receipt pins the exact
//! environment an expression was admitted under ([`TypeEnvRef`]), like the
//! resource-limit profile (`norn_ir::LimitProfileRef`).
//!
//! Derivation is fail-closed: an unresolved name, an inheritance or `ref`
//! cycle, or a member collision is an [`EnvError`], never a silently open
//! field. The input is expected to have passed `norn-checker`'s entity-model
//! check; these errors guard the environment, they do not replace that check.
//!
//! Mapping (see `crates/norn-expr/README.md`):
//!
//! | `EntityIR` v2 | [`Type`] |
//! | --- | --- |
//! | `text {format}` | [`Type::Text`] |
//! | `integer` | [`Type::Integer`] |
//! | `decimal {scale, unit}` | [`Type::Decimal`] |
//! | `boolean` | [`Type::Bool`] |
//! | `timestamp {timezone}` | [`Type::Timestamp`] |
//! | `date` / `duration` | [`Type::Date`] / [`Type::Duration`] |
//! | `enum {values}` | [`Type::Enum`] (a literal union) |
//! | `json-ext` | [`Type::Json`] (opaque: not readable by expressions) |
//! | `ref {name}` | the referenced shared type, resolved |
//! | `nullable: true` (anywhere on the `ref` chain) | [`Type::Optional`] |
//! | link, `to: one` | [`Type::Record`], [`Type::Optional`] unless `required` |
//! | link, `to: many` | [`Type::List`] of [`Type::Record`] |
//! | inverse role | as a link, with multiplicity `cardinality.from` (never required) |
//!
//! Link properties are not part of `norn.expr.env/1`: a traversal yields the
//! target record only.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_ir::canonical::{CanonicalJsonError, HashProfile, JsonValue};
use norn_ir::{
    InterfaceDef, InterfaceName, Link, LinkCardinality, Multiplicity, ObjectTypeDef,
    ObjectTypeExtension, ObjectTypeName, Property, PropertyType, PropertyTypeDef, PropertyTypeName,
    ResourceLimitProfile, TextFormat, TimezonePolicy, UnitRef, UnitSystem,
};

use crate::actor;

/// Identifier of the type-environment format.
pub const TYPE_ENV_ID: &str = "norn.expr.env";

/// Version of the type-environment format. Any change to how an environment
/// is derived or serialized is a new version.
pub const TYPE_ENV_VERSION: u32 = 1;

/// The hash profile the environment content hash is computed under.
pub const TYPE_ENV_HASH_PROFILE: HashProfile = HashProfile::Norn1;

/// The variable an expression site reads its own object through.
pub const SELF_VARIABLE: &str = "self";
/// The variable an action site reads its declared input through.
pub const INPUT_VARIABLE: &str = "input";
/// The closed actor operand (#2419), always present.
pub const ACTOR_VARIABLE: &str = "actor";

/// Record name of a bound `input` (outside the object type name grammar, so
/// it never collides with a model type).
pub const INPUT_RECORD: &str = "norn:Input";

/// A declared value type in the environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Type {
    /// `true` or `false`.
    Bool,
    /// A string, optionally in a well-known format.
    Text {
        /// The declared format, if any.
        format: Option<TextFormat>,
    },
    /// A signed 64-bit integer.
    Integer,
    /// An exact decimal held at `scale` fraction digits.
    Decimal {
        /// Fraction digits.
        scale: u64,
        /// The unit of a quantity, if declared.
        unit: Option<UnitRef>,
    },
    /// Any exact decimal number (no declared scale).
    Number,
    /// An RFC 3339 instant with an explicit offset (a string on the wire).
    Timestamp {
        /// How the offset is held.
        timezone: TimezonePolicy,
    },
    /// An ISO 8601 calendar date (a string on the wire).
    Date,
    /// An exact elapsed duration (a string on the wire).
    Duration,
    /// One of a closed set of strings (a literal union). Sorted, unique.
    Enum {
        /// The admitted values.
        values: Vec<String>,
    },
    /// Opaque JSON. Present in the environment but not readable by an
    /// expression in `norn.expr.env/1`.
    Json,
    /// A record of the environment, by name.
    Record(String),
    /// Zero or more values.
    List(Box<Type>),
    /// A value or `null`.
    Optional(Box<Type>),
}

impl Type {
    /// A `text` type without a format.
    #[must_use]
    pub const fn text() -> Self {
        Self::Text { format: None }
    }

    /// An enum type over `values` (sorted and de-duplicated).
    #[must_use]
    pub fn enumeration<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let values: BTreeSet<String> = values.into_iter().map(Into::into).collect();
        Self::Enum {
            values: values.into_iter().collect(),
        }
    }

    /// `Optional(self)`, unless already optional.
    #[must_use]
    pub fn optional(self) -> Self {
        match self {
            Self::Optional(_) => self,
            other @ (Self::Bool
            | Self::Text { .. }
            | Self::Integer
            | Self::Decimal { .. }
            | Self::Number
            | Self::Timestamp { .. }
            | Self::Date
            | Self::Duration
            | Self::Enum { .. }
            | Self::Json
            | Self::Record(_)
            | Self::List(_)) => Self::Optional(Box::new(other)),
        }
    }

    /// `List(self)`.
    #[must_use]
    pub fn list(self) -> Self {
        Self::List(Box::new(self))
    }

    fn to_canonical_value(&self) -> JsonValue {
        match self {
            Self::Bool => kind("bool", []),
            Self::Text { format } => match format {
                None => kind("text", []),
                Some(format) => kind("text", [("format", string(text_format(*format)))]),
            },
            Self::Integer => kind("integer", []),
            Self::Decimal { scale, unit } => {
                let scale = ("scale", number(*scale));
                match unit {
                    None => kind("decimal", [scale]),
                    Some(unit) => kind(
                        "decimal",
                        [
                            scale,
                            (
                                "unit",
                                object([
                                    ("system", string(unit_system(unit.system))),
                                    ("code", string(&unit.code)),
                                ]),
                            ),
                        ],
                    ),
                }
            }
            Self::Number => kind("number", []),
            Self::Timestamp { timezone } => {
                kind("timestamp", [("timezone", string(tz_policy(*timezone)))])
            }
            Self::Date => kind("date", []),
            Self::Duration => kind("duration", []),
            Self::Enum { values } => kind(
                "enum",
                [(
                    "values",
                    JsonValue::Array(values.iter().map(|v| string(v)).collect()),
                )],
            ),
            Self::Json => kind("json", []),
            Self::Record(name) => kind("record", [("name", string(name))]),
            Self::List(item) => kind("list", [("item", item.to_canonical_value())]),
            Self::Optional(item) => kind("optional", [("item", item.to_canonical_value())]),
        }
    }

    /// Record names this type refers to.
    fn records(&self, out: &mut Vec<String>) {
        match self {
            Self::Record(name) => out.push(name.clone()),
            Self::List(item) | Self::Optional(item) => item.records(out),
            Self::Bool
            | Self::Text { .. }
            | Self::Integer
            | Self::Decimal { .. }
            | Self::Number
            | Self::Timestamp { .. }
            | Self::Date
            | Self::Duration
            | Self::Enum { .. }
            | Self::Json => {}
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("bool"),
            Self::Text { format: None } => f.write_str("text"),
            Self::Text {
                format: Some(format),
            } => write!(f, "text<{}>", text_format(*format)),
            Self::Integer => f.write_str("integer"),
            Self::Decimal { scale, .. } => write!(f, "decimal<{scale}>"),
            Self::Number => f.write_str("number"),
            Self::Timestamp { .. } => f.write_str("timestamp"),
            Self::Date => f.write_str("date"),
            Self::Duration => f.write_str("duration"),
            Self::Enum { values } => write!(f, "enum{values:?}"),
            Self::Json => f.write_str("json"),
            Self::Record(name) => f.write_str(name),
            Self::List(item) => write!(f, "list<{item}>"),
            Self::Optional(item) => write!(f, "{item}?"),
        }
    }
}

/// Where a record field comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FieldSource {
    /// A declared (or inherited, or extension) property.
    Property {
        /// Whether the property is computed (derived, read-only).
        read_only: bool,
    },
    /// A link declared on this record's type (or a supertype, or an
    /// extension of one).
    Link {
        /// Whether every object links to at least one target.
        required: bool,
    },
    /// The inverse role of a link declared on another type.
    Inverse {
        /// The declaring type and link id, `Type.link`.
        link: String,
    },
    /// A member of a built-in record (the actor operand, a bound input).
    Builtin,
}

/// One field of a [`RecordType`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field {
    /// The field's type (already [`Type::Optional`] when nullable).
    pub ty: Type,
    /// Where the field comes from.
    pub source: FieldSource,
}

/// What a record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecordKind {
    /// An object type (with its inherited and extension members).
    Object,
    /// An interface (its required members).
    Interface,
    /// A record Norn defines (the actor operand, a bound input).
    Builtin,
}

/// A closed record: its complete, known set of fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordType {
    /// What the record describes.
    pub kind: RecordKind,
    /// Every field, by name. No other member exists.
    pub fields: BTreeMap<String, Field>,
}

/// The `EntityIR` v2 definitions an environment is derived from: the checked
/// artifact set, base layer included (decoded by `norn-checker`).
#[derive(Debug, Clone, Copy, Default)]
pub struct EntityDefinitions<'a> {
    /// Every shared property type a `ref` may name, registry types included.
    pub property_types: &'a [PropertyTypeDef],
    /// Interface definitions.
    pub interfaces: &'a [InterfaceDef],
    /// Object type definitions.
    pub object_types: &'a [ObjectTypeDef],
    /// Extensions of object types.
    pub extensions: &'a [ObjectTypeExtension],
}

/// Derivation options, part of the environment (and its hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnvOptions {
    /// How many links an expression may traverse from a variable
    /// (`self.customer.orders` is two). Beyond it a link is not a member.
    /// Taken from the resource-limit profile's slice relation depth, since a
    /// declared slice never loads deeper.
    pub max_link_depth: u32,
}

impl EnvOptions {
    /// The options a resource-limit profile implies.
    #[must_use]
    pub const fn from_limit_profile(profile: &ResourceLimitProfile) -> Self {
        Self {
            max_link_depth: profile.slice.max_relation_depth,
        }
    }
}

impl Default for EnvOptions {
    /// `norn.limits/1`'s relation depth.
    fn default() -> Self {
        Self::from_limit_profile(&ResourceLimitProfile::norn_limits_v1())
    }
}

/// Why an environment could not be derived or bound.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvError {
    /// Two definitions (object types, interfaces, or property types) share a
    /// name.
    DuplicateName {
        /// The name.
        name: String,
    },
    /// A name resolves to nothing of the required kind.
    Unresolved {
        /// What names it (`retail.Sku.extends`, `retail.Sku.price`).
        site: String,
        /// The unresolved name.
        name: String,
    },
    /// An `extends` or `ref` chain returns to its start.
    Cycle {
        /// A name on the cycle.
        name: String,
    },
    /// Structured property expansion exceeds the admitted depth or node budget.
    RecordExpansionLimit {
        /// The property being expanded.
        site: String,
    },
    /// Two members of one record share a name.
    MemberCollision {
        /// The record.
        record: String,
        /// The member.
        member: String,
    },
    /// A variable is already bound.
    AlreadyBound {
        /// The variable.
        variable: String,
    },
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => write!(f, "duplicate definition name `{name}`"),
            Self::Unresolved { site, name } => write!(f, "`{site}` names unknown `{name}`"),
            Self::Cycle { name } => write!(f, "`{name}` is on an extends/ref cycle"),
            Self::MemberCollision { record, member } => {
                write!(f, "record `{record}` has two members named `{member}`")
            }
            Self::AlreadyBound { variable } => write!(f, "variable `{variable}` is already bound"),
            Self::RecordExpansionLimit { site } => write!(f, "record expansion limit at `{site}`"),
        }
    }
}

impl std::error::Error for EnvError {}

/// The identity of a type environment, as recorded in checker receipts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeEnvRef {
    /// [`TYPE_ENV_ID`].
    pub id: String,
    /// [`TYPE_ENV_VERSION`].
    pub version: u32,
    /// Lower-case hex Norn1 content hash of the environment.
    pub sha256: String,
}

/// A closed, versioned ZEN Expression type environment. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZenTypeEnv {
    options: EnvOptions,
    records: BTreeMap<String, RecordType>,
    variables: BTreeMap<String, Type>,
}

impl ZenTypeEnv {
    /// Derives the environment of an `EntityIR` v2 artifact set. Only `actor` is
    /// bound; bind `self` and `input` per expression site.
    ///
    /// # Errors
    ///
    /// Every [`EnvError`] found (the derivation fails closed).
    pub fn from_artifact_set(
        defs: &EntityDefinitions<'_>,
        options: EnvOptions,
    ) -> Result<Self, Vec<EnvError>> {
        Self::from_artifact_set_with_registry(defs, &BTreeSet::new(), options)
    }

    /// Derives the environment of an `EntityIR` v2 artifact set with registry
    /// value-type names supplied by the caller. Registry types have no
    /// catalog body here, so fields referring to them become opaque JSON: the
    /// rest of the model can still be checked, while expressions cannot use
    /// the unknown registry type as a known scalar.
    ///
    /// # Errors
    ///
    /// Every [`EnvError`] found (the derivation fails closed).
    pub fn from_artifact_set_with_registry(
        defs: &EntityDefinitions<'_>,
        registry_types: &BTreeSet<PropertyTypeName>,
        options: EnvOptions,
    ) -> Result<Self, Vec<EnvError>> {
        Self::derive_artifact_set(defs, registry_types, false, options)
    }

    /// Derives an expression-checking environment from a partially checked
    /// artifact set. Registry and unresolved property-type refs are opaque
    /// JSON fields, allowing unrelated unresolved refs to be diagnosed by
    /// `norn-checker` without preventing checks over known fields. The
    /// checker must still reject the unresolved refs through its own model
    /// diagnostics.
    ///
    /// # Errors
    ///
    /// Every remaining structural [`EnvError`] found (cycles, member
    /// collisions, and duplicate names still fail closed). Unknown links are
    /// left as opaque records for the model checker to diagnose.
    pub fn from_artifact_set_for_checking(
        defs: &EntityDefinitions<'_>,
        registry_types: &BTreeSet<PropertyTypeName>,
        options: EnvOptions,
    ) -> Result<Self, Vec<EnvError>> {
        Self::derive_artifact_set(defs, registry_types, true, options)
    }

    fn derive_artifact_set(
        defs: &EntityDefinitions<'_>,
        registry_types: &BTreeSet<PropertyTypeName>,
        opaque_unresolved_refs: bool,
        options: EnvOptions,
    ) -> Result<Self, Vec<EnvError>> {
        let mut errors = Vec::new();
        let records =
            Derivation::new(defs, registry_types, opaque_unresolved_refs, &mut errors).run();
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut env = Self {
            options,
            records,
            variables: BTreeMap::new(),
        };
        for (name, record) in actor::records() {
            if env.records.insert(name.clone(), record).is_some() {
                errors.push(EnvError::DuplicateName { name });
            }
        }
        env.variables.insert(
            ACTOR_VARIABLE.to_owned(),
            Type::Record(actor::ACTOR_RECORD.to_owned()),
        );
        if errors.is_empty() {
            Ok(env)
        } else {
            Err(errors)
        }
    }

    /// An environment that binds only `input`, to a closed record with
    /// `fields` (record [`INPUT_RECORD`]): no `actor`, no `self`, and no
    /// model record. This is the source-only environment of `MappingIR`
    /// transforms, which read the source record and nothing else (no actor,
    /// clock, slice, or IO; T8-MAP-1a).
    ///
    /// # Errors
    ///
    /// [`EnvError::Unresolved`] if a field type names a record (the
    /// environment has none).
    pub fn input_only(
        fields: BTreeMap<String, Type>,
        options: EnvOptions,
    ) -> Result<Self, EnvError> {
        Self {
            options,
            records: BTreeMap::new(),
            variables: BTreeMap::new(),
        }
        .bind_input(fields)
    }

    /// An actor-free environment that binds only `input`, with explicitly
    /// registered closed records available to its fields. This is useful for
    /// typed authoring contracts whose inputs are records supplied by a
    /// checker-approved source, rather than `EntityIR` `self` or ambient actor
    /// data.
    ///
    /// Record references in every record and input field must name one of the
    /// supplied records. Record names are unique and may not shadow
    /// [`INPUT_RECORD`].
    ///
    /// # Errors
    ///
    /// [`EnvError::DuplicateName`] for a duplicate or reserved name;
    /// [`EnvError::Unresolved`] for an unknown record reference.
    pub fn input_only_with_records(
        records: BTreeMap<String, BTreeMap<String, Type>>,
        fields: BTreeMap<String, Type>,
        options: EnvOptions,
    ) -> Result<Self, EnvError> {
        if records.contains_key(INPUT_RECORD) {
            return Err(EnvError::DuplicateName {
                name: INPUT_RECORD.to_owned(),
            });
        }
        let record_names: BTreeSet<String> = records.keys().cloned().collect();
        for (record, members) in &records {
            for (member, ty) in members {
                if let Some(name) = first_unknown_record(ty, &record_names) {
                    return Err(EnvError::Unresolved {
                        site: format!("{record}.{member}"),
                        name,
                    });
                }
            }
        }
        for (field, ty) in &fields {
            if let Some(name) = first_unknown_record(ty, &record_names) {
                return Err(EnvError::Unresolved {
                    site: format!("{INPUT_VARIABLE}.{field}"),
                    name,
                });
            }
        }

        let env = Self {
            options,
            records: records
                .into_iter()
                .map(|(name, members)| {
                    (
                        name,
                        RecordType {
                            kind: RecordKind::Builtin,
                            fields: members
                                .into_iter()
                                .map(|(field, ty)| {
                                    (
                                        field,
                                        Field {
                                            ty,
                                            source: FieldSource::Builtin,
                                        },
                                    )
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
            variables: BTreeMap::new(),
        };
        env.bind_input(fields)
    }

    /// Binds `self` to the object type `name`.
    ///
    /// # Errors
    ///
    /// [`EnvError::Unresolved`] if `name` is not an object type of the
    /// environment; [`EnvError::AlreadyBound`] if `self` is bound.
    pub fn bind_self(mut self, name: &ObjectTypeName) -> Result<Self, EnvError> {
        match self.records.get(name.as_str()) {
            Some(record) if record.kind == RecordKind::Object => {}
            _ => {
                return Err(EnvError::Unresolved {
                    site: SELF_VARIABLE.to_owned(),
                    name: name.as_str().to_owned(),
                });
            }
        }
        self.bind(SELF_VARIABLE, Type::Record(name.as_str().to_owned()))?;
        Ok(self)
    }

    /// Binds `self` to the stored fields of an object candidate.
    ///
    /// Computed properties are omitted because they are not present in a
    /// stored candidate. `excluded_fields` additionally hides known fields
    /// that this expression site has not materialized yet. The environment is
    /// consumed so the restricted record cannot change a shared base
    /// environment.
    ///
    /// # Errors
    ///
    /// [`EnvError::Unresolved`] if `name` is not an object type of the
    /// environment; [`EnvError::AlreadyBound`] if `self` is bound.
    pub fn bind_self_to_stored_fields(
        mut self,
        name: &ObjectTypeName,
        excluded_fields: &BTreeSet<String>,
    ) -> Result<Self, EnvError> {
        let Some(record) = self.records.get_mut(name.as_str()) else {
            return Err(EnvError::Unresolved {
                site: SELF_VARIABLE.to_owned(),
                name: name.as_str().to_owned(),
            });
        };
        if record.kind != RecordKind::Object {
            return Err(EnvError::Unresolved {
                site: SELF_VARIABLE.to_owned(),
                name: name.as_str().to_owned(),
            });
        }
        record.fields.retain(|field_name, field| {
            !excluded_fields.contains(field_name)
                && matches!(
                    field.source,
                    FieldSource::Property { read_only: false } | FieldSource::Builtin
                )
        });
        self.bind(SELF_VARIABLE, Type::Record(name.as_str().to_owned()))?;
        Ok(self)
    }

    /// Binds `self` to the required-member record of an interface.
    ///
    /// This is used for interface invariants: the expression may read only
    /// members every implementing object type promises to provide.
    ///
    /// # Errors
    ///
    /// [`EnvError::Unresolved`] if `name` is not an interface of the
    /// environment; [`EnvError::AlreadyBound`] if `self` is bound.
    pub fn bind_interface_self(mut self, name: &InterfaceName) -> Result<Self, EnvError> {
        match self.records.get(name.as_str()) {
            Some(record) if record.kind == RecordKind::Interface => {}
            _ => {
                return Err(EnvError::Unresolved {
                    site: SELF_VARIABLE.to_owned(),
                    name: name.as_str().to_owned(),
                });
            }
        }
        self.bind(SELF_VARIABLE, Type::Record(name.as_str().to_owned()))?;
        Ok(self)
    }

    /// Binds `input` to a closed record with `fields` (record
    /// [`INPUT_RECORD`]).
    ///
    /// # Errors
    ///
    /// [`EnvError::Unresolved`] if a field type names an unknown record;
    /// [`EnvError::AlreadyBound`] if `input` is bound.
    pub fn bind_input(mut self, fields: BTreeMap<String, Type>) -> Result<Self, EnvError> {
        for (field, ty) in &fields {
            let mut names = Vec::new();
            ty.records(&mut names);
            if let Some(name) = names.into_iter().find(|n| !self.records.contains_key(n)) {
                return Err(EnvError::Unresolved {
                    site: format!("{INPUT_VARIABLE}.{field}"),
                    name,
                });
            }
        }
        if self.variables.contains_key(INPUT_VARIABLE) {
            return Err(EnvError::AlreadyBound {
                variable: INPUT_VARIABLE.to_owned(),
            });
        }
        let record = RecordType {
            kind: RecordKind::Builtin,
            fields: fields
                .into_iter()
                .map(|(name, ty)| {
                    (
                        name,
                        Field {
                            ty,
                            source: FieldSource::Builtin,
                        },
                    )
                })
                .collect(),
        };
        self.records.insert(INPUT_RECORD.to_owned(), record);
        self.bind(INPUT_VARIABLE, Type::Record(INPUT_RECORD.to_owned()))?;
        Ok(self)
    }

    fn bind(&mut self, variable: &str, ty: Type) -> Result<(), EnvError> {
        if self.variables.contains_key(variable) {
            return Err(EnvError::AlreadyBound {
                variable: variable.to_owned(),
            });
        }
        self.variables.insert(variable.to_owned(), ty);
        Ok(())
    }

    /// Binds one explicitly declared, site-specific operand.
    ///
    /// Unlike `actor`, `self`, and `input`, an operand is never implicit:
    /// callers must supply its complete type in the environment for the
    /// expression site. Record references must resolve in this environment.
    ///
    /// # Errors
    ///
    /// [`EnvError::Unresolved`] if `ty` names an unknown record;
    /// [`EnvError::AlreadyBound`] if `variable` is already present.
    pub fn bind_operand(mut self, variable: &str, ty: Type) -> Result<Self, EnvError> {
        let mut names = Vec::new();
        ty.records(&mut names);
        if let Some(name) = names
            .into_iter()
            .find(|name| !self.records.contains_key(name))
        {
            return Err(EnvError::Unresolved {
                site: variable.to_owned(),
                name,
            });
        }
        self.bind(variable, ty)?;
        Ok(self)
    }

    /// The derivation options.
    #[must_use]
    pub const fn options(&self) -> EnvOptions {
        self.options
    }

    /// Every record, by name.
    #[must_use]
    pub const fn records(&self) -> &BTreeMap<String, RecordType> {
        &self.records
    }

    /// The record named `name`.
    #[must_use]
    pub fn record(&self, name: &str) -> Option<&RecordType> {
        self.records.get(name)
    }

    /// The bound variables and their types.
    #[must_use]
    pub const fn variables(&self) -> &BTreeMap<String, Type> {
        &self.variables
    }

    /// The environment as a Norn1 canonical JSON value.
    #[must_use]
    pub fn to_canonical_value(&self) -> JsonValue {
        let records = self
            .records
            .iter()
            .map(|(name, record)| (name.clone(), record_value(record)))
            .collect();
        let variables = self
            .variables
            .iter()
            .map(|(name, ty)| (name.clone(), ty.to_canonical_value()))
            .collect();
        object([
            ("id", string(TYPE_ENV_ID)),
            ("version", number(u64::from(TYPE_ENV_VERSION))),
            (
                "maxLinkDepth",
                number(u64::from(self.options.max_link_depth)),
            ),
            ("records", JsonValue::Object(records)),
            ("variables", JsonValue::Object(variables)),
        ])
    }

    /// Norn1 canonical JSON bytes of the environment.
    ///
    /// # Errors
    ///
    /// [`CanonicalJsonError`] if `norn-canonical` rejects the value (not
    /// expected: the value is strings, integers, and booleans).
    pub fn canonical_json(&self) -> Result<Vec<u8>, CanonicalJsonError> {
        norn_ir::canonical::serialize(&self.to_canonical_value())
    }

    /// Lower-case hex content hash under [`TYPE_ENV_HASH_PROFILE`].
    ///
    /// # Errors
    ///
    /// As [`ZenTypeEnv::canonical_json`].
    pub fn content_sha256(&self) -> Result<String, CanonicalJsonError> {
        Ok(TYPE_ENV_HASH_PROFILE.hash_canonical_bytes(&self.canonical_json()?))
    }

    /// The reference a checker receipt records.
    ///
    /// # Errors
    ///
    /// As [`ZenTypeEnv::canonical_json`].
    pub fn reference(&self) -> Result<TypeEnvRef, CanonicalJsonError> {
        Ok(TypeEnvRef {
            id: TYPE_ENV_ID.to_owned(),
            version: TYPE_ENV_VERSION,
            sha256: self.content_sha256()?,
        })
    }
}

fn first_unknown_record(ty: &Type, records: &BTreeSet<String>) -> Option<String> {
    let mut names = Vec::new();
    ty.records(&mut names);
    names.into_iter().find(|name| !records.contains(name))
}

fn record_value(record: &RecordType) -> JsonValue {
    let kind_name = match record.kind {
        RecordKind::Object => "object",
        RecordKind::Interface => "interface",
        RecordKind::Builtin => "builtin",
    };
    let fields = record
        .fields
        .iter()
        .map(|(name, field)| {
            let source = match &field.source {
                FieldSource::Property { read_only } => {
                    kind("property", [("readOnly", JsonValue::Bool(*read_only))])
                }
                FieldSource::Link { required } => {
                    kind("link", [("required", JsonValue::Bool(*required))])
                }
                FieldSource::Inverse { link } => kind("inverse", [("link", string(link))]),
                FieldSource::Builtin => kind("builtin", []),
            };
            (
                name.clone(),
                object([("type", field.ty.to_canonical_value()), ("source", source)]),
            )
        })
        .collect();
    object([
        ("kind", string(kind_name)),
        ("fields", JsonValue::Object(fields)),
    ])
}

// ---------------------------------------------------------------------------
// Derivation
// ---------------------------------------------------------------------------

/// One declared link, for inverse-role derivation.
struct DeclaredLink<'a> {
    source: &'a str,
    link: &'a Link,
}

struct Derivation<'d, 'e> {
    defs: &'d EntityDefinitions<'d>,
    registry_types: &'d BTreeSet<PropertyTypeName>,
    opaque_unresolved_refs: bool,
    errors: &'e mut Vec<EnvError>,
    catalog: BTreeMap<&'d str, &'d PropertyTypeDef>,
    objects: BTreeMap<&'d str, &'d ObjectTypeDef>,
    interfaces: BTreeMap<&'d str, &'d InterfaceDef>,
    value_records: BTreeMap<String, RecordType>,
    intersection_members: BTreeMap<String, BTreeSet<String>>,
    /// Interface property types by declaring site: each ancestor's structured
    /// property is resolved once, not again for every descendant.
    interface_property_types: BTreeMap<String, (Type, bool)>,
    record_depth: usize,
    record_nodes_remaining: usize,
}

impl<'d, 'e> Derivation<'d, 'e> {
    fn new(
        defs: &'d EntityDefinitions<'d>,
        registry_types: &'d BTreeSet<PropertyTypeName>,
        opaque_unresolved_refs: bool,
        errors: &'e mut Vec<EnvError>,
    ) -> Self {
        let mut catalog = BTreeMap::new();
        for def in defs.property_types {
            if catalog.insert(def.name.as_str(), def).is_some() {
                errors.push(EnvError::DuplicateName {
                    name: def.name.as_str().to_owned(),
                });
            }
        }
        let mut objects = BTreeMap::new();
        for def in defs.object_types {
            if objects.insert(def.name.as_str(), def).is_some() {
                errors.push(EnvError::DuplicateName {
                    name: def.name.as_str().to_owned(),
                });
            }
        }
        let mut interfaces = BTreeMap::new();
        for def in defs.interfaces {
            if interfaces.insert(def.name.as_str(), def).is_some()
                || objects.contains_key(def.name.as_str())
            {
                errors.push(EnvError::DuplicateName {
                    name: def.name.as_str().to_owned(),
                });
            }
        }
        Self {
            defs,
            registry_types,
            opaque_unresolved_refs,
            errors,
            catalog,
            objects,
            interfaces,
            value_records: BTreeMap::new(),
            intersection_members: BTreeMap::new(),
            interface_property_types: BTreeMap::new(),
            record_depth: 0,
            record_nodes_remaining: 4096,
        }
    }

    /// Each resolvable interface's lineage names, itself included.
    fn interface_ancestry(&self) -> BTreeMap<&'d str, BTreeSet<&'d str>> {
        self.interfaces
            .iter()
            .filter_map(|(name, interface)| {
                interface_lineage(interface, |parent| {
                    self.interfaces.get(parent.as_str()).copied()
                })
                .ok()
                .map(|lineage| (*name, lineage.iter().map(|def| def.name.as_str()).collect()))
            })
            .collect()
    }

    fn run(mut self) -> BTreeMap<String, RecordType> {
        let mut records = BTreeMap::new();
        for ext in self.defs.extensions {
            if !self.objects.contains_key(ext.target.as_str()) {
                self.errors.push(EnvError::Unresolved {
                    site: format!("{}.extension", ext.namespace.as_str()),
                    name: ext.target.as_str().to_owned(),
                });
            }
        }
        // Chains (supertype-first) of every object type, for inheritance,
        // subtype closure, and interface implementation.
        let mut chains: BTreeMap<&'d str, Vec<&'d ObjectTypeDef>> = BTreeMap::new();
        #[expect(
            clippy::needless_collect,
            reason = "Snapshot object keys before chain mutates self diagnostics; a live keys iterator prevents that mutable borrow"
        )]
        for name in self.objects.keys().copied().collect::<Vec<_>>() {
            if let Some(chain) = self.chain(name) {
                chains.insert(name, chain);
            }
        }

        let mut declared_links: Vec<DeclaredLink<'d>> = Vec::new();
        for (name, chain) in &chains {
            let record = self.object_record(name, chain, &mut declared_links);
            records.insert((*name).to_owned(), record);
        }
        let interfaces: Vec<(&'d str, &'d InterfaceDef)> = self
            .interfaces
            .iter()
            .map(|(name, def)| (*name, *def))
            .collect();
        for (name, def) in interfaces {
            let record = self.interface_record(def);
            records.insert(name.to_owned(), record);
        }

        // Computed once for the inverse-role holder search below, and only when
        // some link has an inverse.
        let ancestry = if declared_links
            .iter()
            .any(|declared| declared.link.inverse.is_some())
        {
            self.interface_ancestry()
        } else {
            BTreeMap::new()
        };
        let inherits = |interface: &str, target: &str| {
            ancestry
                .get(interface)
                .is_some_and(|names| names.contains(target))
        };

        // Inverse roles: on the target, every subtype of it, and (for an
        // interface target) every implementor and its subtypes.
        for declared in &declared_links {
            let Some(inverse) = &declared.link.inverse else {
                continue;
            };
            let target = declared.link.target.as_str();
            let ty = multiplicity_type(
                declared.link.cardinality.from,
                false,
                Type::Record(declared.source.to_owned()),
            );
            let field = Field {
                ty,
                source: FieldSource::Inverse {
                    link: format!("{}.{}", declared.source, declared.link.id.as_str()),
                },
            };
            let holders: Vec<&str> = std::iter::once(target)
                .chain(chains.iter().filter_map(|(name, chain)| {
                    let reaches = chain.iter().any(|def| {
                        def.name.as_str() == target
                            || def
                                .implements
                                .iter()
                                .any(|interface| inherits(interface.as_str(), target))
                    });
                    (reaches && *name != target).then_some(*name)
                }))
                .chain(
                    self.interfaces
                        .keys()
                        .filter(|name| **name != target && inherits(name, target))
                        .copied(),
                )
                .collect();
            for holder in holders {
                if let Some(record) = records.get_mut(holder) {
                    insert_field(
                        self.errors,
                        holder,
                        &mut record.fields,
                        inverse.as_str(),
                        field.clone(),
                    );
                }
            }
        }
        // An intersection target guarantees every member's contract, so it also carries the
        // inverse roles its members gained above.
        self.enrich_intersections(&records);
        records.extend(self.value_records);
        records
    }

    /// Give every intersection record the inverse roles of its members. Merging two members'
    /// inverses can create a further intersection, so repeat until none is pending.
    fn enrich_intersections(&mut self, records: &BTreeMap<String, RecordType>) {
        let mut enriched = BTreeSet::new();
        loop {
            let pending: Vec<(String, BTreeSet<String>)> = self
                .intersection_members
                .iter()
                .filter(|(name, _)| !enriched.contains(*name))
                .map(|(name, members)| (name.clone(), members.clone()))
                .collect();
            if pending.is_empty() {
                break;
            }
            for (name, members) in pending {
                enriched.insert(name.clone());
                self.enrich_intersection(records, &name, &members);
            }
        }
    }

    /// Add the inverse roles of `members` to the intersection record `name`.
    fn enrich_intersection(
        &mut self,
        records: &BTreeMap<String, RecordType>,
        name: &str,
        members: &BTreeSet<String>,
    ) {
        let inverses: Vec<(String, Field)> = members
            .iter()
            .filter_map(|member| records.get(member.as_str()))
            .flat_map(|record: &RecordType| record.fields.iter())
            .filter(|(_, field)| matches!(field.source, FieldSource::Inverse { .. }))
            .map(|(id, field)| (id.clone(), field.clone()))
            .collect();
        for (id, field) in inverses {
            let Some(record) = self.value_records.get_mut(name) else {
                continue;
            };
            let merged = match record.fields.remove(&id) {
                Some(previous) if previous != field => self.interface_field_meet(previous, field),
                Some(previous) => previous,
                None => field,
            };
            if let Some(record) = self.value_records.get_mut(name) {
                record.fields.insert(id, merged);
            }
        }
    }

    /// The `extends` chain of `name`, root supertype first. `None` (with an
    /// error) on an unresolved supertype or a cycle.
    fn chain(&mut self, name: &'d str) -> Option<Vec<&'d ObjectTypeDef>> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = name;
        loop {
            if !seen.insert(current) {
                self.errors.push(EnvError::Cycle {
                    name: name.to_owned(),
                });
                return None;
            }
            let Some(def) = self.objects.get(current).copied() else {
                let child = chain
                    .last()
                    .map_or(name, |def: &&ObjectTypeDef| def.name.as_str());
                self.errors.push(EnvError::Unresolved {
                    site: format!("{child}.extends"),
                    name: current.to_owned(),
                });
                return None;
            };
            chain.push(def);
            match &def.extends {
                Some(parent) => current = parent.as_str(),
                None => break,
            }
        }
        chain.reverse();
        Some(chain)
    }

    fn object_record(
        &mut self,
        name: &'d str,
        chain: &[&'d ObjectTypeDef],
        declared_links: &mut Vec<DeclaredLink<'d>>,
    ) -> RecordType {
        let mut fields: BTreeMap<String, Field> = BTreeMap::new();
        for def in chain {
            let owner = def.name.as_str();
            for property in &def.properties {
                let field = self.property_field(owner, property);
                // An override (same id) replaces the inherited declaration.
                fields.insert(property.id.as_str().to_owned(), field);
            }
            for link in &def.links {
                let overrides = fields.contains_key(link.id.as_str());
                let field = self.link_field(owner, link);
                fields.insert(link.id.as_str().to_owned(), field);
                if owner == name && !overrides {
                    declared_links.push(DeclaredLink {
                        source: owner,
                        link,
                    });
                }
            }
        }
        for ext in self.defs.extensions {
            let target = ext.target.as_str();
            if !chain.iter().any(|def| def.name.as_str() == target) {
                continue;
            }
            for property in &ext.properties {
                let field = self.property_field(target, property);
                insert_field(self.errors, name, &mut fields, property.id.as_str(), field);
            }
            for link in &ext.links {
                let field = self.link_field(target, link);
                insert_field(self.errors, name, &mut fields, link.id.as_str(), field);
                if target == name {
                    declared_links.push(DeclaredLink {
                        source: target,
                        link,
                    });
                }
            }
        }
        RecordType {
            kind: RecordKind::Object,
            fields,
        }
    }

    fn interface_record(&mut self, def: &'d InterfaceDef) -> RecordType {
        let owner = def.name.as_str();
        let mut fields = BTreeMap::new();
        let lineage =
            match interface_lineage(def, |name| self.interfaces.get(name.as_str()).copied()) {
                Ok(lineage) => lineage,
                Err(name) => {
                    // A name that resolves came back round: the lineage is a cycle.
                    let error = if self.interfaces.contains_key(name.as_str()) {
                        EnvError::Cycle { name: name.0 }
                    } else {
                        EnvError::Unresolved {
                            site: format!("{owner}.extends"),
                            name: name.0,
                        }
                    };
                    self.errors.push(error);
                    return RecordType {
                        kind: RecordKind::Interface,
                        fields,
                    };
                }
            };
        for definition in lineage {
            let mut own = BTreeMap::new();
            for property in &definition.properties {
                // The declaring interface names the site, so two parents' structured
                // properties with one id get distinct records and are met, not replaced.
                let site = format!("{}.{}", definition.name.as_str(), property.id.as_str());
                let cached = self.interface_property_types.get(&site).cloned();
                let (ty, nullable) = cached.unwrap_or_else(|| {
                    let resolved = self.resolve(&site, &property.property_type);
                    self.interface_property_types
                        .insert(site.clone(), resolved.clone());
                    resolved
                });
                let ty = nullable_type(ty, nullable || property.constraints.nullable);
                let field = Field {
                    ty,
                    source: FieldSource::Property { read_only: false },
                };
                insert_field(self.errors, owner, &mut own, property.id.as_str(), field);
            }
            for link in &definition.links {
                let ty = self.link_type(owner, link.id.as_str(), &link.target, link.cardinality);
                let field = Field {
                    ty,
                    source: FieldSource::Link {
                        required: link.cardinality.required,
                    },
                };
                insert_field(self.errors, owner, &mut own, link.id.as_str(), field);
            }
            for (id, field) in own {
                let field = match fields.remove(&id) {
                    Some(previous) => self.interface_field_meet(previous, field),
                    None => field,
                };
                fields.insert(id, field);
            }
        }
        RecordType {
            kind: RecordKind::Interface,
            fields,
        }
    }

    /// The read type of jointly required interface members. Admission checks
    /// compatibility separately; enum domains and nullability intersect.
    fn interface_type_meet(&mut self, first: Type, second: Type) -> Type {
        match (first, second) {
            (Type::Optional(first), Type::Optional(second)) => {
                self.interface_type_meet(*first, *second).optional()
            }
            // Narrowing a to-many link to an optional to-one link keeps the to-one side's
            // optionality: a list never guarantees that a single target is present.
            (first @ Type::List(_), Type::Optional(second)) => {
                self.interface_type_meet(first, *second).optional()
            }
            (Type::Optional(first), second @ Type::List(_)) => {
                self.interface_type_meet(*first, second).optional()
            }
            (Type::Optional(first), second) => self.interface_type_meet(*first, second),
            (first, Type::Optional(second)) => self.interface_type_meet(first, *second),
            (Type::Enum { values: first }, Type::Enum { values: second }) => {
                Type::enumeration(first.into_iter().filter(|value| second.contains(value)))
            }
            (first @ Type::Text { format: Some(_) }, Type::Text { format: None }) => first,
            (Type::List(first), Type::List(second)) => {
                self.interface_type_meet(*first, *second).list()
            }
            (Type::List(first), second @ Type::Record(_)) => {
                self.interface_type_meet(*first, second)
            }
            (first @ Type::Record(_), Type::List(second)) => {
                self.interface_type_meet(first, *second)
            }
            (Type::Record(first), Type::Record(second)) => {
                self.interface_record_meet(first, second)
            }
            (_, second) => second,
        }
    }

    fn interface_field_meet(&mut self, first: Field, mut second: Field) -> Field {
        second.ty = self.interface_type_meet(first.ty, second.ty);
        match (&first.source, &mut second.source) {
            (FieldSource::Link { required: first }, FieldSource::Link { required: second }) => {
                *second |= *first;
            }
            (
                FieldSource::Property { read_only: first },
                FieldSource::Property { read_only: second },
            ) => *second |= *first,
            _ => {}
        }
        second
    }

    fn record_narrows(&mut self, sub: &str, sup: &str) -> bool {
        if sub == sup {
            return true;
        }
        if let Some(members) = self.intersection_members.get(sup).cloned() {
            return members
                .iter()
                .all(|member| self.record_narrows(sub, member));
        }
        if let Some(members) = self.intersection_members.get(sub).cloned() {
            return members
                .iter()
                .any(|member| self.record_narrows(member, sup));
        }
        if let Some(definition) = self.objects.get(sub).copied() {
            return self.chain(definition.name.as_str()).is_some_and(|lineage| {
                lineage.iter().any(|owner| {
                    owner.name.as_str() == sup
                        || owner.implements.iter().any(|name| {
                            name.as_str() == sup
                                || self.interfaces.get(name.as_str()).is_some_and(|interface| {
                                    interface
                                        .contract_owners(|parent| {
                                            self.interfaces.get(parent.as_str()).copied()
                                        })
                                        .iter()
                                        .any(|parent| parent.name.as_str() == sup)
                                })
                        })
                })
            });
        }
        self.interfaces.get(sub).is_some_and(|definition| {
            definition
                .contract_owners(|name| self.interfaces.get(name.as_str()).copied())
                .iter()
                .any(|parent| parent.name.as_str() == sup)
        })
    }

    fn record_contract(&mut self, name: &str) -> Option<RecordType> {
        if let Some(record) = self.value_records.get(name) {
            return Some(record.clone());
        }
        if let Some(definition) = self.interfaces.get(name).copied() {
            return Some(self.interface_record(definition));
        }
        let definition = self.objects.get(name).copied()?;
        let chain = self.chain(definition.name.as_str())?;
        Some(self.object_record(definition.name.as_str(), &chain, &mut Vec::new()))
    }

    fn interface_record_meet(&mut self, first: String, second: String) -> Type {
        if self.record_narrows(&first, &second) {
            return Type::Record(first);
        }
        if self.record_narrows(&second, &first) {
            return Type::Record(second);
        }
        let mut members = self
            .intersection_members
            .get(&first)
            .cloned()
            .unwrap_or_else(|| BTreeSet::from([first.clone()]));
        members.extend(
            self.intersection_members
                .get(&second)
                .cloned()
                .unwrap_or_else(|| BTreeSet::from([second.clone()])),
        );
        let name = format!(
            "$intersection:{}",
            members
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("&")
        );
        if self.value_records.contains_key(&name) {
            return Type::Record(name);
        }
        if self.record_nodes_remaining == 0 {
            self.errors
                .push(EnvError::RecordExpansionLimit { site: name });
            return Type::Json;
        }
        self.record_nodes_remaining = self.record_nodes_remaining.saturating_sub(1);
        self.intersection_members
            .insert(name.clone(), members.clone());
        self.value_records.insert(
            name.clone(),
            RecordType {
                kind: RecordKind::Interface,
                fields: BTreeMap::new(),
            },
        );
        // Meet the flattened base members, never a partial intersection: a nested meet
        // can reach this record while it is still the empty placeholder.
        let mut fields: Option<BTreeMap<String, Field>> = None;
        for member in &members {
            let Some(record) = self.record_contract(member) else {
                // Opaque checking admits unresolved targets; never cache the failed placeholder.
                if !self.opaque_unresolved_refs {
                    self.errors.push(EnvError::Unresolved {
                        site: "interface intersection".to_owned(),
                        name: member.clone(),
                    });
                }
                self.value_records.remove(&name);
                self.intersection_members.remove(&name);
                return Type::Json;
            };
            fields = Some(match fields {
                None => record.fields,
                Some(mut met) => {
                    for (id, field) in record.fields {
                        let field = match met.remove(&id) {
                            Some(previous) => self.interface_field_meet(previous, field),
                            None => field,
                        };
                        met.insert(id, field);
                    }
                    met
                }
            });
        }
        let fields = fields.unwrap_or_default();
        self.value_records.insert(
            name.clone(),
            RecordType {
                kind: RecordKind::Interface,
                fields,
            },
        );
        Type::Record(name)
    }

    fn property_field(&mut self, owner: &str, property: &Property) -> Field {
        let site = format!("{owner}.{}", property.id.as_str());
        let (ty, nullable) = self.resolve(&site, &property.property_type);
        Field {
            ty: nullable_type(ty, nullable || property.constraints.nullable),
            source: FieldSource::Property {
                read_only: property.is_read_only(),
            },
        }
    }

    fn link_field(&mut self, owner: &str, link: &Link) -> Field {
        Field {
            ty: self.link_type(owner, link.id.as_str(), &link.target, link.cardinality),
            source: FieldSource::Link {
                required: link.cardinality.required,
            },
        }
    }

    fn link_type(
        &mut self,
        owner: &str,
        id: &str,
        target: &ObjectTypeName,
        cardinality: LinkCardinality,
    ) -> Type {
        let target = target.as_str();
        if !self.opaque_unresolved_refs
            && !self.objects.contains_key(target)
            && !self.interfaces.contains_key(target)
        {
            self.errors.push(EnvError::Unresolved {
                site: format!("{owner}.{id}"),
                name: target.to_owned(),
            });
        }
        multiplicity_type(
            cardinality.to,
            cardinality.required,
            Type::Record(target.to_owned()),
        )
    }

    /// Resolves a property type through its `ref` chain: the type and whether
    /// any definition on the chain is nullable.
    fn resolve(&mut self, site: &str, property_type: &PropertyType) -> (Type, bool) {
        let mut current = property_type;
        let mut nullable = false;
        let mut seen = BTreeSet::new();
        loop {
            let ty = match current {
                PropertyType::Text { format } => Type::Text { format: *format },
                PropertyType::Integer {} => Type::Integer,
                PropertyType::Decimal { scale, unit, .. } => Type::Decimal {
                    scale: scale.0,
                    unit: unit.clone(),
                },
                PropertyType::Boolean {} => Type::Bool,
                PropertyType::Timestamp { timezone } => Type::Timestamp {
                    timezone: *timezone,
                },
                PropertyType::Date {} => Type::Date,
                PropertyType::Duration {} => Type::Duration,
                PropertyType::Enum { values } => {
                    Type::enumeration(values.iter().map(|v| v.value.clone()))
                }
                PropertyType::Record { members } => {
                    if self.record_depth >= 64 || self.record_nodes_remaining == 0 {
                        self.errors.push(EnvError::RecordExpansionLimit {
                            site: site.to_owned(),
                        });
                        return (Type::Json, nullable);
                    }
                    self.record_depth = self.record_depth.saturating_add(1);
                    self.record_nodes_remaining = self.record_nodes_remaining.saturating_sub(1);
                    let name = format!("$record:{site}");
                    let mut fields = BTreeMap::new();
                    for member in members {
                        let (ty, inherited_nullable) = self.resolve(
                            &format!("{site}.{}", member.id.as_str()),
                            &member.property_type,
                        );
                        let field = Field {
                            ty: nullable_type(
                                ty,
                                member.constraints.nullable || inherited_nullable,
                            ),
                            source: FieldSource::Property { read_only: false },
                        };
                        insert_field(self.errors, &name, &mut fields, member.id.as_str(), field);
                    }
                    self.record_depth = self.record_depth.saturating_sub(1);
                    self.value_records.insert(
                        name.clone(),
                        RecordType {
                            kind: RecordKind::Builtin,
                            fields,
                        },
                    );
                    Type::Record(name)
                }
                PropertyType::JsonExt { .. } => Type::Json,
                PropertyType::Ref { name } => {
                    let name = name.as_str();
                    if !seen.insert(name) {
                        self.errors.push(EnvError::Cycle {
                            name: name.to_owned(),
                        });
                        return (Type::Json, nullable);
                    }
                    let Some(def) = self.catalog.get(name).copied() else {
                        if self.opaque_unresolved_refs
                            || self
                                .registry_types
                                .iter()
                                .any(|registry_name| registry_name.as_str() == name)
                        {
                            return (Type::Json, nullable);
                        }
                        self.errors.push(EnvError::Unresolved {
                            site: site.to_owned(),
                            name: name.to_owned(),
                        });
                        return (Type::Json, nullable);
                    };
                    nullable |= def.constraints.nullable;
                    current = &def.property_type;
                    continue;
                }
            };
            return (ty, nullable);
        }
    }
}

fn insert_field(
    errors: &mut Vec<EnvError>,
    record: &str,
    fields: &mut BTreeMap<String, Field>,
    member: &str,
    field: Field,
) {
    if fields.insert(member.to_owned(), field).is_some() {
        errors.push(EnvError::MemberCollision {
            record: record.to_owned(),
            member: member.to_owned(),
        });
    }
}

fn nullable_type(ty: Type, nullable: bool) -> Type {
    if nullable { ty.optional() } else { ty }
}

fn multiplicity_type(multiplicity: Multiplicity, required: bool, target: Type) -> Type {
    match multiplicity {
        Multiplicity::One if required => target,
        Multiplicity::One => target.optional(),
        Multiplicity::Many => target.list(),
    }
}

const fn text_format(format: TextFormat) -> &'static str {
    match format {
        TextFormat::Email => "email",
        TextFormat::Uri => "uri",
        TextFormat::Uuid => "uuid",
    }
}

const fn unit_system(system: UnitSystem) -> &'static str {
    match system {
        UnitSystem::Ucum => "ucum",
        UnitSystem::Qudt => "qudt",
    }
}

const fn tz_policy(policy: TimezonePolicy) -> &'static str {
    match policy {
        TimezonePolicy::Utc => "utc",
        TimezonePolicy::Offset => "offset",
    }
}

fn string(value: &str) -> JsonValue {
    JsonValue::String(value.to_owned())
}

fn number(value: u64) -> JsonValue {
    JsonValue::Number(value.to_string())
}

fn object<const N: usize>(entries: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn kind<const N: usize>(name: &str, entries: [(&str, JsonValue); N]) -> JsonValue {
    let mut map: BTreeMap<String, JsonValue> = entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    map.insert("kind".to_owned(), string(name));
    JsonValue::Object(map)
}

/// The complete contract lineage, parents first and this interface last.
/// Shared ancestors in a diamond occur once, in declared parent order.
///
/// # Errors
/// Returns the first missing or cyclic interface name. The checker supplies
/// admission diagnostics; consumers must not use a partial lineage.
pub fn interface_lineage<'a>(
    definition: &'a InterfaceDef,
    lookup: impl Fn(&InterfaceName) -> Option<&'a InterfaceDef>,
) -> Result<Vec<&'a InterfaceDef>, InterfaceName> {
    let mut pending = vec![(definition, false)];
    let mut active = std::collections::BTreeSet::new();
    let mut complete = std::collections::BTreeSet::new();
    let mut lineage = Vec::new();
    while let Some((definition, exiting)) = pending.pop() {
        if exiting {
            active.remove(&definition.name);
            complete.insert(&definition.name);
            lineage.push(definition);
        } else if !complete.contains(&definition.name) {
            if !active.insert(&definition.name) {
                return Err(definition.name.clone());
            }
            pending.push((definition, true));
            for parent in definition.extends.iter().rev() {
                pending.push((lookup(parent).ok_or_else(|| parent.clone())?, false));
            }
        }
    }
    Ok(lineage)
}
