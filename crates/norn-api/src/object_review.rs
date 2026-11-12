//! HOST-READ-6 (#588, #343): an object's quality findings and its version
//! history, the reads behind "why is this value what it is, and who changed it".
//!
//! Both are value-level and need Population, Values and Sources on the row,
//! the same capabilities as its lineage (D67). Unlike an authoritative read,
//! they also serve a quarantined object: OBS-EVIDENCE-1 keeps imported source
//! truth readable to those allowed to review it, and a finding without its
//! subject cannot be reviewed. A withheld, source-withheld or absent object
//! refuses with the same opaque 404 as lineage.
//!
//! The history lists every proven version of the row, newest first, with the
//! receipt that committed it and what wrote it: a mapping (the version's
//! source lineage, as `/lineage?version=` serves it) or an admitted action
//! (the action-call record of that commit: which action, which verified actor,
//! and the effective input, which carries a correction's reason and evidence).

use std::collections::BTreeMap;

use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::read::LinkDirection;
use norn_cell::source_provenance::InstanceSourceProvenance;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::{DataQualityFinding, FactRef, RuntimeValue};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa::openapi::Ref;
use utoipa::openapi::schema::{
    AdditionalProperties, ArrayBuilder, Object, ObjectBuilder, SchemaType,
};

use crate::authz::ReadCapability::{Population, Sources, Values};
use crate::host::DevHost;
use crate::object_routes::{
    MAX_QUERY_SCAN, ObjectAccess, SCAN_BATCH, collection, locked, not_found, permitted,
};
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// Most versions one history lists, newest first.
pub const MAX_HISTORY_VERSIONS: u64 = 64;
/// Most evidence links one findings read lists.
pub const MAX_EVIDENCE: usize = 200;
/// The core interface of evidence objects (`registry/core/src/evidence.ts`).
pub const SOURCE_EVIDENCE: &str = "norn.SourceEvidence";

/// One evidence object linked to the subject: where one of its values was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornEvidenceRef {
    /// The evidence object's type (it implements `norn.SourceEvidence`).
    pub object_type: String,
    /// The evidence object's id.
    pub id: String,
}

/// An object's quality findings (OBS-EVIDENCE-1), open and resolved.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectFindings {
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// Whether an open finding holds the object out of authoritative reads.
    pub quarantined: bool,
    /// Every finding on the object, each pinned to the rule it records; a
    /// correction resolves a finding without rewriting it.
    #[schema(schema_with = findings_schema)]
    pub findings: Vec<DataQualityFinding>,
    /// The evidence objects linked to it that the caller may read (types
    /// implementing `norn.SourceEvidence`), at most [`MAX_EVIDENCE`]. They are
    /// read at `/v1/objects/{type}/{id}`; a quarantined subject's evidence is
    /// not quarantined with it.
    pub evidence: Vec<NornEvidenceRef>,
}

/// What wrote one version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NornVersionWriter {
    /// A mapping produced the version from a source record.
    #[serde(rename_all = "camelCase")]
    Mapping {
        /// The version's source lineage (`/lineage?version=`).
        #[schema(schema_with = lineage_schema)]
        lineage: InstanceSourceProvenance,
    },
    /// An admitted action committed the version.
    #[serde(rename_all = "camelCase")]
    Action {
        /// The action identity.
        action_id: String,
        /// The verified caller the server attributed the call to.
        actor: String,
        /// The effective input (caller input plus server derivations).
        #[schema(schema_with = free_form)]
        input: RuntimeValue,
    },
    /// Another writer (an import, a migration or a kernel rule).
    Other,
}

/// One proven version of an object.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectVersion {
    /// The version number.
    pub version: u64,
    /// The commit (ledger receipt sequence) that wrote it.
    pub commit: u64,
    /// The commit's logical time, in milliseconds.
    pub logical_time: u64,
    /// Every stored property by property id; null for a tombstone.
    #[schema(schema_with = values_schema)]
    pub values: Option<BTreeMap<String, RuntimeValue>>,
    /// What wrote it.
    pub writer: NornVersionWriter,
}

/// An object's versions, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectHistory {
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The versions listed, newest first.
    pub versions: Vec<NornObjectVersion>,
    /// True when older versions exist beyond [`MAX_HISTORY_VERSIONS`].
    pub truncated: bool,
}

// `schema_with` replaces the field's schema, doc comment included, so each
// helper restates its field's description.
fn findings_schema() -> utoipa::openapi::schema::Array {
    ArrayBuilder::new()
        .items(Ref::from_schema_name("NornIrDataQualityFinding"))
        .description(Some(
            "Every finding on the object, each pinned to the rule it records; a correction resolves a finding without rewriting it.",
        ))
        .build()
}

fn lineage_schema() -> Ref {
    Ref::builder()
        .ref_location_from_schema_name("NornInstanceSourceProvenance")
        .description(Some("The version's source lineage (`/lineage?version=`)."))
        .build()
}

