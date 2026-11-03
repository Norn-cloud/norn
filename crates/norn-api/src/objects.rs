//! Typed object reads for Studio's Data Explorer (#825): one concept's page of
//! objects, one object, its link neighbourhood and its source lineage.
//!
//! Each document is the wire projection of a `norn_cell` authorized read. The
//! host decides what a principal may see before projecting; nothing here
//! grants access. Property values keep the cell's exact JSON number tokens.
//! No physical table, link table, row locator or SQL crosses this boundary
//! (#733).

use norn_cell::read::{LinkEdgeRecord, LinkReadPage};
use norn_cell::{AuthoritativePage, ObjectRecord};
use norn_ir::RuntimeValue;
use serde::Serialize;
use std::collections::BTreeMap;
use utoipa::ToSchema;
use utoipa::openapi::schema::{AdditionalProperties, Object, ObjectBuilder, Ref, SchemaType};

/// One object as the cell stores it, at its current version.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornObjectRecord {
    /// The concrete object type.
    pub object_type: String,
    /// The object's identity.
    pub id: String,
    /// The tenant scope.
    pub scope: String,
    /// The owner version (`+1` per write).
    pub version: u64,
    /// Every stored property by property id; decimals are exact JSON number tokens.
    #[schema(schema_with = values_schema)]
    pub values: BTreeMap<String, RuntimeValue>,
}

impl From<ObjectRecord> for NornObjectRecord {
    fn from(record: ObjectRecord) -> Self {
        Self {
            object_type: record.object_type,
            id: record.id,
            scope: record.scope,
            version: record.version,
            values: record.values,
        }
    }
}

/// One bounded page of a concept's authoritative objects.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornObjectPage {
    /// The concept read; records may be of its subtypes.
    pub object_type: String,
    /// Authoritative records on this page.
    pub records: Vec<NornObjectRecord>,
    /// Candidates on this page held back by open quarantining findings.
    pub quarantine_count: u64,
    /// The last scanned identity: the next page starts after it.
    #[schema(required = true, nullable = true)]
    pub scanned_through: Option<String>,
}

impl NornObjectPage {
    /// Project one `authoritative_page` read of `object_type`.
    #[must_use]
    pub fn new(object_type: &str, page: AuthoritativePage) -> Self {
        Self {
            object_type: object_type.to_owned(),
            records: page.records.into_iter().map(Into::into).collect(),
            quarantine_count: u64::try_from(page.quarantine_count).unwrap_or(u64::MAX),
            scanned_through: page.scanned_through,
        }
    }
}

/// One live link touching the requested object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornLinkEdge {
    /// The link's declaring (source) type.
    pub source_type: String,
    /// The link id.
    pub link: String,
    /// The source object id.
    pub source: String,
    /// The declared target type.
    pub target_type: String,
    /// The target object id.
    pub target: String,
    /// The link version.
    pub version: u64,
}

impl From<LinkEdgeRecord> for NornLinkEdge {
    fn from(record: LinkEdgeRecord) -> Self {
        Self {
            source_type: record.edge.source_type,
            link: record.edge.link,
            source: record.edge.source,
            target_type: record.edge.target_type,
            target: record.edge.target,
            version: record.version,
        }
    }
}

/// One bounded page of live links touching an object, in both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornLinkPage {
    /// The requested object's type.
    pub object_type: String,
    /// The requested object's identity.
    pub id: String,
    /// Live links on this page.
    pub items: Vec<NornLinkEdge>,
    /// Whether more candidate links remain after this page.
    pub has_more: bool,
}

impl NornLinkPage {
    /// Project one `links_touching_page` read; the physical keyset position stays server-side.
    #[must_use]
    pub fn new(object_type: &str, id: &str, page: LinkReadPage) -> Self {
        Self {
            object_type: object_type.to_owned(),
            id: id.to_owned(),
            items: page.items.into_iter().map(Into::into).collect(),
            has_more: page.next_after.is_some() || page.has_more_candidates,
        }
    }
}

/// The exact admitted mapping declaration that produced an object version.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornMappingPin {
    /// Mapping artifact ID.
    pub artifact_id: String,
    /// Admitted artifact version.
    pub version: String,
    /// Canonical artifact digest.
    pub sha256: String,
}

/// Where one object version came from: the source record, its document and the
/// mapping that produced it. Describes the JSON of
/// `norn_cell::source_provenance::InstanceSourceProvenance`; it grants no authority.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornInstanceSourceProvenance {
    /// Content address of the recomputed mapping bundle.
    pub bundle_digest: String,
    /// The source record the mapping names.
    #[schema(schema_with = source_record_schema)]
    pub source_record_ref: serde_json::Value,
    /// The original source document in the sealed envelope.
    #[schema(schema_with = source_record_schema)]
    pub document_ref: serde_json::Value,
    /// Digest of the source document payload.
    pub document_digest: String,
    /// The mapping declaration that produced the version.
    pub mapping_pin: NornMappingPin,
    /// Field-level source and rule provenance, by property id.
    #[schema(schema_with = field_provenance_schema)]
    pub field_provenance: BTreeMap<String, serde_json::Value>,
}

// `schema_with` replaces the field's schema, doc comment included, so each
// helper restates its field's description.
fn values_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(utoipa::openapi::Type::Object))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "Every stored property by property id; decimals are exact JSON number tokens.",
        ))
        .build()
}

fn source_record_schema() -> Ref {
    Ref::builder()
        .ref_location_from_schema_name("NornIrSourceRecordRef")
        .description(Some("A source system and the record's identity in it."))
        .build()
}

fn field_provenance_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(utoipa::openapi::Type::Object))
        .additional_properties(Some(AdditionalProperties::RefOr(
            Ref::from_schema_name("NornIrInstanceProvenance").into(),
        )))
        .description(Some(
            "Field-level source and rule provenance, by property id.",
        ))
        .build()
}
