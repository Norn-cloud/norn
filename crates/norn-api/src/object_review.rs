//! HOST-READ-6 (#588, #343): an object's quality findings and its version
//! history, the reads behind "why is this value what it is, and who changed it".
//!
//! Both are value-level. The findings need Population, Values and Sources on
//! the row, the same capabilities as its lineage (D67); the history needs
//! Population and Values, and names a version's source lineage only to a
//! caller who also holds Sources on that version. Unlike an authoritative read, they also
//! serve a quarantined object: OBS-EVIDENCE-1 keeps imported source truth
//! readable to those allowed to review it, and a finding without its subject
//! cannot be reviewed. A refused object refuses with the same opaque 404 as
//! lineage.
//!
//! The history lists the proven versions of the row the caller may read,
//! newest first, with the receipt that committed it and what wrote it. Each
//! version is decided on its own record, not on the current row: a version
//! the caller could not read is not listed, nor is one that changed only what
//! the caller may not see. What wrote a version is a mapping (the version's
//! source lineage, as `/lineage?version=` serves it, under that version's
//! Sources) or an
//! admitted action (the action-call record of that commit: which action and
//! which verified actor). An action's call record is disclosed under its own
//! read decision: a caller who may not read it (Population and Values on that
//! record) is told no writer. The call's effective input is never listed: the
//! kernel writes it as a plain fact, and it can carry values (a derived value,
//! a prepared input) whose disclosure the call record's read does not decide.

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
use crate::object_links::{GatedFinding, disclosed_findings, gate_findings};
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
        /// Deprecated: always `{}`. The call's effective input is never
        /// disclosed (see the module docs); the property stays, empty, so the
        /// frozen contract keeps its shape.
        #[schema(schema_with = withheld_input_schema)]
        input: WithheldInput,
        /// Optional provenance trace context recorded on the action call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        trace: Option<crate::actions::TraceContext>,
    },
    /// Another writer (an import, a migration or a kernel rule), or one this
    /// caller is not told: an action whose call record the caller may not
    /// read, a mapping whose lineage the caller may not read (no Sources), or
    /// a marked version.
    Other,
}

/// The action writer's deprecated `input`: it serializes as the empty object
/// and carries nothing, so no call input can reach a history reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WithheldInput;

impl Serialize for WithheldInput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        serializer.serialize_map(Some(0))?.end()
    }
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
    /// The versions the caller may read, newest first. A version is listed
    /// only when the caller holds Population and Values on that version's own
    /// record, and only when it changed a value the caller may see.
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

fn withheld_input_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(utoipa::openapi::Type::Object))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .deprecated(Some(utoipa::openapi::Deprecated::True))
        .description(Some(
            "Deprecated: always the empty object. The call's effective input is never disclosed.",
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
    object: (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<ObjectRecord, Refusal> {
    let row = raw_row(host, access, object, start)?;
    if permitted(host, access, actor, &row, Sources) {
        readable(host, access, actor, row)
    } else {
        Err(not_found())
    }
}

/// `row`, when the caller holds Population and Values on it; otherwise the
/// opaque 404.
fn readable<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    row: ObjectRecord,
) -> Result<ObjectRecord, Refusal> {
    if [Population, Values]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &row, capability))
    {
        Ok(row)
    } else {
        Err(not_found())
    }
}

/// The raw row, quarantined or not, before any read decision.
fn raw_row<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    (object_type, id): (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<ObjectRecord, Refusal> {
    locked(host, access, start, |reader| {
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .object(MAIN_WORLD, object_type, id)
            .map_err(Refusal::host)
    })?
    .ok_or_else(not_found)
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
    let (mut document, linked, disputes, gated) = locked(host, access, &mut start, |reader| {
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
                findings: Vec::new(),
                evidence: Vec::new(),
            },
            linked,
            reader
                .source_disagreements(MAIN_WORLD, &row.object_type, &row.id, None)
                .map_err(Refusal::host)?,
            gate_findings(
                reader,
                reader
                    .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                    .map_err(Refusal::host)?,
            )
            .map_err(Refusal::host)?,
        ))
    })?;
    // OBS-LINK-DISPUTE-1: a held link is disclosed as the link would be,
    // at the snapshot the findings were read at.
    document.findings = disclosed_findings(host, access, &actor, &mut start, vec![((), gated)])?
        .into_iter()
        .flat_map(|((), findings)| findings)
        .collect();
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
    // SOURCE-AGREE-1: a disagreement is disclosed only where the caller may
    // read both versions it names: their values and their sources.
    for disputed in disputes {
        if [&disputed.earlier, &disputed.later]
            .into_iter()
            .all(|version| {
                [Population, Values, Sources]
                    .into_iter()
                    .all(|capability| permitted(host, access, &actor, version, capability))
            })
        {
            document.findings.push(disputed.finding);
        }
    }
    Ok(document)
}