fn free_form() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(utoipa::openapi::Type::Object))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "The effective input (caller input plus server derivations); decimals are exact JSON number tokens.",
        ))
        .build()
}

fn values_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::from_iter([
            utoipa::openapi::Type::Object,
            utoipa::openapi::Type::Null,
        ]))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "Every stored property by property id, null for a tombstone; decimals are exact JSON number tokens.",
        ))
        .build()
}

/// The raw row, when the caller holds Population, Values and Sources on it.
/// A quarantined row is served: review reads keep source truth.
fn reviewable<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    (object_type, id): (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<ObjectRecord, Refusal> {
    let row = locked(host, access, start, |reader| {
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .object(MAIN_WORLD, object_type, id)
            .map_err(Refusal::host)
    })?
    .ok_or_else(not_found)?;
    if [Population, Values, Sources]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &row, capability))
    {
        Ok(row)
    } else {
        Err(not_found())
    }
}

/// HOST-READ-6: an object's quality findings, including while it is quarantined.
///
/// # Errors
/// A denied gate (the opaque 403); an absent, withheld or source-withheld
/// object (one 404); a commit between the reads (409 `object.snapshot_moved`);
/// or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}/{id}/findings", operation_id = "get_object_findings", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded."), ("id" = String, Path, description = "The object identity; percent-escapes are decoded.")), responses((status = 200, description = "The object's quality findings, open and resolved, and whether it is quarantined.", body = NornObjectFindings), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, a query over the host bound, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the object is absent, withheld or source-withheld; these are not told apart. A quarantined object is served.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed between the reads.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
pub fn get_object_findings<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<NornObjectFindings, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    let (mut document, linked) = locked(host, access, &mut start, |reader| {
        let page = reader
            .links_touching_page(
                MAIN_WORLD,
                &row.object_type,
                &row.id,
                LinkDirection::In,
                None,
                None,
                MAX_EVIDENCE,
            )
            .map_err(Refusal::host)?;
        let linked: Vec<(String, String)> = page
            .items
            .into_iter()
            .filter(|edge| reader.model().is_a(&edge.edge.source_type, SOURCE_EVIDENCE))
            .map(|edge| (edge.edge.source_type, edge.edge.source))
            .collect();
        Ok((
            NornObjectFindings {
                object_type: row.object_type.clone(),
                id: row.id.clone(),
                quarantined: reader
                    .observation_is_quarantined(MAIN_WORLD, &row)
                    .map_err(Refusal::host)?,
                findings: reader
                    .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                    .map_err(Refusal::host)?,
                evidence: Vec::new(),
            },
            linked,
        ))
    })?;
    // Each evidence row is disclosed on its own Population and Values.
    for (object_type, id) in linked {
        let evidence = locked(host, access, &mut start, |reader| {
            reader
                .object(MAIN_WORLD, &object_type, &id)
                .map_err(Refusal::host)
        })?;
        if let Some(evidence) = evidence
            && [Population, Values]
                .into_iter()
                .all(|capability| permitted(host, access, &actor, &evidence, capability))
        {
            document.evidence.push(NornEvidenceRef { object_type, id });
        }
    }
    Ok(document)
}

fn budget() -> DecodeBudget {
    DecodeBudget::new(&ReactiveLimits::from_profile(
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    ))
}

/// The action-call record written in `commit`, if an action wrote it.
fn action_writer<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    receipt: &norn_cell::Receipt,
) -> Result<Option<NornVersionWriter>, Refusal> {
    let Some(call) = receipt
        .body
        .written
        .iter()
        .find(|row| row.object_type == norn_kernel::ACTION_CALL_TYPE)
    else {
        return Ok(None);
    };
    let Some(record) = reader
        .object_at_version(MAIN_WORLD, &call.object_type, &call.key, call.version)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    let text = |name: &str| {
        record
            .values
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
    };
    let (Some(action_id), Some(actor)) = (text("actionId"), text("actor")) else {
        return Ok(None);
    };
    Ok(Some(NornVersionWriter::Action {
        action_id,
        actor,
        input: record
            .values
            .get("effectiveInput")
            .cloned()
            .unwrap_or(RuntimeValue::Null),
    }))
}

/// One version with its receipt and writer.
fn version_entry<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
    version: u64,
) -> Result<NornObjectVersion, Refusal> {
    let receipt = reader
        .receipt_for_fact_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?;
    let values = reader
        .object_at_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?
        .map(|record| record.values.into_iter().collect());
    let fact = FactRef::new(row.object_type.as_str(), &row.id, version);
    let writer = match reader
        .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
        .map_err(Refusal::host)?
    {
        Some(lineage) => NornVersionWriter::Mapping { lineage },
        None => action_writer(reader, &receipt)?.unwrap_or(NornVersionWriter::Other),
    };
    Ok(NornObjectVersion {
        version,
        commit: receipt.body.commit_seq,
        logical_time: receipt.body.logical_time,
        values,
        writer,
    })
}

