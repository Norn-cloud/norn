//! Schema projection of the native trace; the engine serializes the actual document.
use std::collections::BTreeMap;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        Ref, RefOr,
        schema::{AdditionalProperties, ObjectBuilder, Schema},
    },
};

/// LINEAGE-SUM-1: exact numeric membership with independently disclosed evidence.
#[derive(Debug, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornNumericTrace {
    /// Revalidated exact result at its original frontier.
    pub result: crate::answers::NornQueryResult,
    /// Canonical selected bucket key.
    pub bucket: String,
    /// Ordered original fact incarnations.
    pub contributions: Vec<NornNumericContribution>,
}
/// One original numeric member; enrichment does not enter this list.
#[derive(Debug, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornNumericContribution {
    /// Original object key/type/version.
    #[schema(schema_with = fact_schema)]
    pub fact: serde_json::Value,
    /// Sum or numerator/denominator; raw exact JSON numbers.
    #[schema(schema_with = crate::answers::amounts_schema)]
    pub amounts: Vec<Option<String>>,
    /// Emitting receipt identity; no export capability.
    pub receipt_hash: String,
    /// Full incarnation digest, disclosed with canonical values.
    #[schema(required = true, nullable = true)]
    pub fact_digest: Option<String>,
    /// Independent source state.
    #[schema(schema_with = closed_source_schema)]
    pub source: NornContributionSource,
    /// Writer's immutable commit position.
    pub commit_seq: u64,
    /// Receipt-authenticated knowledge time.
    pub recorded_at_ms: u64,
    /// Canonical property content was withheld.
    pub restricted: bool,
    /// Complete canonical values, only under their own grant.
    #[schema(required = true, nullable = true)]
    pub values: Option<BTreeMap<String, serde_json::Value>>,
}
/// A source grant never follows from membership or canonical-value disclosure.
#[derive(Debug, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NornContributionSource {
    /// Authenticated source bundle was resolved.
    Resolved {
        /// Original semantic source edges.
        provenance: Box<NornInstanceSourceProvenance>,
    },
    /// Source disclosure was denied.
    Restricted,
    /// No retained source bundle exists.
    Unavailable,
}
/// Exact Core 07 source evidence; no physical storage locators.
#[derive(Debug, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornInstanceSourceProvenance {
    /// Recomputed input bundle digest.
    pub bundle_digest: String,
    /// Mapped source-record identity.
    #[schema(schema_with = source_schema)]
    pub source_record_ref: serde_json::Value,
    /// Sealed document identity.
    #[schema(schema_with = source_schema)]
    pub document_ref: serde_json::Value,
    /// Sealed document digest.
    pub document_digest: String,
    /// Producing mapping's exact declaration pin.
    pub mapping_pin: NornMappingPin,
    /// Actual field-level transforms and source references.
    #[schema(schema_with = fields_schema)]
    pub field_provenance: serde_json::Value,
}
/// One admitted mapping declaration.
#[derive(Debug, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NornMappingPin {
    /// Artifact identity.
    pub artifact_id: String,
    /// Artifact version.
    pub version: String,
    /// Canonical declaration digest.
    pub sha256: String,
}
fn fact_schema() -> Ref {
    Ref::from_schema_name("NornIrFactRef")
}
fn source_schema() -> Ref {
    Ref::from_schema_name("NornIrSourceRecordRef")
}
fn fields_schema() -> Schema {
    ObjectBuilder::new()
        .additional_properties(Some(AdditionalProperties::RefOr(
            Ref::from_schema_name("NornIrInstanceProvenance").into(),
        )))
        .build()
        .into()
}

fn closed_source_schema() -> RefOr<Schema> {
    let mut source = NornContributionSource::schema();
    if let RefOr::T(Schema::OneOf(schema)) = &mut source {
        for item in &mut schema.items {
            if let RefOr::T(Schema::Object(variant)) = item {
                variant.additional_properties = Some(AdditionalProperties::FreeForm(false));
            }
        }
    }
    source
}
