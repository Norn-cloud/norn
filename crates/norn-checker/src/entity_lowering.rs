//! `EntityIR` v1 → v2 lowering (T1-ONT-4, #2517).
//!
//! `EntityIR` v1 (`objectKind` plus a `fields` map, [`EntityRootV1`]) stays a
//! valid wire form during the migration to `EntityIR` v2, and its bytes and
//! hashes never change: lowering is a **read-side view**, not
//! canonicalization. Consumers that need an object type (the engine's entity
//! catalog, BAC, and object validation) read every `entity` artifact through
//! [`entity_object_type`], which returns the one internal representation, an
//! [`EntityRootV2`]: decoded directly for a v2 `objectType` payload, and
//! lowered by [`lower_entity_v1`] for a v1 payload.
//!
//! # Mapping
//!
//! | v1 | v2 |
//! | --- | --- |
//! | `objectKind` | `objectType.name` |
//! | `fields` (name → field) | `objectType.properties`, in field-name order; the id is the field name, verbatim |
//! | `required: true` | `constraints.nullable` omitted (`false`) |
//! | `required` omitted or `false` | `constraints.nullable: true` |
//! | `text`, `string` | `{ "kind": "text" }` |
//! | `integer` | `{ "kind": "integer" }` |
//! | `number` | `{ "kind": "json-ext", "schema": { "type": "number" } }` |
//! | `boolean` | `{ "kind": "boolean" }` |
//! | `object` | `{ "kind": "json-ext", "schema": { "type": "object" } }` |
//! | `array` | `{ "kind": "json-ext", "schema": { "type": "array" } }` |
//! | `enum` (`values`) | `{ "kind": "enum", "values": [{ "value": … }] }` |
//! | `mutationAuthority`, `extensions` | the same members of the v2 root |
//! | `overlaySources` | dropped (engine provenance of applied overlays, not part of the type) |
//!
//! **`number` is not a decimal.** `EntityIR` v2 has no binary floating-point
//! kind (OPEN-DECISIONS D37), and a `decimal` needs a declared scale. A v1
//! `number` admits any JSON number, so every finite scale would either reject
//! or round values v1 accepts, and lowering would change which stored objects
//! are valid. No existing fixture uses `number`, so there is no evidence for a
//! scale either; the scale is domain knowledge (cents, grams) that belongs to
//! the author. A v1 `number` therefore lowers to opaque JSON constrained by
//! the JSON Schema `{ "type": "number" }`, exactly the values v1 admits. A v2
//! artifact authored from it declares an exact `decimal` with a chosen scale.
//! `object` and `array` lower the same way, because they are not scalars.
//!
//! **Names are kept verbatim.** A v1 field name is the key of stored object
//! payloads, so the property id is the field name even where it does not
//! follow the v2 member id grammar (`skuId`). The lowered root is a consumer
//! view and is not re-checked against v2 authoring rules; a v2 artifact
//! authored from a v1 one chooses conforming ids, which is a data migration.
//!
//! # Relations
//!
//! A v1 `relation` artifact stays v1. [`lower_relation_v1`] maps it onto the
//! v2 [`Link`] it describes, declared on its `from` type: the link id is the
//! relation artifact's envelope `name` (v1 has no role name), the target is
//! `to`, there is no inverse role, and the cardinality maps as documented in
//! `docs/platform/entity-ir-v2.md` (#2845). An omitted v1 cardinality is
//! many-to-many, the engine's existing default and the only reading that
//! constrains nothing.
//!
//! # Deprecation
//!
//! [`entity_v1_deprecation`] is the warning BAC attaches to every change that
//! adds or modifies a v1 `entity` artifact: v1 is accepted during the
//! migration, and new versions should be authored in the v2 form.

use std::collections::BTreeMap;

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, DiagnosticRemediation, DiagnosticSeverity, EntityFieldV1,
    EntityIrVersion, EntityPayload, EntityRoot, EntityRootV1, EntityRootV2, EnumValue, JsonValue,
    Link, LinkCardinality, LinkId, Multiplicity, ObjectTypeDef, ObjectTypeName, Property,
    PropertyConstraints, PropertyId, PropertyType, RelationCardinalityV1, RelationPayload,
    RelationRoot,
};

/// The deprecation warning on a v1 `entity` artifact in a BAC plan.
pub const ENTITY_V1_DEPRECATED: &str = "entity.deprecation.v1_payload";

