//! Typed Entity and Relation roots (T1-IR-1b, #2507).
//!
//! An `entity` artifact payload has two forms, selected by the presence of
//! `entityVersion`:
//!
//! - **EntityIR v1** (no `entityVersion`): [`EntityRootV1`], the map-backed
//!   form every existing fixture and stored artifact uses (`objectKind` and a
//!   `fields` map of eight JSON-typed field kinds). Its wire bytes and hashes
//!   are unchanged: v1 payloads are never rewritten into v2, because lowering
//!   is not canonicalization. `norn_checker::entity_lowering` (T1-ONT-4,
//!   #2517) lowers a v1 root into the v2 structs for consumers.
//! - **EntityIR v2** (`entityVersion` [`ENTITY_IR_V2_VERSION`]): exactly one
//!   EntityIR v2 definition from [`crate::entity_v2`], in one of five
//!   definition forms selected by the member that carries it (#2844):
//!   - `objectType` ([`EntityRootV2`]): one [`ObjectTypeDef`] — properties,
//!     identity, inheritance, interfaces, links, and invariants;
//!   - `interface` ([`EntityInterfaceRootV2`]): one [`InterfaceDef`];
//!   - `objectTypeExtension` ([`EntityExtensionRootV2`]): one
//!     [`ObjectTypeExtension`], a layer's namespaced additions to a type it
//!     does not own;
//!   - `propertyTypes` ([`PropertyTypeCatalogRootV2`]): a catalog of shared
//!     [`PropertyTypeDef`]s.
//!   - `markings` ([`MarkingCatalogRootV2`]): tenant-declared markings and
//!     their masking policies.
//!
//!   The five forms are one artifact kind because they are one vocabulary:
//!   the whole-model check (`norn_checker::check_artifact_set_entity_model`)
//!   resolves supertypes, interfaces, link targets, `ref`s, and extension
//!   targets across all of them. A new artifact kind per definition would
//!   ripple through every closed kind match (envelope, BAC, lifecycle,
//!   generated SDKs) for no gain in expressiveness.
//!
//! A `relation` artifact is v1 only ([`RelationRoot`]). In v2 a relation is a
//! [`crate::Link`] declared once on its source object type
//! (`objectType.links`), with both-end cardinality and an optional inverse
//! role; a separate v2 relation artifact would be a second declaration of the
//! same link, so there is none. `norn_checker::entity_lowering` lowers a v1
//! relation into a link.
//!
//! The envelope's [`crate::EntityPayload`] and [`crate::RelationPayload`]
//! stay the lenient wire roots (unknown or malformed members become checker
//! diagnostics); `norn-checker` decodes them into these strict roots. Every
//! consistency rule is a `norn-checker` diagnostic, not a decode error.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::entity_v2::extension::ObjectTypeExtension;
use crate::entity_v2::interface::InterfaceDef;
use crate::entity_v2::object::ObjectTypeDef;
use crate::entity_v2::property::PropertyTypeDef;
use crate::marking::{MarkingId, MarkingIr};
use crate::members::present_non_null;
use crate::rule::JsonObject;

/// `entityVersion` of an EntityIR v2 `entity` payload. A v1 payload has no
/// `entityVersion`.
pub const ENTITY_IR_V2_VERSION: &str = "2.0";

/// Typed payload of an `entity` artifact: EntityIR v1, or one of the five
/// EntityIR v2 definition forms.
///
/// Untagged: the checker selects the form by the presence of `entityVersion`
/// (present → v2, absent → v1) and, in v2, by the one definition member
/// (`objectType`, `interface`, `objectTypeExtension`, `propertyTypes`, or
/// `markings`).
/// The forms are disjoint: each is closed and requires a member every other
/// form rejects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EntityRoot {
    /// v2: one object type.
    V2(EntityRootV2),
    /// v2: one interface.
    V2Interface(EntityInterfaceRootV2),
    /// v2: one extension of an object type another layer owns.
    V2Extension(EntityExtensionRootV2),
    /// v2: a catalog of shared property types.
    V2PropertyTypes(PropertyTypeCatalogRootV2),
    /// v2: a catalog of data markings.
    V2Markings(MarkingCatalogRootV2),
    /// v1: the map-backed form.
    V1(EntityRootV1),
}

impl EntityRoot {
    /// The object kind the artifact defines: v1 `objectKind`, or v2
    /// `objectType.name`. `None` for v2 forms that define no object type
    /// (interface, extension, property type, and marking catalogs).
    #[must_use]
    pub fn object_kind(&self) -> Option<&str> {
        match self {
            Self::V1(root) => Some(&root.object_kind),
            Self::V2(root) => Some(root.object_type.name.as_str()),
            Self::V2Interface(_)
            | Self::V2Extension(_)
            | Self::V2PropertyTypes(_)
            | Self::V2Markings(_) => None,
        }
    }

    /// The declared mutation authority, if any (only an object type has one).
    #[must_use]
    pub const fn mutation_authority(&self) -> Option<&EntityMutationAuthority> {
        match self {
            Self::V1(root) => root.mutation_authority.as_ref(),
            Self::V2(root) => root.mutation_authority.as_ref(),
            Self::V2Interface(_)
            | Self::V2Extension(_)
            | Self::V2PropertyTypes(_)
            | Self::V2Markings(_) => None,
        }
    }
}

/// The EntityIR v2 definition members of an `entity` payload; a v2 payload
/// carries exactly one of them.
pub const ENTITY_IR_V2_DEFINITION_MEMBERS: [&str; 5] = [
    "objectType",
    "interface",
    "objectTypeExtension",
    "propertyTypes",
    "markings",
];