fn budget() -> DecodeBudget {
    DecodeBudget::new(&ReactiveLimits::from_profile(
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    ))
}

/// The action-call record written in `commit`, if an action wrote it and
/// the caller may read it.
///
/// The writer's action and actor are the call record's own values, so they
/// are disclosed only under the read decision that covers that record:
/// Population and Values on it, unmarked. Otherwise the version names no
/// writer ([`NornVersionWriter::Other`]). The call's effective input is never
/// disclosed here: the kernel stores it as one plain value even when
/// preparation or a derivation put restricted values in it, so no read of the
/// call record establishes its disclosure.
fn action_writer<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
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
    let disclosed = [Population, Values]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &record, capability));
    if !disclosed {
        return Ok(None);
    }
    let catalog = host.marking_catalog(reader)?;
    let marked = crate::privacy::version_markings(reader, &catalog, &record)?
        .is_none_or(|markings| markings.values().any(|ids| !ids.is_empty()));
    if marked {
        return Ok(None);
    }
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
    let Ok(trace) = decode_writer_trace(&record.values) else {
        return Ok(None);
    };
    Ok(Some(NornVersionWriter::Action {
        action_id,
        actor,
        input: WithheldInput,
        trace,
    }))
}

fn decode_writer_trace(
    values: &BTreeMap<String, RuntimeValue>,
) -> Result<Option<crate::actions::TraceContext>, ()> {
    match values.get("trace") {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => {
            let decoded: norn_ir::TraceContext =
                norn_ir::runtime::decode(value).map_err(|_decode| ())?;
            decoded.validate().map_err(|_invalid| ())?;
            Ok(Some(crate::actions::TraceContext::from(decoded)))
        }
    }
}

/// The read decision over one version: the version's own record (the row
/// it deletes, for a tombstone) holds Population and Values for the caller,
/// and whether it also holds Sources. `None` withholds the whole version.
///
/// Each version is decided on its own row, never on the current one: a
/// permission field the row once held is the one its earlier values were
/// written under, and a grant made later does not open them.
fn version_read<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
    version: u64,
) -> Result<Option<(Option<ObjectRecord>, bool)>, Refusal> {
    let stored = reader
        .object_at_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?;
    let decided_on = match (&stored, version.checked_sub(1).filter(|before| *before > 0)) {
        (Some(record), _) => Some(record.clone()),
        (None, Some(before)) => reader
            .object_at_version(MAIN_WORLD, &row.object_type, &row.id, before)
            .map_err(Refusal::host)?,
        (None, None) => None,
    };
    let Some(decided_on) = decided_on else {
        return Ok(None);
    };
    let readable = [Population, Values]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &decided_on, capability));
    Ok(readable.then(|| (stored, permitted(host, access, actor, &decided_on, Sources))))
}

/// One version with its receipt and writer, or `None` when the caller may not
/// read it ([`version_read`]); its source lineage only under that version's
/// Sources.
fn version_entry<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
    version: u64,
) -> Result<Option<NornObjectVersion>, Refusal> {
    let Some((stored, sources)) = version_read((host, access, actor), reader, row, version)? else {
        return Ok(None);
    };
    let receipt = reader
        .receipt_for_fact_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?;
    let catalog = host.marking_catalog(reader)?;
    // PRIVACY-1: each version's values are projected under that version's
    // own markings; a marked version names no writer input or source row.
    let mut marked = false;
    let values = match stored {
        Some(mut record) => {
            let markings = crate::privacy::version_markings(reader, &catalog, &record)?;
            marked = markings
                .as_ref()
                .is_none_or(|markings| markings.values().any(|ids| !ids.is_empty()));
            crate::privacy::project(host, actor, &catalog, markings.as_ref(), &mut record);
            Some(record.values.into_iter().collect())
        }
        None => None,
    };
    let fact = FactRef::new(row.object_type.as_str(), &row.id, version);
    let lineage = if marked || !sources {
        None
    } else {
        reader
            .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
            .map_err(Refusal::host)?
    };
    let writer = match lineage {
        _ if marked => NornVersionWriter::Other,
        Some(lineage) => NornVersionWriter::Mapping { lineage },
        None => action_writer((host, access, actor), reader, &receipt)?
            .unwrap_or(NornVersionWriter::Other),
    };
    Ok(Some(NornObjectVersion {
        version,
        commit: receipt.body.commit_seq,
        logical_time: receipt.body.logical_time,
        values,
        writer,
    }))
}