/// Lower an `EntityIR` v1 root into the `EntityIR` v2 object type root it
/// describes (see the module docs for the mapping). Pure and total: every
/// decodable v1 root has exactly one lowering.
#[must_use]
pub fn lower_entity_v1(v1: &EntityRootV1) -> EntityRootV2 {
    EntityRootV2 {
        entity_version: EntityIrVersion::V2_0,
        object_type: ObjectTypeDef {
            immutable: false,
            name: ObjectTypeName(v1.object_kind.clone()),
            description: None,
            status: norn_ir::ResourceStatus::default(),
            extends: None,
            implements: Vec::new(),
            markings: v1.markings.clone(),
            properties: v1
                .fields
                .iter()
                .map(|(name, field)| lower_field_v1(name, field))
                .collect(),
            links: Vec::new(),
            owned_collections: Vec::new(),
            identity: Vec::new(),
            invariants: Vec::new(),
            lifecycles: Vec::new(),
            accounting_leg: None,
        },
        mutation_authority: v1.mutation_authority,
        extensions: v1.extensions.clone(),
    }
}

/// Lower one v1 field into the v2 property with the same name.
#[must_use]
pub fn lower_field_v1(name: &str, field: &EntityFieldV1) -> Property {
    Property {
        protected: false,
        id: PropertyId(name.to_owned()),
        description: None,
        property_type: lower_field_type_v1(field),
        constraints: PropertyConstraints {
            nullable: !field.is_required(),
            ..PropertyConstraints::default()
        },
        default: None,
        markings: field.markings().to_vec(),
        computed: None,
    }
}

/// The v2 property type of a v1 field type.
#[must_use]
pub fn lower_field_type_v1(field: &EntityFieldV1) -> PropertyType {
    match field {
        EntityFieldV1::Text { .. } | EntityFieldV1::String { .. } => {
            PropertyType::Text { format: None }
        }
        EntityFieldV1::Integer { .. } => PropertyType::Integer {},
        EntityFieldV1::Number { .. } => json_of_type("number"),
        EntityFieldV1::Boolean { .. } => PropertyType::Boolean {},
        EntityFieldV1::Object { .. } => json_of_type("object"),
        EntityFieldV1::Array { .. } => json_of_type("array"),
        EntityFieldV1::Enum { values, .. } => PropertyType::Enum {
            values: values
                .iter()
                .map(|value| EnumValue {
                    value: value.clone(),
                    description: None,
                })
                .collect(),
        },
    }
}

/// `json-ext` constrained by the one-keyword JSON Schema `{ "type": <type> }`.
fn json_of_type(json_type: &str) -> PropertyType {
    PropertyType::JsonExt {
        schema: Some(BTreeMap::from([(
            "type".to_owned(),
            JsonValue::String(json_type.to_owned()),
        )])),
    }
}

/// The JSON Schema primitive type a lowered v1 `number`, `object`, or `array`
/// property constrains its values to: `Some("number")` for
/// `{ "kind": "json-ext", "schema": { "type": "number" } }`. `None` for every
/// other property type, including a `json-ext` whose schema is anything but
/// that one keyword.
#[must_use]
pub fn json_ext_primitive_type(property_type: &PropertyType) -> Option<&str> {
    let PropertyType::JsonExt {
        schema: Some(schema),
    } = property_type
    else {
        return None;
    };
    match (schema.len(), schema.get("type")) {
        (1, Some(JsonValue::String(json_type))) => Some(json_type.as_str()),
        _ => None,
    }
}

/// A v1 relation lowered onto the v2 link it describes.
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredRelation {
    /// The object type that declares the link (v1 `from`).
    pub source: ObjectTypeName,
    /// The link (target v1 `to`).
    pub link: Link,
}

/// Lower a v1 relation root onto a link on its `from` type. `name` is the
/// relation artifact's envelope `name`, which becomes the link id.
#[must_use]
pub fn lower_relation_v1(name: &str, relation: &RelationRoot) -> LoweredRelation {
    LoweredRelation {
        source: ObjectTypeName(relation.from.clone()),
        link: Link {
            id: LinkId(name.to_owned()),
            description: None,
            target: ObjectTypeName(relation.to.clone()),
            cardinality: lower_cardinality_v1(relation.cardinality),
            inverse: None,
            acyclic: false,
            properties: Vec::new(),
        },
    }
}

/// The v2 cardinality of a v1 relation cardinality, read from `from` to
/// `to` (#2845); omitted is many-to-many.
#[must_use]
pub const fn lower_cardinality_v1(cardinality: Option<RelationCardinalityV1>) -> LinkCardinality {
    let (to, from) = match cardinality {
        Some(RelationCardinalityV1::OneToOne) => (Multiplicity::One, Multiplicity::One),
        Some(RelationCardinalityV1::OneToMany) => (Multiplicity::Many, Multiplicity::One),
        Some(RelationCardinalityV1::ManyToOne) => (Multiplicity::One, Multiplicity::Many),
        Some(RelationCardinalityV1::ManyToMany) | None => (Multiplicity::Many, Multiplicity::Many),
    };
    LinkCardinality {
        to,
        from,
        required: false,
    }
}