/// The only `entityVersion` literal: EntityIR v2 ([`ENTITY_IR_V2_VERSION`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum EntityIrVersion {
    #[default]
    #[serde(rename = "2.0")]
    V2_0,
}

/// EntityIR v2 payload of an `entity` artifact: one object type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityRootV2 {
    /// The form selector; always [`ENTITY_IR_V2_VERSION`].
    pub entity_version: EntityIrVersion,
    /// The object type. Its `name` is the artifact's object kind (v1
    /// `objectKind`), and its `links` are the type's relations.
    pub object_type: ObjectTypeDef,
    /// Who may mutate objects of this type. Omitted: generic actions.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "EntityMutationAuthority")]
    pub mutation_authority: Option<EntityMutationAuthority>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// EntityIR v2 payload of an `entity` artifact that carries one interface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityInterfaceRootV2 {
    /// The form selector; always [`ENTITY_IR_V2_VERSION`].
    pub entity_version: EntityIrVersion,
    /// The interface.
    pub interface: InterfaceDef,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// EntityIR v2 payload of an `entity` artifact that carries one extension of
/// an object type another layer owns. (`extensions` stays the reserved
/// extension-slot map every payload root has.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityExtensionRootV2 {
    /// The form selector; always [`ENTITY_IR_V2_VERSION`].
    pub entity_version: EntityIrVersion,
    /// The extension.
    pub object_type_extension: ObjectTypeExtension,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// EntityIR v2 payload of an `entity` artifact that carries a catalog of
/// shared property types (non-empty; checker rule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyTypeCatalogRootV2 {
    /// The form selector; always [`ENTITY_IR_V2_VERSION`].
    pub entity_version: EntityIrVersion,
    /// The shared property types, in presentation order.
    pub property_types: Vec<PropertyTypeDef>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// EntityIR v2 payload of an `entity` artifact that carries data marking
/// definitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkingCatalogRootV2 {
    /// The form selector; always [`ENTITY_IR_V2_VERSION`].
    pub entity_version: EntityIrVersion,
    /// The marking definitions, in presentation order.
    pub markings: Vec<MarkingIr>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// EntityIR v1 payload of an `entity` artifact (the map-backed form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityRootV1 {
    /// The object kind: an exact identity without whitespace, control
    /// characters, or wildcards (checker rule).
    pub object_kind: String,
    /// Field name → field definition. Names are non-empty (checker rule).
    pub fields: BTreeMap<String, EntityFieldV1>,
    /// Classification markings inherited by every field. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// Who may mutate objects of this kind. Omitted: generic actions.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "EntityMutationAuthority")]
    pub mutation_authority: Option<EntityMutationAuthority>,
    /// Customization artifact ids whose `add_field` overlays the engine
    /// applied, in application order. Each is a non-empty string (checker
    /// rule); the list may be empty.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    pub overlay_sources: Option<Vec<String>>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// One EntityIR v1 field definition, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntityFieldV1 {
    Text {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    String {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    Integer {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    Number {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    Boolean {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    Object {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    Array {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
    },
    /// A closed set of string values: non-empty, unique, and each non-blank
    /// (checker rules).
    Enum {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "bool")]
        required: Option<bool>,
        /// Classification markings declared on this field. Omitted when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        markings: Vec<MarkingId>,
        values: Vec<String>,
    },
}

impl EntityFieldV1 {
    /// Whether the field is declared required (omitted means not required).
    #[must_use]
    pub const fn is_required(&self) -> bool {
        let (Self::Text { required, .. }
        | Self::String { required, .. }
        | Self::Integer { required, .. }
        | Self::Number { required, .. }
        | Self::Boolean { required, .. }
        | Self::Object { required, .. }
        | Self::Array { required, .. }
        | Self::Enum { required, .. }) = self;
        matches!(required, Some(true))
    }

    /// The field's declared classification markings.
    #[must_use]
    pub fn markings(&self) -> &[MarkingId] {
        let (Self::Text { markings, .. }
        | Self::String { markings, .. }
        | Self::Integer { markings, .. }
        | Self::Number { markings, .. }
        | Self::Boolean { markings, .. }
        | Self::Object { markings, .. }
        | Self::Array { markings, .. }
        | Self::Enum { markings, .. }) = self;
        markings
    }
}

/// Who may mutate objects of an entity type (the closed EntityIR authority
/// descriptor; the engine's final admission predicate is
/// [`crate::entity_mutation_authority_admits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntityMutationAuthority {
    /// Ordinary generic actions (the default when omitted).
    GenericAction {},
    /// Only commands of one hard-invariant domain engine.
    DomainEngine { domain: EntityAuthorityDomain },
}

/// A hard-invariant domain that may own an entity type's mutations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityAuthorityDomain {
    Inventory,
    Accounting,
    Compliance,
    Settlement,
    Tax,
}

/// Typed payload of a `relation` artifact (RelationIR v1).
///
/// v1 only: in EntityIR v2 a relation is a [`crate::Link`] on its source
/// object type (see the module docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationRoot {
    /// Source entity kind (`retail.Item`); non-empty (checker rule).
    pub from: String,
    /// Target entity kind; non-empty (checker rule).
    pub to: String,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "RelationCardinalityV1")]
    pub cardinality: Option<RelationCardinalityV1>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// RelationIR v1 cardinality, read from `from` to `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationCardinalityV1 {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}