/// HOST-READ-6: an object's proven versions, newest first, with what wrote each.
///
/// # Errors
/// A denied gate (the opaque 403); an absent or withheld object (one 404;
/// Sources is not needed, and gates only each version's lineage); a commit
/// between the reads (409 `object.snapshot_moved`); or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}/{id}/history", operation_id = "get_object_history", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded."), ("id" = String, Path, description = "The object identity; percent-escapes are decoded.")), responses((status = 200, description = "The object's proven versions the caller may read, newest first, each with its commit and what wrote it.", body = NornObjectHistory), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, a query over the host bound, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the object is absent or withheld (no Population or Values); these are not told apart. Sources is not needed: without it no version names its source lineage. A quarantined object is served.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed between the reads.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
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
    // The history is the row's values over time: Population and Values on the
    // current row to read it at all, then each version's own decision.
    let row = raw_row(host, access, (object_type, id), &mut start)?;
    let row = readable(host, access, &actor, row)?;
    let oldest = row
        .version
        .saturating_sub(MAX_HISTORY_VERSIONS)
        .saturating_add(1)
        .max(1);
    locked(host, access, &mut start, |reader| {
        // Oldest first, so each version is compared with the last one listed.
        let mut versions: Vec<NornObjectVersion> = Vec::new();
        for version in oldest..=row.version {
            let Some(entry) = version_entry((host, access, &actor), reader, &row, version)? else {
                continue;
            };
            // A write that changed nothing the caller may see (only a withheld
            // field, or nothing) is not listed: its version, commit and time
            // would tell that a hidden value changed then.
            if versions
                .last()
                .is_some_and(|last| last.values == entry.values)
            {
                continue;
            }
            versions.push(entry);
        }
        versions.reverse();
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
    let mut candidates: Vec<(ObjectRecord, Vec<GatedFinding>)> = Vec::new();
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
                    let open: Vec<DataQualityFinding> = reader
                        .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                        .map_err(Refusal::host)?
                        .into_iter()
                        .filter(|finding| finding.status == norn_ir::DataQualityFindingStatus::Open)
                        .collect();
                    held.push((row, gate_findings(reader, open).map_err(Refusal::host)?));
                } else {
                    held.push((row, Vec::new()));
                }
            }
            Ok((held, done))
        })?;
        scanned = scanned.saturating_add(batch.len());
        after = batch.last().map(|(row, _)| row.id.clone());
        candidates.extend(batch.into_iter().filter(|(_, open)| !open.is_empty()));
        if done {
            break true;
        }
        if scanned >= MAX_QUERY_SCAN {
            break false;
        }
    };
    // Authorization runs outside the cell lock, row by row.
    let readable: Vec<_> = candidates
        .into_iter()
        .filter(|(row, _)| {
            [Population, Values]
                .into_iter()
                .all(|capability| permitted(host, access, &actor, row, capability))
        })
        .collect();
    // OBS-LINK-DISPUTE-1: a held link counts only where it is disclosed, at
    // the snapshot the rows were read at.
    let items = disclosed_findings(host, access, &actor, &mut start, readable)?
        .into_iter()
        .map(|(row, open)| NornQuarantinedObject {
            id: row.id,
            open_findings: u64::try_from(open.len()).unwrap_or(u64::MAX),
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
        let catalog = host.marking_catalog(reader)?;
        reader
            .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |instance| {
                crate::privacy::discloses_source(reader, &catalog, instance)
            })
            .map_err(Refusal::host)?
            .ok_or_else(not_found)
    })
}

#[cfg(test)]
mod trace_tests {
    use super::*;

    #[test]
    fn decode_writer_trace_fails_closed_on_corrupt_persisted_trace() -> Result<(), String> {
        let empty = BTreeMap::new();
        assert_eq!(decode_writer_trace(&empty), Ok(None));

        let explicit_null = BTreeMap::from([("trace".to_owned(), RuntimeValue::Null)]);
        assert_eq!(decode_writer_trace(&explicit_null), Ok(None));

        let valid_value =
            RuntimeValue::from_json_text(r#"{"traceId":"tr-1","intentRef":"goal:1"}"#)
                .map_err(|error| error.to_string())?;
        let valid = BTreeMap::from([("trace".to_owned(), valid_value)]);
        assert_eq!(
            decode_writer_trace(&valid),
            Ok(Some(crate::actions::TraceContext {
                trace_id: "tr-1".to_owned(),
                intent_ref: Some("goal:1".to_owned()),
                session_digest: None,
            }))
        );

        for bad in [
            r#"["tr-1"]"#,
            r#"{"traceId":""}"#,
            r#"{"traceId":"tr\u202e1"}"#,
            r#"{"traceId":"tr-1","sessionDigest":"raw transcript"}"#,
            r#"{"traceId":"tr-1","unknown":"x"}"#,
        ] {
            let value = RuntimeValue::from_json_text(bad).map_err(|error| error.to_string())?;
            let map = BTreeMap::from([("trace".to_owned(), value)]);
            assert_eq!(
                decode_writer_trace(&map),
                Err(()),
                "must fail closed on {bad}"
            );
        }
        Ok(())
    }
}