/// The `EntityIR` form an `entity` payload was authored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityForm {
    /// `EntityIR` v1, lowered on read.
    V1,
    /// `EntityIR` v2.
    V2,
}

/// The object type an `entity` artifact defines, in the internal (v2) form.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityObjectType {
    /// The form the payload was authored in.
    pub form: EntityForm,
    /// The v2 object type root: decoded (v2) or lowered (v1).
    pub root: EntityRootV2,
}

/// Read an `entity` payload as the object type it defines: the v2
/// `objectType` root as decoded, or the v1 root lowered by
/// [`lower_entity_v1`]. `Ok(None)` for the v2 forms that define no object type
/// (an interface, an extension, or a property type catalog).
///
/// # Errors
///
/// The decode error, when the payload is neither a well-formed v1 root nor a
/// well-formed v2 root (the per-artifact check reports it located).
pub fn entity_object_type(payload: &EntityPayload) -> Result<Option<EntityObjectType>, String> {
    reject_lenient_leftovers(payload.unknown_fields(), payload.has_invalid_extensions())?;
    let value = serde_json::to_value(payload.to_json_value()).map_err(|error| error.to_string())?;
    if payload.is_entity_ir_v2() {
        // Selected exactly as the per-artifact check does: an `objectType`
        // payload decodes into the strict root, so its error is located.
        if payload.object_type.as_value().is_some() {
            let root =
                serde_json::from_value::<EntityRootV2>(value).map_err(|error| error.to_string())?;
            return Ok(Some(EntityObjectType {
                form: EntityForm::V2,
                root,
            }));
        }
        return match serde_json::from_value::<EntityRoot>(value) {
            Ok(
                EntityRoot::V2Interface(_)
                | EntityRoot::V2Extension(_)
                | EntityRoot::V2PropertyTypes(_)
                | EntityRoot::V2Markings(_),
            ) => Ok(None),
            Ok(EntityRoot::V2(root)) => Ok(Some(EntityObjectType {
                form: EntityForm::V2,
                root,
            })),
            Ok(EntityRoot::V1(_)) => {
                Err("an EntityIR v2 payload decoded as EntityIR v1".to_owned())
            }
            Err(error) => Err(error.to_string()),
        };
    }
    let v1 = serde_json::from_value::<EntityRootV1>(value).map_err(|error| error.to_string())?;
    Ok(Some(EntityObjectType {
        form: EntityForm::V1,
        root: lower_entity_v1(&v1),
    }))
}

/// Read a `relation` payload as the link it lowers to (see
/// [`lower_relation_v1`]); `name` is the relation artifact's envelope `name`.
///
/// # Errors
///
/// The decode error, when the payload is not a well-formed v1 relation root.
pub fn relation_link(name: &str, payload: &RelationPayload) -> Result<LoweredRelation, String> {
    reject_lenient_leftovers(payload.unknown_fields(), payload.has_invalid_extensions())?;
    let value = serde_json::to_value(payload.to_json_value()).map_err(|error| error.to_string())?;
    let root = serde_json::from_value::<RelationRoot>(value).map_err(|error| error.to_string())?;
    Ok(lower_relation_v1(name, &root))
}

/// The lenient wire root drops unknown members and a malformed `extensions`
/// from its JSON view, so the strict decode must reject them itself.
fn reject_lenient_leftovers(
    unknown_fields: &std::collections::BTreeSet<String>,
    invalid_extensions: bool,
) -> Result<(), String> {
    if let Some(member) = unknown_fields.iter().next() {
        return Err(format!("unknown payload member `{member}`"));
    }
    if invalid_extensions {
        return Err("payload member `extensions` must be an object".to_owned());
    }
    Ok(())
}

/// The deprecation warning for an `entity` artifact in the v1 form, or `None`
/// for any other artifact (a v2 entity, or another kind).
#[must_use]
pub fn entity_v1_deprecation(artifact: &ArtifactEnvelope) -> Option<norn_ir::Diagnostic> {
    let ArtifactPayload::Entity(payload) = &artifact.payload else {
        return None;
    };
    if payload.is_entity_ir_v2() {
        return None;
    }
    Some(norn_ir::Diagnostic {
        code: ENTITY_V1_DEPRECATED.to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "entity {} uses the deprecated EntityIR v1 payload form; it is accepted during the migration and lowered to EntityIR v2 on read; author new versions with entityVersion \"2.0\"",
            artifact.artifact_id
        ),
        artifact_id: Some(artifact.artifact_id.clone()),
        path: Some("payload".to_owned()),
        remediation: Some(DiagnosticRemediation::UpgradeIr),
    })
}