/// HOST-READ-6: an object's proven versions, newest first, with what wrote each.
///
/// # Errors
/// A denied gate (the opaque 403); an absent, withheld or source-withheld
/// object (one 404); a commit between the reads (409 `object.snapshot_moved`);
/// or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}/{id}/history", operation_id = "get_object_history", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded."), ("id" = String, Path, description = "The object identity; percent-escapes are decoded.")), responses((status = 200, description = "The object's proven versions, newest first, each with its commit and what wrote it.", body = NornObjectHistory), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, a query over the host bound, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the object is absent, withheld or source-withheld; these are not told apart. A quarantined object is served.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed between the reads.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
pub fn get_object_history<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<NornObjectHistory, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    let oldest = row
        .version
        .saturating_sub(MAX_HISTORY_VERSIONS)
        .saturating_add(1)
        .max(1);
    locked(host, access, &mut start, |reader| {
        let versions = (oldest..=row.version)
            .rev()
            .map(|version| version_entry(reader, &row, version))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(NornObjectHistory {
            object_type: row.object_type.clone(),
            id: row.id.clone(),
            versions,
            truncated: oldest > 1,
        })
    })
}

/// One quarantined object the caller may review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornQuarantinedObject {
    /// The object id.
    pub id: String,
    /// Open findings that hold it out of authoritative reads.
    pub open_findings: u64,
}

/// A type's quarantined objects, as the caller may see them.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornQuarantinedPage {
    /// The concept read.
    pub object_type: String,
    /// Quarantined rows whose values the caller may read, in id order. A row
    /// whose values are withheld is never named: quarantine is value-level.
    pub items: Vec<NornQuarantinedObject>,
    /// True when the scan bound was reached: the list covers only the rows read.
    pub approximate: bool,
    /// The row scan bound.
    pub scan_limit: u64,
}

/// HOST-READ-6: the ids of a type's quarantined objects the caller may review,
/// so a grid's quarantine count can lead to them.
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared type (404), a commit while
/// the rows were read (409 `object.snapshot_moved`), or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}:quarantined", operation_id = "get_quarantined_objects", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded.")), responses((status = 200, description = "The type's quarantined objects whose values the caller may read, with their open finding counts.", body = NornQuarantinedPage), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the type is not declared.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed while the rows were read.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
pub fn get_quarantined_objects<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
) -> Result<NornQuarantinedPage, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let mut after: Option<String> = None;
    let mut scanned = 0usize;
    let mut candidates: Vec<(ObjectRecord, u64)> = Vec::new();
    let exhausted = loop {
        let (batch, done) = locked(host, access, &mut start, |reader| {
            if !reader.model().has_type(object_type) {
                return Err(not_found());
            }
            let rows = reader
                .list_after(MAIN_WORLD, object_type, after.as_deref(), SCAN_BATCH)
                .map_err(Refusal::host)?;
            let done = rows.len() < SCAN_BATCH;
            let mut held = Vec::new();
            for row in rows {
                if reader
                    .observation_is_quarantined(MAIN_WORLD, &row)
                    .map_err(Refusal::host)?
                {
                    let open = reader
                        .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                        .map_err(Refusal::host)?
                        .iter()
                        .filter(|finding| finding.status == norn_ir::DataQualityFindingStatus::Open)
                        .count();
                    held.push((row, u64::try_from(open).unwrap_or(u64::MAX)));
                } else {
                    held.push((row, 0));
                }
            }
            Ok((held, done))
        })?;
        scanned = scanned.saturating_add(batch.len());
        after = batch.last().map(|(row, _)| row.id.clone());
        candidates.extend(batch.into_iter().filter(|(_, open)| *open > 0));
        if done {
            break true;
        }
        if scanned >= MAX_QUERY_SCAN {
            break false;
        }
    };
    // Authorization runs outside the cell lock, row by row.
    let items = candidates
        .into_iter()
        .filter(|(row, _)| {
            [Population, Values]
                .into_iter()
                .all(|capability| permitted(host, access, &actor, row, capability))
        })
        .map(|(row, open_findings)| NornQuarantinedObject {
            id: row.id,
            open_findings,
        })
        .collect();
    Ok(NornQuarantinedPage {
        object_type: object_type.to_owned(),
        items,
        approximate: !exhausted,
        scan_limit: u64::try_from(MAX_QUERY_SCAN).unwrap_or(u64::MAX),
    })
}

/// HOST-READ-6: where one version of an object came from, by version.
///
/// # Errors
/// As [`get_object_findings`]; a version that a mapping did not write, or
/// that does not exist, is the same 404.
pub fn get_object_lineage_at<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    (object_type, id, version): (&str, &str, u64),
    access: &dyn ObjectAccess,
) -> Result<InstanceSourceProvenance, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    if version == 0 || version > row.version {
        return Err(not_found());
    }
    let fact = FactRef::new(row.object_type.as_str(), &row.id, version);
    locked(host, access, &mut start, |reader| {
        reader
            .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
            .map_err(Refusal::host)?
            .ok_or_else(not_found)
    })
}
